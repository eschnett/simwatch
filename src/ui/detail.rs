//! The detail view: everything about one simulation, plus its images.

use chrono::Utc;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui_image::{Resize, StatefulImage};

use super::{App, ImageState, cards, fmt, health_style};
use crate::format::{STATUS_FILE, Status};
use crate::model::Sim;
use crate::slurm::Snapshot;

const KEY_WIDTH: usize = 15;

pub fn draw(app: &mut App, f: &mut Frame, area: Rect) {
    let Some(sim) = app.selected_sim().cloned() else {
        f.render_widget(Paragraph::new("No simulation selected").dim(), area);
        return;
    };
    let h = app.health(&sim);
    let lines = lines(&sim, app.slurm.as_ref(), app.health(&sim));
    let images = &sim.st().images;
    let show_images = app.images_enabled() && !images.is_empty() && area.width >= 60;

    let (text_area, image_area) = if show_images {
        let [l, r] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .areas(area);
        (l, Some(r))
    } else {
        (area, None)
    };

    let max_scroll = (lines.len() as u16).saturating_sub(text_area.height.saturating_sub(2));
    app.detail_scroll = app.detail_scroll.min(max_scroll);
    let title = Line::from(vec![
        Span::styled(format!(" {} ", h.glyph()), health_style(h)),
        Span::raw(sim.display_name()).bold(),
        Span::raw(" "),
    ]);
    let block = Block::bordered()
        .border_style(Style::new().fg(Color::DarkGray))
        .title(title)
        .title_bottom(Line::from(" n/p: next/prev sim  [/]: image  Esc: back ").dim().right_aligned());
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .scroll((app.detail_scroll, 0)),
        text_area,
    );

    if let Some(area) = image_area {
        let n = images.len();
        let idx = app.image_idx.min(n - 1);
        let img = images[idx].clone();
        let title = format!(
            " {}/{n}: {} ",
            idx + 1,
            img.title.clone().unwrap_or_else(|| img.file.clone())
        );
        let block = Block::bordered()
            .border_style(Style::new().fg(Color::DarkGray))
            .title(Line::from(title).bold());
        let inner = block.inner(area);
        f.render_widget(block, area);
        let desc_lines = if img.description.is_some() { 3 } else { 0 };
        let [pic, desc] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(desc_lines)]).areas(inner);
        if let Some(d) = &img.description {
            f.render_widget(Paragraph::new(d.as_str()).wrap(Wrap { trim: true }).dim(), desc);
        }
        match app.image(&sim.dir, sim.mtime, &img.file) {
            ImageState::Loading => {
                f.render_widget(Paragraph::new("loading…").dim(), pic);
            }
            ImageState::Failed(e) => {
                f.render_widget(
                    Paragraph::new(format!("({e})"))
                        .style(Style::new().fg(Color::Yellow))
                        .wrap(Wrap { trim: true }),
                    pic,
                );
            }
            ImageState::Ready(proto) => {
                // Fit shrinks large images but never enlarges small ones
                f.render_stateful_widget(StatefulImage::default().resize(Resize::Fit(None)), pic, proto.as_mut());
            }
        }
    }
}

fn section(out: &mut Vec<Line<'static>>, title: &str) {
    out.push(Line::default());
    out.push(Line::from(Span::styled(
        title.to_string(),
        Style::new().fg(Color::Cyan).bold(),
    )));
}

fn kv(out: &mut Vec<Line<'static>>, key: &str, value: impl Into<String>) {
    out.push(Line::from(vec![
        Span::raw(format!("{:<KEY_WIDTH$} ", fmt::trunc(key, KEY_WIDTH))).dim(),
        Span::raw(value.into()),
    ]));
}

