//! Simulations as seen by SimWatch, and their derived health.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};

use crate::format::{Entry, Status};
use crate::slurm::{self, Job};

/// Identifies a simulation: its host (`None` for local) and directory
pub type SimId = (Option<String>, PathBuf);

/// What SimWatch knows about one simulation directory
#[derive(Clone, Debug)]
pub struct Sim {
    /// The remote host, or `None` for a local directory
    pub host: Option<String>,
    pub dir: PathBuf,
    /// Modification time and size of the status file when it was last read
    pub mtime: Option<SystemTime>,
    pub size: u64,
    /// Whether reading the status file has been attempted
    pub read: bool,
    /// The last successfully parsed contents
    pub status: Option<Status>,
    /// Set if the most recent read or parse failed
    pub error: Option<String>,
    /// The raw text, if it was read in the latest pass; only kept when
    /// serving a remote client, which parses it itself
    pub text: Option<String>,
}

impl Sim {
    pub fn new(dir: PathBuf) -> Self {
        Self::on(None, dir)
    }

    pub fn on(host: Option<String>, dir: PathBuf) -> Self {
        Sim {
            host,
            dir,
            mtime: None,
            size: 0,
            read: false,
            status: None,
            error: None,
            text: None,
        }
    }

    pub fn id(&self) -> SimId {
        (self.host.clone(), self.dir.clone())
    }

    pub fn is(&self, id: &SimId) -> bool {
        self.host == id.0 && self.dir == id.1
    }

    /// `host:dir` for remote simulations, else the directory
    pub fn location(&self) -> String {
        location(self.host.as_deref(), &self.dir)
    }

    pub fn st(&self) -> &Status {
        static EMPTY: std::sync::OnceLock<Status> = std::sync::OnceLock::new();
        self.status
            .as_ref()
            .unwrap_or_else(|| EMPTY.get_or_init(Status::default))
    }

