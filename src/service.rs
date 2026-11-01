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

/// Install and start the agent for `config`, replacing any earlier install.
pub fn install(config: &Path) -> Result<()> {
    let config = std::fs::canonicalize(config)
        .with_context(|| format!("{} does not exist", config.display()))?;
    // `current_exe` keeps the path the program was started by, so starting
    // it through a stable symlink (a Nix profile, ./result) keeps rebuilds
    // working without reinstalling.
    let exe = std::env::current_exe().context("cannot find the mx-keypad executable")?;
    if exe.starts_with("/nix/store") {
        eprintln!(
            "note: {} is a Nix store path; it stops existing after garbage collection. \
             Install from a stable path such as ~/.nix-profile/bin/mx-keypad.",
            exe.display()
        );
    }
    let path = plist_path()?;
    let log = log_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Replace a running install: bootout fails harmlessly when none is loaded.
    let _ = launchctl(&["bootout", &format!("{}/{LABEL}", domain())]);
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
