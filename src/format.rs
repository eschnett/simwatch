//! Parsing of `simwatch.toml` status files.
//!
//! SimWatch is an opportunistic viewer: every key is optional, well-known
//! keys with an unexpected type are shown generically, and anything we do not
//! understand is kept as a flattened "problem-specific" entry.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use toml::{Table, Value};

/// Name of the status file a simulation writes into its directory.
pub const STATUS_FILE: &str = "simwatch.toml";

/// Status files larger than this are not read.
pub const MAX_STATUS_BYTES: u64 = 128 * 1024;

/// At most this many images per simulation are considered.
pub const MAX_IMAGES: usize = 10;

/// At most this many problem-specific entries are kept per simulation.
pub const MAX_EXTRA: usize = 1000;

/// At most this many headline values are shown
pub const MAX_SUMMARY: usize = 6;

/// History series are cut to their last this many points
pub const MAX_HISTORY_POINTS: usize = 200;

/// At most this many history series are kept
pub const MAX_HISTORY_SERIES: usize = 32;

/// The parsed contents of a status file. Everything is optional.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub name: Option<String>,
    pub status: Option<String>,
    pub updated: Option<DateTime<Utc>>,
    pub started: Option<DateTime<Utc>>,
    /// Expected seconds between updates
    pub update_interval: Option<f64>,
    pub message: Option<String>,
    pub code: Option<String>,
    pub host: Option<String>,
    pub pid: Option<i64>,
    /// Simulations with the same group belong together, e.g. a parameter study
    pub group: Option<String>,
    /// Keys (as in `values`) of the headline values
    pub summary: Vec<SummaryKey>,
    pub progress: Progress,
    pub resources: Resources,
    pub slurm: SlurmInfo,
    pub black_holes: Vec<BlackHole>,
    pub images: Vec<ImageRef>,
    pub history: History,
    /// Problem-specific (or not understood) entries, flattened to dotted keys
    pub extra: Vec<Entry>,
    /// The whole document flattened to dotted keys, for looking up `summary`
    pub values: Vec<Entry>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SummaryKey {
    pub key: String,
    pub label: Option<String>,
}

/// A short window of recent values, written by the simulation
#[derive(Clone, Debug, Default, PartialEq)]
pub struct History {
    /// The x axis; without it, points are equally spaced
    pub time: Option<Vec<f64>>,
    /// Named series, in key order
    pub series: Vec<(String, Vec<f64>)>,
}

