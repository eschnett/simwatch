//! Querying Slurm with a single, rate-limited `squeue` call.

use std::collections::HashMap;
use std::io::{ErrorKind, Read};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Fields requested from squeue, separated by `|`
const SQUEUE_FORMAT: &str = "%i|%T|%j|%P|%D|%M|%l|%R";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub state: String,
    pub name: String,
    pub partition: String,
    pub nodes: String,
    pub elapsed: String,
    pub limit: String,
    pub reason: String,
}

/// The result of one successful squeue call
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub taken: DateTime<Utc>,
    pub jobs: HashMap<String, Job>,
    /// Base job id (without array index or het-job offset) to a full job id
    by_base: HashMap<String, String>,
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
        self.jobs.values().filter(|j| j.state == state).count()
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

pub fn parse_squeue(output: &str) -> Vec<Job> {
    output
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.trim().splitn(8, '|').map(str::trim).collect();
            if f.len() < 2 || f[0].is_empty() {
                return None;
            }
            let get = |i: usize| f.get(i).copied().unwrap_or("").to_string();
            Some(Job {
                id: get(0),
                state: get(1),
                name: get(2),
                partition: get(3),
                nodes: get(4),
                elapsed: get(5),
                limit: get(6),
                reason: get(7),
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
        .arg(format!("--format={SQUEUE_FORMAT}"));
    if user.is_empty() {
        cmd.arg("--me");
    } else {
        cmd.arg(format!("--user={user}"));
    }
    let out = run_with_timeout(cmd, timeout)?;
    Ok(Snapshot::new(Utc::now(), parse_squeue(&out)))
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
  123456|RUNNING|bbh-q1|amdq|1|1:02:03|1-00:00:00|cn101
123457|PENDING|bbh-q2|amdq|2|0:00|1-00:00:00|(Priority)
123458_[1-5]|PENDING|scan|amddebugq|1|0:00|1:00:00|(Resources)
123459_3|RUNNING|scan2|amddebugq|1|0:10|1:00:00|cn7
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
