//! Write a set of fake simulations for trying out SimWatch.
//!
//! ```sh
//! cargo run --example fake_sims -- /tmp/simwatch-demo          # keeps updating
//! cargo run --example fake_sims -- /tmp/simwatch-demo --once   # writes once
//! cargo run -- /tmp/simwatch-demo
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use image::{Rgb, RgbImage};
use toml::{Table, Value};

const WIDTH: u32 = 400;
const HEIGHT: u32 = 300;
const T_END: f64 = 200.0;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(root) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("usage: fake_sims DIR [--once]");
        std::process::exit(1);
    };
    let root = PathBuf::from(root);
    let once = args.iter().any(|a| a == "--once");

    write_static(&root);
    let mut t = 60.0;
    loop {
        write_bbh(&root.join("bbh-q1"), t);
        for w in 1..=2 {
            write_worker(&root.join(format!("campaign/l0/worker-{w}")), w, t);
        }
        if once {
            break;
        }
        println!("t = {t:.1}");
        thread::sleep(Duration::from_secs(2));
        t = if t + 1.5 >= T_END { 60.0 } else { t + 1.5 };
    }
}

fn now_minus(secs: i64) -> Value {
    let t = Utc::now() - chrono::Duration::seconds(secs);
    Value::Datetime(t.to_rfc3339_opts(SecondsFormat::Secs, true).parse().unwrap())
}

fn table(pairs: &[(&str, Value)]) -> Table {
    pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

fn s(x: &str) -> Value {
    Value::String(x.into())
}

fn f(x: f64) -> Value {
    Value::Float(x)
}

fn i(x: i64) -> Value {
    Value::Integer(x)
}

fn arr(xs: &[f64]) -> Value {
    Value::Array(xs.iter().map(|x| f(*x)).collect())
}

/// Replace a file atomically, as simulations should
fn write_atomic(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).unwrap();
    fs::rename(&tmp, path).unwrap();
}

fn write_status(dir: &Path, t: &Table) {
    write_atomic(&dir.join("simwatch.toml"), toml::to_string(t).unwrap().as_bytes());
}

fn write_png(path: &Path, img: &RgbImage) {
    let tmp = path.with_extension("tmp.png");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    img.save(&tmp).unwrap();
    fs::rename(&tmp, path).unwrap();
}

/// Orbital separation and phase of a fake inspiral
fn orbit(t: f64) -> (f64, f64) {
    let tm = T_END * 1.02;
    let r = 10.0 * (1.0 - t / tm).max(0.0).powf(0.25) + 0.5;
    // Integrate the orbital frequency numerically
    let n = 400;
    let dt = t / n as f64;
    let phase: f64 = (0..n)
        .map(|k| {
            let tk = (k as f64 + 0.5) * dt;
            let rk = 10.0 * (1.0 - tk / tm).max(0.0).powf(0.25) + 0.5;
            rk.powf(-1.5) * dt
        })
        .sum();
    (r, phase)
}

