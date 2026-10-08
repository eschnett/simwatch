//! Background workers: for local directories one thread for all file system
//! access and one for Slurm, and for each remote host a client that keeps one
//! ssh connection. The UI talks to them through channels and never blocks on
//! I/O.
//!
//! Each worker does one thing at a time. Requests that arrive while it is
//! busy are coalesced, so a hung file system cannot pile up threads or work.

use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::discover::{self, ScanResult};
use crate::format::{STATUS_FILE, parse_status, read_status_text};
use crate::images::Fetch;
use crate::model::Sim;
use crate::remote::client::{self, Client};
use crate::slurm::{self, QueryError, Snapshot};

/// Minimum time between two squeue calls, even when asked explicitly
const MIN_SQUEUE_GAP: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// Re-read the known status files
    Reread,
    /// Look for new simulations, then re-read
    Rescan,
}

#[derive(Debug)]
pub enum Update {
    ScanStarted,
    ScanFinished(ScanResult),
    ReadStarted,
    /// The complete current list of simulations
    Sims(Vec<Sim>),
    SlurmStarted,
    Slurm(Result<Snapshot, String>),
    SlurmDisabled(String),
    /// A remote host is connected (again)
    Connected,
    /// The connection to a remote host was lost, or could not be made
    Disconnected(String),
    /// A problem reported by a remote host
    Warning(String),
}

/// An update and the index of its source in `Config::sources`
pub type Tagged = (usize, Update);

/// Sends the updates of one source
#[derive(Clone)]
pub struct Out {
    source: usize,
    tx: Sender<Tagged>,
}

impl Out {
    pub fn new(source: usize, tx: Sender<Tagged>) -> Out {
        Out { source, tx }
    }

    /// False once the receiver is gone
    pub fn send(&self, u: Update) -> bool {
        self.tx.send((self.source, u)).is_ok()
    }
}

pub struct Monitor {
    local: Option<Local>,
    remotes: Vec<Client>,
}

/// The workers for local directories
struct Local {
    fs: Sender<Request>,
    slurm: Option<Sender<()>>,
}

impl Monitor {
    /// Start the local workers, and connect to each remote host in turn.
    /// Connecting may ask for passwords on the terminal, so this must run
    /// before the TUI takes over the terminal.
    pub fn start(cfg: &Config, tx: Sender<Tagged>) -> Monitor {
        let mut local = None;
        let mut remotes = Vec::new();
        for (i, source) in cfg.sources().into_iter().enumerate() {
            let out = Out::new(i, tx.clone());
            match source {
                None => local = Some(Local::start(cfg, out)),
                Some(host) => {
                    let remote = cfg
                        .remotes
                        .iter()
                        .find(|r| r.host == host)
                        .expect("known host");
                    remotes.push(Client::start(cfg, remote, out));
                }
            }
        }
        Monitor { local, remotes }
    }

    pub fn request(&self, r: Request) {
        if let Some(l) = &self.local {
            l.request(r);
        }
        for c in &self.remotes {
            c.request(r);
        }
    }

    /// Whether every source failed to start (all hosts unreachable)
    pub fn all_failed(&self) -> bool {
        self.local.is_none() && self.remotes.iter().all(|c| !c.is_connected())
    }

    /// Whether some remote host is not connected
    pub fn any_disconnected(&self) -> bool {
        self.remotes.iter().any(|c| !c.is_connected())
    }

    /// Reconnect lost hosts one after another, asking for passwords on the
    /// terminal. The TUI must have released the terminal.
    pub fn reconnect(&self) {
        for c in &self.remotes {
            c.reconnect();
        }
    }

    /// Fetches images from remote hosts, for the image loader
    pub fn image_fetch(&self) -> Fetch {
        let fetchers: Vec<(String, client::Fetcher)> = self
            .remotes
            .iter()
            .map(|c| (c.host.clone(), c.fetcher()))
            .collect();
        Box::new(move |host, dir, file| {
            let (_, f) = fetchers
                .iter()
                .find(|(h, _)| h == host)
                .ok_or_else(|| format!("unknown host {host}"))?;
            f.fetch(dir, file)
        })
    }
}

impl Local {
    fn start(cfg: &Config, out: Out) -> Local {
        let (fs_tx, fs_rx) = channel();
        {
            let cfg = cfg.clone();
            let out = out.clone();
            thread::Builder::new()
                .name("simwatch-fs".into())
                .spawn(move || fs_worker(cfg, fs_rx, out))
                .expect("spawning file system thread");
        }
        let slurm = if cfg.slurm {
            let (tx, rx) = channel();
            let cfg = cfg.clone();
            thread::Builder::new()
                .name("simwatch-slurm".into())
                .spawn(move || slurm_worker(cfg, rx, out))
                .expect("spawning slurm thread");
            Some(tx)
        } else {
            out.send(Update::SlurmDisabled("disabled".into()));
            None
        };
        Local { fs: fs_tx, slurm }
    }

