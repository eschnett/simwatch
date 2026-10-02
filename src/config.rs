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
    /// Directories to search for simulations (default: from the config file,
    /// else the current directory)
    pub dirs: Vec<PathBuf>,

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
}

#[derive(Clone, Debug)]
pub struct Config {
    pub roots: Vec<PathBuf>,
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
    pub print: bool,
}

/// Cell size in pixels if neither the terminal nor the configuration says
pub const DEFAULT_FONT_SIZE: (u16, u16) = (10, 20);

impl Default for Config {
    fn default() -> Self {
        Config {
            roots: vec![PathBuf::from(".")],
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
            print: false,
        }
    }
}

pub fn default_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config/simwatch/config.toml"))
}

/// Expand a leading `~/`
fn expand(s: &str) -> PathBuf {
    match (s.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ if s == "~" => dirs::home_dir().unwrap_or_else(|| PathBuf::from(s)),
        _ => PathBuf::from(s),
    }
}

/// Durations below one second would hammer the file system
fn secs(x: f64) -> Duration {
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
        let roots = if !cli.dirs.is_empty() {
            cli.dirs.clone()
        } else if let Some(r) = f.roots.filter(|r| !r.is_empty()) {
            r.iter().map(|s| expand(s)).collect()
        } else {
            d.roots
        };
        let dl = Limits::default();
        let dh = HealthParams::default();
        Config {
            roots,
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
            print: cli.print,
        }
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
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(toml::from_str::<FileConfig>("refresh = 3").is_err());
    }
}
