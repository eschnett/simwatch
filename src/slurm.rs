//! Querying Slurm with a single, rate-limited `squeue` call.

use std::collections::HashMap;
use std::io::{ErrorKind, Read};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

/// Fields requested from squeue, separated by `|`; the reason (free text) comes last
const SQUEUE_FORMAT: &str = "%i|%T|%j|%P|%D|%M|%l|%S|%e|%V|%Q|%E|%R";
const SQUEUE_FIELDS: usize = 13;

/// Missing fields (from an older `simwatch --serve`) are empty
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Job {
    pub id: String,
    pub state: String,
    pub name: String,
    pub partition: String,
    pub nodes: String,
    pub elapsed: String,
    pub limit: String,
    pub reason: String,
    /// Pending: Slurm's estimate, if the scheduler made one. Otherwise: the actual start.
    pub start: Option<DateTime<Utc>>,
    /// Running: when the time limit is reached
    pub end: Option<DateTime<Utc>>,
    pub submit: Option<DateTime<Utc>>,
    pub priority: String,
    pub dependency: String,
}

/// The result of one successful squeue call: the jobs of the host's own Slurm,
/// and those of the other clusters that the host can ask
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub taken: DateTime<Utc>,
    pub jobs: HashMap<String, Job>,
    /// Base job id (without array index or het-job offset) to a full job id
    by_base: HashMap<String, String>,
    /// The name of the host's own cluster, if Slurm said it
    pub own: Option<String>,
    /// The jobs of other clusters, by cluster name in lower case
    pub others: HashMap<String, Snapshot>,
}

/// What `simwatch --squeue` prints: the jobs of one host's Slurm, for a
/// simwatch on another host
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reply {
    pub cluster: Option<String>,
    pub jobs: Vec<Job>,
}

impl Snapshot {
    pub fn new(taken: DateTime<Utc>, jobs: Vec<Job>) -> Self {
        let mut by_base = HashMap::new();
        for j in &jobs {
            by_base
                .entry(base_id(&j.id).to_string())
                .or_insert_with(|| j.id.clone());
        }
        let jobs = jobs.into_iter().map(|j| (j.id.clone(), j)).collect();
        Snapshot {
            taken,
            jobs,
            by_base,
            own: None,
            others: HashMap::new(),
        }
    }

    /// Add the name of the host's own cluster and the jobs of other clusters
    pub fn with_clusters(
        mut self,
        own: Option<String>,
        others: impl IntoIterator<Item = (String, Snapshot)>,
    ) -> Self {
        self.own = own.filter(|c| !c.trim().is_empty());
        self.others = others
            .into_iter()
            .map(|(name, snap)| (name.trim().to_lowercase(), snap))
            .collect();
        self
    }

    /// The jobs of the Slurm that a status file's `slurm.cluster` names.
    /// `None`: that Slurm was not asked, so nothing is known about its jobs.
    /// A file that names no cluster belongs to the host's own Slurm, and so
    /// does every file while the host's own cluster name is not known.
    pub fn of(&self, cluster: Option<&str>) -> Option<&Snapshot> {
        let name = cluster.map(|c| c.trim().to_lowercase());
        let Some(name) = name.filter(|c| !c.is_empty()) else {
            return Some(self);
        };
        if let Some(other) = self.others.get(&name) {
            return Some(other);
        }
        match &self.own {
            Some(own) if own.trim().to_lowercase() != name => None,
            _ => Some(self),
        }
    }

    /// Look up a job by its exact id, or else by its base id
    pub fn find(&self, id: &str) -> Option<&Job> {
        let id = id.trim();
        self.jobs.get(id).or_else(|| {
            self.by_base
                .get(base_id(id))
                .and_then(|full| self.jobs.get(full))
        })
    }

    pub fn count(&self, state: &str) -> usize {
        let other: usize = self.others.values().map(|s| s.count(state)).sum();
        self.jobs.values().filter(|j| j.state == state).count() + other
    }
}

/// `123_4` and `123_[1-5]` (array jobs) and `123+0` (het jobs) all have base id `123`
pub fn base_id(id: &str) -> &str {
    id.split(['_', '+']).next().unwrap_or(id)
}

