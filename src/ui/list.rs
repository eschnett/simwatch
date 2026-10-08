//! The list view: one line per simulation.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Cell, Paragraph, Row, Table};

use super::{App, fmt, health_style};
use crate::model::{Health, Sim, trend};
use crate::slurm::{Snapshot, short_state};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Col {
    Glyph,
    Name,
    Host,
    Group,
    State,
    Iter,
    Time,
    Pct,
    Speed,
    Eta,
    Wall,
    Upd,
    Job,
    Res,
    Summary,
}

impl Col {
    pub fn header(self) -> &'static str {
        match self {
            Col::Glyph => "",
            Col::Name => "Name",
            Col::Host => "Host",
            Col::Group => "Group",
            Col::State => "State",
            Col::Iter => "Iter",
            Col::Time => "Time",
            Col::Pct => "%",
            Col::Speed => "Speed",
            Col::Eta => "ETA",
            Col::Wall => "Wall",
            Col::Upd => "Upd",
            Col::Job => "Job",
            Col::Res => "Res",
            Col::Summary => "Summary",
        }
    }

    fn width(self) -> Constraint {
        match self {
            Col::Glyph => Constraint::Length(1),
            Col::Name | Col::Summary => Constraint::Fill(1),
            Col::Host => Constraint::Length(12),
            Col::Group => Constraint::Length(14),
            Col::State => Constraint::Length(11),
            Col::Iter => Constraint::Length(8),
            Col::Time => Constraint::Length(15),
            Col::Pct => Constraint::Length(4),
            Col::Speed => Constraint::Length(11),
            Col::Eta => Constraint::Length(5),
            Col::Wall => Constraint::Length(9),
            Col::Upd => Constraint::Length(4),
            Col::Job => Constraint::Length(16),
            Col::Res => Constraint::Length(9),
        }
    }

    /// Columns that give way first when the terminal is too narrow
    const DROP_ORDER: [Col; 8] = [
        Col::Res,
        Col::Wall,
        Col::Speed,
        Col::Iter,
        Col::Host,
        Col::Group,
        Col::Eta,
        Col::Job,
    ];

    /// Least useful width
    fn min_width(self) -> u16 {
        match self.width() {
            Constraint::Length(n) => n,
            _ if self == Col::Summary => 24,
            _ => 16,
        }
    }

    /// Right-aligned in plain text
    fn numeric(self) -> bool {
        matches!(self, Col::Iter | Col::Pct | Col::Wall | Col::Upd)
    }
}

/// The columns to show: Group and Summary only if some simulation has one,
/// Host only if the simulations come from more than one host
pub fn columns<'a>(sims: impl IntoIterator<Item = &'a Sim> + Clone) -> Vec<Col> {
    let mut cols = vec![Col::Glyph, Col::Name];
    let mut hosts = sims.clone().into_iter().map(|s| &s.host);
    if let Some(first) = hosts.next() {
        if hosts.any(|h| h != first) {
            cols.push(Col::Host);
        }
    }
    if sims.clone().into_iter().any(|s| s.st().group.is_some()) {
        cols.push(Col::Group);
    }
    cols.extend([
        Col::State,
        Col::Iter,
        Col::Time,
        Col::Pct,
        Col::Speed,
        Col::Eta,
        Col::Wall,
        Col::Upd,
        Col::Job,
        Col::Res,
    ]);
    if sims.into_iter().any(|s| !s.st().summary.is_empty()) {
        cols.push(Col::Summary);
    }
    cols
}

/// The text of one cell
pub fn cell(col: Col, sim: &Sim, h: Health, now: DateTime<Utc>, snap: Option<&Snapshot>) -> String {
    let st = sim.st();
    let p = &st.progress;
    let unit = p.time_unit.as_deref().unwrap_or("");
    match col {
        Col::Glyph => h.glyph().to_string(),
        Col::Name => sim.display_name(),
        Col::Host => sim.host.clone().unwrap_or_else(|| "local".into()),
        Col::Group => st.group.clone().unwrap_or_default(),
        Col::State => {
            let mut state = h.label().to_string();
            if sim.error.is_some() && sim.status.is_some() {
                // Showing an older parse
                state.push('!');
            }
            state
        }
        Col::Iter => p.iteration.map(|i| i.to_string()).unwrap_or_default(),
        Col::Time => match (p.time, p.time_end) {
            (Some(t), Some(e)) => format!("{}/{} {unit}", fmt::num(t), fmt::num(e)),
            (Some(t), None) => format!("{} {unit}", fmt::num(t)),
            _ => String::new(),
        },
        Col::Pct => sim
            .fraction()
            .map(|x| format!("{:.0}%", 100.0 * x))
            .unwrap_or_default(),
        Col::Speed => match sim.speed() {
            Some((s, avg)) => {
                let unit = p.speed_unit.clone().unwrap_or_else(|| {
                    if unit.is_empty() {
                        "/h".into()
                    } else {
                        format!("{unit}/h")
                    }
                });
                format!("{}{} {unit}", if avg { "~" } else { "" }, fmt::num(s))
            }
            None => String::new(),
        },
        Col::Eta => match sim.eta().filter(|_| !h.is_done()) {
            Some((eta, avg)) => format!("{}{}", if avg { "~" } else { "" }, fmt::age(eta)),
            None => String::new(),
        },
        Col::Wall => match (p.walltime, p.walltime_limit) {
            (Some(w), Some(l)) if l > 0.0 => format!("{}/{}", fmt::age(w), fmt::age(l)),
            (Some(w), _) => fmt::age(w),
            _ => String::new(),
        },
        Col::Upd => sim.age(now).map(fmt::age).unwrap_or_default(),
        Col::Job => match sim.shown_job() {
            Some((id, n, next)) => {
                let state = match snap.map(|s| s.find(id)) {
                    Some(Some(j)) => short_state(&j.state).to_string(),
                    Some(None) => "gone".into(),
                    None if next => "next".into(),
                    None => String::new(),
                };
                let mut parts = vec![id.to_string()];
                if !state.is_empty() {
                    parts.push(state);
                }
                if n > 1 {
                    parts.push(format!("#{n}"));
                }
                parts.join(" ")
            }
            None => String::new(),
        },
        Col::Res => {
            let r = &st.resources;
            let mut res = Vec::new();
            if let Some(n) = r.nodes {
                res.push(format!("{n}n"));
            }
            if let Some(t) = r.tasks.filter(|t| *t > 1) {
                res.push(format!("{t}p"));
            }
            if let Some(t) = r.threads {
                res.push(format!("{t}t"));
            }
            if let Some(g) = r.gpus.filter(|g| *g > 0) {
                res.push(format!("{g}g"));
            }
            res.join(" ")
        }
        Col::Summary => summary_text(sim),
    }
}

