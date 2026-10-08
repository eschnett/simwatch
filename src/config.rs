//! Configuration from `~/.config/simwatch/config.toml` and the command line.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use serde::Deserialize;

use crate::discover::Limits;
use crate::model::HealthParams;

#[derive(Parser, Debug)]
#[command(version, about = "Watch the progress of HPC simulations")]
pub struct Cli {
    /// Directories to search for simulations, `HOST:DIR` for a remote host
    /// (default: from the config file, else the current directory)
    pub dirs: Vec<String>,

    /// Configuration file [default: ~/.config/simwatch/config.toml]
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    /// Seconds between re-reading the status files
    #[arg(long)]
    pub refresh: Option<f64>,

    /// Seconds between scans for new simulations
    #[arg(long)]
    pub scan_interval: Option<f64>,

    /// How deep to search below each directory
    #[arg(long)]
    pub max_depth: Option<usize>,

    /// How to show images
    #[arg(long, value_enum)]
    pub images: Option<ImageMode>,

    /// Do not call squeue
    #[arg(long)]
    pub no_slurm: bool,

    /// Print the list of simulations once and exit
    #[arg(long)]
    pub print: bool,

    /// Serve another simwatch over ssh (speaks a protocol on stdin/stdout)
    #[arg(long)]
    pub serve: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageMode {
    Sixel,
    None,
}

/// The contents of the configuration file; every setting is optional
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileConfig {
    roots: Option<Vec<String>>,
    refresh_interval: Option<f64>,
    scan_interval: Option<f64>,
    squeue_interval: Option<f64>,
    squeue_timeout: Option<f64>,
    squeue_program: Option<String>,
    slurm: Option<bool>,
    stale_factor: Option<f64>,
    stale_floor: Option<f64>,
    default_update_interval: Option<f64>,
    max_depth: Option<usize>,
    max_dirs: Option<usize>,
    max_entries_per_dir: Option<usize>,
    max_sims: Option<usize>,
    skip_dirs: Option<Vec<String>>,
    images: Option<ImageMode>,
    /// Terminal cell size in pixels, overriding detection
    font_size: Option<[u16; 2]>,
    ssh: Option<Vec<String>>,
    remote_program: Option<String>,
    auto_reconnect: Option<bool>,
}

/// A remote host and the directories to watch there
#[derive(Clone, Debug, PartialEq)]
pub struct Remote {
    /// Passed to ssh as given, so aliases from `~/.ssh/config` work
    pub host: String,
    /// Expanded on the remote host
    pub roots: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Config {
    /// Local directories
    pub roots: Vec<PathBuf>,
    pub remotes: Vec<Remote>,
    pub refresh_interval: Duration,
    pub scan_interval: Duration,
    pub squeue_interval: Duration,
    pub squeue_timeout: Duration,
    pub squeue_program: String,
    pub slurm: bool,
    pub health: HealthParams,
    pub limits: Limits,
    pub images: ImageMode,
    /// Terminal cell size in pixels; detected if not set
    pub font_size: Option<[u16; 2]>,
    /// The ssh command and its options
    pub ssh: Vec<String>,
    /// The simwatch command on remote hosts
    pub remote_program: String,
    /// Reconnect lost hosts automatically if that needs no password
    pub auto_reconnect: bool,
    pub print: bool,
    /// Keep the raw text of status files instead of parsing them (`--serve`)
    pub keep_text: bool,
}

/// Cell size in pixels if neither the terminal nor the configuration says
pub const DEFAULT_FONT_SIZE: (u16, u16) = (10, 20);

impl Default for Config {
    fn default() -> Self {
        Config {
            roots: vec![PathBuf::from(".")],
            remotes: Vec::new(),
            refresh_interval: Duration::from_secs(60),
            scan_interval: Duration::from_secs(300),
            squeue_interval: Duration::from_secs(120),
            squeue_timeout: Duration::from_secs(20),
            squeue_program: "squeue".into(),
            slurm: true,
            health: HealthParams::default(),
            limits: Limits::default(),
            images: ImageMode::Sixel,
            font_size: None,
            ssh: vec!["ssh".into()],
            remote_program: "simwatch".into(),
            auto_reconnect: true,
            print: false,
            keep_text: false,
        }
    }
}

pub fn default_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config/simwatch/config.toml"))
}

/// Expand a leading `~/`
pub fn expand(s: &str) -> PathBuf {
    match (s.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ if s == "~" => dirs::home_dir().unwrap_or_else(|| PathBuf::from(s)),
        _ => PathBuf::from(s),
    }
}

/// Split a remote root `HOST:PATH` (like scp: a colon before any slash)
pub fn split_remote(s: &str) -> Option<(&str, &str)> {
    let (host, path) = s.split_once(':')?;
    (!host.is_empty() && !host.contains('/') && !host.starts_with('-')).then_some((host, path))
}

/// Durations below one second would hammer the file system
pub fn secs(x: f64) -> Duration {
    Duration::from_secs_f64(if x.is_finite() { x.max(1.0) } else { 60.0 })
}

impl Config {
    pub fn load(cli: &Cli) -> Result<Config> {
        let (path, explicit) = match &cli.config {
            Some(p) => (Some(p.clone()), true),
            None => (default_config_path(), false),
        };
        let file = match path {
            Some(p) if explicit || p.exists() => read_file_config(&p)?,
            _ => FileConfig::default(),
        };
        Ok(Self::merge(cli, file))
    }

