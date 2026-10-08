//! The help overlay.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::model::Health;

const KEYS: &[(&str, &str)] = &[
    ("↑ ↓  j k", "select simulation (scroll in detail view)"),
    ("PgUp PgDn g G", "page, first, last"),
    ("Enter  →", "show details"),
    ("Esc  ←", "back from details"),
    ("n p", "next / previous simulation in detail view"),
    ("[ ]", "previous / next image"),
    ("Tab  1 2 3", "list, cards, detail view"),
    ("s", "change sort order"),
    ("f", "hide / show finished and failed"),
    ("/", "filter by name (Enter keeps, Esc clears)"),
    ("r", "re-read status files now"),
    ("R", "scan for new simulations now"),
    ("c", "reconnect to lost remote hosts"),
    ("Ctrl-L", "repaint the screen"),
    ("q", "quit"),
];

pub fn draw(f: &mut Frame, area: Rect) {
    let mut lines: Vec<Line> = KEYS
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!(" {k:<14}"), Style::new().fg(Color::Cyan).bold()),
                Span::raw(*d),
            ])
        })
        .collect();
    lines.push(Line::default());
    let states = [
        (Health::Running, "status file is fresh"),
        (Health::Queued, "waiting in the Slurm queue"),
        (Health::Stale, "status file has not been updated in a while"),
        (
            Health::Lost,
            "Slurm job is gone, but the simulation did not say it ended",
        ),
        (Health::Stopped, "stopped, e.g. at the wall time limit"),
        (Health::Finished, "finished"),
        (Health::Failed, "the simulation reported a failure"),
        (Health::Unreadable, "status file cannot be read"),
    ];
    for (h, d) in states {
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} {:<12}", h.glyph(), h.label()),
                super::health_style(h),
            ),
            Span::raw(d).dim(),
        ]));
    }
    let height = lines.len() as u16 + 2;
    let [v] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [r] = Layout::horizontal([Constraint::Length(76)])
        .flex(Flex::Center)
        .areas(v);
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(Line::from(" SimWatch help — any key closes ").bold())
                .border_style(Style::new().fg(Color::Cyan)),
        ),
        r,
    );
}
