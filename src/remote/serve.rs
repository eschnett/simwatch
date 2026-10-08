//! The remote side: `simwatch --serve`, started over ssh by another simwatch.
//!
//! Runs the usual local workers with the client's settings and sends their
//! updates on stdout. Exits when stdin ends, i.e. when the ssh connection is
//! gone, so nothing lingers on the login node.

use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use clap::Parser;

use super::{FromServer, PROTOCOL, ToServer, magic_line, read_msg, write_msg};
use crate::config::{Cli, Config};
use crate::images::read_image_bytes;
use crate::model::Sim;
use crate::monitor::{self, Monitor, Request, Update};

enum Event {
    Update(Update),
    Image {
        dir: String,
        file: String,
        res: Result<Vec<u8>, String>,
    },
    /// The client is gone
    Eof,
}

/// Serve one client on `input` and `output` until `input` ends
pub fn serve(input: impl Read + Send + 'static, mut output: impl Write) -> Result<()> {
    writeln!(output, "{}", magic_line())?;
    output.flush()?;
    let mut input = BufReader::new(input);
    let hello = match read_msg::<ToServer>(&mut input)? {
        Some(ToServer::Hello(h)) => h,
        Some(_) => bail!("expected hello"),
        None => return Ok(()),
    };
    if hello.protocol != PROTOCOL {
        bail!("client speaks protocol {}, this is {PROTOCOL}", hello.protocol);
    }
    // The server's own configuration file may set squeue_program
    let mut cfg = match Config::load(&Cli::parse_from(["simwatch"])) {
        Ok(c) => c,
        Err(e) => {
            let message = format!("{e:#}");
            write_msg(&mut output, &FromServer::Warning { message })?;
            Config::default()
        }
    };
    hello.apply(&mut cfg);
    let mut writer = Writer::default();

    if hello.once {
        let (sims, scan, snap) = monitor::collect_local(&cfg);
        writer.update(&mut output, Update::ScanFinished(scan))?;
        writer.update(&mut output, Update::Sims(sims))?;
        let slurm = snap.map_or(Update::SlurmDisabled("disabled".into()), Update::Slurm);
        writer.update(&mut output, slurm)?;
        return Ok(write_msg(&mut output, &FromServer::Done)?);
    }

    let (tx, rx) = channel();
    let (mon_tx, mon_rx) = channel();
    let monitor = Monitor::start(&cfg, mon_tx);
    {
        let tx = tx.clone();
        thread::spawn(move || {
            for (_, u) in mon_rx {
                if tx.send(Event::Update(u)).is_err() {
                    return;
                }
            }
        });
    }

    // Images are served only from the current simulation directories
    let known = writer.known.clone();
    let (img_tx, img_rx) = channel::<(String, String)>();
    {
        let tx = tx.clone();
        thread::spawn(move || {
            for (dir, file) in img_rx {
                let res = if known.lock().unwrap().contains(Path::new(&dir)) {
                    read_image_bytes(Path::new(&dir), &file)
                } else {
                    Err("not a known simulation directory".into())
                };
                if tx.send(Event::Image { dir, file, res }).is_err() {
                    return;
                }
            }
        });
    }

    thread::spawn(move || {
        loop {
            match read_msg::<ToServer>(&mut input) {
                Ok(Some(ToServer::Reread)) => monitor.request(Request::Reread),
                Ok(Some(ToServer::Rescan)) => monitor.request(Request::Rescan),
                Ok(Some(ToServer::Image { dir, file })) => {
                    let _ = img_tx.send((dir, file));
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => {
                    let _ = tx.send(Event::Eof);
                    return;
                }
            }
        }
    });

    for ev in rx {
        match ev {
            Event::Update(u) => writer.update(&mut output, u)?,
            Event::Image { dir, file, res } => match res {
                Ok(bytes) => {
                    let len = bytes.len() as u64;
                    write_msg(&mut output, &FromServer::Image { dir, file, len })?;
                    output.write_all(&bytes)?;
                    output.flush()?;
                }
                Err(error) => write_msg(&mut output, &FromServer::ImageError { dir, file, error })?,
            },
            Event::Eof => break,
        }
    }
    Ok(())
}

/// What a status file looked like when it was last sent
type Sent = (Option<SystemTime>, u64, Option<String>);

/// Turns updates into messages, sending only status files that changed
#[derive(Default)]
struct Writer {
    sent: HashMap<PathBuf, Sent>,
    known: Arc<Mutex<HashSet<PathBuf>>>,
}

impl Writer {
    fn update(&mut self, out: &mut impl Write, u: Update) -> std::io::Result<()> {
        let msg = match u {
            Update::ScanStarted => FromServer::ScanStarted,
            Update::ScanFinished(res) => FromServer::ScanFinished {
                dirs_visited: res.dirs_visited,
                warnings: res.warnings,
            },
            Update::ReadStarted => FromServer::ReadStarted,
            Update::Sims(sims) => return self.sims(out, sims),
            Update::SlurmStarted => FromServer::SlurmStarted,
            Update::Slurm(Ok(snap)) => FromServer::Slurm {
                jobs: snap.jobs.into_values().collect(),
            },
            Update::Slurm(Err(error)) => FromServer::SlurmError { error },
            Update::SlurmDisabled(why) => FromServer::SlurmDisabled { why },
            Update::Warning(message) => FromServer::Warning { message },
            Update::Connected | Update::Disconnected(_) => return Ok(()),
        };
        write_msg(out, &msg)
    }

    fn sims(&mut self, out: &mut impl Write, sims: Vec<Sim>) -> std::io::Result<()> {
        let mut sent = HashMap::new();
        let mut dirs = Vec::new();
        for sim in sims {
            let stamp: Sent = (sim.mtime, sim.size, sim.error.clone());
            if sim.text.is_some() || self.sent.get(&sim.dir) != Some(&stamp) {
                write_msg(
                    out,
                    &FromServer::File {
                        dir: sim.dir.to_string_lossy().into_owned(),
                        mtime: sim
                            .mtime
                            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                            .map(|d| d.as_secs_f64()),
                        size: sim.size,
                        error: sim.error,
                        text: sim.text,
                    },
                )?;
            }
            dirs.push(sim.dir.to_string_lossy().into_owned());
            sent.insert(sim.dir, stamp);
        }
        *self.known.lock().unwrap() = sent.keys().cloned().collect();
        self.sent = sent;
        write_msg(out, &FromServer::Sims { dirs })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::STATUS_FILE;
    use crate::images::decode_image;
    use crate::remote::client::State;
    use crate::remote::{Hello, read_bytes, read_magic};
    use image::{Rgb, RgbImage};
    use std::fs::{create_dir, write};
    use std::os::unix::net::UnixStream;

    /// Read messages until one gives an update matching `f`, answering
    /// `Image` messages with their bytes
    fn until<T>(
        r: &mut BufReader<UnixStream>,
        state: &mut State,
        mut f: impl FnMut(FromServer, Option<Vec<u8>>, &mut State) -> Option<T>,
    ) -> T {
        loop {
            let m = read_msg::<FromServer>(r).unwrap().expect("server ended");
            let bytes = match &m {
                FromServer::Image { len, .. } => Some(read_bytes(r, *len).unwrap()),
                _ => None,
            };
            if let Some(t) = f(m, bytes, state) {
                return t;
            }
        }
    }

    fn sims(m: FromServer, _: Option<Vec<u8>>, state: &mut State) -> Option<Vec<Sim>> {
        match state.apply(m) {
            Some(Update::Sims(s)) => Some(s),
            _ => None,
        }
    }

    #[test]
    fn serve_client() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a");
        create_dir(&dir).unwrap();
        write(dir.join(STATUS_FILE), "name = \"one\"").unwrap();
        RgbImage::from_pixel(8, 6, Rgb([1, 2, 3])).save(dir.join("p.png")).unwrap();
        write(tmp.path().join("secret.png"), "x").unwrap();

        let (client, server) = UnixStream::pair().unwrap();
        let input = server.try_clone().unwrap();
        let handle = thread::spawn(move || serve(input, server));
        let mut w = client.try_clone().unwrap();
        let cfg = Config {
            slurm: false,
            ..Config::default()
        };
        let root = tmp.path().display().to_string();
        write_msg(&mut w, &ToServer::Hello(Hello::new(&cfg, &[root], false))).unwrap();
        let mut r = BufReader::new(client);
        read_magic(&mut r).unwrap();
        let mut state = State::new("h");

        let s = until(&mut r, &mut state, sims);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].host.as_deref(), Some("h"));
        assert_eq!(s[0].st().name.as_deref(), Some("one"));

        // A broken update keeps the last good contents
        write(dir.join(STATUS_FILE), "name = = broken").unwrap();
        write_msg(&mut w, &ToServer::Reread).unwrap();
        let s = until(&mut r, &mut state, sims);
        assert_eq!(s[0].st().name.as_deref(), Some("one"));
        assert!(s[0].error.is_some());

        // Images, only from inside simulation directories
        let d = s[0].dir.display().to_string();
        for file in ["p.png", "../secret.png"] {
            let msg = ToServer::Image {
                dir: d.clone(),
                file: file.into(),
            };
            write_msg(&mut w, &msg).unwrap();
        }
        let msg = ToServer::Image {
            dir: tmp.path().display().to_string(),
            file: "secret.png".into(),
        };
        write_msg(&mut w, &msg).unwrap();
        let mut got = Vec::new();
        while got.len() < 3 {
            got.push(until(&mut r, &mut state, |m, bytes, _| match m {
                FromServer::Image { file, .. } => Some((file, Ok(bytes.unwrap()))),
                FromServer::ImageError { file, error, .. } => Some((file, Err(error))),
                _ => None,
            }));
        }
        let img = decode_image(got[0].1.as_ref().unwrap()).unwrap();
        assert_eq!((got[0].0.as_str(), img.width()), ("p.png", 8));
        assert!(got[1].1.is_err() && got[2].1.is_err(), "{got:?}");

        // The server ends with its input
        drop(w);
        r.get_ref().shutdown(std::net::Shutdown::Write).unwrap();
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn serve_once() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path().join(STATUS_FILE), "name = \"x\"").unwrap();
        let (client, server) = UnixStream::pair().unwrap();
        let input = server.try_clone().unwrap();
        let handle = thread::spawn(move || serve(input, server));
        let cfg = Config {
            slurm: false,
            ..Config::default()
        };
        let hello = Hello::new(&cfg, &[tmp.path().display().to_string()], true);
        write_msg(&mut client.try_clone().unwrap(), &ToServer::Hello(hello)).unwrap();
        let mut r = BufReader::new(client);
        read_magic(&mut r).unwrap();
        let mut state = State::new("h");
        let s = until(&mut r, &mut state, sims);
        assert_eq!(s[0].st().name.as_deref(), Some("x"));
        until(&mut r, &mut state, |m, _, _| (m == FromServer::Done).then_some(()));
        handle.join().unwrap().unwrap();
    }
}