    fn merge(cli: &Cli, f: FileConfig) -> Config {
        let d = Config::default();
        let given = if !cli.dirs.is_empty() {
            cli.dirs.clone()
        } else {
            f.roots.unwrap_or_default()
        };
        let mut roots = Vec::new();
        let mut remotes: Vec<Remote> = Vec::new();
        for r in &given {
            match split_remote(r) {
                Some((host, path)) => match remotes.iter_mut().find(|x| x.host == host) {
                    Some(x) => x.roots.push(path.to_string()),
                    None => remotes.push(Remote {
                        host: host.to_string(),
                        roots: vec![path.to_string()],
                    }),
                },
                None => roots.push(expand(r)),
            }
        }
        if roots.is_empty() && remotes.is_empty() {
            roots = d.roots;
        }
        let dl = Limits::default();
        let dh = HealthParams::default();
        Config {
            roots,
            remotes,
            refresh_interval: cli
                .refresh
                .or(f.refresh_interval)
                .map_or(d.refresh_interval, secs),
            scan_interval: cli
                .scan_interval
                .or(f.scan_interval)
                .map_or(d.scan_interval, secs),
            squeue_interval: f.squeue_interval.map_or(d.squeue_interval, secs),
            squeue_timeout: f.squeue_timeout.map_or(d.squeue_timeout, secs),
            squeue_program: f.squeue_program.unwrap_or(d.squeue_program),
            slurm: !cli.no_slurm && f.slurm.unwrap_or(true),
            health: HealthParams {
                stale_factor: f.stale_factor.unwrap_or(dh.stale_factor),
                stale_floor: f.stale_floor.unwrap_or(dh.stale_floor),
                default_update_interval: f
                    .default_update_interval
                    .unwrap_or(dh.default_update_interval),
            },
            limits: Limits {
                max_depth: cli.max_depth.or(f.max_depth).unwrap_or(dl.max_depth),
                max_dirs: f.max_dirs.unwrap_or(dl.max_dirs),
                max_entries_per_dir: f.max_entries_per_dir.unwrap_or(dl.max_entries_per_dir),
                max_sims: f.max_sims.unwrap_or(dl.max_sims),
                skip_dirs: f.skip_dirs.unwrap_or(dl.skip_dirs),
            },
            images: cli.images.or(f.images).unwrap_or(d.images),
            font_size: f.font_size.or(d.font_size),
            ssh: f.ssh.filter(|s| !s.is_empty()).unwrap_or(d.ssh),
            remote_program: f.remote_program.unwrap_or(d.remote_program),
            auto_reconnect: f.auto_reconnect.unwrap_or(d.auto_reconnect),
            print: cli.print,
            keep_text: false,
        }
    }