fn write_bbh(dir: &Path, t: f64) {
    let (r, phase) = orbit(t);
    let pos1 = [0.5 * r * phase.cos(), 0.5 * r * phase.sin(), 0.0];
    let pos2 = [-pos1[0], -pos1[1], 0.0];
    let ham = 1e-6 * (1.0 + 0.5 * (t / 7.0).sin()) * (t / 60.0).exp();
    let started = (t * 30.0) as i64;

    let mut st = table(&[
        ("name", s("bbh-q1-d10")),
        ("status", s("running")),
        ("updated", now_minus(0)),
        ("started", now_minus(started)),
        ("update_interval", i(5)),
        ("message", s(&format!("chunk {}: both horizons found", (t / 0.5) as i64))),
        ("code", s("TreeGeneralizedHarmonic")),
        ("host", s("cn042")),
        ("pid", i(31337)),
    ]);
    st.insert(
        "progress".into(),
        Value::Table(table(&[
            ("iteration", i((t * 64.0) as i64)),
            ("time", f(t)),
            ("time_end", f(T_END)),
            ("time_unit", s("M")),
            ("walltime", f(started as f64)),
            ("walltime_limit", f(86400.0)),
            ("speed", f(120.0)),
            ("checkpoint", s("ck/bbh.it0000012345.h5")),
        ])),
    );
    st.insert(
        "resources".into(),
        Value::Table(table(&[
            ("nodes", i(1)),
            ("tasks", i(1)),
            ("threads", i(64)),
            ("memory_bytes", f(23.4e9)),
            ("memory_limit_bytes", f(256e9)),
        ])),
    );
    st.insert(
        "slurm".into(),
        Value::Table(table(&[("job_id", s("1234567")), ("partition", s("amdq"))])),
    );
    let bh = |name: &str, pos: [f64; 3], chi: f64| {
        Value::Table(table(&[
            ("name", s(name)),
            ("mass", f(0.5)),
            ("irreducible_mass", f(0.5 * (1.0 - 0.02 * chi))),
            ("spin", arr(&[0.0, 0.0, chi])),
            ("position", arr(&pos)),
            ("found", Value::Boolean(true)),
        ]))
    };
    st.insert(
        "black_holes".into(),
        Value::Array(vec![bh("BH1", pos1, 0.6), bh("BH2", pos2, -0.3)]),
    );
    st.insert(
        "images".into(),
        Value::Array(vec![
            Value::Table(table(&[
                ("file", s("plots/track.png")),
                ("title", s("Black hole tracks")),
                ("description", s("x-y plane; BH1 orange, BH2 cyan")),
            ])),
            Value::Table(table(&[
                ("file", s("plots/constraints.png")),
                ("title", s("Constraint violation")),
                ("description", s("log10 of the L2 norm of the Hamiltonian constraint vs. time")),
            ])),
        ]),
    );
    st.insert(
        "constraints".into(),
        Value::Table(table(&[
            (
                "ham_l2",
                Value::Table(table(&[("value", f(ham)), ("label", s("Hamiltonian, L2"))])),
            ),
            ("mom_l2", f(ham * 0.7)),
            ("gauge_linf", f(ham * 40.0)),
        ])),
    );
    st.insert(
        "mesh".into(),
        Value::Table(table(&[("blocks", i(4096 + (t * 3.0) as i64)), ("levels", i(9))])),
    );
    st.insert(
        "separation".into(),
        Value::Table(table(&[("value", f(r)), ("unit", s("M"))])),
    );
    write_status(dir, &st);

    write_png(&dir.join("plots/track.png"), &track_plot(t));
    write_png(&dir.join("plots/constraints.png"), &constraint_plot(t));
}

fn write_worker(dir: &Path, w: i64, t: f64) {
    let mut st = table(&[
        ("name", s(&format!("calibration worker {w}"))),
        ("status", s("running")),
        ("updated", now_minus(0)),
        ("update_interval", i(5)),
    ]);
    st.insert(
        "progress".into(),
        Value::Table(table(&[
            ("iteration", i((t * 10.0) as i64 * w)),
            ("time", f(t / 2.0)),
            ("time_end", f(T_END / 2.0)),
            ("time_unit", s("M")),
            ("walltime", f(t * 3.0)),
        ])),
    );
    st.insert("resources".into(), Value::Table(table(&[("threads", i(8))])));
    write_status(dir, &st);
}