    fn request(&self, r: Request) {
        let _ = self.fs.send(r);
        if r == Request::Rescan {
            if let Some(s) = &self.slurm {
                let _ = s.send(());
            }
        }
    }
}

fn fs_worker(cfg: Config, rx: Receiver<Request>, out: Out) {
    let mut sims: Vec<Sim> = Vec::new();
    let mut next_scan = Instant::now();
    let mut next_read = Instant::now();
    let mut pending: Option<Request> = None;
    loop {
        let now = Instant::now();
        let do_scan = pending == Some(Request::Rescan) || now >= next_scan;
        let do_read = do_scan || pending.is_some() || now >= next_read;
        pending = None;
        if do_scan {
            if !out.send(Update::ScanStarted) {
                return;
            }
            let res = discover::scan(&cfg.roots, &cfg.limits);
            sims = merge_found(std::mem::take(&mut sims), &res.sims);
            next_scan = Instant::now() + cfg.scan_interval;
            if !out.send(Update::ScanFinished(res)) {
                return;
            }
        }
        if do_read {
            if !out.send(Update::ReadStarted) {
                return;
            }
            reread(&mut sims, cfg.keep_text);
            next_read = Instant::now() + cfg.refresh_interval;
            if !out.send(Update::Sims(sims.clone())) {
                return;
            }
        }
        let wait = next_scan
            .min(next_read)
            .saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok(r) => pending = Some(r),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        // Coalesce requests that queued up while we were busy
        while let Ok(r) = rx.try_recv() {
            if pending != Some(Request::Rescan) {
                pending = Some(r);
            }
        }
    }
}

/// Keep what we know about simulations that are still there, add new ones
fn merge_found(old: Vec<Sim>, found: &[PathBuf]) -> Vec<Sim> {
    let mut old: HashMap<PathBuf, Sim> = old.into_iter().map(|s| (s.dir.clone(), s)).collect();
    found
        .iter()
        .map(|d| old.remove(d).unwrap_or_else(|| Sim::new(d.clone())))
        .collect()
}

/// Re-read status files whose modification time or size changed. A missing
/// status file is only reported here; the next scan drops the simulation.
///
/// With `keep_text` (when serving a remote client) the text of the files read
/// in this pass is kept in `Sim::text` instead of being parsed.
pub fn reread(sims: &mut [Sim], keep_text: bool) {
    for sim in sims {
        sim.text = None;
        let path = sim.dir.join(STATUS_FILE);
        match fs::metadata(&path) {
            Ok(m) => {
                let mtime = m.modified().ok();
                if sim.read && sim.error.is_none() && sim.mtime == mtime && sim.size == m.len() {
                    continue;
                }
                sim.mtime = mtime;
                sim.size = m.len();
                sim.read = true;
                match read_status_text(&path) {
                    Ok(text) if keep_text => {
                        sim.text = Some(text);
                        sim.error = None;
                    }
                    Ok(text) => match parse_status(&text) {
                        Ok(st) => {
                            sim.status = Some(st);
                            sim.error = None;
                        }
                        Err(e) => sim.error = Some(e),
                    },
                    Err(e) => sim.error = Some(e),
                }
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                sim.error = Some("status file has disappeared".into())
            }
            Err(e) => sim.error = Some(e.to_string()),
        }
    }
}

