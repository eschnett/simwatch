//! The local side of a remote host: one ssh connection, kept open.
//!
//! The first connection is made before the TUI starts, so that ssh can ask
//! for passwords or MFA codes on the terminal (it uses `/dev/tty`, not the
//! piped stdin). A lost connection is retried only without prompting
//! (`BatchMode`), or interactively when the user asks for it.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use chrono::Utc;

use super::{FromServer, Hello, MagicError, ToServer, read_bytes, read_magic, read_msg, write_msg};
use crate::config::{Config, Remote};
use crate::discover::ScanResult;
use crate::format::parse_status;
use crate::model::Sim;
use crate::monitor::{Once, Out, Request, Update};
use crate::slurm::Snapshot;

/// Whether the TUI owns the terminal. ssh's messages are then kept for the
/// status bar instead of being printed.
pub static TUI_ACTIVE: AtomicBool = AtomicBool::new(false);

/// First and longest wait before reconnecting automatically
const RETRY_FIRST: Duration = Duration::from_secs(60);
const RETRY_MAX: Duration = Duration::from_secs(30 * 60);
/// How long a non-interactive connection attempt may take
const BATCH_TIMEOUT: Duration = Duration::from_secs(60);
/// How long to wait for an image from the remote host
const IMAGE_TIMEOUT: Duration = Duration::from_secs(60);

type ImageReply = Sender<Result<Vec<u8>, String>>;
/// Image requests waiting for an answer, by directory and file
type Pending = Arc<Mutex<HashMap<(String, String), Vec<ImageReply>>>>;

enum Cmd {
    Request(Request),
    Image {
        dir: String,
        file: String,
        reply: ImageReply,
    },
    /// Connect interactively unless connected; answer when done
    Connect(Sender<()>),
    /// The connection with this number ended
    Lost {
        generation: u64,
        why: String,
    },
}

/// A remote host, handled by a background thread
pub struct Client {
    pub host: String,
    cmd: Sender<Cmd>,
    connected: Arc<AtomicBool>,
}

impl Client {
    /// Connect (interactively, which may ask for a password), then keep the
    /// connection on a background thread
    pub fn start(cfg: &Config, remote: &Remote, out: Out) -> Client {
        let (tx, rx) = channel();
        let connected = Arc::new(AtomicBool::new(false));
        let mut ctl = Control {
            cfg: cfg.clone(),
            remote: remote.clone(),
            out,
            me: tx.clone(),
            conn: None,
            generation: 0,
            connected: connected.clone(),
            pending: Pending::default(),
            retry: None,
            backoff: RETRY_FIRST,
        };
        ctl.connect(true);
        thread::Builder::new()
            .name(format!("simwatch-{}", remote.host))
            .spawn(move || ctl.run(rx))
            .expect("spawning client thread");
        Client {
            host: remote.host.clone(),
            cmd: tx,
            connected,
        }
    }

    pub fn request(&self, r: Request) {
        let _ = self.cmd.send(Cmd::Request(r));
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Connect again interactively if the connection was lost, and wait for
    /// the result
    pub fn reconnect(&self) {
        if self.is_connected() {
            return;
        }
        let (tx, rx) = channel();
        if self.cmd.send(Cmd::Connect(tx)).is_ok() {
            let _ = rx.recv();
        }
    }

    pub fn fetcher(&self) -> Fetcher {
        Fetcher(self.cmd.clone())
    }
}

/// Fetches image files from a remote host
pub struct Fetcher(Sender<Cmd>);

impl Fetcher {
    pub fn fetch(&self, dir: &Path, file: &str) -> Result<Vec<u8>, String> {
        let (reply, rx) = channel();
        let dir = dir.to_string_lossy().into_owned();
        let file = file.to_string();
        self.0
            .send(Cmd::Image { dir, file, reply })
            .map_err(|_| "not connected".to_string())?;
        match rx.recv_timeout(IMAGE_TIMEOUT) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => Err("timed out".into()),
            Err(RecvTimeoutError::Disconnected) => Err("connection lost".into()),
        }
    }
}

/// A running ssh process
struct Conn {
    child: Child,
    stdin: ChildStdin,
}