/// Short Slurm-style state code
pub fn short_state(state: &str) -> &str {
    match state {
        "PENDING" => "PD",
        "RUNNING" => "R",
        "COMPLETING" => "CG",
        "CONFIGURING" => "CF",
        "SUSPENDED" => "S",
        "STOPPED" => "ST",
        "PREEMPTED" => "PR",
        "REQUEUED" => "RQ",
        "RESIZING" => "RS",
        "SIGNALING" => "SI",
        "COMPLETED" => "CD",
        "CANCELLED" => "CA",
        "FAILED" => "F",
        "TIMEOUT" => "TO",
        "NODE_FAIL" => "NF",
        "OUT_OF_MEMORY" => "OOM",
        other => other,
    }
}

/// A squeue time such as `2026-10-09T14:34:13`, in the local time zone of the host
/// that ran squeue. `N/A`, `Unknown` and anything else unparsable give `None`.
pub fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    let t = NaiveDateTime::parse_from_str(s.trim(), "%Y-%m-%dT%H:%M:%S").ok()?;
    Local
        .from_local_datetime(&t)
        .earliest()
        .map(|t| t.with_timezone(&Utc))
}

/// Parses the output of [`SQUEUE_FORMAT`]. Times are converted to UTC here, on the
/// host that ran squeue, so that they are right wherever they are shown.
pub fn parse_squeue(output: &str) -> Vec<Job> {
    output
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line
                .trim()
                .splitn(SQUEUE_FIELDS, '|')
                .map(str::trim)
                .collect();
            if f.len() < 2 || f[0].is_empty() {
                return None;
            }
            let get = |i: usize| f.get(i).copied().unwrap_or("").to_string();
            let time = |i: usize| f.get(i).and_then(|s| parse_time(s));
            let dependency = get(11);
            Some(Job {
                id: get(0),
                state: get(1),
                name: get(2),
                partition: get(3),
                nodes: get(4),
                elapsed: get(5),
                limit: get(6),
                start: time(7),
                end: time(8),
                submit: time(9),
                priority: get(10),
                dependency: if dependency == "(null)" {
                    String::new()
                } else {
                    dependency
                },
                reason: get(12),
            })
        })
        .collect()
}

#[derive(Debug)]
pub enum QueryError {
    /// squeue is not installed; Slurm checks are disabled
    NotFound,
    Timeout,
    Failed(String),
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueryError::NotFound => write!(f, "squeue not found"),
            QueryError::Timeout => write!(f, "squeue timed out"),
            QueryError::Failed(s) => write!(f, "squeue failed: {s}"),
        }
    }
}

/// Run squeue for the current user, killing it after `timeout`.
pub fn query(program: &str, timeout: Duration) -> Result<Snapshot, QueryError> {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_default();
    let mut cmd = Command::new(program);
    cmd.arg("--noheader")
        .arg(format!("--format={SQUEUE_FORMAT}"))
        // ISO times, whatever the user's environment says
        .env("SLURM_TIME_FORMAT", "standard");
    if user.is_empty() {
        cmd.arg("--me");
    } else {
        cmd.arg(format!("--user={user}"));
    }
    let out = run_with_timeout(cmd, timeout)?;
    Ok(Snapshot::new(Utc::now(), parse_squeue(&out)))
}

/// The name of this host's cluster (`ClusterName` in `scontrol show config`),
/// asked with the `scontrol` next to `squeue_program`. `None` if that fails.
pub fn own_cluster(squeue_program: &str, timeout: Duration) -> Option<String> {
    let scontrol = match std::path::Path::new(squeue_program).parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join("scontrol"),
        _ => "scontrol".into(),
    };
    let mut cmd = Command::new(scontrol);
    cmd.args(["show", "config"]);
    parse_cluster_name(&run_with_timeout(cmd, timeout).ok()?)
}

fn parse_cluster_name(config: &str) -> Option<String> {
    config.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "ClusterName" && !value.trim().is_empty()).then(|| value.trim().to_string())
    })
}