    /// The sources of simulations in a fixed order: the local directories
    /// (`None`) if any, then each remote host
    pub fn sources(&self) -> Vec<Option<String>> {
        let local = (!self.roots.is_empty()).then_some(None);
        local
            .into_iter()
            .chain(self.remotes.iter().map(|r| Some(r.host.clone())))
            .collect()
    }

    /// All roots for display, remote ones as `HOST:PATH`
    pub fn root_names(&self) -> Vec<String> {
        let local = self.roots.iter().map(|r| r.display().to_string());
        let remote = self
            .remotes
            .iter()
            .flat_map(|r| r.roots.iter().map(|p| format!("{}:{p}", r.host)));
        local.chain(remote).collect()
    }
}

fn read_file_config(path: &Path) -> Result<FileConfig> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge() {
        let cli = Cli::parse_from(["simwatch", "--refresh", "30", "--no-slurm"]);
        let f: FileConfig = toml::from_str(
            r#"
roots = ["~/runs", "/scratch/x"]
refresh_interval = 10
max_depth = 7
images = "none"
"#,
        )
        .unwrap();
        let c = Config::merge(&cli, f);
        assert_eq!(c.roots[1], PathBuf::from("/scratch/x"));
        assert!(c.roots[0].ends_with("runs") && c.roots[0].is_absolute());
        assert_eq!(c.refresh_interval, Duration::from_secs(30));
        assert_eq!(c.limits.max_depth, 7);
        assert_eq!(c.images, ImageMode::None);
        assert!(!c.slurm);

        let cli = Cli::parse_from(["simwatch", "a", "b"]);
        let c = Config::merge(&cli, FileConfig::default());
        assert_eq!(c.roots, [PathBuf::from("a"), PathBuf::from("b")]);
        assert_eq!(c.refresh_interval, Duration::from_secs(60));
        assert!(c.remotes.is_empty());
        assert_eq!(c.sources(), [None]);
    }

    #[test]
    fn remote_roots() {
        assert_eq!(split_remote("sym:/mnt/runs"), Some(("sym", "/mnt/runs")));
        assert_eq!(split_remote("me@sym:~/runs"), Some(("me@sym", "~/runs")));
        assert_eq!(split_remote("sym:"), Some(("sym", "")));
        assert_eq!(split_remote("./a:b"), None);
        assert_eq!(split_remote("/x/a:b"), None);
        assert_eq!(split_remote(":x"), None);
        assert_eq!(split_remote("-oProxyCommand=x:y"), None);
        assert_eq!(split_remote("runs"), None);

        let cli = Cli::parse_from(["simwatch", "sym:/a", "local", "other:b", "sym:~/c"]);
        let c = Config::merge(&cli, FileConfig::default());
        assert_eq!(c.roots, [PathBuf::from("local")]);
        assert_eq!(c.remotes.len(), 2);
        assert_eq!(c.remotes[0].host, "sym");
        assert_eq!(c.remotes[0].roots, ["/a", "~/c"]);
        assert_eq!(c.remotes[1].roots, ["b"]);
        assert_eq!(c.sources(), [None, Some("sym".into()), Some("other".into())]);
        assert_eq!(c.root_names(), ["local", "sym:/a", "sym:~/c", "other:b"]);

        // Only remote roots: no local source
        let f: FileConfig =
            toml::from_str("roots = [\"sym:/a\"]\nssh = [\"ssh\", \"-C\"]\nauto_reconnect = false")
                .unwrap();
        let c = Config::merge(&Cli::parse_from(["simwatch"]), f);
        assert!(c.roots.is_empty());
        assert_eq!(c.sources(), [Some("sym".into())]);
        assert_eq!(c.ssh, ["ssh", "-C"]);
        assert!(!c.auto_reconnect);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(toml::from_str::<FileConfig>("refresh = 3").is_err());
    }
}
