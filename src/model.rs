//! Simulations as seen by SimWatch, and their derived health.

use std::path::PathBuf;
use std::time::SystemTime;

use chrono::{DateTime, Utc};

use crate::format::Status;
use crate::slurm::{self, Job};

/// What SimWatch knows about one simulation directory
#[derive(Clone, Debug)]
pub struct Sim {
    pub dir: PathBuf,
    /// Modification time and size of the status file when it was last read
    pub mtime: Option<SystemTime>,
    pub size: u64,
    /// The last successfully parsed contents
    pub status: Option<Status>,
    /// Set if the most recent read or parse failed
    pub error: Option<String>,
}

impl Sim {
    pub fn new(dir: PathBuf) -> Self {
        Sim {
            dir,
            mtime: None,
            size: 0,
            status: None,
            error: None,
        }
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

    /// Fraction of the simulation done, from `time / time_end`
    pub fn fraction(&self) -> Option<f64> {
        let p = &self.st().progress;
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

    /// Estimated seconds until `time_end` is reached
    pub fn eta(&self) -> Option<f64> {
        let p = &self.st().progress;
        let remaining = p.time_end? - p.time?;
        let (speed, _) = self.speed()?;
        (speed > 0.0 && remaining > 0.0).then(|| remaining / speed * 3600.0)
    }
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

    // Some(None) means: Slurm was asked and the job is gone
    let job: Option<Option<&Job>> = match (&st.slurm.job_id, snap) {
        (Some(id), Some(snap)) => Some(snap.find(id)),
        _ => None,
    };
    fn job_state(j: &Job) -> &str {
        j.state.as_str()
    }

    match reported.as_deref() {
        Some("finished" | "done" | "completed" | "complete" | "success" | "succeeded") => {
            Health::Finished
        }
        Some("failed" | "error" | "crashed" | "aborted" | "killed" | "cancelled") => {
            Health::Failed
        }
        Some("stopped" | "checkpointed" | "requeued" | "paused" | "suspended") => Health::Stopped,
        Some("queued" | "pending" | "submitted") => match job {
            Some(Some(j)) if job_state(j) == "PENDING" => Health::Queued,
            // Started, but the simulation has not written its first status yet
            Some(Some(_)) => Health::Running,
            Some(None) => Health::Lost,
            None => Health::Queued,
        },
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
            parse_squeue("100|RUNNING|a|q|1|1:00|2:00|cn1\n101|PENDING|b|q|1|0:00|2:00|(Priority)\n"),
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
            format!("status = \"{status}\"\nupdated = 2026-10-02T15:59:00Z\nslurm.job_id = \"{id}\"")
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
    fn unreadable() {
        let mut s = Sim::new(PathBuf::from("/x"));
        s.error = Some("bad".into());
        assert_eq!(health(&s, now(), None, &HealthParams::default()), Health::Unreadable);
        // A previous good parse wins over a later error
        let mut s = sim("status = \"finished\"");
        s.error = Some("bad".into());
        assert_eq!(health(&s, now(), None, &HealthParams::default()), Health::Finished);
    }

    #[test]
    fn derived_numbers() {
        let s = sim(
            "[progress]\ntime = 25.0\ntime_end = 100.0\nwalltime = 3600.0\ntime_start = 5.0",
        );
        assert_eq!(s.fraction(), Some(0.25));
        assert_eq!(s.speed(), Some((20.0, true)));
        assert_eq!(s.eta(), Some(75.0 / 20.0 * 3600.0));
        let s = sim("[progress]\ntime = 25.0\ntime_end = 100.0\nspeed = 75.0");
        assert_eq!(s.speed(), Some((75.0, false)));
        assert_eq!(s.eta(), Some(3600.0));
        assert_eq!(sim("").display_name(), "(sim1)");
        assert_eq!(sim("name = \"x\"").display_name(), "x");
    }
}