fn write_static(root: &Path) {
    // Running, but has not written for 20 minutes
    let mut st = table(&[
        ("name", s("kerr-boost-v0.3")),
        ("status", s("running")),
        ("updated", now_minus(1200)),
        ("started", now_minus(4 * 3600)),
        ("update_interval", i(60)),
        ("message", s("chunk 812")),
    ]);
    st.insert(
        "progress".into(),
        Value::Table(table(&[
            ("iteration", i(51968)),
            ("time", f(406.0)),
            ("time_end", f(1000.0)),
            ("time_unit", s("M")),
            ("walltime", f(4.0 * 3600.0 - 1200.0)),
        ])),
    );
    st.insert("slurm".into(), Value::Table(table(&[("job_id", i(1234500))])));
    st.insert(
        "black_holes".into(),
        Value::Array(vec![Value::Table(table(&[
            ("irreducible_mass", f(0.9452)),
            ("spin", f(0.7)),
            ("position", arr(&[40.6, 0.0, 0.0])),
            ("found", Value::Boolean(false)),
        ]))]),
    );
    write_status(&root.join("kerr-boost"), &st);

    let mut st = table(&[
        ("name", s("kerr-static")),
        ("status", s("finished")),
        ("updated", now_minus(86400)),
        ("started", now_minus(2 * 86400)),
        ("message", s("reached t_end")),
    ]);
    st.insert(
        "progress".into(),
        Value::Table(table(&[
            ("iteration", i(128000)),
            ("time", f(1000.0)),
            ("time_end", f(1000.0)),
            ("time_unit", s("M")),
            ("walltime", f(86400.0)),
        ])),
    );
    write_status(&root.join("kerr-static"), &st);

    let mut st = table(&[
        ("name", s("bbh-q4")),
        ("status", s("failed")),
        ("updated", now_minus(7200)),
        ("started", now_minus(9000)),
        ("message", s("non-finite value in Π at t = 37.25 M (block 1871)")),
    ]);
    st.insert(
        "progress".into(),
        Value::Table(table(&[("iteration", i(4768)), ("time", f(37.25)), ("time_end", f(T_END))])),
    );
    write_status(&root.join("bbh-q4"), &st);

    let mut st = table(&[
        ("name", s("bbh-q2-d12")),
        ("status", s("queued")),
        ("updated", now_minus(3000)),
    ]);
    st.insert(
        "slurm".into(),
        Value::Table(table(&[("job_id", s("1234601")), ("partition", s("amdq"))])),
    );
    write_status(&root.join("bbh-q2"), &st);

    let mut st = table(&[
        ("name", s("bbh-q1-d14")),
        ("status", s("stopped")),
        ("updated", now_minus(600)),
        ("message", s("wall time limit reached; checkpoint written; resubmitted")),
    ]);
    st.insert(
        "progress".into(),
        Value::Table(table(&[("iteration", i(98304)), ("time", f(512.0)), ("time_end", f(3000.0))])),
    );
    st.insert(
        "slurm".into(),
        Value::Table(table(&[("job_id", s("1234400")), ("next_job_id", s("1234602"))])),
    );
    write_status(&root.join("bbh-q1-d14"), &st);

    // Opportunistic: no name, no status, just a couple of numbers
    write_atomic(
        &root.join("minimal/simwatch.toml"),
        format!(
            "updated = {}\niteration = 5\nresidual = 1.5e-3\n",
            now_minus(10)
        )
        .as_bytes(),
    );

    write_atomic(&root.join("broken/simwatch.toml"), b"name = \"broken\nstatus = = running\n");

    let st = table(&[
        ("name", s("big-picture")),
        ("status", s("finished")),
        ("updated", now_minus(400)),
        (
            "images",
            Value::Array(vec![
                Value::Table(table(&[("file", s("too-wide.png")), ("title", s("Too wide"))])),
                Value::Table(table(&[("file", s("../escape.png")), ("title", s("Escaping"))])),
                Value::Table(table(&[("file", s("missing.png")), ("title", s("Missing"))])),
            ]),
        ),
    ]);
    write_status(&root.join("big-picture"), &st);
    write_png(
        &root.join("big-picture/too-wide.png"),
        &RgbImage::from_pixel(2000, 10, Rgb([200, 0, 0])),
    );
    // Exists, but outside the simulation directory, so SimWatch must not show it
    write_png(&root.join("escape.png"), &RgbImage::from_pixel(40, 40, Rgb([0, 200, 0])));
}

// A tiny rasterizer

const BG: Rgb<u8> = Rgb([16, 20, 24]);
const AXIS: Rgb<u8> = Rgb([90, 96, 104]);
const ORANGE: Rgb<u8> = Rgb([255, 150, 40]);
const CYAN: Rgb<u8> = Rgb([60, 200, 230]);
const GREEN: Rgb<u8> = Rgb([120, 220, 100]);

