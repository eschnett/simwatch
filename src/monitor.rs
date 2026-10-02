//! Background workers: one thread for all file system access and one for
//! Slurm. The UI talks to them through channels and never blocks on I/O.
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
use crate::format::{STATUS_FILE, read_status_file};
use crate::model::Sim;
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
}

pub struct Monitor {
    fs: Sender<Request>,
    slurm: Option<Sender<()>>,
}

impl Monitor {
    pub fn start(cfg: &Config, out: Sender<Update>) -> Monitor {
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
            let _ = out.send(Update::SlurmDisabled("disabled".into()));
            None
        };
        Monitor { fs: fs_tx, slurm }
    }

    pub fn request(&self, r: Request) {
        let _ = self.fs.send(r);
        if r == Request::Rescan {
            if let Some(s) = &self.slurm {
                let _ = s.send(());
            }
        }
    }
}

fn fs_worker(cfg: Config, rx: Receiver<Request>, out: Sender<Update>) {
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
            if out.send(Update::ScanStarted).is_err() {
                return;
            }
            let res = discover::scan(&cfg.roots, &cfg.limits);
            sims = merge_found(std::mem::take(&mut sims), &res.sims);
            next_scan = Instant::now() + cfg.scan_interval;
            if out.send(Update::ScanFinished(res)).is_err() {
                return;
            }
        }
        if do_read {
            if out.send(Update::ReadStarted).is_err() {
                return;
            }
            reread(&mut sims);
            next_read = Instant::now() + cfg.refresh_interval;
            if out.send(Update::Sims(sims.clone())).is_err() {
                return;
            }
        }
        let wait = next_scan.min(next_read).saturating_duration_since(Instant::now());
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
pub fn reread(sims: &mut [Sim]) {
    for sim in sims {
        let path = sim.dir.join(STATUS_FILE);
        match fs::metadata(&path) {
            Ok(m) => {
                let mtime = m.modified().ok();
                let known = sim.status.is_some() || sim.error.is_some();
                if known && sim.error.is_none() && sim.mtime == mtime && sim.size == m.len() {
                    continue;
                }
                sim.mtime = mtime;
                sim.size = m.len();
                match read_status_file(&path) {
                    Ok(st) => {
                        sim.status = Some(st);
                        sim.error = None;
                    }
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

fn slurm_worker(cfg: Config, rx: Receiver<()>, out: Sender<Update>) {
    loop {
        if out.send(Update::SlurmStarted).is_err() {
            return;
        }
        let started = Instant::now();
        let msg = match slurm::query(&cfg.squeue_program, cfg.squeue_timeout) {
            Ok(snap) => Update::Slurm(Ok(snap)),
            Err(QueryError::NotFound) => {
                let _ = out.send(Update::SlurmDisabled("squeue not found".into()));
                return;
            }
            Err(e) => Update::Slurm(Err(e.to_string())),
        };
        if out.send(msg).is_err() {
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

/// Everything at once, synchronously (for `--print`)
pub fn collect_once(cfg: &Config) -> (Vec<Sim>, ScanResult, Option<Result<Snapshot, String>>) {
    let res = discover::scan(&cfg.roots, &cfg.limits);
    let mut sims = merge_found(Vec::new(), &res.sims);
    reread(&mut sims);
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
        reread(&mut sims);
        assert_eq!(sims[0].st().name.as_deref(), Some("one"));

        // A broken update keeps the last good contents
        write(&path, "name = = broken").unwrap();
        reread(&mut sims);
        assert_eq!(sims[0].st().name.as_deref(), Some("one"));
        assert!(sims[0].error.is_some());

        write(&path, "name = \"two\"").unwrap();
        reread(&mut sims);
        assert_eq!(sims[0].st().name.as_deref(), Some("two"));
        assert!(sims[0].error.is_none());

        // Merging keeps known state
        let sims2 = merge_found(sims.clone(), std::slice::from_ref(&dir));
        assert_eq!(sims2[0].st().name.as_deref(), Some("two"));

        // A vanished file is reported, and dropped by the next scan
        fs::remove_file(&path).unwrap();
        reread(&mut sims);
        assert_eq!(sims[0].st().name.as_deref(), Some("two"));
        assert!(sims[0].error.as_deref().unwrap().contains("disappeared"));
        assert!(merge_found(sims, &[]).is_empty());
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
            if let Ok(Update::Sims(s)) = rx.recv_timeout(Duration::from_secs(1)) {
                got = Some(s);
            }
        }
        let sims = got.expect("no update from worker");
        assert_eq!(sims.len(), 1);
        assert_eq!(sims[0].st().name.as_deref(), Some("w"));
        mon.request(Request::Rescan);
        let mut rescanned = false;
        while !rescanned && Instant::now() < deadline {
            rescanned = matches!(rx.recv_timeout(Duration::from_secs(1)), Ok(Update::ScanFinished(_)));
        }
        assert!(rescanned);
    }
}
