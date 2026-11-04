//! Remote pages: another program adds pages to `mx-keypad run`'s keypad
//! over a Unix socket, instead of taking the device over. The agent keeps the
//! keypad, draws the pages after the config's own, and sends back presses on
//! them; every other key keeps running in the agent, which is the process
//! macOS trusts to send keystrokes.
//!
//! The protocol is newline-delimited JSON, one [`Request`] or [`Reply`] per
//! line; the README's "Remote pages" section lists the messages. [`Server`]
//! is the agent's end, [`Client`] a reconnecting publisher for the other.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{self, Key};
use crate::session::Session;
use crate::{Button, Event};

/// The protocol version both ends exchange in `hello` and `welcome`.
pub const PROTOCOL: u32 = 1;

/// Identifies one client connection for as long as `run` runs.
pub type ConnId = u64;

/// The agent socket in the same state directory as the legacy claim file.
pub fn socket_path() -> Result<PathBuf> {
    Ok(crate::claim::state_dir()?.join("agent.sock"))
}

/// A message from a client to the agent. Pages' keys are `"1"` to `"9"`,
/// each a [`Key`] exactly as the config writes one: a face, and optionally an
/// action, long press, shift binding and blink. The agent runs a key's
/// actions itself; a key with no action and no long press is forwarded to
/// the client as `down`/`up` instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// The first line on a connection, and only there.
    Hello { client: String, protocol: u32 },
    /// Add a page, or replace one this connection owns. Keys left out are
    /// blank; `show` turns to it.
    Page {
        name: String,
        keys: BTreeMap<String, Key>,
        #[serde(default)]
        show: bool,
    },
    /// Replace some keys on an owned page.
    Keys {
        page: String,
        keys: BTreeMap<String, Key>,
    },
    /// Turn to an owned page.
    Show(String),
    /// Remove an owned page.
    Remove(String),
    /// Bind the MX Dialpad's controls for as long as this connection lasts,
    /// in place of the config's `dialpad` section; `null` lets go. The last
    /// connection to send one wins.
    Dialpad(Option<config::DialpadConfig>),
    /// Set the panel brightness, 1–100, for as long as this connection
    /// lasts, in place of the config's `brightness`; `null` lets go. The
    /// last connection to send one wins.
    Brightness(Option<u8>),
}

/// A message from the agent to a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    /// The answer to a good `hello`.
    Welcome { protocol: u32, version: String },
    /// A grid key with no action went down on the client's page.
    Down { page: String, key: u8 },
    /// That key came up: sent to the page it went down on.
    Up { page: String, key: u8 },
    /// A refused message. After the handshake the connection stays open.
    Error { message: String },
}

/// What [`Server::drain`] hands the agent's loop.
#[derive(Debug)]
pub enum Incoming {
    /// A request from a connection that has said hello.
    Request(ConnId, Request),
    /// The connection closed; its pages should go.
    Gone(ConnId),
}

type Writer = Arc<Mutex<UnixStream>>;
enum Message {
    Connected(ConnId, String, Writer),
    Incoming(Incoming),
}

fn write_json(stream: &mut UnixStream, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    stream.write_all(&bytes)?;
    Ok(())
}

/// The agent's end of the socket. It outlives keypad connections, so
/// clients stay connected while the keypad is unplugged. Accepting and
/// reading run on threads; call [`Server::drain`] every tick.
pub struct Server {
    path: PathBuf,
    rx: mpsc::Receiver<Message>,
    clients: HashMap<ConnId, (String, Writer)>,
    stop: Arc<AtomicBool>,
    accept: Option<thread::JoinHandle<()>>,
}

impl Server {
    /// Listen at `path`, readable and writable by this user only. A socket
    /// file another agent still answers on is an error; a stale one is
    /// replaced.
    pub fn bind(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if path.exists() {
            if UnixStream::connect(&path).is_ok() {
                bail!("another mx-keypad run is serving");
            }
            fs::remove_file(&path)?;
        }
        let listener = UnixListener::bind(&path)?;
        if let Err(error) = fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .and_then(|()| listener.set_nonblocking(true))
        {
            let _ = fs::remove_file(&path);
            return Err(error.into());
        }
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let accept = thread::spawn(move || {
            let mut id = 0;
            while !stopped.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        id += 1;
                        let tx = tx.clone();
                        let stop = stopped.clone();
                        thread::spawn(move || {
                            let _ = read_client(id, stream, &tx, &stop);
                        });
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            path,
            rx,
            clients: HashMap::new(),
            stop,
            accept: Some(accept),
        })
    }