fn dot(img: &mut RgbImage, x: f64, y: f64, c: Rgb<u8>, size: i64) {
    for dx in 0..size {
        for dy in 0..size {
            let (px, py) = (x.round() as i64 + dx, y.round() as i64 + dy);
            if px >= 0 && py >= 0 && (px as u32) < img.width() && (py as u32) < img.height() {
                img.put_pixel(px as u32, py as u32, c);
            }
        }
    }
}

fn line(img: &mut RgbImage, (x0, y0): (f64, f64), (x1, y1): (f64, f64), c: Rgb<u8>, size: i64) {
    let n = ((x1 - x0).abs().max((y1 - y0).abs()).ceil() as usize).max(1);
    for k in 0..=n {
        let a = k as f64 / n as f64;
        dot(img, x0 + a * (x1 - x0), y0 + a * (y1 - y0), c, size);
    }
}

fn frame(img: &mut RgbImage, m: f64) {
    let (w, h) = (img.width() as f64 - 1.0, img.height() as f64 - 1.0);
    for (a, b) in [((m, m), (w - m, m)), ((w - m, m), (w - m, h - m)), ((w - m, h - m), (m, h - m)), ((m, h - m), (m, m))] {
        line(img, a, b, AXIS, 1);
    }
}

fn track_plot(t: f64) -> RgbImage {
    let mut img = RgbImage::from_pixel(WIDTH, HEIGHT, BG);
    frame(&mut img, 10.0);
    let (cx, cy) = (WIDTH as f64 / 2.0, HEIGHT as f64 / 2.0);
    let scale = (HEIGHT as f64 / 2.0 - 20.0) / 5.5;
    line(&mut img, (cx - 5.0, cy), (cx + 5.0, cy), AXIS, 1);
    line(&mut img, (cx, cy - 5.0), (cx, cy + 5.0), AXIS, 1);
    let steps = (t * 4.0) as usize;
    let mut prev: Option<[(f64, f64); 2]> = None;
    for k in 0..=steps {
        let tk = t * k as f64 / steps.max(1) as f64;
        let (r, phase) = orbit(tk);
        let p1 = (cx + 0.5 * r * phase.cos() * scale, cy - 0.5 * r * phase.sin() * scale);
        let p2 = (2.0 * cx - p1.0, 2.0 * cy - p1.1);
        if let Some([q1, q2]) = prev {
            line(&mut img, q1, p1, ORANGE, 2);
            line(&mut img, q2, p2, CYAN, 2);
        }
        prev = Some([p1, p2]);
    }
    if let Some([p1, p2]) = prev {
        dot(&mut img, p1.0 - 3.0, p1.1 - 3.0, ORANGE, 7);
        dot(&mut img, p2.0 - 3.0, p2.1 - 3.0, CYAN, 7);
    }
    img
}

fn constraint_plot(t: f64) -> RgbImage {
    let mut img = RgbImage::from_pixel(WIDTH, HEIGHT, BG);
    frame(&mut img, 10.0);
    let (x0, x1, y0, y1) = (10.0, WIDTH as f64 - 11.0, HEIGHT as f64 - 11.0, 10.0);
    let (lmin, lmax) = (-8.0, -3.0);
    // Decade grid lines
    for l in (lmin as i64 + 1)..(lmax as i64) {
        let y = y0 + (l as f64 - lmin) / (lmax - lmin) * (y1 - y0);
        for x in (x0 as i64..x1 as i64).step_by(6) {
            dot(&mut img, x as f64, y, AXIS, 1);
        }
    }
    let n = (t / T_END * (x1 - x0)) as usize;
    let mut prev = None;
    for k in 0..=n {
        let tk = T_END * k as f64 / (x1 - x0);
        let v = 1e-6 * (1.0 + 0.5 * (tk / 7.0).sin()) * (tk / 60.0).exp();
        let y = y0 + (v.log10() - lmin) / (lmax - lmin) * (y1 - y0);
        let p = (x0 + k as f64, y.clamp(y1, y0));
        if let Some(q) = prev {
            line(&mut img, q, p, GREEN, 2);
        }
        prev = Some(p);
    }
    img
}
