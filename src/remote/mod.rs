//! Watching remote hosts over a single ssh connection per host.
//!
//! The local simwatch runs `ssh HOST simwatch --serve` once and keeps that
//! connection open, since logging in may need a password or MFA. The remote
//! simwatch scans, reads and calls `squeue` with the usual workers and limits,
//! and sends what changed as one JSON message per line. Status files travel
//! as raw text and are parsed locally, so the format can change without
//! changing this protocol.

pub mod client;
pub mod serve;

use std::io::{self, BufRead, ErrorKind, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::config::{Config, expand, secs};
use crate::images::MAX_IMAGE_BYTES;
use crate::slurm::Job;

/// Increase when the messages change incompatibly
pub const PROTOCOL: u32 = 1;
/// Starts the first line the server prints
const MAGIC: &str = "simwatch-serve";
/// Longest message line (a status file is at most 128 KiB, escaped)
const MAX_LINE: u64 = 1024 * 1024;
/// Output before the magic line (e.g. from login scripts) that is skipped
const MAX_PREAMBLE: u64 = 64 * 1024;

/// What to watch and how often; the first message to the server
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    pub roots: Vec<String>,
    pub refresh_interval: f64,
    pub scan_interval: f64,
    pub squeue_interval: f64,
    pub squeue_timeout: f64,
    pub slurm: bool,
    pub max_depth: usize,
    pub max_dirs: usize,
    pub max_entries_per_dir: usize,
    pub max_sims: usize,
    pub skip_dirs: Vec<String>,
    /// Collect everything once, then exit (for `--print`)
    pub once: bool,
}

impl Hello {
    pub fn new(cfg: &Config, roots: &[String], once: bool) -> Hello {
        Hello {
            protocol: PROTOCOL,
            roots: roots.to_vec(),
            refresh_interval: cfg.refresh_interval.as_secs_f64(),
            scan_interval: cfg.scan_interval.as_secs_f64(),
            squeue_interval: cfg.squeue_interval.as_secs_f64(),
            squeue_timeout: cfg.squeue_timeout.as_secs_f64(),
            slurm: cfg.slurm,
            max_depth: cfg.limits.max_depth,
            max_dirs: cfg.limits.max_dirs,
            max_entries_per_dir: cfg.limits.max_entries_per_dir,
            max_sims: cfg.limits.max_sims,
            skip_dirs: cfg.limits.skip_dirs.clone(),
            once,
        }
    }

    /// Override the server's configuration. Only `squeue_program` and
    /// `slurm = false` come from the server's own configuration file.
    pub fn apply(&self, cfg: &mut Config) {
        cfg.roots = self
            .roots
            .iter()
            .map(|r| expand(if r.is_empty() { "." } else { r }))
            .collect();
        cfg.remotes.clear();
        cfg.refresh_interval = secs(self.refresh_interval);
        cfg.scan_interval = secs(self.scan_interval);
        cfg.squeue_interval = secs(self.squeue_interval);
        cfg.squeue_timeout = secs(self.squeue_timeout);
        cfg.slurm &= self.slurm;
        cfg.limits.max_depth = self.max_depth;
        cfg.limits.max_dirs = self.max_dirs;
        cfg.limits.max_entries_per_dir = self.max_entries_per_dir;
        cfg.limits.max_sims = self.max_sims;
        cfg.limits.skip_dirs = self.skip_dirs.clone();
        cfg.keep_text = true;
    }
}

/// Messages to the server
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToServer {
    Hello(Hello),
    Reread,
    Rescan,
    Image {
        dir: String,
        file: String,
    },
    /// From a newer client; ignored
    #[serde(other)]
    Unknown,
}

/// Messages from the server
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromServer {
    ScanStarted,
    ScanFinished {
        dirs_visited: usize,
        warnings: Vec<String>,
    },
    ReadStarted,
    /// A status file that changed or was read in this pass
    File {
        dir: String,
        /// Unix time in seconds
        mtime: Option<f64>,
        size: u64,
        /// Why the file could not be read
        error: Option<String>,
        /// The contents, if read in this pass
        text: Option<String>,
    },
    /// All simulation directories, in order; ends a pass
    Sims {
        dirs: Vec<String>,
    },
    SlurmStarted,
    Slurm {
        jobs: Vec<Job>,
    },
    SlurmError {
        error: String,
    },
    SlurmDisabled {
        why: String,
    },
    /// Followed by `len` bytes of image file
    Image {
        dir: String,
        file: String,
        len: u64,
    },
    ImageError {
        dir: String,
        file: String,
        error: String,
    },
    Warning {
        message: String,
    },
    /// Everything was sent (with `Hello::once`)
    Done,
    /// From a newer server; ignored
    #[serde(other)]
    Unknown,
}

/// Write one message as a line, and flush
pub fn write_msg<W: Write + ?Sized>(w: &mut W, msg: &impl Serialize) -> io::Result<()> {
    let mut line = serde_json::to_vec(msg)?;
    line.push(b'\n');
    w.write_all(&line)?;
    w.flush()
}