    /// Requests and closed connections since the last call, oldest first.
    pub fn drain(&mut self) -> Vec<Incoming> {
        let mut incoming = Vec::new();
        while let Ok(message) = self.rx.try_recv() {
            match message {
                Message::Connected(id, name, writer) => {
                    self.clients.insert(id, (name, writer));
                }
                Message::Incoming(event) => {
                    if let Incoming::Gone(id) = &event {
                        self.clients.remove(id);
                    }
                    incoming.push(event);
                }
            }
        }
        incoming
    }

    /// Send `reply` to connection `id`. A connection that can't take it
    /// within the write timeout is closed, and its reader reports `Gone`.
    pub fn send(&mut self, id: ConnId, reply: &Reply) {
        let failed = self.clients.get(&id).is_some_and(|(_, writer)| {
            let mut stream = writer.lock().unwrap_or_else(|e| e.into_inner());
            if write_json(&mut stream, reply).is_err() {
                let _ = stream.shutdown(Shutdown::Both);
                true
            } else {
                false
            }
        });
        if failed {
            self.clients.remove(&id);
        }
    }

    /// Whether any client has said hello and is still connected.
    pub fn has_clients(&self) -> bool {
        !self.clients.is_empty()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
        self.drain();
        for (_, writer) in self.clients.values() {
            let _ = writer
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .shutdown(Shutdown::Both);
        }
        let _ = fs::remove_file(&self.path);
    }
}

/// Serve one connection: answer `hello` right here, so a client connects
/// even while the agent's loop is busy or waiting for USB, then pass its
/// requests to the loop until it closes or the server stops.
fn read_client(
    id: ConnId,
    mut stream: UnixStream,
    tx: &mpsc::Sender<Message>,
    stop: &AtomicBool,
) -> Result<()> {
    // On macOS the stream inherits the listener's O_NONBLOCK, which would
    // make the read timeouts below return at once.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.set_write_timeout(Some(Duration::from_millis(100)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line)?;
    let name = match serde_json::from_slice::<Request>(&line) {
        Ok(Request::Hello {
            client,
            protocol: PROTOCOL,
        }) => client,
        _ => {
            write_json(
                &mut stream,
                &Reply::Error {
                    message: "first message must be hello with protocol 1".into(),
                },
            )?;
            return Ok(());
        }
    };
    write_json(
        &mut stream,
        &Reply::Welcome {
            protocol: PROTOCOL,
            version: env!("CARGO_PKG_VERSION").into(),
        },
    )?;
    stream.set_read_timeout(Some(Duration::from_millis(200)))?;
    let writer = Arc::new(Mutex::new(stream));
    tx.send(Message::Connected(id, name, writer.clone()))?;
    line.clear();
    while !stop.load(Ordering::Relaxed) {
        match reader.read_until(b'\n', &mut line) {
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                continue
            }
            Ok(0) | Err(_) => break,
            Ok(_) => match serde_json::from_slice(&line) {
                Ok(request) => {
                    if tx
                        .send(Message::Incoming(Incoming::Request(id, request)))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(error) => {
                    let mut stream = writer.lock().unwrap_or_else(|e| e.into_inner());
                    if write_json(
                        &mut stream,
                        &Reply::Error {
                            message: error.to_string(),
                        },
                    )
                    .is_err()
                    {
                        break;
                    }
                }
            },
        }
        line.clear();
    }
    let _ = writer
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .shutdown(Shutdown::Both);
    let _ = tx.send(Message::Incoming(Incoming::Gone(id)));
    Ok(())
}

/// The 0-based slots `keys` names, refusing anything but `"1"` to `"9"`.
fn slots(keys: &BTreeMap<String, Key>) -> Result<Vec<usize>> {
    keys.keys()
        .map(|key| match key.as_bytes() {
            [digit @ b'1'..=b'9'] => Ok(usize::from(digit - b'1')),
            _ => bail!("keys.{key:?}: keys are \"1\" to \"9\""),
        })
        .collect()
}

