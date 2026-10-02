//! The list view: one line per simulation.

use chrono::{DateTime, Utc};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Cell, Paragraph, Row, Table};

use super::{App, fmt, health_style};
use crate::model::{Health, Sim};
use crate::slurm::{Snapshot, short_state};

pub const HEADERS: [&str; 12] = [
    "", "Name", "State", "Iter", "Time", "%", "Speed", "ETA", "Wall", "Upd", "Job", "Res",
];

/// The text of each column for one simulation
pub fn cells(sim: &Sim, h: Health, now: DateTime<Utc>, snap: Option<&Snapshot>) -> [String; 12] {
    let st = sim.st();
    let p = &st.progress;
    let unit = p.time_unit.as_deref().unwrap_or("");
    let time = match (p.time, p.time_end) {
        (Some(t), Some(e)) => format!("{}/{} {unit}", fmt::num(t), fmt::num(e)),
        (Some(t), None) => format!("{} {unit}", fmt::num(t)),
        _ => String::new(),
    };
    let speed = match sim.speed() {
        Some((s, avg)) => {
            let unit = p
                .speed_unit
                .clone()
                .unwrap_or_else(|| if unit.is_empty() { "/h".into() } else { format!("{unit}/h") });
            format!("{}{} {unit}", if avg { "~" } else { "" }, fmt::num(s))
        }
        None => String::new(),
    };
    let wall = match (p.walltime, p.walltime_limit) {
        (Some(w), Some(l)) if l > 0.0 => format!("{}/{}", fmt::age(w), fmt::age(l)),
        (Some(w), _) => fmt::age(w),
        _ => String::new(),
    };
    let job = match (&st.slurm.job_id, snap) {
        (Some(id), Some(snap)) => match snap.find(id) {
            Some(j) => format!("{id} {}", short_state(&j.state)),
            None => format!("{id} gone"),
        },
        (Some(id), None) => id.clone(),
        _ => String::new(),
    };
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
    let mut state = h.label().to_string();
    if sim.error.is_some() && sim.status.is_some() {
        // Showing an older parse
        state.push('!');
    }
    [
        h.glyph().to_string(),
        sim.display_name(),
        state,
        p.iteration.map(|i| i.to_string()).unwrap_or_default(),
        time,
        sim.fraction()
            .map(|x| format!("{:.0}%", 100.0 * x))
            .unwrap_or_default(),
        speed,
        sim.eta()
            .filter(|_| !h.is_done())
            .map(fmt::age)
            .unwrap_or_default(),
        wall,
        sim.age(now).map(fmt::age).unwrap_or_default(),
        job,
        res.join(" "),
    ]
}

/// Plain-text table (for `--print`)
pub fn text(sims: &[Sim], rows: &[(usize, Health)], now: DateTime<Utc>, snap: Option<&Snapshot>) -> String {
    let mut table: Vec<[String; 12]> = vec![HEADERS.map(String::from)];
    table.extend(rows.iter().map(|(i, h)| cells(&sims[*i], *h, now, snap)));
    let mut width = [0usize; 12];
    for row in &table {
        for (w, c) in width.iter_mut().zip(row) {
            *w = (*w).max(c.chars().count());
        }
    }
    width[1] = width[1].min(40);
    let numeric = [3, 5, 8, 9];
    let mut out = String::new();
    for row in &table {
        let mut line = String::new();
        for (j, c) in row.iter().enumerate() {
            let c = fmt::trunc(c, width[j]);
            let pad = width[j] - c.chars().count();
            if j > 0 {
                line.push_str("  ");
            }
            if numeric.contains(&j) {
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
            if app.scan.done.is_none() {
                "Looking for simulations…".to_string()
            } else {
                let roots: Vec<String> =
                    app.cfg.roots.iter().map(|r| r.display().to_string()).collect();
                format!(
                    "No simulations (no {} files) found below {}",
                    crate::format::STATUS_FILE,
                    roots.join(", ")
                )
            }
        } else {
            "No simulations match the current filter".to_string()
        };
        f.render_widget(Paragraph::new(Line::from(msg).dim()), area);
        return;
    }
    let now = Utc::now();
    let snap = app.slurm.as_ref();
    let rows: Vec<Row> = vis
        .iter()
        .map(|(i, h)| {
            let c = cells(&app.sims[*i], *h, now, snap);
            let hs = health_style(*h);
            Row::new(c.into_iter().enumerate().map(|(j, s)| {
                let cell = Cell::from(s);
                match j {
                    0 | 2 => cell.style(hs),
                    1 => cell.bold(),
                    _ => cell,
                }
            }))
        })
        .collect();
    let widths = [
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(11),
        Constraint::Length(8),
        Constraint::Length(15),
        Constraint::Length(4),
        Constraint::Length(11),
        Constraint::Length(5),
        Constraint::Length(9),
        Constraint::Length(4),
        Constraint::Length(13),
        Constraint::Length(9),
    ];
    let table = Table::new(rows, widths)
        .header(Row::new(HEADERS).style(Style::new().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)))
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .column_spacing(1);
    let pos = app.selected_pos(&vis);
    app.table_state.select(pos);
    f.render_stateful_widget(table, area, &mut app.table_state);
}