    /// The last path component of the directory
    pub fn dir_name(&self) -> String {
        self.dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.dir.display().to_string())
    }

    pub fn display_name(&self) -> String {
        match &self.st().name {
            Some(n) if !n.trim().is_empty() => n.clone(),
            _ => format!("({})", self.dir_name()),
        }
    }

    /// The time of the last update: `updated` if given, else the file mtime
    pub fn last_update(&self) -> Option<DateTime<Utc>> {
        self.st()
            .updated
            .or_else(|| self.mtime.map(DateTime::<Utc>::from))
    }

    /// Seconds since the last update
    pub fn age(&self, now: DateTime<Utc>) -> Option<f64> {
        self.last_update()
            .map(|t| (now - t).num_milliseconds() as f64 / 1000.0)
    }

    /// Used for "newest first" ordering
    pub fn sort_time(&self) -> Option<DateTime<Utc>> {
        self.st().started.or_else(|| self.last_update())
    }

    pub fn job<'a>(&self, snap: Option<&'a slurm::Snapshot>) -> Option<&'a Job> {
        let id = self.st().slurm.job_id.as_deref()?;
        snap?.find(id)
    }

    /// Fraction of the simulation done: `progress.fraction`, else `time / time_end`
    pub fn fraction(&self) -> Option<f64> {
        let p = &self.st().progress;
        if let Some(f) = p.fraction.filter(|f| f.is_finite()) {
            return Some(f.clamp(0.0, 1.0));
        }
        let (t, t_end) = (p.time?, p.time_end?);
        (t_end > 0.0 && t.is_finite()).then(|| (t / t_end).clamp(0.0, 1.0))
    }

    /// Simulation time per wall-clock hour, and whether it is a derived average
    pub fn speed(&self) -> Option<(f64, bool)> {
        let p = &self.st().progress;
        if let Some(s) = p.speed {
            return Some((s, false));
        }
        let wall = p.walltime?;
        let dt = p.time? - p.time_start.unwrap_or(0.0);
        (wall > 0.0 && dt > 0.0).then(|| (dt / wall * 3600.0, true))
    }

    /// Estimated seconds until the end, and whether it comes from an average
    pub fn eta(&self) -> Option<(f64, bool)> {
        let p = &self.st().progress;
        if p.fraction.is_none()
            && let (Some(t), Some(t_end), Some((speed, avg))) = (p.time, p.time_end, self.speed())
        {
            let remaining = t_end - t;
            return (speed > 0.0 && remaining > 0.0).then(|| (remaining / speed * 3600.0, avg));
        }
        // From the fraction done in the wall time so far
        let (f, wall) = (self.fraction()?, p.walltime?);
        (f > 0.0 && f < 1.0 && wall > 0.0).then(|| (wall * (1.0 - f) / f, true))
    }

    /// The headline values named by `summary` that exist
    pub fn summary(&self) -> Vec<SummaryItem<'_>> {
        let st = self.st();
        st.summary
            .iter()
            .filter_map(|k| {
                let value = st.values.iter().find(|e| e.key == k.key)?;
                let label = k
                    .label
                    .clone()
                    .or_else(|| value.label.clone())
                    .unwrap_or_else(|| short_label(&k.key));
                Some(SummaryItem {
                    label,
                    value,
                    history: st.history.get(&k.key),
                })
            })
            .collect()
    }

    /// Number of this simulation's Slurm job among all its jobs, if there were earlier ones
    pub fn job_number(&self) -> Option<usize> {
        let n = self.st().slurm.previous_job_ids.len();
        (n > 0).then_some(n + 1)
    }

    /// The job to show: the next one while waiting for it, else the current one.
    /// Also returns the job's number within the simulation, and whether it is the next job.
    pub fn shown_job(&self) -> Option<(&str, usize, bool)> {
        let s = &self.st().slurm;
        let before = s.previous_job_ids.len();
        match (&s.next_job_id, &s.job_id) {
            (Some(next), cur) => Some((next, before + 1 + usize::from(cur.is_some()), true)),
            (None, Some(cur)) => Some((cur, before + 1, false)),
            (None, None) => None,
        }
    }
}

pub struct SummaryItem<'a> {
    pub label: String,
    pub value: &'a Entry,
    pub history: Option<&'a [f64]>,
}

/// The last component of a dotted key: `shells.r2.ham_l2` -> `ham_l2`
pub fn short_label(key: &str) -> String {
    key.rsplit('.').next().unwrap_or(key).to_string()
}

/// Exponential growth rate: the least-squares slope of ln y against t, over the
/// finite positive points. Without times, the index is used.
pub fn growth_rate(time: Option<&[f64]>, y: &[f64]) -> Option<f64> {
    // Series and times are aligned at their ends
    let n = time.map_or(y.len(), |t| t.len().min(y.len()));
    let y = &y[y.len() - n..];
    let pts: Vec<(f64, f64)> = (0..n)
        .map(|i| (time.map_or(i as f64, |t| t[t.len() - n + i]), y[i]))
        .filter(|(t, v)| t.is_finite() && v.is_finite() && *v > 0.0)
        .map(|(t, v)| (t, v.ln()))
        .collect();
    if pts.len() < 3 {
        return None;
    }
    let m = pts.len() as f64;
    let (st, sy) = pts.iter().fold((0.0, 0.0), |(a, b), (t, v)| (a + t, b + v));
    let (mt, my) = (st / m, sy / m);
    let (num, den) = pts.iter().fold((0.0, 0.0), |(a, b), (t, v)| {
        (a + (t - mt) * (v - my), b + (t - mt) * (t - mt))
    });
    (den > 0.0).then(|| num / den)
}

/// Up, down or flat, comparing the last finite value with the first
pub fn trend(y: &[f64]) -> Option<char> {
    let first = y.iter().find(|v| v.is_finite())?;
    let last = y.iter().rev().find(|v| v.is_finite())?;
    if *first == 0.0 {
        return None;
    }
    let r = last / first;
    Some(if r > 1.1 {
        '↑'
    } else if r < 0.9 {
        '↓'
    } else {
        '→'
    })
}

