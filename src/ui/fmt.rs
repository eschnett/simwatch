//! Compact formatting of numbers, durations and values.

use chrono::{DateTime, Local, Utc};
use toml::Value;

/// About four significant digits, switching to exponential notation for
/// very large or small magnitudes
pub fn num(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf" } else { "-inf" }.into();
    }
    if x == 0.0 {
        return "0".into();
    }
    let a = x.abs();
    if (1e-3..1e5).contains(&a) {
        let digits = (3 - a.log10().floor() as i32).clamp(0, 6) as usize;
        let s = format!("{x:.digits$}");
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        }
    } else {
        let s = format!("{x:.3e}");
        // 1.200e5 -> 1.2e5
        match s.split_once('e') {
            Some((m, e)) if m.contains('.') => {
                format!("{}e{e}", m.trim_end_matches('0').trim_end_matches('.'))
            }
            _ => s,
        }
    }
}

/// 45s, 12m, 3h05m, 2d04h
pub fn duration(secs: f64) -> String {
    if !secs.is_finite() {
        return "?".into();
    }
    let neg = secs < 0.0;
    let s = secs.abs().round() as u64;
    let out = if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else if s < 86400 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}d{:02}h", s / 86400, (s % 86400) / 3600)
    };
    if neg { format!("-{out}") } else { out }
}

/// Short form for table cells: 45s, 12m, 3h, 2d
pub fn age(secs: f64) -> String {
    if !secs.is_finite() {
        return "?".into();
    }
    if secs < -5.0 {
        return "future".into();
    }
    let s = secs.max(0.0).round() as u64;
    if s < 120 {
        format!("{s}s")
    } else if s < 7200 {
        format!("{}m", s / 60)
    } else if s < 2 * 86400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86400)
    }
}

/// Binary units: 12.3 GiB
pub fn bytes(b: f64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if !b.is_finite() || b < 0.0 {
        return num(b);
    }
    let mut x = b;
    let mut u = 0;
    while x >= 1024.0 && u + 1 < UNITS.len() {
        x /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{} B", x as u64)
    } else {
        format!("{x:.1} {}", UNITS[u])
    }
}

pub fn vector(v: &[f64]) -> String {
    if v.len() == 1 {
        return num(v[0]);
    }
    let parts: Vec<String> = v.iter().map(|x| num(*x)).collect();
    format!("({})", parts.join(", "))
}

pub fn norm(v: &[f64]) -> f64 {
    v.iter().map(|x| x * x).sum::<f64>().sqrt()
}

pub fn local_time(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S").to_string()
}

/// A generic TOML value, without quotes around strings
pub fn value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => num(*f),
        Value::Boolean(b) => b.to_string(),
        Value::Datetime(d) => d.to_string(),
        Value::Array(a) => {
            let parts: Vec<String> = a
                .iter()
                .map(|x| match x {
                    Value::String(s) => format!("{s:?}"),
                    x => value(x),
                })
                .collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Table(t) => {
            let parts: Vec<String> = t.iter().map(|(k, v)| format!("{k}={}", value(v))).collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

/// Truncate to at most `n` characters, marking the cut with an ellipsis
pub fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else if n == 0 {
        String::new()
    } else {
        let mut t: String = s.chars().take(n - 1).collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(num(0.0), "0");
        assert_eq!(num(1.0), "1");
        assert_eq!(num(12.5), "12.5");
        assert_eq!(num(2.71234), "2.712");
        assert_eq!(num(1234.5678), "1235");
        assert_eq!(num(0.0012346), "0.001235");
        assert_eq!(num(-0.5), "-0.5");
        assert_eq!(num(1.2e-7), "1.2e-7");
        assert_eq!(num(123456.0), "1.235e5");
        assert_eq!(num(f64::NAN), "nan");
    }

    #[test]
    fn durations() {
        assert_eq!(duration(5.0), "5s");
        assert_eq!(duration(65.0), "1m05s");
        assert_eq!(duration(3.0 * 3600.0 + 300.0), "3h05m");
        assert_eq!(duration(2.0 * 86400.0 + 4.0 * 3600.0), "2d04h");
        assert_eq!(age(30.0), "30s");
        assert_eq!(age(600.0), "10m");
        assert_eq!(age(-60.0), "future");
        assert_eq!(bytes(512.0), "512 B");
        assert_eq!(bytes(1.5 * 1024.0 * 1024.0 * 1024.0), "1.5 GiB");
    }

    #[test]
    fn values() {
        let v: toml::Table = "a = [1, 2.5, \"x\"]\nb = { c = true }".parse().unwrap();
        assert_eq!(value(&v["a"]), "[1, 2.5, \"x\"]");
        assert_eq!(value(&v["b"]), "{c=true}");
        assert_eq!(trunc("abcdef", 4), "abc…");
        assert_eq!(trunc("abc", 4), "abc");
        assert_eq!(vector(&[1.0, 0.0, 0.5]), "(1, 0, 0.5)");
    }
}