/// Read one message line; `None` at the end of the input
pub fn read_msg<T: DeserializeOwned>(r: &mut impl BufRead) -> io::Result<Option<T>> {
    let mut buf = Vec::new();
    r.take(MAX_LINE + 1).read_until(b'\n', &mut buf)?;
    if buf.is_empty() {
        return Ok(None);
    }
    if buf.last() != Some(&b'\n') {
        return Err(if buf.len() as u64 > MAX_LINE {
            io::Error::new(ErrorKind::InvalidData, "message too long")
        } else {
            io::Error::new(ErrorKind::UnexpectedEof, "connection closed in a message")
        });
    }
    Ok(Some(serde_json::from_slice(&buf)?))
}

/// Read the bytes that follow an `Image` message
pub fn read_bytes(r: &mut impl Read, len: u64) -> io::Result<Vec<u8>> {
    if len > MAX_IMAGE_BYTES {
        return Err(io::Error::new(ErrorKind::InvalidData, "image too large"));
    }
    let mut bytes = vec![0; len as usize];
    r.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// The first line the server prints
pub fn magic_line() -> String {
    format!("{MAGIC} {PROTOCOL} {}", env!("CARGO_PKG_VERSION"))
}

#[derive(Debug, PartialEq)]
pub enum MagicError {
    /// The output ended before the magic line
    NoAnswer,
    Other(String),
}

/// Skip output until the magic line, and check the protocol version
pub fn read_magic(r: &mut impl BufRead) -> Result<(), MagicError> {
    let mut seen = 0;
    while seen <= MAX_PREAMBLE {
        let mut buf = Vec::new();
        let n = r
            .take(MAX_PREAMBLE + 1 - seen)
            .read_until(b'\n', &mut buf)
            .map_err(|e| MagicError::Other(e.to_string()))?;
        if n == 0 {
            return Err(MagicError::NoAnswer);
        }
        seen += n as u64;
        let line = String::from_utf8_lossy(&buf);
        if let Some(rest) = line.trim().strip_prefix(MAGIC) {
            let mut words = rest.split_whitespace();
            let protocol = words.next().and_then(|p| p.parse::<u32>().ok());
            let version = words.next().unwrap_or("unknown version");
            return match protocol {
                Some(PROTOCOL) => Ok(()),
                _ => Err(MagicError::Other(format!(
                    "the remote simwatch ({version}) does not match this one ({}); \
                     install the same version there",
                    env!("CARGO_PKG_VERSION")
                ))),
            };
        }
    }
    Err(MagicError::Other("unexpected output instead of simwatch --serve".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn messages() {
        let mut buf = Vec::new();
        let msgs = [
            FromServer::ScanStarted,
            FromServer::File {
                dir: "/runs/a".into(),
                mtime: Some(1.5e9),
                size: 12,
                error: None,
                text: Some("name = \"a\"\n# ünïcode".into()),
            },
            FromServer::Sims {
                dirs: vec!["/runs/a".into()],
            },
        ];
        for m in &msgs {
            write_msg(&mut buf, m).unwrap();
        }
        buf.extend_from_slice(b"{\"type\":\"from_the_future\",\"x\":1}\n");
        let mut r = Cursor::new(buf);
        for m in &msgs {
            assert_eq!(read_msg::<FromServer>(&mut r).unwrap().as_ref(), Some(m));
        }
        assert_eq!(read_msg::<FromServer>(&mut r).unwrap(), Some(FromServer::Unknown));
        assert_eq!(read_msg::<FromServer>(&mut r).unwrap(), None);

        // Truncated and over-long lines
        let mut r = Cursor::new(b"{\"type\":\"scan_st".to_vec());
        assert!(read_msg::<FromServer>(&mut r).is_err());
        let mut r = Cursor::new(vec![b' '; MAX_LINE as usize + 10]);
        assert!(read_msg::<FromServer>(&mut r).is_err());
        let mut r = Cursor::new(b"not json\n".to_vec());
        assert!(read_msg::<FromServer>(&mut r).is_err());
    }

    #[test]
    fn magic() {
        let ok = format!("Welcome to the cluster!\n\n{}\n{{\"type\":\"done\"}}\n", magic_line());
        let mut r = Cursor::new(ok.into_bytes());
        assert_eq!(read_magic(&mut r), Ok(()));
        assert_eq!(read_msg::<FromServer>(&mut r).unwrap(), Some(FromServer::Done));

        let mut r = Cursor::new(b"motd\n".to_vec());
        assert_eq!(read_magic(&mut r), Err(MagicError::NoAnswer));
        let mut r = Cursor::new(format!("{MAGIC} 999 9.9.9\n").into_bytes());
        assert!(matches!(read_magic(&mut r), Err(MagicError::Other(e)) if e.contains("9.9.9")));
        let mut r = Cursor::new(vec![b'x'; 200 * 1024]);
        assert!(matches!(read_magic(&mut r), Err(MagicError::Other(_))));
    }

    #[test]
    fn hello() {
        let mut cfg = Config {
            slurm: false,
            ..Config::default()
        };
        let h = Hello::new(&cfg, &["~/runs".into(), "".into()], false);
        let mut buf = Vec::new();
        write_msg(&mut buf, &ToServer::Hello(h.clone())).unwrap();
        let back: ToServer = read_msg(&mut Cursor::new(buf)).unwrap().unwrap();
        assert_eq!(back, ToServer::Hello(h.clone()));

        cfg.slurm = true;
        h.apply(&mut cfg);
        assert!(!cfg.slurm && cfg.keep_text);
        assert!(cfg.roots[0].is_absolute() || dirs::home_dir().is_none());
        assert_eq!(cfg.roots[1], std::path::PathBuf::from("."));
    }
}
