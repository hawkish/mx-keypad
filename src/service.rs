//! Run `mx-keypad run` as a per-user LaunchAgent: started at login, restarted
//! if it exits, and responsible for itself, so macOS asks Accessibility
//! permission of mx-keypad rather than of a terminal or tmux server.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// The LaunchAgent's label, after the user's bundle prefix.
const LABEL: &str = "dk.mortenhogh.mx-keypad";

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

fn plist_path() -> Result<PathBuf> {
    Ok(home()?.join(format!("Library/LaunchAgents/{LABEL}.plist")))
}

/// Where the agent's output goes.
pub fn log_path() -> Result<PathBuf> {
    Ok(home()?.join("Library/Logs/mx-keypad.log"))
}

fn domain() -> String {
    // SAFETY: getuid has no failure mode.
    format!("gui/{}", unsafe { libc_getuid() })
}

extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The agent's property list, running `exe run --config config`.
fn plist(exe: &Path, config: &Path, log: &Path) -> String {
    let arg = |p: &Path| xml_escape(&p.display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>run</string>
    <string>--config</string>
    <string>{config}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ThrottleInterval</key>
  <integer>10</integer>
  <key>ProcessType</key>
  <string>Interactive</string>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
        exe = arg(exe),
        config = arg(config),
        log = arg(log),
    )
}

fn launchctl(args: &[&str]) -> Result<bool> {
    let status = Command::new("launchctl")
        .args(args)
        .status()
        .context("cannot run launchctl")?;
    Ok(status.success())
}

/// Where `install` keeps its signed copy of the executable. A fixed path
/// and a stable signature keep the Accessibility permission across rebuilds.
fn installed_exe() -> Result<PathBuf> {
    Ok(home()?.join("Library/Application Support/mx-keypad/mx-keypad"))
}

/// The signing identity to use: `requested` when given, else the first
/// "Apple Development" identity in the keychain, else the first
/// "Developer ID Application" one. `None` means ad-hoc signing.
fn signing_identity(requested: Option<&str>) -> Result<Option<String>> {
    if let Some(requested) = requested {
        return Ok(Some(requested.to_string()));
    }
    let output = Command::new("security")
        .args(["find-identity", "-v", "-p", "codesigning"])
        .output()
        .context("cannot run security")?;
    let listing = String::from_utf8_lossy(&output.stdout);
    Ok(pick_identity(&listing))
}

/// The SHA-1 of the preferred identity in `security find-identity` output.
fn pick_identity(listing: &str) -> Option<String> {
    let identities: Vec<(&str, &str)> = listing
        .lines()
        .filter_map(|line| {
            let (_, rest) = line.split_once(") ")?;
            let (hash, name) = rest.split_once(' ')?;
            Some((hash, name.trim_matches('"')))
        })
        .collect();
    ["Apple Development:", "Developer ID Application:"]
        .iter()
        .find_map(|kind| identities.iter().find(|(_, name)| name.starts_with(kind)))
        .map(|(hash, _)| (*hash).to_string())
}

/// Copy the running executable to [`installed_exe`] and sign it.
fn install_exe(identity: Option<&str>) -> Result<PathBuf> {
    let source = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .context("cannot find the mx-keypad executable")?;
    let target = installed_exe()?;
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Copy beside the target and rename over it, so a running agent keeps
    // its own file until it is restarted.
    let staged = target.with_extension("new");
    std::fs::copy(&source, &staged).with_context(|| format!("cannot copy {}", source.display()))?;
    let mut perms = std::fs::metadata(&staged)?.permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&staged, perms)?;

    let identity = signing_identity(identity)?;
    let sign_with = identity.as_deref().unwrap_or("-");
    let status = Command::new("codesign")
        .args([
            "--force",
            "--timestamp=none",
            "--identifier",
            LABEL,
            "--sign",
            sign_with,
        ])
        .arg(&staged)
        .status()
        .context("cannot run codesign")?;
    if !status.success() {
        let _ = std::fs::remove_file(&staged);
        bail!("codesign failed with identity {sign_with}");
    }
    std::fs::rename(&staged, &target)?;
    match identity {
        Some(hash) => println!("signed {} with identity {hash}", target.display()),
        None => eprintln!(
            "note: no signing identity found; signed ad hoc, so macOS will ask for \
             Accessibility again after every install"
        ),
    }
    Ok(target)
}

/// Install and start the agent for `config`, replacing any earlier install.
/// The agent runs a signed copy of this executable, so reinstalling after a
/// rebuild keeps its Accessibility permission when the identity is the same.
pub fn install(config: &Path, identity: Option<&str>) -> Result<()> {
    let config = std::fs::canonicalize(config)
        .with_context(|| format!("{} does not exist", config.display()))?;
    let path = plist_path()?;
    let log = log_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Stop a running install first, so it has cleared the keys before the
    // new one draws: bootout fails harmlessly when none is loaded.
    let _ = launchctl(&["bootout", &format!("{}/{LABEL}", domain())]);
    let exe = install_exe(identity)?;
    std::fs::write(&path, plist(&exe, &config, &log))
        .with_context(|| format!("cannot write {}", path.display()))?;
    if !launchctl(&["bootstrap", &domain(), &path.display().to_string()])? {
        bail!("launchctl could not load {}", path.display());
    }
    println!(
        "installed {LABEL}: {} run --config {}",
        exe.display(),
        config.display()
    );
    println!("log: {}", log.display());
    Ok(())
}

/// Stop the agent and remove it.
pub fn uninstall() -> Result<()> {
    let path = plist_path()?;
    let _ = launchctl(&["bootout", &format!("{}/{LABEL}", domain())]);
    let _ = std::fs::remove_file(installed_exe()?);
    match std::fs::remove_file(&path) {
        Ok(()) => println!("uninstalled {LABEL}"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("{LABEL} was not installed");
        }
        Err(error) => {
            return Err(error).with_context(|| format!("cannot remove {}", path.display()))
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apple_development_wins_over_developer_id() {
        let listing = r#"  1) AAAA "Developer ID Application: Someone (TEAM1)"
  2) BBBB "Apple Development: Someone (TEAM2)"
     2 valid identities found"#;
        assert_eq!(pick_identity(listing).as_deref(), Some("BBBB"));
        assert_eq!(
            pick_identity(r#"  1) AAAA "Developer ID Application: Someone (TEAM1)""#).as_deref(),
            Some("AAAA")
        );
        assert_eq!(pick_identity("     0 valid identities found"), None);
    }

    #[test]
    fn the_plist_runs_the_config_and_escapes_paths() {
        let text = plist(
            Path::new("/opt/mx&keypad/mx-keypad"),
            Path::new("/Users/me/.config/mx-keypad/config.json"),
            Path::new("/Users/me/Library/Logs/mx-keypad.log"),
        );
        assert!(text.contains("<string>/opt/mx&amp;keypad/mx-keypad</string>"));
        assert!(text.contains("<string>run</string>\n    <string>--config</string>"));
        assert!(text.contains(LABEL));
    }
}