/// Ask another cluster's Slurm with a command that prints what
/// `simwatch --squeue` prints, e.g. `ssh HOST simwatch --squeue`
pub fn query_other(command: &[String], timeout: Duration) -> Result<Snapshot, QueryError> {
    let Some((program, args)) = command.split_first() else {
        return Err(QueryError::Failed("no command".into()));
    };
    let mut cmd = Command::new(program);
    cmd.args(args);
    let out = run_with_timeout(cmd, timeout)?;
    let reply: Reply = serde_json::from_str(out.trim())
        .map_err(|e| QueryError::Failed(format!("not the output of simwatch --squeue: {e}")))?;
    Ok(Snapshot::new(Utc::now(), reply.jobs))
}

/// The host's own jobs and those of the clusters it can ask. A cluster whose
/// command fails is left out: nothing is then known about its jobs.
pub fn query_all(
    program: &str,
    own: &Option<String>,
    clusters: &[(String, Vec<String>)],
    timeout: Duration,
) -> Result<Snapshot, QueryError> {
    let snap = query(program, timeout)?;
    let others: Vec<(String, Snapshot)> = clusters
        .iter()
        .filter_map(|(name, command)| Some((name.clone(), query_other(command, timeout).ok()?)))
        .collect();
    Ok(snap.with_clusters(own.clone(), others))
}

