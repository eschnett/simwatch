//! The card view: a few lines per simulation.

use chrono::Utc;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

use super::list::{self, Col};
use super::{App, fmt, health_style};
use crate::format::BlackHole;
use crate::model::{Health, Sim};
use crate::slurm::Snapshot;

/// Rows per card, including the border
const CARD_HEIGHT: u16 = 7;

pub fn draw(app: &mut App, f: &mut Frame, area: Rect) {
    let vis = app.visible();
    if vis.is_empty() {
        // Same message as the list view
        list::draw(app, f, area);
        return;
    }
    let fit = (area.height / CARD_HEIGHT).max(1) as usize;
    let sel = app.selected_pos(&vis).unwrap_or(0);
    if sel < app.card_offset {
        app.card_offset = sel;
    } else if sel >= app.card_offset + fit {
        app.card_offset = sel + 1 - fit;
    }
    app.card_offset = app.card_offset.min(vis.len().saturating_sub(1));
    let snap = app.slurm.as_ref();
    for (n, (i, h)) in vis.iter().enumerate().skip(app.card_offset).take(fit) {
        let y = area.y + ((n - app.card_offset) as u16) * CARD_HEIGHT;
        let height = CARD_HEIGHT.min(area.bottom().saturating_sub(y));
        if height < 3 {
            break;
        }
        let rect = Rect::new(area.x, y, area.width, height);
        card(f, rect, &app.sims[*i], *h, snap, n == sel);
    }
}

fn card(f: &mut Frame, area: Rect, sim: &Sim, h: Health, snap: Option<&Snapshot>, selected: bool) {
    let st = sim.st();
    let now = Utc::now();
    let c = |col: Col| list::cell(col, sim, h, now, snap);
    let hs = health_style(h);
    let border = if selected {
        hs.bold()
    } else {
        Style::new().fg(Color::DarkGray)
    };
    let block = Block::bordered()
        .border_type(if selected {
            BorderType::Thick
        } else {
            BorderType::Rounded
        })
        .border_style(border)
        .title(Line::from(vec![
            Span::styled(format!(" {} ", h.glyph()), hs),
            Span::raw(sim.display_name()).bold(),
            Span::raw(" "),
            Span::styled(format!("{} ", c(Col::State)), hs),
            Span::raw(st.group.as_deref().map(|g| format!("· {g} ")).unwrap_or_default()).dim(),
        ]))
        .title_top(Line::from(format!(" {} ", sim.dir.display())).dim().right_aligned());

    let dim = |s: &str| Span::raw(s.to_string()).dim();
    let mut progress: Vec<Span> = Vec::new();
    let mut add = |label: &str, value: &str| {
        if !value.is_empty() {
            if !progress.is_empty() {
                progress.push(dim("  "));
            }
            progress.push(dim(&format!("{label} ")));
            progress.push(Span::raw(value.to_string()));
        }
    };
    add("it", &c(Col::Iter));
    add("t", &c(Col::Time));
    add("", &c(Col::Pct));
    add("speed", &c(Col::Speed));
    add("ETA", &c(Col::Eta));
    add("wall", &c(Col::Wall));
    let updated = sim.age(now).map(|a| format!("{} ago", fmt::age(a)));
    add("updated", updated.as_deref().unwrap_or(""));

    let message = match (&sim.error, &st.message) {
        (Some(e), _) => Line::from(Span::styled(format!("status file: {e}"), Style::new().fg(Color::Red))),
        (None, Some(m)) => Line::from(m.clone()),
        (None, None) => Line::from(dim("–")),
    };

    let mut jobline: Vec<Span> = Vec::new();
    let job = c(Col::Job);
    if !job.is_empty() {
        jobline.push(dim("job "));
        jobline.push(Span::raw(job));
        if let Some(j) = sim.job(snap) {
            jobline.push(dim(&format!("  {} {}", j.partition, j.reason)));
        }
    }
    let r = &st.resources;
    let mut parts = Vec::new();
    if let Some(hst) = &st.host {
        parts.push(hst.clone());
    }
    if let Some(n) = r.nodes {
        parts.push(format!("{n} node{}", if n == 1 { "" } else { "s" }));
    }
    if let Some(t) = r.tasks {
        parts.push(format!("{t} tasks"));
    }
    if let Some(t) = r.threads {
        parts.push(format!("{t} threads"));
    }
    if let Some(g) = r.gpus {
        parts.push(format!("{g} GPUs"));
    }
    if let Some(m) = r.memory_bytes {
        parts.push(fmt::bytes(m));
    }
    if !parts.is_empty() {
        if !jobline.is_empty() {
            jobline.push(dim("  "));
        }
        jobline.push(Span::raw(parts.join(", ")));
    }

    let bh_line = if st.black_holes.is_empty() {
        None
    } else {
        Some(Line::from(
            st.black_holes
                .iter()
                .enumerate()
                .map(|(i, b)| black_hole(i, b))
                .collect::<Vec<_>>()
                .join("   "),
        ))
    };
    let summary = sim.summary();
    let extra_line = if summary.is_empty() {
        let extras: Vec<String> = st
            .extra
            .iter()
            .take(12)
            .map(|e| {
                let unit = e.unit.as_deref().map(|u| format!(" {u}")).unwrap_or_default();
                format!("{}={}{unit}", e.key, fmt::value(&e.value))
            })
            .collect();
        Line::from(dim(&extras.join("  ")))
    } else {
        // Headline values, with a sparkline of their recent history
        let mut spans = Vec::new();
        for it in &summary {
            if !spans.is_empty() {
                spans.push(dim("   "));
            }
            let unit = it.value.unit.as_deref().map(|u| format!(" {u}")).unwrap_or_default();
            spans.push(dim(&format!("{} ", it.label)));
            spans.push(Span::raw(format!("{}{unit}", fmt::value(&it.value.value))));
            if let Some(h) = it.history {
                let line = fmt::sparkline(h, 12, fmt::wants_log(h));
                spans.push(Span::styled(format!(" {line}"), Style::new().fg(Color::Cyan)));
            }
        }
        Line::from(spans)
    };

    let mut lines = vec![Line::from(progress), message, Line::from(jobline)];
    match bh_line {
        Some(b) => lines.extend([b, extra_line]),
        None => lines.push(extra_line),
    }
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// One-line black hole summary
pub fn black_hole(i: usize, b: &BlackHole) -> String {
    let mut s = b.name.clone().unwrap_or_else(|| format!("BH{}", i + 1));
    if let Some(m) = b.mass.or(b.irreducible_mass) {
        s.push_str(&format!(" M={}", fmt::num(m)));
    }
    if let Some(chi) = &b.spin {
        s.push_str(&format!(" χ={}", fmt::num(fmt::norm(chi))));
    }
    if let Some(x) = &b.position {
        s.push_str(&format!(" @{}", fmt::vector(x)));
    }
    if b.found == Some(false) {
        s.push_str(" (not found)");
    }
    s
}