/// Settings that influence the derived health
#[derive(Clone, Debug)]
pub struct HealthParams {
    /// A simulation is stale after `stale_factor * update_interval` seconds without update
    pub stale_factor: f64,
    /// ... but never before this many seconds
    pub stale_floor: f64,
    /// Assumed `update_interval` when a simulation does not provide one
    pub default_update_interval: f64,
}

impl Default for HealthParams {
    fn default() -> Self {
        HealthParams {
            stale_factor: 3.0,
            stale_floor: 180.0,
            default_update_interval: 60.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Health {
    // Ordered by urgency, for sorting
    Lost,
    Failed,
    Stale,
    Unreadable,
    Running,
    Queued,
    Stopped,
    Unknown,
    Finished,
}

impl Health {
    pub fn label(self) -> &'static str {
        match self {
            Health::Queued => "queued",
            Health::Running => "running",
            Health::Stale => "stale",
            Health::Lost => "lost",
            Health::Finished => "finished",
            Health::Failed => "failed",
            Health::Stopped => "stopped",
            Health::Unreadable => "unreadable",
            Health::Unknown => "unknown",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            Health::Queued => "◷",
            Health::Running => "▶",
            Health::Stale => "?",
            Health::Lost => "✗",
            Health::Finished => "✓",
            Health::Failed => "✗",
            Health::Stopped => "‖",
            Health::Unreadable => "!",
            Health::Unknown => "·",
        }
    }

    /// Finished or failed: nothing will happen any more
    pub fn is_done(self) -> bool {
        matches!(self, Health::Finished | Health::Failed)
    }
}

/// `host:path`, or `path` for local paths
pub fn location(host: Option<&str>, path: &Path) -> String {
    match host {
        Some(h) => format!("{h}:{}", path.display()),
        None => path.display().to_string(),
    }
}

pub fn health(
    sim: &Sim,
    now: DateTime<Utc>,
    snap: Option<&slurm::Snapshot>,
    params: &HealthParams,
) -> Health {
    let Some(st) = &sim.status else {
        return if sim.error.is_some() {
            Health::Unreadable
        } else {
            Health::Unknown
        };
    };
    let reported = st.status.as_deref().map(|s| s.trim().to_ascii_lowercase());
    let interval = st
        .update_interval
        .filter(|x| *x > 0.0)
        .unwrap_or(params.default_update_interval);
    let threshold = (params.stale_factor * interval).max(params.stale_floor);
    let fresh = sim.age(now).is_none_or(|a| a <= threshold);

    let kind = match reported.as_deref() {
        Some("finished" | "done" | "completed" | "complete" | "success" | "succeeded") => {
            Health::Finished
        }
        Some("failed" | "error" | "crashed" | "aborted" | "killed" | "cancelled") => Health::Failed,
        Some("stopped" | "checkpointed" | "requeued" | "paused" | "suspended") => Health::Stopped,
        Some("queued" | "pending" | "submitted") => Health::Queued,
        _ => Health::Running,
    };

    // A resubmitted simulation waits for its next job
    let next = st
        .slurm
        .next_job_id
        .as_deref()
        .filter(|_| matches!(kind, Health::Queued | Health::Stopped | Health::Failed));
    // Some(None) means: Slurm was asked and the job is gone
    let job: Option<Option<&Job>> = match (next.or(st.slurm.job_id.as_deref()), snap) {
        (Some(id), Some(snap)) => Some(snap.find(id)),
        _ => None,
    };
    fn job_state(j: &Job) -> &str {
        j.state.as_str()
    }

    match kind {
        Health::Queued | Health::Stopped | Health::Failed
            if next.is_some() || kind == Health::Queued =>
        {
            match job {
                Some(Some(j)) if job_state(j) == "PENDING" => Health::Queued,
                // Started, but the simulation has not written its first status yet
                Some(Some(_)) => Health::Running,
                Some(None) => Health::Lost,
                None => kind,
            }
        }
        Health::Finished | Health::Failed | Health::Stopped => kind,
        // "running", "starting", anything else, or no status at all
        _ => match job {
            Some(None) => Health::Lost,
            Some(Some(j)) if job_state(j) == "PENDING" => Health::Queued,
            _ if !fresh => Health::Stale,
            _ => Health::Running,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::parse_status;
    use crate::slurm::{Snapshot, parse_squeue};
    use chrono::TimeZone;

    fn sim(text: &str) -> Sim {
        let mut s = Sim::new(PathBuf::from("/runs/sim1"));
        s.status = Some(parse_status(text).unwrap());
        s
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 2, 16, 0, 0).unwrap()
    }

    fn snap() -> Snapshot {
        Snapshot::new(
            now(),
            parse_squeue(
                "100|RUNNING|a|q|1|1:00|2:00|N/A|N/A|N/A|1||cn1\n\
                 101|PENDING|b|q|1|0:00|2:00|N/A|N/A|N/A|1||(Priority)\n",
            ),
        )
    }

    fn h(text: &str, snap: Option<&Snapshot>) -> Health {
        health(&sim(text), now(), snap, &HealthParams::default())
    }

    #[test]
    fn fresh_and_stale() {
        let fresh = "status = \"running\"\nupdated = 2026-10-02T15:59:00Z";
        assert_eq!(h(fresh, None), Health::Running);
        // 10 minutes old, default interval 60 s × 3 = 180 s
        let stale = "status = \"running\"\nupdated = 2026-10-02T15:50:00Z";
        assert_eq!(h(stale, None), Health::Stale);
        // A long update interval makes it fresh again
        let slow = format!("{stale}\nupdate_interval = 600");
        assert_eq!(h(&slow, None), Health::Running);
        // No status key at all is treated like running
        assert_eq!(h("updated = 2026-10-02T15:59:00Z", None), Health::Running);
    }

    #[test]
    fn reported_states() {
        assert_eq!(h("status = \"finished\"", None), Health::Finished);
        assert_eq!(h("status = \"Failed\"", None), Health::Failed);
        assert_eq!(h("status = \"stopped\"", None), Health::Stopped);
        assert_eq!(h("status = \"queued\"", None), Health::Queued);
    }

    #[test]
    fn slurm_states() {
        let snap = snap();
        let s = |id: &str, status: &str| {
            format!(
                "status = \"{status}\"\nupdated = 2026-10-02T15:59:00Z\nslurm.job_id = \"{id}\""
            )
        };
        assert_eq!(h(&s("100", "running"), Some(&snap)), Health::Running);
        assert_eq!(h(&s("999", "running"), Some(&snap)), Health::Lost);
        assert_eq!(h(&s("999", "running"), None), Health::Running);
        assert_eq!(h(&s("101", "queued"), Some(&snap)), Health::Queued);
        assert_eq!(h(&s("100", "queued"), Some(&snap)), Health::Running);
        assert_eq!(h(&s("999", "queued"), Some(&snap)), Health::Lost);
        assert_eq!(h(&s("999", "finished"), Some(&snap)), Health::Finished);
    }

    #[test]
    fn next_job() {
        let snap = snap();
        let s = |status: &str, next: &str| {
            format!(
                "status = \"{status}\"\nupdated = 2026-10-02T15:00:00Z\n\
                 [slurm]\njob_id = \"50\"\nnext_job_id = \"{next}\""
            )
        };
        // The old job 50 is gone; what counts is the next one
        assert_eq!(h(&s("queued", "101"), Some(&snap)), Health::Queued);
        assert_eq!(h(&s("stopped", "101"), Some(&snap)), Health::Queued);
        assert_eq!(h(&s("failed", "101"), Some(&snap)), Health::Queued);
        assert_eq!(h(&s("stopped", "100"), Some(&snap)), Health::Running);
        assert_eq!(h(&s("stopped", "999"), Some(&snap)), Health::Lost);
        assert_eq!(h(&s("stopped", "101"), None), Health::Stopped);
        assert_eq!(h(&s("finished", "999"), Some(&snap)), Health::Finished);
        // Without a next job, a stopped run is just stopped
        let stopped = "status = \"stopped\"\n[slurm]\njob_id = \"50\"";
        assert_eq!(h(stopped, Some(&snap)), Health::Stopped);
    }

    #[test]
    fn summary_and_trends() {
        let s = sim(r#"
summary = ["a.x", "missing", { key = "b", label = "B" }, "c"]
c = { value = 3, label = "Cee" }
b = 2
[a]
x = 1.5
[history]
"a.x" = [1.0, 1.2, 1.5]
"#);
        let items = s.summary();
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["x", "B", "Cee"]);
        assert_eq!(items[0].history.map(|h| h.len()), Some(3));
        assert!(items[1].history.is_none());

        let t: Vec<f64> = (0..10).map(|i| i as f64).collect();
        let y: Vec<f64> = t.iter().map(|t| 1e-6 * (0.3 * t).exp()).collect();
        assert!((growth_rate(Some(&t), &y).unwrap() - 0.3).abs() < 1e-12);
        assert!((growth_rate(None, &y).unwrap() - 0.3).abs() < 1e-12);
        assert_eq!(growth_rate(None, &[1.0, -1.0, f64::NAN]), None);
        assert_eq!(trend(&y), Some('↑'));
        assert_eq!(trend(&[2.0, f64::NAN, 1.0]), Some('↓'));
        assert_eq!(trend(&[1.0, 1.05]), Some('→'));
        assert_eq!(trend(&[f64::NAN]), None);

        let s = sim("[slurm]\nprevious_job_ids = [1, 2]");
        assert_eq!(s.job_number(), Some(3));
        let s = sim("[slurm]\njob_id = 3\nprevious_job_ids = [1, 2]\nnext_job_id = 4");
        assert_eq!(s.shown_job(), Some(("4", 4, true)));
        let s = sim("[slurm]\nnext_job_id = 4");
        assert_eq!(s.shown_job(), Some(("4", 1, true)));
        assert_eq!(sim("").job_number(), None);
    }

    #[test]
    fn unreadable() {
        let mut s = Sim::new(PathBuf::from("/x"));
        s.error = Some("bad".into());
        assert_eq!(
            health(&s, now(), None, &HealthParams::default()),
            Health::Unreadable
        );
        // A previous good parse wins over a later error
        let mut s = sim("status = \"finished\"");
        s.error = Some("bad".into());
        assert_eq!(
            health(&s, now(), None, &HealthParams::default()),
            Health::Finished
        );
    }

    #[test]
    fn derived_numbers() {
        let s =
            sim("[progress]\ntime = 25.0\ntime_end = 100.0\nwalltime = 3600.0\ntime_start = 5.0");
        assert_eq!(s.fraction(), Some(0.25));
        assert_eq!(s.speed(), Some((20.0, true)));
        assert_eq!(s.eta(), Some((75.0 / 20.0 * 3600.0, true)));
        let s = sim("[progress]\ntime = 25.0\ntime_end = 100.0\nspeed = 75.0");
        assert_eq!(s.speed(), Some((75.0, false)));
        assert_eq!(s.eta(), Some((3600.0, false)));
        assert_eq!(sim("").display_name(), "(sim1)");

        // Progress by fraction, e.g. frames
        let s = sim("[progress]\nfraction = 0.25\nwalltime = 600.0\ntime = 3.0");
        assert_eq!(s.fraction(), Some(0.25));
        assert_eq!(s.eta(), Some((1800.0, true)));
        assert_eq!(sim("[progress]\nfraction = 1.5").fraction(), Some(1.0));
        assert_eq!(sim("name = \"x\"").display_name(), "x");
    }
}