/// Carry out connection `id`'s `request` on `session`. The whole request is
/// validated before anything changes; the caller sends back any error.
pub fn apply(session: &mut Session, id: ConnId, request: Request) -> Result<()> {
    match request {
        Request::Hello { .. } => bail!("hello is only allowed as the first message"),
        Request::Page { name, keys, show } => {
            slots(&keys)?;
            let page = config::resolve_remote_page(&name, &keys)?;
            session.add_remote(id, page)?;
            if show {
                session.show_remote(id, &name)?;
            }
        }
        Request::Keys { page, keys } => {
            let slots = slots(&keys)?;
            let resolved = config::resolve_remote_page(&page, &keys)?;
            session.update_remote(id, &resolved, &slots)?;
        }
        Request::Show(name) => session.show_remote(id, &name)?,
        Request::Remove(name) => session.remove_remote(id, &name)?,
        Request::Dialpad(dialpad) => {
            let resolved = dialpad.map(config::resolve_remote_dialpad).transpose()?;
            session.set_remote_dialpad(id, resolved);
        }
        Request::Brightness(level) => {
            if let Some(level) = level.filter(|l| !(1..=100).contains(l)) {
                bail!("brightness: {level} is outside 1–100");
            }
            session.set_remote_brightness(id, level);
        }
    }
    Ok(())
}

/// A program's end of the socket: the pages it wants shown, kept up to date
/// on the agent. It never blocks — handshakes run on a thread — and while
/// the agent isn't running it just keeps trying, with backoff, and sends
/// every page again once connected.
pub struct Client {
    name: String,
    path: PathBuf,
    stream: Option<UnixStream>,
    connecting: Option<mpsc::Receiver<Result<UnixStream>>>,
    desired: Vec<(String, [Key; 9])>,
    dialpad: Option<config::DialpadConfig>,
    brightness: Option<u8>,
    input: Vec<u8>,
    errors: Vec<String>,
    retry: Instant,
    backoff: Duration,
}

impl Client {
    /// A client called `name` in the agent's log, for the default socket.
    /// It connects on the first [`Client::poll`].
    pub fn new(name: &str) -> Self {
        let path = socket_path();
        let mut client = Self::at(name, path.as_ref().cloned().unwrap_or_default());
        if let Err(error) = path {
            client.errors.push(error.to_string());
        }
        client
    }

    fn at(name: &str, path: PathBuf) -> Self {
        Self {
            name: name.into(),
            path,
            stream: None,
            connecting: None,
            desired: Vec::new(),
            dialpad: None,
            brightness: None,
            input: Vec::new(),
            errors: Vec::new(),
            retry: Instant::now(),
            backoff: Duration::from_secs(1),
        }
    }

    /// Show `keys` on page `name`, adding the page the first time. Safe to
    /// call every frame: only keys that changed are sent.
    pub fn set_page(&mut self, name: &str, faces: [Key; 9]) {
        let request = if let Some((_, old)) = self.desired.iter_mut().find(|(n, _)| n == name) {
            let keys = faces
                .iter()
                .zip(old.iter())
                .enumerate()
                .filter(|(_, (new, old))| new != old)
                .map(|(i, (face, _))| ((i + 1).to_string(), face.clone()))
                .collect::<BTreeMap<_, _>>();
            *old = faces;
            if keys.is_empty() {
                return;
            }
            Request::Keys {
                page: name.into(),
                keys,
            }
        } else {
            let keys = all_keys(&faces);
            self.desired.push((name.into(), faces));
            Request::Page {
                name: name.into(),
                keys,
                show: false,
            }
        };
        self.send(&request);
    }

    /// Bind the MX Dialpad's controls to `dialpad` while connected, or let
    /// them go with `None`. Safe to call every frame: only a change is sent,
    /// and the bindings are sent again after a reconnect.
    pub fn set_dialpad(&mut self, dialpad: Option<config::DialpadConfig>) {
        if dialpad == self.dialpad {
            return;
        }
        self.dialpad = dialpad;
        self.send(&Request::Dialpad(self.dialpad.clone()));
    }

    /// Set the panel brightness, 1–100, while connected, or leave it to the
    /// agent's config with `None`. Safe to call every frame: only a change
    /// is sent, and the level is sent again after a reconnect.
    pub fn set_brightness(&mut self, level: Option<u8>) {
        if level == self.brightness {
            return;
        }
        self.brightness = level;
        self.send(&Request::Brightness(level));
    }

    /// Turn the keypad to page `name`. Sent only while connected, so call
    /// it once [`Client::connected`] is true.
    pub fn show(&mut self, name: &str) {
        self.send(&Request::Show(name.into()));
    }

