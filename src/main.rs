//! SimWatch: a terminal user interface to watch the progress of HPC simulations.

mod config;
mod discover;
mod format;
mod images;
mod model;
mod monitor;
mod slurm;
mod ui;

use std::io::IsTerminal;
use std::sync::mpsc::channel;

use anyhow::{Result, bail};
use chrono::Utc;
use clap::Parser;
use ratatui_image::picker::{Picker, ProtocolType};

use config::{Cli, Config, ImageMode};
use model::health;

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = Config::load(&cli)?;
    if cfg.print {
        return print_once(&cfg);
    }
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        bail!("simwatch needs a terminal; use --print for plain text output");
    }

    let picker = match cfg.images {
        ImageMode::None => None,
        ImageMode::Sixel => Some(sixel_picker(&cfg)),
    };

    let (tx, rx) = channel();
    let monitor = monitor::Monitor::start(&cfg, tx);
    let app = ui::App::new(cfg, picker);
    let mut terminal = ratatui::init();
    let res = ui::run(&mut terminal, app, rx, monitor);
    ratatui::restore();
    res
}

/// Determine the terminal's cell size in pixels, then force sixel output.
///
/// The window-size ioctl is tried first: WezTerm reports pixel sizes and ssh
/// forwards them. Only if that fails is the terminal queried with escape
/// sequences. (If a terminal never answers that query, ratatui-image leaves a
/// thread behind that keeps reading stdin, so we avoid it when we can.)
fn sixel_picker(cfg: &Config) -> Picker {
    let cell = cfg.font_size.or_else(|| {
        let ws = crossterm::terminal::window_size().ok()?;
        (ws.columns > 0 && ws.rows > 0 && ws.width > 0 && ws.height > 0)
            .then(|| [ws.width / ws.columns, ws.height / ws.rows])
            .filter(|[w, h]| *w > 0 && *h > 0)
    });
    #[allow(deprecated)]
    let mut picker = match cell {
        Some([w, h]) => Picker::from_fontsize((w, h).into()),
        None => Picker::from_query_stdio()
            .unwrap_or_else(|_| Picker::from_fontsize(config::DEFAULT_FONT_SIZE.into())),
    };
    picker.set_protocol_type(ProtocolType::Sixel);
    picker
}

fn print_once(cfg: &Config) -> Result<()> {
    let (sims, scan, snap) = monitor::collect_once(cfg);
    let snap = match snap {
        Some(Ok(s)) => Some(s),
        Some(Err(e)) => {
            eprintln!("simwatch: {e}");
            None
        }
        None => None,
    };
    for w in &scan.warnings {
        eprintln!("simwatch: {w}");
    }
    let now = Utc::now();
    let mut rows: Vec<(usize, model::Health)> = sims
        .iter()
        .enumerate()
        .map(|(i, s)| (i, health(s, now, snap.as_ref(), &cfg.health)))
        .collect();
    rows.sort_by(|a, b| {
        sims[b.0]
            .sort_time()
            .cmp(&sims[a.0].sort_time())
            .then_with(|| sims[a.0].display_name().cmp(&sims[b.0].display_name()))
    });
    print!("{}", ui::list::text(&sims, &rows, now, snap.as_ref()));
    Ok(())
}