fn run_with_timeout(mut cmd: Command, timeout: Duration) -> Result<String, QueryError> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            ErrorKind::NotFound => QueryError::NotFound,
            _ => QueryError::Failed(e.to_string()),
        })?;
    // Read the pipes on helper threads so that a large output cannot block the child
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out_reader = thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let err_reader = thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < timeout => thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(QueryError::Timeout);
            }
            Err(e) => return Err(QueryError::Failed(e.to_string())),
        }
    };
    let out = out_reader.join().unwrap_or_default();
    let err = err_reader.join().unwrap_or_default();
    if status.success() {
        Ok(out)
    } else {
        let msg = err.lines().next().unwrap_or("").trim().to_string();
        Err(QueryError::Failed(if msg.is_empty() {
            status.to_string()
        } else {
            msg
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUT: &str = "\
  123456|RUNNING|bbh-q1|amdq|1|1:02:03|1-00:00:00|2026-10-09T13:00:00|2026-10-10T13:00:00|2026-10-09T12:59:00|15230|(null)|cn101
123457|PENDING|bbh-q2|amdq|2|0:00|1-00:00:00|2026-10-09T14:34:13|2026-10-10T14:34:13|2026-10-09T13:15:54|15000|afterany:123456|(Priority)
123458_[1-5]|PENDING|scan|amddebugq|1|0:00|1:00:00|N/A|N/A|2026-10-09T13:15:54|100|(null)|(Resources)
123459_3|RUNNING|scan2|amddebugq|1|0:10|1:00:00|2026-10-09T13:00:00|2026-10-09T14:00:00|2026-10-09T12:00:00|100|(null)|cn7
garbage
";

    #[test]
    fn parse() {
        let jobs = parse_squeue(OUT);
        // The "garbage" line has a single field and is ignored
        assert_eq!(jobs.len(), 4);
        assert_eq!(jobs[0].id, "123456");
        assert_eq!(jobs[0].elapsed, "1:02:03");
        assert_eq!(jobs[0].reason, "cn101");
        assert_eq!(jobs[0].priority, "15230");
        assert_eq!(jobs[0].dependency, "");
        assert_eq!(jobs[1].dependency, "afterany:123456");
        assert_eq!(jobs[1].start, parse_time("2026-10-09T14:34:13"));
        assert!(jobs[1].start.is_some());
        assert_eq!(jobs[2].start, None);
        assert!(jobs[2].submit.is_some());
        // Too few fields leave the rest empty
        let short = parse_squeue("1|PENDING");
        assert_eq!(short[0].reason, "");
        assert_eq!(short[0].start, None);
    }

    #[test]
    fn times() {
        let t = parse_time("2026-10-09T14:34:13").unwrap();
        let local = t.with_timezone(&Local);
        assert_eq!(
            local.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-10-09 14:34:13"
        );
        assert_eq!(parse_time("N/A"), None);
        assert_eq!(parse_time("Unknown"), None);
        assert_eq!(parse_time(""), None);
        assert_eq!(parse_time("14:34"), None);
    }

    #[test]
    fn serde_old_server() {
        // An older server sends jobs without the newer fields
        let j: Job = serde_json::from_str(r#"{"id":"1","state":"PENDING","name":"a","partition":"q","nodes":"1","elapsed":"0:00","limit":"1:00","reason":"(Priority)"}"#).unwrap();
        assert_eq!(j.reason, "(Priority)");
        assert_eq!(j.start, None);
    }

    #[test]
    fn lookup() {
        let snap = Snapshot::new(Utc::now(), parse_squeue(OUT));
        assert_eq!(snap.find("123456").unwrap().state, "RUNNING");
        assert_eq!(snap.find(" 123457 ").unwrap().reason, "(Priority)");
        assert_eq!(snap.find("123458").unwrap().name, "scan");
        assert_eq!(snap.find("123458_2").unwrap().name, "scan");
        assert_eq!(snap.find("123459_3").unwrap().name, "scan2");
        assert_eq!(snap.find("123459").unwrap().name, "scan2");
        assert!(snap.find("999").is_none());
        assert_eq!(snap.count("PENDING"), 2);
    }

    #[test]
    fn clusters() {
        let own = || Snapshot::new(Utc::now(), parse_squeue(OUT));
        let gpu = Snapshot::new(
            Utc::now(),
            parse_squeue("77|PENDING|g|compute|1|0:00|1:00:00"),
        );
        // Without the host's own cluster name, every file belongs to its Slurm
        let snap = own();
        assert!(snap.of(None).unwrap().find("123456").is_some());
        assert!(snap.of(Some("grillium")).unwrap().find("123456").is_some());
        // With it, another cluster's jobs are unknown unless that cluster was asked
        let snap = own().with_clusters(Some("trillium".into()), []);
        assert!(snap.of(Some("")).unwrap().find("123456").is_some());
        assert!(snap.of(Some("Trillium")).unwrap().find("123456").is_some());
        assert!(snap.of(Some("grillium")).is_none());
        let snap = own().with_clusters(Some("trillium".into()), [("Grillium".to_string(), gpu)]);
        let other = snap.of(Some("grillium")).unwrap();
        assert!(other.find("77").is_some() && other.find("123456").is_none());
        assert!(snap.of(None).unwrap().find("77").is_none());
        assert_eq!(snap.count("PENDING"), 3);
    }

    #[test]
    fn cluster_name_and_reply() {
        let config = "Configuration data as of 2026-11-18T10:00:00\nClusterName             = grillium\nCommitDelay = 0\n";
        assert_eq!(parse_cluster_name(config).as_deref(), Some("grillium"));
        assert_eq!(parse_cluster_name("ClusterName ="), None);
        let reply = Reply {
            cluster: Some("grillium".into()),
            jobs: parse_squeue(OUT),
        };
        let text = serde_json::to_string(&reply).unwrap();
        assert_eq!(serde_json::from_str::<Reply>(&text).unwrap(), reply);
        // The command of another cluster prints such a reply
        let cmd = ["echo".to_string(), text];
        let snap = query_other(&cmd, Duration::from_secs(5)).unwrap();
        assert_eq!(snap.find("123457").unwrap().state, "PENDING");
        assert!(
            query_other(
                &["echo".into(), "squeue: error".into()],
                Duration::from_secs(5)
            )
            .is_err()
        );
    }

    #[test]
    fn missing_program() {
        let r = query("/nonexistent/squeue", Duration::from_secs(1));
        assert!(matches!(r, Err(QueryError::NotFound)));
    }

    #[test]
    fn timeout() {
        let mut cmd = Command::new("sleep");
        cmd.arg("10");
        let start = Instant::now();
        let r = run_with_timeout(cmd, Duration::from_millis(200));
        assert!(matches!(r, Err(QueryError::Timeout)));
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