fn slurm_worker(cfg: Config, rx: Receiver<()>, out: Out) {
    loop {
        if !out.send(Update::SlurmStarted) {
            return;
        }
        let started = Instant::now();
        let msg = match slurm::query(&cfg.squeue_program, cfg.squeue_timeout) {
            Ok(snap) => Update::Slurm(Ok(snap)),
            Err(QueryError::NotFound) => {
                out.send(Update::SlurmDisabled("squeue not found".into()));
                return;
            }
            Err(e) => Update::Slurm(Err(e.to_string())),
        };
        if !out.send(msg) {
            return;
        }
        // Wait for the next regular call, or an explicit request
        let wait = (started + cfg.squeue_interval).saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok(()) => {
                while rx.try_recv().is_ok() {}
                let earliest = started + MIN_SQUEUE_GAP;
                thread::sleep(earliest.saturating_duration_since(Instant::now()));
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Everything from all sources at once, synchronously (for `--print`)
pub struct Collected {
    pub sims: Vec<Sim>,
    pub snaps: HashMap<Option<String>, Snapshot>,
    /// Problems, for stderr
    pub warnings: Vec<String>,
}

pub fn collect_once(cfg: &Config) -> Collected {
    let mut all = Collected {
        sims: Vec::new(),
        snaps: HashMap::new(),
        warnings: Vec::new(),
    };
    if !cfg.roots.is_empty() {
        all.add(None, collect_local(cfg));
    }
    for r in &cfg.remotes {
        match client::collect_once(cfg, r) {
            Ok(once) => all.add(Some(r.host.clone()), once),
            Err(e) => all.warnings.push(format!("{}: {e}", r.host)),
        }
    }
    all
}

impl Collected {
    fn add(&mut self, host: Option<String>, (sims, scan, snap): Once) {
        let prefix = host.as_ref().map(|h| format!("{h}: ")).unwrap_or_default();
        self.sims.extend(sims);
        self.warnings
            .extend(scan.warnings.iter().map(|w| format!("{prefix}{w}")));
        match snap {
            Some(Ok(s)) => {
                self.snaps.insert(host, s);
            }
            Some(Err(e)) => self.warnings.push(format!("{prefix}{e}")),
            None => {}
        }
    }
}

/// The simulations, the scan result and the Slurm snapshot (unless Slurm is
/// off), collected once
pub type Once = (Vec<Sim>, ScanResult, Option<Result<Snapshot, String>>);

/// Everything from the local directories at once, synchronously
pub fn collect_local(cfg: &Config) -> Once {
    let res = discover::scan(&cfg.roots, &cfg.limits);
    let mut sims = merge_found(Vec::new(), &res.sims);
    reread(&mut sims, cfg.keep_text);
    let snap = cfg
        .slurm
        .then(|| slurm::query(&cfg.squeue_program, cfg.squeue_timeout))
        .and_then(|r| match r {
            Err(QueryError::NotFound) => None,
            r => Some(r.map_err(|e| e.to_string())),
        });
    (sims, res, snap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::write;

    #[test]
    fn reread_tracks_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let path = dir.join(STATUS_FILE);
        write(&path, "name = \"one\"").unwrap();
        let mut sims = merge_found(Vec::new(), std::slice::from_ref(&dir));
        reread(&mut sims, false);
        assert_eq!(sims[0].st().name.as_deref(), Some("one"));

        // A broken update keeps the last good contents
        write(&path, "name = = broken").unwrap();
        reread(&mut sims, false);
        assert_eq!(sims[0].st().name.as_deref(), Some("one"));
        assert!(sims[0].error.is_some());

        write(&path, "name = \"two\"").unwrap();
        reread(&mut sims, false);
        assert_eq!(sims[0].st().name.as_deref(), Some("two"));
        assert!(sims[0].error.is_none());

        // Merging keeps known state
        let sims2 = merge_found(sims.clone(), std::slice::from_ref(&dir));
        assert_eq!(sims2[0].st().name.as_deref(), Some("two"));

        // A vanished file is reported, and dropped by the next scan
        fs::remove_file(&path).unwrap();
        reread(&mut sims, false);
        assert_eq!(sims[0].st().name.as_deref(), Some("two"));
        assert!(sims[0].error.as_deref().unwrap().contains("disappeared"));
        assert!(merge_found(sims, &[]).is_empty());
    }

    #[test]
    fn reread_keeps_text() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let path = dir.join(STATUS_FILE);
        write(&path, "name = = broken").unwrap();
        let mut sims = merge_found(Vec::new(), std::slice::from_ref(&dir));
        reread(&mut sims, true);
        // Not parsed: the client does that
        assert_eq!(sims[0].text.as_deref(), Some("name = = broken"));
        assert!(sims[0].status.is_none() && sims[0].error.is_none());
        // Only files read in this pass carry their text
        reread(&mut sims, true);
        assert!(sims[0].text.is_none());
        write(&path, "name = \"ok\"").unwrap();
        reread(&mut sims, true);
        assert_eq!(sims[0].text.as_deref(), Some("name = \"ok\""));
    }

    #[test]
    fn worker_reports() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("s")).unwrap();
        write(tmp.path().join("s").join(STATUS_FILE), "name = \"w\"").unwrap();
        let cfg = Config {
            roots: vec![tmp.path().to_path_buf()],
            slurm: false,
            ..Config::default()
        };
        let (tx, rx) = channel();
        let mon = Monitor::start(&cfg, tx);
        let mut got = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        while got.is_none() && Instant::now() < deadline {
            if let Ok((0, Update::Sims(s))) = rx.recv_timeout(Duration::from_secs(1)) {
                got = Some(s);
            }
        }
        let sims = got.expect("no update from worker");
        assert_eq!(sims.len(), 1);
        assert_eq!(sims[0].st().name.as_deref(), Some("w"));
        mon.request(Request::Rescan);
        let mut rescanned = false;
        while !rescanned && Instant::now() < deadline {
            rescanned = matches!(
                rx.recv_timeout(Duration::from_secs(1)),
                Ok((_, Update::ScanFinished(_)))
            );
        }
        assert!(rescanned);
    }
}