impl Drop for Conn {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The state of one host, owned by its background thread
struct Control {
    cfg: Config,
    remote: Remote,
    out: Out,
    /// For the reader thread to report a lost connection
    me: Sender<Cmd>,
    conn: Option<Conn>,
    /// Numbers the connections, so that old ones are ignored
    generation: u64,
    connected: Arc<AtomicBool>,
    pending: Pending,
    /// When to try again without prompting
    retry: Option<Instant>,
    backoff: Duration,
}

impl Control {
    fn run(mut self, rx: Receiver<Cmd>) {
        loop {
            let cmd = match self.retry {
                Some(t) => match rx.recv_timeout(t.saturating_duration_since(Instant::now())) {
                    Ok(c) => c,
                    Err(RecvTimeoutError::Timeout) => {
                        self.retry = None;
                        self.connect(false);
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                },
                None => match rx.recv() {
                    Ok(c) => c,
                    Err(_) => return,
                },
            };
            match cmd {
                Cmd::Request(r) => self.send(&match r {
                    Request::Reread => ToServer::Reread,
                    Request::Rescan => ToServer::Rescan,
                }),
                Cmd::Image { dir, file, reply } => {
                    if self.conn.is_none() {
                        let _ = reply.send(Err("not connected".into()));
                        continue;
                    }
                    let key = (dir.clone(), file.clone());
                    self.pending
                        .lock()
                        .unwrap()
                        .entry(key)
                        .or_default()
                        .push(reply);
                    self.send(&ToServer::Image { dir, file });
                }
                Cmd::Connect(done) => {
                    if self.conn.is_none() {
                        self.connect(true);
                    }
                    let _ = done.send(());
                }
                Cmd::Lost { generation, why } => {
                    if generation == self.generation && self.conn.is_some() {
                        self.lost(why);
                    }
                }
            }
        }
    }

    fn send(&mut self, msg: &ToServer) {
        if let Some(c) = &mut self.conn {
            if let Err(e) = write_msg(&mut c.stdin, msg) {
                self.lost(e.to_string());
            }
        }
    }

    fn lost(&mut self, why: String) {
        self.conn = None;
        self.connected.store(false, Ordering::Relaxed);
        // Dropping the senders answers waiting image requests
        self.pending.lock().unwrap().clear();
        self.out.send(Update::Disconnected(why));
        self.schedule_retry();
    }

    fn schedule_retry(&mut self) {
        if self.cfg.auto_reconnect {
            self.retry = Some(Instant::now() + self.backoff);
            self.backoff = (self.backoff * 2).min(RETRY_MAX);
        }
    }

    /// Connect; `interactive` lets ssh ask for passwords
    fn connect(&mut self, interactive: bool) {
        self.generation += 1;
        let host = self.remote.host.clone();
        if interactive && !TUI_ACTIVE.load(Ordering::Relaxed) {
            eprintln!("simwatch: connecting to {host} …");
        }
        match self.open(interactive) {
            Ok(conn) => {
                self.conn = Some(conn);
                self.connected.store(true, Ordering::Relaxed);
                self.retry = None;
                self.backoff = RETRY_FIRST;
                self.out.send(Update::Connected);
            }
            Err(e) => {
                if interactive && !TUI_ACTIVE.load(Ordering::Relaxed) {
                    eprintln!("simwatch: {host}: {e}");
                }
                self.out.send(Update::Disconnected(e));
                self.schedule_retry();
            }
        }
    }

    fn open(&mut self, interactive: bool) -> Result<Conn, String> {
        let mut cmd = ssh_command(&self.cfg, &self.remote.host, !interactive);
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{}: {e}", self.cfg.ssh[0]))?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let mut conn = Conn {
            stdin: child.stdin.take().unwrap(),
            child,
        };

        // ssh's own messages: shown while there is no TUI, else kept
        let last_err = Arc::new(Mutex::new(String::new()));
        let (err_done, err_finished) = channel::<()>();
        {
            let last_err = last_err.clone();
            thread::spawn(move || {
                for line in BufReader::new(stderr).lines() {
                    let Ok(line) = line else { break };
                    if !TUI_ACTIVE.load(Ordering::Relaxed) {
                        eprintln!("{line}");
                    }
                    if !line.trim().is_empty() {
                        *last_err.lock().unwrap() = line.trim().to_string();
                    }
                }
                drop(err_done);
            });
        }

        let hello = Hello::new(&self.cfg, &self.remote.roots, false);
        // A failure shows up as the end of stdout
        let _ = write_msg(&mut conn.stdin, &ToServer::Hello(hello));

        let (ready_tx, ready) = channel();
        {
            let reader = Reader {
                host: self.remote.host.clone(),
                out: self.out.clone(),
                me: self.me.clone(),
                generation: self.generation,
                pending: self.pending.clone(),
                last_err: last_err.clone(),
            };
            thread::spawn(move || reader.run(stdout, ready_tx));
        }
        let res = if interactive {
            ready.recv().map_err(|_| MagicError::NoAnswer)
        } else {
            ready
                .recv_timeout(BATCH_TIMEOUT)
                .map_err(|_| MagicError::Other("timed out".into()))
        };
        match res.and_then(|r| r) {
            Ok(()) => Ok(conn),
            Err(MagicError::Other(e)) => Err(e),
            Err(MagicError::NoAnswer) => {
                // Let ssh exit and finish its messages, to explain why
                let deadline = Instant::now() + Duration::from_secs(2);
                let mut code = None;
                while Instant::now() < deadline {
                    if let Ok(Some(status)) = conn.child.try_wait() {
                        code = status.code();
                        break;
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                let _ = err_finished.recv_timeout(Duration::from_secs(1));
                let last = last_err.lock().unwrap().clone();
                let prog = &self.cfg.remote_program;
                Err(if code == Some(127) || last.contains("not found") {
                    format!("`{prog}` not found there; install simwatch, or set remote_program")
                } else if last.is_empty() {
                    format!("no answer from `{prog} --serve`")
                } else {
                    last
                })
            }
        }
    }
}

/// `ssh [options] HOST 'simwatch --serve'`
fn ssh_command(cfg: &Config, host: &str, batch: bool) -> Command {
    let mut cmd = Command::new(&cfg.ssh[0]);
    cmd.args(&cfg.ssh[1..]);
    // Notice a dead connection within about two minutes
    cmd.args([
        "-T",
        "-o",
        "ServerAliveInterval=30",
        "-o",
        "ServerAliveCountMax=4",
    ]);
    if batch {
        // Never prompt
        cmd.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=30"]);
    }
    cmd.arg(host).arg(format!("{} --serve", cfg.remote_program));
    cmd
}

/// Reads the messages of one connection
struct Reader {
    host: String,
    out: Out,
    me: Sender<Cmd>,
    generation: u64,
    pending: Pending,
    last_err: Arc<Mutex<String>>,
}

impl Reader {
    fn run(self, stdout: impl Read, ready: Sender<Result<(), MagicError>>) {
        let mut r = BufReader::new(stdout);
        if let Err(e) = read_magic(&mut r) {
            let _ = ready.send(Err(e));
            return;
        }
        let _ = ready.send(Ok(()));
        let mut state = State::new(&self.host);
        let why = loop {
            match read_msg::<FromServer>(&mut r) {
                Ok(Some(FromServer::Image { dir, file, len })) => match read_bytes(&mut r, len) {
                    Ok(bytes) => self.reply(dir, file, Ok(bytes)),
                    Err(e) => break e.to_string(),
                },
                Ok(Some(FromServer::ImageError { dir, file, error })) => {
                    self.reply(dir, file, Err(error))
                }
                Ok(Some(m)) => {
                    if let Some(u) = state.apply(m) {
                        if !self.out.send(u) {
                            return;
                        }
                    }
                }
                Ok(None) => {
                    // ssh usually says why
                    thread::sleep(Duration::from_millis(300));
                    let last = self.last_err.lock().unwrap().clone();
                    break if last.is_empty() {
                        "connection closed".into()
                    } else {
                        last
                    };
                }
                Err(e) => break e.to_string(),
            }
        };
        let _ = self.me.send(Cmd::Lost {
            generation: self.generation,
            why,
        });
    }

    fn reply(&self, dir: String, file: String, res: Result<Vec<u8>, String>) {
        let waiting = self.pending.lock().unwrap().remove(&(dir, file));
        for w in waiting.into_iter().flatten() {
            let _ = w.send(res.clone());
        }
    }
}

/// The simulations of a host, built from the server's messages
pub struct State {
    host: String,
    sims: HashMap<PathBuf, Sim>,
}

impl State {
    pub fn new(host: &str) -> State {
        State {
            host: host.to_string(),
            sims: HashMap::new(),
        }
    }

    fn sim(&self, dir: PathBuf) -> Sim {
        Sim::on(Some(self.host.clone()), dir)
    }

    /// Apply a message; some result in an update
    pub fn apply(&mut self, m: FromServer) -> Option<Update> {
        Some(match m {
            FromServer::ScanStarted => Update::ScanStarted,
            FromServer::ScanFinished {
                dirs_visited,
                warnings,
            } => Update::ScanFinished(ScanResult {
                sims: Vec::new(),
                dirs_visited,
                warnings,
            }),
            FromServer::ReadStarted => Update::ReadStarted,
            FromServer::File {
                dir,
                mtime,
                size,
                error,
                text,
            } => {
                let dir = PathBuf::from(dir);
                let new = self.sim(dir.clone());
                let sim = self.sims.entry(dir).or_insert(new);
                sim.mtime = mtime
                    .and_then(|s| Duration::try_from_secs_f64(s).ok())
                    .map(|d| UNIX_EPOCH + d);
                sim.size = size;
                sim.read = true;
                if let Some(text) = text {
                    match parse_status(&text) {
                        Ok(st) => {
                            sim.status = Some(st);
                            sim.error = None;
                        }
                        Err(e) => sim.error = Some(e),
                    }
                } else if error.is_some() {
                    // Keeps the last good contents
                    sim.error = error;
                }
                return None;
            }
            FromServer::Sims { dirs } => {
                let list: Vec<Sim> = dirs
                    .into_iter()
                    .map(PathBuf::from)
                    .map(|d| self.sims.remove(&d).unwrap_or_else(|| self.sim(d)))
                    .collect();
                self.sims = list.iter().map(|s| (s.dir.clone(), s.clone())).collect();
                Update::Sims(list)
            }
            FromServer::SlurmStarted => Update::SlurmStarted,
            FromServer::Slurm { jobs } => Update::Slurm(Ok(Snapshot::new(Utc::now(), jobs))),
            FromServer::SlurmError { error } => Update::Slurm(Err(error)),
            FromServer::SlurmDisabled { why } => Update::SlurmDisabled(why),
            FromServer::Warning { message } => Update::Warning(message),
            FromServer::Image { .. }
            | FromServer::ImageError { .. }
            | FromServer::Done
            | FromServer::Unknown => return None,
        })
    }
}

/// Collect everything from a remote host once (for `--print`). ssh may ask
/// for a password on the terminal.
pub fn collect_once(cfg: &Config, remote: &Remote) -> Result<Once, String> {
    let mut child = ssh_command(cfg, &remote.host, false)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("{}: {e}", cfg.ssh[0]))?;
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let hello = Hello::new(cfg, &remote.roots, true);
    let _ = write_msg(&mut stdin, &ToServer::Hello(hello));
    let res = once(BufReader::new(stdout), &remote.host);
    drop(stdin);
    let status = child.wait();
    res.map_err(|e| match (e, status.ok().and_then(|s| s.code())) {
        (_, Some(127)) => format!(
            "`{}` not found there; install simwatch, or set remote_program",
            cfg.remote_program
        ),
        (MagicError::NoAnswer, _) => format!("no answer from `{} --serve`", cfg.remote_program),
        (MagicError::Other(e), _) => e,
    })
}

fn once(mut r: impl BufRead, host: &str) -> Result<Once, MagicError> {
    read_magic(&mut r)?;
    let mut state = State::new(host);
    let mut sims = Vec::new();
    let mut scan = ScanResult::default();
    let mut snap = None;
    loop {
        let m = read_msg::<FromServer>(&mut r).map_err(|e| MagicError::Other(e.to_string()))?;
        let Some(m) = m else {
            return Err(MagicError::Other("connection closed".into()));
        };
        if m == FromServer::Done {
            return Ok((sims, scan, snap));
        }
        match state.apply(m) {
            Some(Update::Sims(s)) => sims = s,
            Some(Update::ScanFinished(s)) => scan = s,
            Some(Update::Slurm(s)) => snap = Some(s),
            Some(Update::Warning(w)) => scan.warnings.push(w),
            _ => {}
        }
    }
}