/// `ham_l2 3.4e-5↑  M_irr 0.9452`
pub fn summary_text(sim: &Sim) -> String {
    let items: Vec<String> = sim
        .summary()
        .iter()
        .map(|it| {
            let unit = it
                .value
                .unit
                .as_deref()
                .map(|u| format!(" {u}"))
                .unwrap_or_default();
            let arrow = it
                .history
                .and_then(trend)
                .map(String::from)
                .unwrap_or_default();
            format!("{} {}{unit}{arrow}", it.label, fmt::value(&it.value.value))
        })
        .collect();
    items.join("  ")
}

/// Plain-text table (for `--print`), with the Slurm snapshot of each host
pub fn text(
    sims: &[Sim],
    rows: &[(usize, Health)],
    now: DateTime<Utc>,
    snaps: &HashMap<Option<String>, Snapshot>,
) -> String {
    let cols = columns(rows.iter().map(|(i, _)| &sims[*i]));
    let mut table: Vec<Vec<String>> = vec![cols.iter().map(|c| c.header().to_string()).collect()];
    table.extend(rows.iter().map(|(i, h)| {
        let sim = &sims[*i];
        cols.iter()
            .map(|c| cell(*c, sim, *h, now, snaps.get(&sim.host)))
            .collect()
    }));
    let mut width = vec![0usize; cols.len()];
    for row in &table {
        for (w, c) in width.iter_mut().zip(row) {
            *w = (*w).max(c.chars().count());
        }
    }
    for (w, c) in width.iter_mut().zip(&cols) {
        match c {
            Col::Name => *w = (*w).min(40),
            Col::Group => *w = (*w).min(24),
            _ => {}
        }
    }
    let mut out = String::new();
    for row in &table {
        let mut line = String::new();
        for (j, c) in row.iter().enumerate() {
            let c = fmt::trunc(c, width[j]);
            let pad = width[j] - c.chars().count();
            if j > 0 {
                line.push_str("  ");
            }
            if cols[j].numeric() {
                line.push_str(&" ".repeat(pad));
                line.push_str(&c);
            } else {
                line.push_str(&c);
                line.push_str(&" ".repeat(pad));
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

pub fn draw(app: &mut App, f: &mut Frame, area: Rect) {
    let vis = app.visible();
    if vis.is_empty() {
        let msg = if app.sims.is_empty() {
            let looking = app
                .sources
                .iter()
                .any(|s| s.scan.done.is_none() && s.disconnected.is_none());
            if looking {
                "Looking for simulations…".to_string()
            } else {
                format!(
                    "No simulations (no {} files) found below {}",
                    crate::format::STATUS_FILE,
                    app.cfg.root_names().join(", ")
                )
            }
        } else {
            "No simulations match the current filter".to_string()
        };
        f.render_widget(Paragraph::new(Line::from(msg).dim()), area);
        return;
    }
    let now = Utc::now();
    let mut cols = columns(vis.iter().map(|(i, _)| &app.sims[*i]));
    let needed = |cols: &[Col]| -> u16 { cols.iter().map(|c| c.min_width() + 1).sum() };
    for drop in Col::DROP_ORDER {
        if needed(&cols) <= area.width {
            break;
        }
        cols.retain(|c| *c != drop);
    }
    let rows: Vec<Row> = vis
        .iter()
        .map(|(i, h)| {
            let hs = health_style(*h);
            let sim = &app.sims[*i];
            let snap = app.snap(sim);
            Row::new(cols.iter().map(|c| {
                let cell = Cell::from(cell(*c, sim, *h, now, snap));
                match c {
                    Col::Glyph | Col::State => cell.style(hs),
                    Col::Name => cell.bold(),
                    Col::Host | Col::Group => cell.dim(),
                    _ => cell,
                }
            }))
        })
        .collect();
    let table = Table::new(rows, cols.iter().map(|c| c.width()))
        .header(
            Row::new(cols.iter().map(|c| c.header()))
                .style(Style::new().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        )
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .column_spacing(1);
    let pos = app.selected_pos(&vis);
    app.table_state.select(pos);
    f.render_stateful_widget(table, area, &mut app.table_state);
}