    /// Take page `name` off the keypad and stop sending it.
    pub fn remove(&mut self, name: &str) {
        self.desired.retain(|(n, _)| n != name);
        self.send(&Request::Remove(name.into()));
    }

    /// Whether the agent has welcomed this client.
    pub fn connected(&self) -> bool {
        self.stream.is_some()
    }
    /// Errors since the last call: refused requests and connection trouble.
    pub fn take_errors(&mut self) -> Vec<String> {
        std::mem::take(&mut self.errors)
    }

    fn disconnect(&mut self) {
        if let Some(stream) = self.stream.take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
        self.input.clear();
        self.retry = Instant::now() + self.backoff;
        self.backoff = (self.backoff * 2).min(Duration::from_secs(30));
    }

    fn send(&mut self, request: &Request) {
        if let Some(stream) = &mut self.stream {
            if let Err(error) = write_json(stream, request) {
                self.errors.push(error.to_string());
                self.disconnect();
            }
        }
    }

    /// Presses on this client's pages since the last call, as `(page,
    /// event)`. Also moves reconnecting along. Never blocks.
    pub fn poll(&mut self) -> Vec<(String, Event)> {
        if let Some(rx) = &self.connecting {
            match rx.try_recv() {
                Ok(result) => {
                    self.connecting = None;
                    match result {
                        Ok(stream) => {
                            self.stream = Some(stream);
                            self.backoff = Duration::from_secs(1);
                            for (name, faces) in self.desired.clone() {
                                self.send(&Request::Page {
                                    name,
                                    keys: all_keys(&faces),
                                    show: false,
                                });
                            }
                            if self.dialpad.is_some() {
                                self.send(&Request::Dialpad(self.dialpad.clone()));
                            }
                            if self.brightness.is_some() {
                                self.send(&Request::Brightness(self.brightness));
                            }
                        }
                        Err(error) => {
                            self.errors.push(error.to_string());
                            self.disconnect();
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.connecting = None;
                    self.disconnect();
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.stream.is_none() && self.connecting.is_none() && Instant::now() >= self.retry {
            let (tx, rx) = mpsc::channel();
            let path = self.path.clone();
            let name = self.name.clone();
            thread::spawn(move || {
                let _ = tx.send(connect_client(&path, &name));
            });
            self.connecting = Some(rx);
        }
        let mut events = Vec::new();
        // Bound each poll's work even if a peer keeps producing data.
        for _ in 0..64 {
            let Some(stream) = &mut self.stream else {
                break;
            };
            let mut bytes = [0; 8192];
            match stream.read(&mut bytes) {
                Ok(0) => {
                    self.disconnect();
                    break;
                }
                Ok(n) => self.input.extend_from_slice(&bytes[..n]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    self.errors.push(error.to_string());
                    self.disconnect();
                    break;
                }
            }
            while let Some(end) = self.input.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.input.drain(..=end).collect();
                match serde_json::from_slice::<Reply>(&line) {
                    Ok(Reply::Down { page, key }) if (1..=9).contains(&key) => {
                        events.push((page, Event::Down(Button::Key(key))))
                    }
                    Ok(Reply::Up { page, key }) if (1..=9).contains(&key) => {
                        events.push((page, Event::Up(Button::Key(key))))
                    }
                    Ok(Reply::Error { message }) => self.errors.push(message),
                    Ok(_) => self.errors.push("unexpected agent reply".into()),
                    Err(error) => self.errors.push(error.to_string()),
                }
            }
        }
        events
    }
}

fn all_keys(faces: &[Key; 9]) -> BTreeMap<String, Key> {
    faces
        .iter()
        .enumerate()
        .map(|(i, face)| ((i + 1).to_string(), face.clone()))
        .collect()
}

fn connect_client(path: &Path, name: &str) -> Result<UnixStream> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    write_json(
        &mut stream,
        &Request::Hello {
            client: name.into(),
            protocol: PROTOCOL,
        },
    )?;
    // Read exactly the welcome line, leaving any subsequent events in the socket.
    let mut line = Vec::new();
    loop {
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
    }
    match serde_json::from_slice::<Reply>(&line).context("invalid welcome")? {
        Reply::Welcome {
            protocol: PROTOCOL, ..
        } => {}
        Reply::Error { message } => bail!("{message}"),
        _ => bail!("expected welcome with protocol 1"),
    }
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(None)?;
    stream.set_nonblocking(true)?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_messages_round_trip_with_the_documented_shapes() {
        for json in [
            r#"{"hello":{"client":"tmux-legion","protocol":1}}"#,
            r#"{"page":{"name":"agents","keys":{"1":{}},"show":false}}"#,
            r##"{"keys":{"page":"agents","keys":{"3":{"label":"Ready","background":"#12ab00"}}}}"##,
            r#"{"show":"agents"}"#,
            r#"{"remove":"agents"}"#,
            r#"{"page":{"name":"keys","keys":{"1":{"label":"Esc","action":{"keys":"escape"}}},"show":false}}"#,
            r#"{"dialpad":{"buttons":{"1":{"keys":"escape"}},"dial":{"left":{"page":"prev"},"right":{"page":"next"},"step":5}}}"#,
            r#"{"dialpad":null}"#,
            r#"{"brightness":70}"#,
            r#"{"brightness":null}"#,
        ] {
            let request: Request = serde_json::from_str(json).unwrap();
            assert_eq!(serde_json::to_string(&request).unwrap(), json);
        }
        for json in [
            r#"{"welcome":{"protocol":1,"version":"0.7.0"}}"#,
            r#"{"down":{"page":"agents","key":3}}"#,
            r#"{"up":{"page":"agents","key":3}}"#,
            r#"{"error":{"message":"bad icon"}}"#,
        ] {
            let reply: Reply = serde_json::from_str(json).unwrap();
            assert_eq!(serde_json::to_string(&reply).unwrap(), json);
        }
        assert!(matches!(
            serde_json::from_str::<Request>(r#"{"page":{"name":"a","keys":{}}}"#).unwrap(),
            Request::Page { show: false, .. }
        ));
        assert!(serde_json::from_str::<Key>(r#"{"bogus":1}"#).is_err());
    }

    #[test]
    fn a_bad_key_refuses_the_whole_request() {
        let setup = config::Setup::resolve(
            serde_json::from_str(r#"{"pages":[{"name":"main"}]}"#).unwrap(),
            Path::new("."),
        )
        .unwrap();
        let mut session = Session::new(setup);
        let request = |json: &str| serde_json::from_str::<Request>(json).unwrap();
        apply(
            &mut session,
            1,
            request(r#"{"page":{"name":"a","keys":{},"show":true}}"#),
        )
        .unwrap();
        let changed = r#""1":{"label":"changed"}"#;
        for bad in [
            r#""0":{}"#,
            r#""10":{}"#,
            r#""01":{}"#,
            r#""2":{"glyph_scale":1.5}"#,
            r#""2":{"icon":"missing-icon"}"#,
            r#""2":{"icon":"icons/bell.svg"}"#,
            r#""2":{"action":{"page":"main"}}"#,
            r#""2":{"action":{"keys":"not-a-key"}}"#,
        ] {
            let json = format!(r#"{{"keys":{{"page":"a","keys":{{{changed},{bad}}}}}}}"#);
            assert!(apply(&mut session, 1, request(&json)).is_err(), "{bad}");
            assert_eq!(session.face(0, Instant::now()).label, None, "{bad}");
        }
        apply(
            &mut session,
            1,
            request(
                r#"{"keys":{"page":"a","keys":{"1":{"icon":"bell","action":{"keys":"escape"}}}}}"#,
            ),
        )
        .unwrap();
    }

    #[test]
    fn a_client_binds_the_dialpad_until_it_lets_go_or_leaves() {
        let setup = config::Setup::resolve(
            serde_json::from_str(r#"{"pages":[{"name":"main"},{"name":"other"}]}"#).unwrap(),
            Path::new("."),
        )
        .unwrap();
        let mut session = Session::new(setup);
        let request = |json: &str| serde_json::from_str::<Request>(json).unwrap();
        assert!(session.dialpad().is_none());
        // Only next and prev, as on a client's page.
        assert!(apply(
            &mut session,
            1,
            request(r#"{"dialpad":{"buttons":{"1":{"page":"other"}}}}"#)
        )
        .is_err());
        let bind = request(r#"{"dialpad":{"buttons":{"1":{"page":"next"}}}}"#);
        apply(&mut session, 1, bind.clone()).unwrap();
        session.down(Button::Dial(1)).unwrap();
        assert_eq!(session.page_name(), "other");
        // Another client's `null` leaves the first one's bindings alone.
        apply(&mut session, 2, request(r#"{"dialpad":null}"#)).unwrap();
        assert!(session.dialpad().is_some());
        apply(&mut session, 1, request(r#"{"dialpad":null}"#)).unwrap();
        assert!(session.dialpad().is_none());
        apply(&mut session, 1, bind).unwrap();
        session.drop_owner(1);
        assert!(session.dialpad().is_none());
    }

    #[test]
    fn a_client_sets_the_brightness_until_it_lets_go_or_leaves() {
        let setup = config::Setup::resolve(
            serde_json::from_str(r#"{"brightness":40,"pages":[{"name":"main"}]}"#).unwrap(),
            Path::new("."),
        )
        .unwrap();
        let mut session = Session::new(setup);
        let request = |json: &str| serde_json::from_str::<Request>(json).unwrap();
        assert_eq!(session.brightness(), Some(40));
        for bad in [r#"{"brightness":0}"#, r#"{"brightness":101}"#] {
            assert!(apply(&mut session, 1, request(bad)).is_err(), "{bad}");
        }
        assert_eq!(session.brightness(), Some(40));
        apply(&mut session, 1, request(r#"{"brightness":70}"#)).unwrap();
        assert_eq!(session.brightness(), Some(70));
        // The last client to send one wins; another's `null` leaves it alone.
        apply(&mut session, 2, request(r#"{"brightness":90}"#)).unwrap();
        assert_eq!(session.brightness(), Some(90));
        apply(&mut session, 1, request(r#"{"brightness":null}"#)).unwrap();
        assert_eq!(session.brightness(), Some(90));
        session.drop_owner(2);
        assert_eq!(session.brightness(), Some(40));
    }

    struct Sandbox(PathBuf);
    impl Sandbox {
        fn new() -> Self {
            // Short enough for macOS's sockaddr_un even with its long TMPDIR,
            // and numbered, so tests running in parallel never share one.
            static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "mxr-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn path(&self) -> PathBuf {
            self.0.join("s")
        }
    }
    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn until(mut check: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !check() {
            assert!(Instant::now() < deadline, "socket test timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn server_client_handshake_diffs_events_errors_disconnect_and_replay() {
        let dir = Sandbox::new();
        let mut server = Server::bind(dir.path()).unwrap();
        assert_eq!(
            fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(Server::bind(dir.path()).is_err());
        let mut client = Client::at("test", dir.path());
        let mut faces: [Key; 9] = std::array::from_fn(|_| Key::default());
        client.set_page("z", faces.clone());
        client.set_page("a", faces.clone());
        // The handshake succeeds without the main thread draining anything.
        until(|| {
            client.poll();
            client.connected()
        });
        let mut requests = Vec::new();
        until(|| {
            requests.extend(server.drain());
            requests.len() >= 2
        });
        let id = match &requests[0] {
            Incoming::Request(id, Request::Page { name, keys, show }) => {
                assert_eq!(name, "z");
                assert_eq!(keys.len(), 9);
                assert!(!show);
                *id
            }
            other => panic!("{other:?}"),
        };
        assert!(
            matches!(&requests[1], Incoming::Request(_, Request::Page { name, .. }) if name == "a")
        );
        faces[2].label = Some("changed".into());
        client.set_page("z", faces.clone());
        client.set_page("z", faces.clone());
        let mut diff = Vec::new();
        until(|| {
            diff.extend(server.drain());
            !diff.is_empty()
        });
        assert_eq!(diff.len(), 1);
        assert!(
            matches!(&diff[0], Incoming::Request(_, Request::Keys { keys, .. }) if keys.len() == 1 && keys["3"] == faces[2])
        );
        server.send(
            id,
            &Reply::Down {
                page: "z".into(),
                key: 3,
            },
        );
        server.send(
            id,
            &Reply::Up {
                page: "z".into(),
                key: 3,
            },
        );
        server.send(
            id,
            &Reply::Error {
                message: "test error".into(),
            },
        );
        let mut events = Vec::new();
        until(|| {
            events.extend(client.poll());
            events.len() == 2
        });
        assert_eq!(
            events,
            [
                ("z".into(), Event::Down(Button::Key(3))),
                ("z".into(), Event::Up(Button::Key(3)))
            ]
        );
        until(|| {
            client.poll();
            !client.errors.is_empty()
        });
        assert_eq!(client.take_errors(), ["test error"]);
        drop(server);
        until(|| {
            client.poll();
            !client.connected()
        });
        let mut server = Server::bind(dir.path()).unwrap();
        client.retry = Instant::now();
        until(|| {
            client.poll();
            client.connected()
        });
        let mut replay = Vec::new();
        until(|| {
            replay.extend(server.drain());
            replay.len() == 2
        });
        assert!(
            matches!(&replay[0], Incoming::Request(_, Request::Page { name, keys, .. }) if name == "z" && keys["3"] == faces[2])
        );
        drop(client);
        until(|| {
            server
                .drain()
                .iter()
                .any(|e| matches!(e, Incoming::Gone(_)))
        });
        assert!(!server.has_clients());
        drop(server);
        assert!(!dir.path().exists());
    }

    #[test]
    fn a_connection_blocks_on_reads_though_the_listener_does_not() {
        // BSD sockets inherit O_NONBLOCK from the listener on accept, which
        // turned every read timeout into an instant WouldBlock: the hello
        // read failed at once and idle clients spun a core.
        let dir = Sandbox::new();
        let _server = Server::bind(dir.path()).unwrap();
        let mut stream = UnixStream::connect(dir.path()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        thread::sleep(Duration::from_millis(300));
        write_json(
            &mut stream,
            &Request::Hello {
                client: "slow".into(),
                protocol: PROTOCOL,
            },
        )
        .unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(matches!(
            serde_json::from_str::<Reply>(&line).unwrap(),
            Reply::Welcome { .. }
        ));
    }

    #[test]
    fn handshake_rejects_bad_versions_and_parse_errors_keep_the_connection() {
        let dir = Sandbox::new();
        let mut server = Server::bind(dir.path()).unwrap();
        let mut stream = UnixStream::connect(dir.path()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        write_json(
            &mut stream,
            &Request::Hello {
                client: "test".into(),
                protocol: 99,
            },
        )
        .unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(matches!(
            serde_json::from_str::<Reply>(&line).unwrap(),
            Reply::Error { .. }
        ));
        line.clear();
        assert_eq!(reader.read_line(&mut line).unwrap(), 0);
        let mut stream = connect_client(&dir.path(), "valid").unwrap();
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .write_all(b"invalid json\n{\"show\":\"a\"}\n")
            .unwrap();
        let mut reader = BufReader::new(stream);
        reader.read_line(&mut line).unwrap();
        assert!(matches!(
            serde_json::from_str::<Reply>(&line).unwrap(),
            Reply::Error { .. }
        ));
        until(|| {
            server
                .drain()
                .iter()
                .any(|e| matches!(e, Incoming::Request(_, Request::Show(name)) if name == "a"))
        });
    }

    #[test]
    fn a_press_on_a_client_page_travels_from_the_session_back_to_the_client() {
        let dir = Sandbox::new();
        let mut server = Server::bind(dir.path()).unwrap();
        let setup = config::Setup::resolve(
            serde_json::from_str(r#"{"pages":[{"name":"main"}]}"#).unwrap(),
            Path::new("."),
        )
        .unwrap();
        let mut session = Session::new(setup);
        let mut client = Client::at("test", dir.path());
        let mut faces: [Key; 9] = std::array::from_fn(|_| Key::default());
        faces[2].label = Some("agent".into());
        client.set_page("agents", faces);
        until(|| {
            client.poll();
            for incoming in server.drain() {
                if let Incoming::Request(id, request) = incoming {
                    apply(&mut session, id, request).unwrap();
                }
            }
            session.needs_page_buttons()
        });

        // What `run` does with the page buttons and a key on that page.
        session.down(Button::PageRight).unwrap();
        assert_eq!(session.page_name(), "agents");
        assert_eq!(
            session.face(2, Instant::now()).label.as_deref(),
            Some("agent")
        );
        session.down(Button::Key(3)).unwrap();
        session.up(Button::Key(3)).unwrap();
        for event in session.take_remote_events() {
            let reply = match event.event {
                Event::Down(Button::Key(key)) => Reply::Down {
                    page: event.page,
                    key,
                },
                Event::Up(Button::Key(key)) => Reply::Up {
                    page: event.page,
                    key,
                },
                _ => continue,
            };
            server.send(event.owner, &reply);
        }

        let mut events = Vec::new();
        until(|| {
            events.extend(client.poll());
            events.len() == 2
        });
        assert_eq!(
            events,
            [
                ("agents".into(), Event::Down(Button::Key(3))),
                ("agents".into(), Event::Up(Button::Key(3)))
            ]
        );
    }
}