impl History {
    pub fn get(&self, key: &str) -> Option<&[f64]> {
        self.series
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_slice())
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    pub iteration: Option<i64>,
    pub time: Option<f64>,
    /// Simulation time when the current job started (for average speed)
    pub time_start: Option<f64>,
    pub time_end: Option<f64>,
    pub time_unit: Option<String>,
    /// Fraction done (0 to 1), for runs whose progress is not simulation time
    pub fraction: Option<f64>,
    /// Wall-clock seconds since the current job started
    pub walltime: Option<f64>,
    pub walltime_limit: Option<f64>,
    /// Simulation time per wall-clock hour
    pub speed: Option<f64>,
    pub speed_unit: Option<String>,
    pub checkpoint: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resources {
    pub nodes: Option<i64>,
    pub tasks: Option<i64>,
    pub threads: Option<i64>,
    pub gpus: Option<i64>,
    pub memory_bytes: Option<f64>,
    pub memory_peak_bytes: Option<f64>,
    pub memory_limit_bytes: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SlurmInfo {
    pub job_id: Option<String>,
    pub job_name: Option<String>,
    pub partition: Option<String>,
    /// Submitted, but has not written yet
    pub next_job_id: Option<String>,
    /// Earlier jobs of this simulation, oldest first
    pub previous_job_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BlackHole {
    pub name: Option<String>,
    pub mass: Option<f64>,
    pub irreducible_mass: Option<f64>,
    /// Dimensionless spin, either a magnitude or a vector
    pub spin: Option<Vec<f64>>,
    pub position: Option<Vec<f64>>,
    pub found: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImageRef {
    pub file: String,
    pub title: Option<String>,
    pub description: Option<String>,
}

/// A generic key-value pair, possibly annotated with a unit and a label.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub key: String,
    pub value: Value,
    pub unit: Option<String>,
    pub label: Option<String>,
}

/// Read and parse a status file, refusing files that are too large.
pub fn read_status_file(path: &Path) -> Result<Status, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_STATUS_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_STATUS_BYTES {
        return Err(format!("file larger than {} KiB", MAX_STATUS_BYTES / 1024));
    }
    let text = String::from_utf8_lossy(&bytes);
    parse_status(&text)
}

/// Parse the text of a status file.
pub fn parse_status(text: &str) -> Result<Status, String> {
    let mut table: Table = text.parse().map_err(|e: toml::de::Error| {
        // Keep only the first line; TOML errors include a multi-line excerpt
        e.message().lines().next().unwrap_or("parse error").to_string()
    })?;
    Ok(status_from_table(&mut table))
}

fn status_from_table(t: &mut Table) -> Status {
    let (history, history_rest) = take_history(t);
    let summary = take_summary(t);
    let mut values = Vec::new();
    flatten("", &Value::Table(t.clone()), &mut values);
    let mut st = Status {
        group: take_string(t, "group"),
        summary,
        history,
        values,
        name: take_string(t, "name"),
        status: take_string(t, "status"),
        updated: take_time(t, "updated"),
        started: take_time(t, "started"),
        update_interval: take_number(t, "update_interval"),
        message: take_string(t, "message"),
        code: take_string(t, "code"),
        host: take_string(t, "host"),
        pid: take_int(t, "pid"),
        ..Status::default()
    };

    if let Some(mut p) = take_table(t, "progress") {
        st.progress = Progress {
            iteration: take_int(&mut p, "iteration"),
            time: take_number(&mut p, "time"),
            time_start: take_number(&mut p, "time_start"),
            time_end: take_number(&mut p, "time_end"),
            time_unit: take_string(&mut p, "time_unit"),
            fraction: take_number(&mut p, "fraction"),
            walltime: take_number(&mut p, "walltime"),
            walltime_limit: take_number(&mut p, "walltime_limit"),
            speed: take_number(&mut p, "speed"),
            speed_unit: take_string(&mut p, "speed_unit"),
            checkpoint: take_string(&mut p, "checkpoint"),
        };
        put_back(t, "progress", p);
    }

    if let Some(mut r) = take_table(t, "resources") {
        st.resources = Resources {
            nodes: take_int(&mut r, "nodes"),
            tasks: take_int(&mut r, "tasks"),
            threads: take_int(&mut r, "threads"),
            gpus: take_int(&mut r, "gpus"),
            memory_bytes: take_number(&mut r, "memory_bytes"),
            memory_peak_bytes: take_number(&mut r, "memory_peak_bytes"),
            memory_limit_bytes: take_number(&mut r, "memory_limit_bytes"),
        };
        put_back(t, "resources", r);
    }

    if let Some(mut s) = take_table(t, "slurm") {
        st.slurm = SlurmInfo {
            job_id: take_id(&mut s, "job_id"),
            job_name: take_string(&mut s, "job_name"),
            partition: take_string(&mut s, "partition"),
            next_job_id: take_id(&mut s, "next_job_id"),
            previous_job_ids: take_if(&mut s, "previous_job_ids", |v| {
                v.as_array()?.iter().map(as_id).collect()
            })
            .unwrap_or_default(),
        };
        put_back(t, "slurm", s);
    }

    if let Some(Value::Array(arr)) = t.get("black_holes") {
        if arr.iter().all(Value::is_table) {
            let Some(Value::Array(arr)) = t.remove("black_holes") else {
                unreachable!()
            };
            let mut rest = Vec::new();
            for v in arr {
                let Value::Table(mut b) = v else { unreachable!() };
                st.black_holes.push(BlackHole {
                    name: take_string(&mut b, "name"),
                    mass: take_number(&mut b, "mass"),
                    irreducible_mass: take_number(&mut b, "irreducible_mass"),
                    spin: take_vector(&mut b, "spin"),
                    position: take_vector(&mut b, "position"),
                    found: take_bool(&mut b, "found"),
                });
                rest.push(Value::Table(b));
            }
            if rest.iter().any(|v| v.as_table().is_some_and(|b| !b.is_empty())) {
                t.insert("black_holes".into(), Value::Array(rest));
            }
        }
    }

    if let Some(Value::Array(arr)) = t.get("images") {
        let parsed: Option<Vec<ImageRef>> = arr.iter().map(image_ref).collect();
        if let Some(mut images) = parsed {
            images.truncate(MAX_IMAGES);
            st.images = images;
            t.remove("images");
        }
    }

    flatten("", &Value::Table(std::mem::take(t)), &mut st.extra);
    st.extra.extend(history_rest);
    st
}

/// The `[history]` table: arrays of numbers become series, anything else is
/// returned as generic entries
fn take_history(t: &mut Table) -> (History, Vec<Entry>) {
    let mut h = History::default();
    let Some(v @ Value::Table(_)) = t.remove("history") else {
        return (h, Vec::new());
    };
    let mut flat = Vec::new();
    flatten("history", &v, &mut flat);
    let mut rest = Vec::new();
    for e in flat {
        let nums: Option<Vec<f64>> = match &e.value {
            Value::Array(a) if !a.is_empty() => a.iter().map(as_number).collect(),
            _ => None,
        };
        let key = e.key.strip_prefix("history.").unwrap_or(&e.key).to_string();
        match nums {
            Some(mut v) => {
                v.drain(..v.len().saturating_sub(MAX_HISTORY_POINTS));
                if key == "time" || key == "t" {
                    h.time = Some(v);
                } else if h.series.len() < MAX_HISTORY_SERIES {
                    h.series.push((key, v));
                }
            }
            None => rest.push(e),
        }
    }
    (h, rest)
}

/// `summary = ["a.b", { key = "c", label = "C" }]`
fn take_summary(t: &mut Table) -> Vec<SummaryKey> {
    let parse = |v: &Value| match v {
        Value::String(s) => Some(SummaryKey {
            key: s.clone(),
            label: None,
        }),
        Value::Table(t) => Some(SummaryKey {
            key: t.get("key")?.as_str()?.to_string(),
            label: t.get("label").and_then(Value::as_str).map(String::from),
        }),
        _ => None,
    };
    take_if(t, "summary", |v| {
        let mut keys: Vec<SummaryKey> = v.as_array()?.iter().map(parse).collect::<Option<_>>()?;
        keys.truncate(MAX_SUMMARY);
        Some(keys)
    })
    .unwrap_or_default()
}

fn image_ref(v: &Value) -> Option<ImageRef> {
    match v {
        Value::String(s) => Some(ImageRef {
            file: s.clone(),
            ..ImageRef::default()
        }),
        Value::Table(t) => Some(ImageRef {
            file: t.get("file")?.as_str()?.to_string(),
            title: t.get("title").and_then(Value::as_str).map(String::from),
            description: t
                .get("description")
                .and_then(Value::as_str)
                .map(String::from),
        }),
        _ => None,
    }
}

/// Re-insert a partially consumed table so that leftover keys are shown.
fn put_back(t: &mut Table, key: &str, rest: Table) {
    if !rest.is_empty() {
        t.insert(key.into(), Value::Table(rest));
    }
}

/// Is this table an annotated value `{ value = ..., unit = ..., label = ... }`?
fn is_annotated(t: &Table) -> bool {
    t.contains_key("value")
        && t.keys()
            .all(|k| matches!(k.as_str(), "value" | "unit" | "label" | "description"))
}

fn flatten(prefix: &str, v: &Value, out: &mut Vec<Entry>) {
    if out.len() >= MAX_EXTRA {
        return;
    }
    let join = |k: &str| {
        if prefix.is_empty() {
            k.to_string()
        } else {
            format!("{prefix}.{k}")
        }
    };
    match v {
        Value::Table(t) if is_annotated(t) => out.push(Entry {
            key: prefix.to_string(),
            value: t["value"].clone(),
            unit: t.get("unit").and_then(Value::as_str).map(String::from),
            label: t
                .get("label")
                .or_else(|| t.get("description"))
                .and_then(Value::as_str)
                .map(String::from),
        }),
        Value::Table(t) => {
            for (k, v) in t {
                flatten(&join(k), v, out);
            }
        }
        Value::Array(a) if a.iter().any(|x| x.is_table() || x.is_array()) => {
            for (i, x) in a.iter().enumerate() {
                flatten(&format!("{prefix}[{}]", i + 1), x, out);
            }
        }
        _ => out.push(Entry {
            key: prefix.to_string(),
            value: v.clone(),
            unit: None,
            label: None,
        }),
    }
}

// Typed extraction: remove a key only if it has a usable type.

/// Unwrap an annotated value `{ value = ... }`
fn plain(v: &Value) -> &Value {
    match v {
        Value::Table(t) if is_annotated(t) => &t["value"],
        _ => v,
    }
}

fn take_if<T>(t: &mut Table, key: &str, f: impl Fn(&Value) -> Option<T>) -> Option<T> {
    let r = f(plain(t.get(key)?))?;
    t.remove(key);
    Some(r)
}

fn take_string(t: &mut Table, key: &str) -> Option<String> {
    take_if(t, key, |v| v.as_str().map(String::from))
}

fn take_number(t: &mut Table, key: &str) -> Option<f64> {
    take_if(t, key, as_number)
}

fn take_int(t: &mut Table, key: &str) -> Option<i64> {
    take_if(t, key, |v| match v {
        Value::Integer(i) => Some(*i),
        Value::Float(f) if f.fract() == 0.0 && f.abs() < 9e15 => Some(*f as i64),
        _ => None,
    })
}

fn take_bool(t: &mut Table, key: &str) -> Option<bool> {
    take_if(t, key, Value::as_bool)
}

/// Job ids may be written as integers or strings
fn take_id(t: &mut Table, key: &str) -> Option<String> {
    take_if(t, key, as_id)
}

fn as_id(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Integer(i) => Some(i.to_string()),
        _ => None,
    }
}

fn take_vector(t: &mut Table, key: &str) -> Option<Vec<f64>> {
    take_if(t, key, |v| match v {
        Value::Array(a) => a.iter().map(as_number).collect(),
        _ => as_number(v).map(|x| vec![x]),
    })
}

fn take_table(t: &mut Table, key: &str) -> Option<Table> {
    match t.get(key) {
        Some(Value::Table(_)) => match t.remove(key) {
            Some(Value::Table(x)) => Some(x),
            _ => None,
        },
        _ => None,
    }
}

fn take_time(t: &mut Table, key: &str) -> Option<DateTime<Utc>> {
    take_if(t, key, as_time)
}

pub fn as_number(v: &Value) -> Option<f64> {
    match plain(v) {
        Value::Integer(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

/// Accept TOML datetimes, RFC 3339 strings, and Unix seconds. Datetimes
/// without an offset are interpreted in the viewer's local time zone.
pub fn as_time(v: &Value) -> Option<DateTime<Utc>> {
    match v {
        Value::Datetime(d) => parse_time_str(&d.to_string()),
        Value::String(s) => parse_time_str(s.trim()),
        Value::Integer(i) => Utc.timestamp_opt(*i, 0).single(),
        Value::Float(f) if f.is_finite() => {
            Utc.timestamp_millis_opt((f * 1000.0) as i64).single()
        }
        _ => None,
    }
}

fn parse_time_str(s: &str) -> Option<DateTime<Utc>> {
    let s = s.replacen(' ', "T", 1);
    if let Ok(t) = DateTime::parse_from_rfc3339(&s) {
        return Some(t.with_timezone(&Utc));
    }
    for fmt in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M"] {
        if let Ok(n) = NaiveDateTime::parse_from_str(&s, fmt) {
            return Local
                .from_local_datetime(&n)
                .earliest()
                .map(|t| t.with_timezone(&Utc));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_file() {
        let st = parse_status(
            r#"
name = "bbh"
status = "running"
updated = 2026-10-02T15:35:00Z
update_interval = 60
message = "all good"
pid = 42
mystery = 3.5

[progress]
iteration = 100
time = { value = 12.5, unit = "M" }
time_end = 100
speed = 2.5
extra_progress = "x"

[slurm]
job_id = 123456

[[black_holes]]
name = "BH1"
mass = 0.5
spin = [0.0, 0.0, 0.6]
position = [1, 0, 0]
color = "red"

[[images]]
file = "track.png"
title = "Track"

[horizon]
area = { value = 50.2, unit = "M^2", label = "Horizon area" }
coeffs = [1, 2, 3]
"#,
        )
        .unwrap();
        assert_eq!(st.name.as_deref(), Some("bbh"));
        assert_eq!(st.status.as_deref(), Some("running"));
        assert_eq!(st.updated.unwrap().to_rfc3339(), "2026-10-02T15:35:00+00:00");
        assert_eq!(st.update_interval, Some(60.0));
        assert_eq!(st.pid, Some(42));
        assert_eq!(st.progress.iteration, Some(100));
        assert_eq!(st.progress.time, Some(12.5));
        assert_eq!(st.progress.time_end, Some(100.0));
        assert_eq!(st.slurm.job_id.as_deref(), Some("123456"));
        assert_eq!(st.black_holes.len(), 1);
        assert_eq!(st.black_holes[0].spin, Some(vec![0.0, 0.0, 0.6]));
        assert_eq!(st.black_holes[0].position, Some(vec![1.0, 0.0, 0.0]));
        assert_eq!(st.images[0].file, "track.png");
        assert_eq!(st.images[0].title.as_deref(), Some("Track"));

        let keys: Vec<&str> = st.extra.iter().map(|e| e.key.as_str()).collect();
        assert!(keys.contains(&"mystery"));
        assert!(keys.contains(&"progress.extra_progress"));
        assert!(keys.contains(&"black_holes[1].color"));
        assert!(keys.contains(&"horizon.coeffs"));
        let area = st.extra.iter().find(|e| e.key == "horizon.area").unwrap();
        assert_eq!(area.unit.as_deref(), Some("M^2"));
        assert_eq!(area.label.as_deref(), Some("Horizon area"));
        assert_eq!(as_number(&area.value), Some(50.2));
    }

    #[test]
    fn group_summary_history() {
        let st = parse_status(
            r#"
group = "octant-study"
summary = ["shells.r2.ham_l2", { key = "progress.iteration", label = "it" }]

[progress]
iteration = 7
fraction = 0.25

[shells.r2]
ham_l2 = 1.5e-6

[slurm]
job_id = 300
previous_job_ids = [100, "200"]

[history]
time = [1, 2, 3]
"plain" = [1.0, 2.0, nan]
note = "not a series"
words = ["a", "b"]

[history.shells.r2]
ham_l2 = [1e-6, 2e-6, 4e-6]
"#,
        )
        .unwrap();
        assert_eq!(st.group.as_deref(), Some("octant-study"));
        assert_eq!(st.summary.len(), 2);
        assert_eq!(st.summary[0].key, "shells.r2.ham_l2");
        assert_eq!(st.summary[1].label.as_deref(), Some("it"));
        assert_eq!(st.progress.fraction, Some(0.25));
        assert_eq!(st.slurm.previous_job_ids, ["100", "200"]);
        assert_eq!(st.history.time, Some(vec![1.0, 2.0, 3.0]));
        assert_eq!(st.history.get("shells.r2.ham_l2"), Some(&[1e-6, 2e-6, 4e-6][..]));
        assert!(st.history.get("plain").unwrap()[2].is_nan());
        assert!(st.history.get("note").is_none());

        // Everything is in `values`, including well-known keys, but not history or summary
        let value = |k: &str| st.values.iter().find(|e| e.key == k).map(|e| &e.value);
        assert_eq!(value("progress.iteration").and_then(as_number), Some(7.0));
        assert_eq!(value("shells.r2.ham_l2").and_then(as_number), Some(1.5e-6));
        assert!(st.values.iter().all(|e| !e.key.starts_with("history") && e.key != "summary"));

        // What is not a series stays visible
        let keys: Vec<&str> = st.extra.iter().map(|e| e.key.as_str()).collect();
        assert!(keys.contains(&"history.note"));
        assert!(keys.contains(&"history.words"));
        assert!(!keys.contains(&"group"));
    }

    #[test]
    fn history_limits_and_bad_summary() {
        let long: Vec<String> = (0..300).map(|i| i.to_string()).collect();
        let st = parse_status(&format!(
            "summary = [1, 2]\n[history]\nt = [{0}]\ny = [{0}]",
            long.join(", ")
        ))
        .unwrap();
        let t = st.history.time.unwrap();
        assert_eq!(t.len(), MAX_HISTORY_POINTS);
        assert_eq!(t[0], 100.0);
        assert_eq!(st.history.series[0].1.len(), MAX_HISTORY_POINTS);
        // A summary that is not understood is shown generically
        assert!(st.summary.is_empty());
        assert!(st.extra.iter().any(|e| e.key == "summary"));
    }

    #[test]
    fn empty_and_minimal() {
        assert_eq!(parse_status("").unwrap(), Status::default());
        let st = parse_status("iteration = 5").unwrap();
        assert_eq!(st.name, None);
        assert_eq!(st.extra.len(), 1);
        assert_eq!(st.extra[0].key, "iteration");
    }

    #[test]
    fn wrong_types_become_extra() {
        let st = parse_status(
            r#"
name = 17
updated = "yesterday"
progress = "fast"
images = [1, 2]
"#,
        )
        .unwrap();
        assert_eq!(st.name, None);
        assert_eq!(st.updated, None);
        assert!(st.images.is_empty());
        let keys: Vec<&str> = st.extra.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, ["images", "name", "progress", "updated"]);
    }

    #[test]
    fn time_forms() {
        let t = |s: &str| parse_status(&format!("updated = {s}")).unwrap().updated;
        let z = t("2026-10-02T15:35:00Z").unwrap();
        assert_eq!(t("\"2026-10-02T15:35:00Z\""), Some(z));
        assert_eq!(t("\"2026-10-02 15:35:00+00:00\""), Some(z));
        assert_eq!(t(&z.timestamp().to_string()), Some(z));
        assert!(t("2026-10-02T15:35:00").is_some());
    }

    #[test]
    fn garbage() {
        let e = parse_status("this is = = not toml").unwrap_err();
        assert!(!e.is_empty());
        assert!(!e.contains('\n'));
    }

    #[test]
    fn oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATUS_FILE);
        let mut text = String::from("name = \"big\"\n");
        while text.len() as u64 <= MAX_STATUS_BYTES {
            text.push_str("# padding padding padding padding padding padding\n");
        }
        std::fs::write(&path, &text).unwrap();
        assert!(read_status_file(&path).unwrap_err().contains("larger"));
        std::fs::write(&path, "name = \"small\"").unwrap();
        assert_eq!(read_status_file(&path).unwrap().name.as_deref(), Some("small"));
    }
}