pub fn lines(sim: &Sim, snap: Option<&Snapshot>, h: crate::model::Health) -> Vec<Line<'static>> {
    let now = Utc::now();
    let st: &Status = sim.st();
    let p = &st.progress;
    let mut out = Vec::new();

    kv(
        &mut out,
        "Name",
        st.name.clone().unwrap_or_else(|| "(missing simulation name)".into()),
    );
    kv(&mut out, "Directory", sim.dir.display().to_string());
    let mut state = h.label().to_string();
    if let Some(s) = &st.status {
        if !s.eq_ignore_ascii_case(h.label()) {
            state.push_str(&format!("  (reported: {s})"));
        }
    }
    out.push(Line::from(vec![
        Span::raw(format!("{:<KEY_WIDTH$} ", "State")).dim(),
        Span::styled(state, health_style(h)),
    ]));
    if let Some(m) = &st.message {
        kv(&mut out, "Message", m.clone());
    }
    if let Some(c) = &st.code {
        kv(&mut out, "Code", c.clone());
    }
    if let Some(hst) = &st.host {
        let pid = st.pid.map(|p| format!("  (pid {p})")).unwrap_or_default();
        kv(&mut out, "Host", format!("{hst}{pid}"));
    } else if let Some(pid) = st.pid {
        kv(&mut out, "PID", pid.to_string());
    }
    if let Some(t) = st.started {
        let ago = fmt::duration((now - t).num_seconds() as f64);
        kv(&mut out, "Started", format!("{}  ({ago} ago)", fmt::local_time(t)));
    }
    if let Some(t) = sim.last_update() {
        let ago = fmt::duration((now - t).num_seconds() as f64);
        let every = st
            .update_interval
            .map(|i| format!(", every {}", fmt::duration(i)))
            .unwrap_or_default();
        kv(&mut out, "Updated", format!("{}  ({ago} ago{every})", fmt::local_time(t)));
    }

    let unit = p.time_unit.clone().unwrap_or_default();
    let mut prog = Vec::new();
    if let Some(i) = p.iteration {
        prog.push(("Iteration", i.to_string()));
    }
    if let Some(t) = p.time {
        let mut s = format!("{} {unit}", fmt::num(t));
        if let Some(e) = p.time_end {
            s = format!("{} / {} {unit}", fmt::num(t), fmt::num(e));
        }
        if let Some(x) = sim.fraction() {
            s.push_str(&format!("  ({:.1}%)", 100.0 * x));
        }
        prog.push(("Time", s));
    }
    if let Some((s, avg)) = sim.speed() {
        let u = p.speed_unit.clone().unwrap_or_else(|| format!("{unit}/h"));
        let note = if avg { "  (average)" } else { "" };
        prog.push(("Speed", format!("{} {u}{note}", fmt::num(s))));
    }
    if let Some(eta) = sim.eta() {
        prog.push(("ETA", fmt::duration(eta)));
    }
    if let Some(w) = p.walltime {
        let lim = p
            .walltime_limit
            .map(|l| format!(" / {}", fmt::duration(l)))
            .unwrap_or_default();
        prog.push(("Walltime", format!("{}{lim}", fmt::duration(w))));
    }
    if let Some(c) = &p.checkpoint {
        prog.push(("Checkpoint", c.clone()));
    }
    if !prog.is_empty() {
        section(&mut out, "Progress");
        for (k, v) in prog {
            kv(&mut out, k, v);
        }
    }

    let r = &st.resources;
    let mut res = Vec::new();
    for (k, v) in [("Nodes", r.nodes), ("Tasks", r.tasks), ("Threads", r.threads), ("GPUs", r.gpus)] {
        if let Some(v) = v {
            res.push((k, v.to_string()));
        }
    }
    if let Some(m) = r.memory_bytes {
        let lim = r
            .memory_limit_bytes
            .map(|l| format!(" / {}", fmt::bytes(l)))
            .unwrap_or_default();
        res.push(("Memory", format!("{}{lim}", fmt::bytes(m))));
    }
    if let Some(m) = r.memory_peak_bytes {
        res.push(("Peak memory", fmt::bytes(m)));
    }
    if !res.is_empty() {
        section(&mut out, "Resources");
        for (k, v) in res {
            kv(&mut out, k, v);
        }
    }

    let s = &st.slurm;
    if s.job_id.is_some() || s.job_name.is_some() || s.next_job_id.is_some() {
        section(&mut out, "Slurm");
        if let Some(id) = &s.job_id {
            let status = match (snap, sim.job(snap)) {
                (_, Some(j)) => format!(
                    "{}  {}  elapsed {} of {}",
                    j.state, j.reason, j.elapsed, j.limit
                ),
                (Some(snap), None) => format!(
                    "not in squeue ({} ago)",
                    fmt::age((now - snap.taken).num_seconds() as f64)
                ),
                (None, None) => "(no squeue information)".into(),
            };
            kv(&mut out, "Job", format!("{id}  {status}"));
        }
        if let Some(n) = &s.job_name {
            kv(&mut out, "Job name", n.clone());
        }
        if let Some(pt) = &s.partition {
            kv(&mut out, "Partition", pt.clone());
        }
        if let Some(n) = &s.next_job_id {
            let state = snap
                .and_then(|sn| sn.find(n))
                .map(|j| format!("  {}", j.state))
                .unwrap_or_default();
            kv(&mut out, "Next job", format!("{n}{state}"));
        }
    }

    if !st.black_holes.is_empty() {
        section(&mut out, "Black holes");
        for (i, b) in st.black_holes.iter().enumerate() {
            let name = b.name.clone().unwrap_or_else(|| format!("BH{}", i + 1));
            let mut parts = Vec::new();
            if let Some(m) = b.mass {
                parts.push(format!("M {}", fmt::num(m)));
            }
            if let Some(m) = b.irreducible_mass {
                parts.push(format!("M_irr {}", fmt::num(m)));
            }
            if let Some(chi) = &b.spin {
                if chi.len() > 1 {
                    parts.push(format!("χ {} |χ| {}", fmt::vector(chi), fmt::num(fmt::norm(chi))));
                } else {
                    parts.push(format!("χ {}", fmt::vector(chi)));
                }
            }
            if let Some(x) = &b.position {
                parts.push(format!("at {}", fmt::vector(x)));
            }
            match b.found {
                Some(true) => parts.push("found".into()),
                Some(false) => parts.push("NOT FOUND".into()),
                None => {}
            }
            if parts.is_empty() {
                parts.push(cards::black_hole(i, b));
            }
            kv(&mut out, &name, parts.join("  "));
        }
    }

    if !st.images.is_empty() {
        section(&mut out, "Images");
        for (i, img) in st.images.iter().enumerate() {
            let title = img.title.as_deref().map(|t| format!("  {t}")).unwrap_or_default();
            kv(&mut out, &format!("{}", i + 1), format!("{}{title}", img.file));
        }
    }

    if !st.extra.is_empty() {
        section(&mut out, "Other");
        let width = st
            .extra
            .iter()
            .map(|e| e.key.chars().count())
            .max()
            .unwrap_or(0)
            .clamp(KEY_WIDTH, 32);
        for e in &st.extra {
            let mut spans = vec![
                Span::raw(format!("{:<width$} ", fmt::trunc(&e.key, width))).dim(),
                Span::raw(fmt::value(&e.value)),
            ];
            if let Some(u) = &e.unit {
                spans.push(Span::raw(format!(" {u}")));
            }
            if let Some(l) = &e.label {
                spans.push(Span::raw(format!("  ({l})")).dim());
            }
            out.push(Line::from(spans));
        }
    }

    section(&mut out, "Status file");
    let mut file = format!("{}", sim.dir.join(STATUS_FILE).display());
    if sim.mtime.is_some() {
        file.push_str(&format!("  ({})", fmt::bytes(sim.size as f64)));
    }
    kv(&mut out, "File", file);
    if let Some(e) = &sim.error {
        let what = if sim.status.is_some() {
            "showing an earlier version; "
        } else {
            ""
        };
        out.push(Line::from(vec![
            Span::raw(format!("{:<KEY_WIDTH$} ", "Problem")).dim(),
            Span::styled(format!("{what}{e}"), Style::new().fg(Color::Red)),
        ]));
    }
    out
}
