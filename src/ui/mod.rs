//! The terminal user interface.

mod cards;
mod detail;
pub mod fmt;
mod help;
pub mod list;

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use chrono::Utc;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, TableState};
use ratatui::{DefaultTerminal, Frame};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

use crate::config::Config;
use crate::images::{Fetch, ImageKey, Loader};
use crate::model::{Health, Sim, SimId, health};
use crate::monitor::{Monitor, Request, Tagged, Update};
use crate::remote::client::TUI_ACTIVE;
use crate::slurm::Snapshot;

/// Manual refreshes are rate-limited to one per this interval
const MANUAL_GAP: Duration = Duration::from_secs(5);
/// A scan taking longer than this is flagged as slow
const SLOW_SCAN: Duration = Duration::from_secs(30);
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    List,
    Cards,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    Newest,
    Name,
    Health,
    Updated,
    Group,
}

impl Sort {
    fn next(self) -> Sort {
        match self {
            Sort::Newest => Sort::Name,
            Sort::Name => Sort::Health,
            Sort::Health => Sort::Updated,
            Sort::Updated => Sort::Group,
            Sort::Group => Sort::Newest,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Sort::Newest => "newest",
            Sort::Name => "name",
            Sort::Health => "state",
            Sort::Updated => "updated",
            Sort::Group => "group",
        }
    }
}

/// Start time of a running job, and end time of the last one
#[derive(Default)]
pub struct Activity {
    pub busy: Option<Instant>,
    pub done: Option<Instant>,
}

impl Activity {
    fn start(&mut self) {
        self.busy = Some(Instant::now());
    }
    fn finish(&mut self) {
        self.busy = None;
        self.done = Some(Instant::now());
    }
}

pub enum ImageState {
    Loading,
    Ready(Box<StatefulProtocol>),
    Failed(String),
}

struct ImageEntry {
    /// Status file mtime when requested; images are reloaded when it changes
    version: Option<SystemTime>,
    pending: bool,
    state: ImageState,
}

/// What the UI knows about one source of simulations: the local
/// directories, or a remote host
#[derive(Default)]
pub struct Source {
    /// The remote host, or `None` for local directories
    pub host: Option<String>,
    pub sims: Vec<Sim>,
    pub slurm: Option<Snapshot>,
    pub slurm_error: Option<String>,
    pub slurm_disabled: Option<String>,
    pub scan: Activity,
    pub read: Activity,
    pub squeue: Activity,
    pub scan_warnings: Vec<String>,
    pub scan_dirs: usize,
    /// Why a remote host is not connected
    pub disconnected: Option<String>,
    /// The latest problem a remote host reported
    pub warning: Option<String>,
}

impl Source {
    pub fn name(&self) -> &str {
        self.host.as_deref().unwrap_or("local")
    }

    fn busy(&self) -> bool {
        self.scan.busy.is_some() || self.read.busy.is_some() || self.squeue.busy.is_some()
    }
}

pub struct App {
    pub cfg: Config,
    /// The simulations of all sources
    pub sims: Vec<Sim>,
    /// In the order of `Config::sources`
    pub sources: Vec<Source>,

    pub view: View,
    pub detail: bool,
    pub selected: Option<SimId>,
    pub hide_done: bool,
    pub sort: Sort,
    pub filter: String,
    pub editing_filter: bool,
    pub help: bool,
    pub table_state: TableState,
    pub card_offset: usize,
    pub detail_scroll: u16,
    pub image_idx: usize,

    picker: Option<Picker>,
    loader: Option<Loader>,
    images: HashMap<ImageKey, ImageEntry>,

    last_reread: Option<Instant>,
    last_rescan: Option<Instant>,
    notice: Option<(String, Instant)>,
    needs_clear: bool,
    /// Reconnect lost hosts, with the terminal released for passwords
    reconnect: bool,
    quit: bool,
}

impl App {
    /// `fetch` gets image files from remote hosts
    pub fn new(cfg: Config, picker: Option<Picker>, fetch: Fetch) -> App {
        let loader = picker.is_some().then(|| Loader::start(fetch));
        let sources = cfg
            .sources()
            .into_iter()
            .map(|host| Source {
                host,
                ..Source::default()
            })
            .collect();
        App {
            cfg,
            sims: Vec::new(),
            sources,
            view: View::List,
            detail: false,
            selected: None,
            hide_done: false,
            sort: Sort::Newest,
            filter: String::new(),
            editing_filter: false,
            help: false,
            table_state: TableState::default(),
            card_offset: 0,
            detail_scroll: 0,
            image_idx: 0,
            picker,
            loader,
            images: HashMap::new(),
            last_reread: None,
            last_rescan: None,
            notice: None,
            needs_clear: false,
            reconnect: false,
            quit: false,
        }
    }

    pub fn apply(&mut self, (source, u): Tagged) {
        let Some(src) = self.sources.get_mut(source) else {
            return;
        };
        match u {
            Update::ScanStarted => src.scan.start(),
            Update::ScanFinished(res) => {
                src.scan.finish();
                src.scan_warnings = res.warnings;
                src.scan_dirs = res.dirs_visited;
            }
            Update::ReadStarted => src.read.start(),
            Update::Sims(sims) => {
                src.read.finish();
                src.sims = sims;
                self.sims = self
                    .sources
                    .iter()
                    .flat_map(|s| s.sims.iter().cloned())
                    .collect();
            }
            Update::SlurmStarted => src.squeue.start(),
            Update::Slurm(r) => {
                src.squeue.finish();
                match r {
                    Ok(s) => {
                        src.slurm = Some(s);
                        src.slurm_error = None;
                    }
                    // Keep the previous snapshot; its age is shown
                    Err(e) => src.slurm_error = Some(e),
                }
            }
            Update::SlurmDisabled(why) => {
                src.squeue.busy = None;
                src.slurm_disabled = Some(why);
            }
            Update::Connected => src.disconnected = None,
            Update::Disconnected(why) => {
                // Nothing is running on the other side any more
                src.scan.busy = None;
                src.read.busy = None;
                src.squeue.busy = None;
                src.disconnected = Some(why);
            }
            Update::Warning(w) => src.warning = Some(w),
        }
    }

    /// The Slurm snapshot of a simulation's host
    pub fn snap(&self, sim: &Sim) -> Option<&Snapshot> {
        self.sources
            .iter()
            .find(|s| s.host == sim.host)?
            .slurm
            .as_ref()
    }

    pub fn health(&self, sim: &Sim) -> Health {
        health(sim, Utc::now(), self.snap(sim), &self.cfg.health)
    }

    /// Indices into `sims` of the shown simulations, in display order
    pub fn visible(&self) -> Vec<(usize, Health)> {
        let filter = self.filter.to_lowercase();
        let mut v: Vec<(usize, Health)> = self
            .sims
            .iter()
            .enumerate()
            .map(|(i, s)| (i, self.health(s)))
            .filter(|(_, h)| !(self.hide_done && h.is_done()))
            .filter(|(i, _)| {
                filter.is_empty() || {
                    let s = &self.sims[*i];
                    s.display_name().to_lowercase().contains(&filter)
                        || s.location().to_lowercase().contains(&filter)
                        || s.st()
                            .group
                            .as_ref()
                            .is_some_and(|g| g.to_lowercase().contains(&filter))
                }
            })
            .collect();
        let sims = &self.sims;
        let name = |i: usize| sims[i].display_name().to_lowercase();
        let newest = |a: usize, b: usize| {
            sims[b]
                .sort_time()
                .cmp(&sims[a].sort_time())
                .then_with(|| name(a).cmp(&name(b)))
        };
        match self.sort {
            Sort::Newest => v.sort_by(|a, b| newest(a.0, b.0)),
            // Simulations without a group come last
            Sort::Group => v.sort_by(|a, b| {
                let g = |i: usize| (sims[i].st().group.is_none(), sims[i].st().group.clone());
                g(a.0).cmp(&g(b.0)).then_with(|| newest(a.0, b.0))
            }),
            Sort::Name => v.sort_by_key(|a| name(a.0)),
            Sort::Health => v.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| name(a.0).cmp(&name(b.0)))),
            Sort::Updated => v.sort_by(|a, b| {
                sims[b.0]
                    .last_update()
                    .cmp(&sims[a.0].last_update())
                    .then_with(|| name(a.0).cmp(&name(b.0)))
            }),
        }
        v
    }

    /// Position of the selected simulation within `visible`
    pub fn selected_pos(&self, visible: &[(usize, Health)]) -> Option<usize> {
        if visible.is_empty() {
            return None;
        }
        let pos = self
            .selected
            .as_ref()
            .and_then(|d| visible.iter().position(|(i, _)| self.sims[*i].is(d)));
        Some(pos.unwrap_or(0))
    }

    pub fn selected_sim(&self) -> Option<&Sim> {
        let vis = self.visible();
        let pos = self.selected_pos(&vis)?;
        Some(&self.sims[vis[pos].0])
    }

    fn move_selection(&mut self, delta: isize) {
        let vis = self.visible();
        let Some(pos) = self.selected_pos(&vis) else {
            return;
        };
        let new = (pos as isize + delta).clamp(0, vis.len() as isize - 1) as usize;
        let id = self.sims[vis[new].0].id();
        if self.detail && self.selected.as_ref() != Some(&id) {
            self.detail_scroll = 0;
            self.image_idx = 0;
            self.needs_clear = true;
        }
        self.selected = Some(id);
    }

    fn page(&self) -> isize {
        match self.view {
            View::List => 20,
            View::Cards => 3,
        }
    }

    fn notify(&mut self, msg: impl Into<String>) {
        self.notice = Some((msg.into(), Instant::now()));
    }

    fn manual(&mut self, monitor: &Monitor, r: Request) {
        let last = match r {
            Request::Reread => &mut self.last_reread,
            Request::Rescan => &mut self.last_rescan,
        };
        if last.is_some_and(|t| t.elapsed() < MANUAL_GAP) {
            self.notify("please wait a few seconds between refreshes");
            return;
        }
        *last = Some(Instant::now());
        monitor.request(r);
        self.notify(match r {
            Request::Reread => "re-reading status files",
            Request::Rescan => "scanning for new simulations",
        });
    }

    fn handle_key(&mut self, k: KeyEvent, monitor: &Monitor) {
        if k.kind == KeyEventKind::Release {
            return;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.editing_filter {
            match k.code {
                KeyCode::Enter => self.editing_filter = false,
                KeyCode::Esc => {
                    self.editing_filter = false;
                    self.filter.clear();
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c) if !ctrl => self.filter.push(c),
                _ => {}
            }
            return;
        }
        if self.help {
            // Any key closes the help
            self.help = false;
            self.needs_clear = true;
            if k.code != KeyCode::Char('q') {
                return;
            }
        }
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('l') if ctrl => self.needs_clear = true,
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.help = true;
                self.needs_clear = true;
            }
            KeyCode::Char('r') => self.manual(monitor, Request::Reread),
            KeyCode::Char('R') => self.manual(monitor, Request::Rescan),
            KeyCode::Char('c') => {
                if monitor.any_disconnected() {
                    self.reconnect = true;
                } else {
                    self.notify("all hosts are connected");
                }
            }
            KeyCode::Char('f') => {
                self.hide_done = !self.hide_done;
                self.notify(if self.hide_done {
                    "hiding finished and failed simulations"
                } else {
                    "showing all simulations"
                });
            }
            KeyCode::Char('s') => {
                self.sort = self.sort.next();
                self.notify(format!("sorted by {}", self.sort.label()));
            }
            KeyCode::Char('/') => {
                self.editing_filter = true;
                self.filter.clear();
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.detail = false;
                self.view = match self.view {
                    View::List => View::Cards,
                    View::Cards => View::List,
                };
                self.needs_clear = true;
            }
            KeyCode::Char('1') => self.set_view(View::List, false),
            KeyCode::Char('2') => self.set_view(View::Cards, false),
            KeyCode::Char('3') => self.set_view(self.view, true),
            KeyCode::Enter => self.set_view(self.view, true),
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Left if self.detail => {
                self.set_view(self.view, false)
            }
            _ if self.detail => self.handle_detail_key(k),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.move_selection(self.page()),
            KeyCode::PageUp => self.move_selection(-self.page()),
            KeyCode::Home | KeyCode::Char('g') => self.move_selection(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.move_selection(isize::MAX / 2),
            KeyCode::Right => self.set_view(self.view, true),
            _ => {}
        }
    }

    fn handle_detail_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.detail_scroll = self.detail_scroll.saturating_add(1)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1)
            }
            KeyCode::PageDown | KeyCode::Char(' ') => {
                self.detail_scroll = self.detail_scroll.saturating_add(20)
            }
            KeyCode::PageUp => self.detail_scroll = self.detail_scroll.saturating_sub(20),
            KeyCode::Home | KeyCode::Char('g') => self.detail_scroll = 0,
            KeyCode::Char('n') => self.move_selection(1),
            KeyCode::Char('p') => self.move_selection(-1),
            KeyCode::Char(']') => self.cycle_image(1),
            KeyCode::Char('[') => self.cycle_image(-1),
            _ => {}
        }
    }

    fn cycle_image(&mut self, delta: isize) {
        let n = self.selected_sim().map_or(0, |s| s.st().images.len());
        if n > 1 {
            self.image_idx = (self.image_idx as isize + delta).rem_euclid(n as isize) as usize;
            self.needs_clear = true;
        }
    }

    fn set_view(&mut self, view: View, detail: bool) {
        if detail && self.selected_sim().is_none() {
            return;
        }
        if self.view != view || self.detail != detail {
            self.needs_clear = true;
        }
        if detail && !self.detail {
            self.detail_scroll = 0;
            self.image_idx = 0;
            // Pin the selection so that it survives re-sorting
            self.selected = self.selected_sim().map(|s| s.id());
        }
        if !detail {
            // Only images of the simulation being looked at are kept
            self.images.clear();
        }
        self.view = view;
        self.detail = detail;
    }

    pub fn images_enabled(&self) -> bool {
        self.picker.is_some()
    }

    /// The image state for an image of a simulation, requesting it if needed
    pub fn image(
        &mut self,
        sim: SimId,
        version: Option<SystemTime>,
        file: &str,
    ) -> &mut ImageState {
        let key: ImageKey = (sim, file.to_string());
        let loader = self.loader.as_ref();
        let entry = self.images.entry(key.clone()).or_insert_with(|| {
            if let Some(l) = loader {
                l.request(key.clone());
            }
            ImageEntry {
                version,
                pending: true,
                state: ImageState::Loading,
            }
        });
        if entry.version != version && !entry.pending {
            if let Some(l) = loader {
                l.request(key);
            }
            entry.version = version;
            entry.pending = true;
        }
        &mut entry.state
    }

    fn receive_images(&mut self) {
        let Some(loader) = &self.loader else { return };
        let mut got = Vec::new();
        while let Ok(r) = loader.rx.try_recv() {
            got.push(r);
        }
        for (key, res) in got {
            let Some(entry) = self.images.get_mut(&key) else {
                continue;
            };
            entry.pending = false;
            entry.state = match (res, &self.picker) {
                (Ok(img), Some(p)) => {
                    // Images are padded to whole cells. Sixel has no transparency, so
                    // pad with the image's own background (its corner pixel).
                    let mut p = p.clone();
                    p.set_background_color(Some(image::GenericImageView::get_pixel(&img, 0, 0)));
                    ImageState::Ready(Box::new(p.new_resize_protocol(img)))
                }
                (Ok(_), None) => ImageState::Failed("images disabled".into()),
                (Err(e), _) => ImageState::Failed(e),
            };
            // A new image may be smaller than the old one, leaving pixels behind
            self.needs_clear = true;
        }
    }

    fn busy(&self) -> bool {
        self.sources.iter().any(Source::busy)
    }

    fn draw(&mut self, f: &mut Frame) {
        let [header, body, status] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(f.area());
        self.draw_header(f, header);
        if self.detail {
            detail::draw(self, f, body);
        } else {
            match self.view {
                View::List => list::draw(self, f, body),
                View::Cards => cards::draw(self, f, body),
            }
        }
        self.draw_status(f, status);
        if self.help {
            help::draw(f, f.area());
        }
    }

    fn draw_header(&self, f: &mut Frame, area: Rect) {
        let tab = |label: &str, on: bool| {
            if on {
                Span::styled(format!(" {label} "), Style::new().reversed().bold())
            } else {
                Span::raw(format!(" {label} "))
            }
        };
        let mut spans = vec![
            Span::styled(" SimWatch ", Style::new().bold().fg(Color::Cyan)),
            Span::raw(" "),
            tab("1 List", self.view == View::List && !self.detail),
            tab("2 Cards", self.view == View::Cards && !self.detail),
            tab("3 Detail", self.detail),
            Span::raw(format!("   sort: {}", self.sort.label())).dim(),
        ];
        if self.hide_done {
            spans.push(Span::raw("   hiding done").dim());
        }
        if self.editing_filter || !self.filter.is_empty() {
            spans.push(Span::raw("   filter: ").dim());
            spans.push(Span::styled(
                format!(
                    "{}{}",
                    self.filter,
                    if self.editing_filter { "▏" } else { "" }
                ),
                Style::new().fg(Color::Yellow),
            ));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_status(&self, f: &mut Frame, area: Rect) {
        let sep = || Span::raw(" │ ").dim();
        // Only the local directories: no need to name them
        let named = self.sources.len() > 1 || self.sources.iter().any(|s| s.host.is_some());
        let compact = self.sources.len() > 1;
        let mut spans = Vec::new();
        let mut warnings = Vec::new();
        for src in &self.sources {
            if !spans.is_empty() {
                spans.push(sep());
            }
            if named {
                spans.push(Span::raw(format!("{}: ", src.name())).bold());
            }
            if let Some(why) = &src.disconnected {
                spans.push(Span::styled(
                    format!("disconnected ({}), c reconnects", fmt::trunc(why, 40)),
                    Style::new().fg(Color::Red),
                ));
                continue;
            }
            if compact {
                spans.extend(source_compact(src));
            } else {
                spans.extend(source_full(src));
            }
            let prefix = if compact {
                format!("{}: ", src.name())
            } else {
                String::new()
            };
            warnings.extend(
                src.scan_warnings
                    .iter()
                    .chain(&src.warning)
                    .map(|w| format!("{prefix}{w}")),
            );
        }
        if !warnings.is_empty() {
            spans.push(sep());
            spans.push(Span::styled(
                fmt::trunc(&warnings.join("; "), 60),
                Style::new().fg(Color::Yellow),
            ));
        }
        let vis = self.visible().len();
        let total = self.sims.len();
        let mut right = if vis == total {
            format!("{total} sims")
        } else {
            format!("{vis}/{total} sims")
        };
        if let Some((msg, t)) = &self.notice
            && t.elapsed() < Duration::from_secs(3)
        {
            right = format!("{msg} │ {right}");
        }
        right.push_str(" │ ? help ");
        let [l, r] = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Length(right.chars().count() as u16),
        ])
        .areas(area);
        f.render_widget(Paragraph::new(Line::from(spans)), l);
        f.render_widget(Paragraph::new(right).dim(), r);
    }
}

static START: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);

fn spinner() -> &'static str {
    SPINNER[(START.elapsed().as_millis() / 100) as usize % SPINNER.len()]
}

/// Activity of one source: spinners while busy, else how long ago
fn source_full(src: &Source) -> Vec<Span<'static>> {
    let spin = spinner();
    let activity = |name: &str, a: &Activity| -> Span<'static> {
        match (a.busy, a.done) {
            (Some(t), _) if name == "scan" && t.elapsed() > SLOW_SCAN => Span::styled(
                format!(
                    "{spin} {name} slow ({})",
                    fmt::age(t.elapsed().as_secs_f64())
                ),
                Style::new().fg(Color::Yellow).bold(),
            ),
            (Some(_), _) => Span::styled(
                format!("{spin} {name}"),
                Style::new().fg(Color::Cyan).bold(),
            ),
            (None, Some(t)) => Span::raw(format!(
                "{name} {} ago",
                fmt::age(t.elapsed().as_secs_f64())
            ))
            .dim(),
            (None, None) => Span::raw(format!("{name} –")).dim(),
        }
    };
    let sep = || Span::raw(" │ ").dim();
    let mut spans = vec![
        activity("scan", &src.scan),
        sep(),
        activity("read", &src.read),
        sep(),
    ];
    if let Some(why) = &src.slurm_disabled {
        spans.push(Span::raw(format!("squeue off ({why})")).dim());
    } else {
        spans.push(activity("squeue", &src.squeue));
        if let Some(s) = &src.slurm {
            spans.push(
                Span::raw(format!(
                    ": {} R, {} PD",
                    s.count("RUNNING"),
                    s.count("PENDING")
                ))
                .dim(),
            );
        }
        if let Some(e) = &src.slurm_error {
            spans.push(Span::styled(format!(" ({e})"), Style::new().fg(Color::Red)));
        }
    }
    spans
}

/// Activity of one of several sources, in a few characters
fn source_compact(src: &Source) -> Vec<Span<'static>> {
    let busy: Vec<&str> = [
        ("scan", &src.scan),
        ("read", &src.read),
        ("squeue", &src.squeue),
    ]
    .into_iter()
    .filter(|(_, a)| a.busy.is_some())
    .map(|(n, _)| n)
    .collect();
    let mut spans = vec![if !busy.is_empty() {
        Span::styled(
            format!("{} {}", spinner(), busy.join(" ")),
            Style::new().fg(Color::Cyan).bold(),
        )
    } else {
        match src.read.done {
            Some(t) => Span::raw(format!("read {} ago", fmt::age(t.elapsed().as_secs_f64()))).dim(),
            None => Span::raw("–").dim(),
        }
    }];
    if let Some(s) = &src.slurm {
        spans.push(
            Span::raw(format!(
                ", {} R {} PD",
                s.count("RUNNING"),
                s.count("PENDING")
            ))
            .dim(),
        );
    }
    if src.slurm_error.is_some() {
        spans.push(Span::styled(
            " (squeue failed)",
            Style::new().fg(Color::Red),
        ));
    }
    spans
}

pub fn health_style(h: Health) -> Style {
    match h {
        Health::Running => Style::new().fg(Color::Green),
        Health::Queued => Style::new().fg(Color::Blue),
        Health::Stale => Style::new().fg(Color::Yellow),
        Health::Lost | Health::Failed | Health::Unreadable => {
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)
        }
        Health::Stopped => Style::new().fg(Color::Magenta),
        Health::Finished => Style::new().fg(Color::DarkGray),
        Health::Unknown => Style::new().fg(Color::Gray),
    }
}

/// Erase the screen (including any sixel images) and make the next draw
/// repaint every cell. Unlike `Terminal::clear`, this does not ask the
/// terminal for the cursor position, which would cost a round trip over ssh.
fn full_clear(terminal: &mut DefaultTerminal) -> std::io::Result<()> {
    use ratatui::backend::Backend;
    terminal.backend_mut().clear()?;
    // Resets the "previous" buffer, so that the next diff covers everything
    terminal.swap_buffers();
    Ok(())
}

/// Give the terminal back for ssh to ask for passwords, reconnect lost hosts,
/// and take the terminal again
fn reconnect(terminal: &mut DefaultTerminal, monitor: &Monitor) -> Result<()> {
    use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
    TUI_ACTIVE.store(false, Ordering::Relaxed);
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        std::io::stdout(),
        LeaveAlternateScreen,
        crossterm::cursor::Show
    )?;
    eprintln!("simwatch: reconnecting (Ctrl-C quits)");
    monitor.reconnect();
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(std::io::stdout(), EnterAlternateScreen)?;
    TUI_ACTIVE.store(true, Ordering::Relaxed);
    terminal.hide_cursor()?;
    full_clear(terminal)?;
    Ok(())
}

/// Run the interactive UI until the user quits
pub fn run(
    terminal: &mut DefaultTerminal,
    mut app: App,
    rx: Receiver<Tagged>,
    monitor: Monitor,
) -> Result<()> {
    let _ = *START;
    let mut last_draw = Instant::now() - Duration::from_secs(10);
    let mut dirty = true;
    loop {
        loop {
            match rx.try_recv() {
                Ok(u) => {
                    app.apply(u);
                    dirty = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => anyhow::bail!("background worker stopped"),
            }
        }
        app.receive_images();

        // Animate spinners while busy; otherwise update ages once per second
        let tick = if app.busy() {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(1)
        };
        if dirty || app.needs_clear || last_draw.elapsed() >= tick {
            if app.needs_clear {
                full_clear(terminal)?;
                app.needs_clear = false;
            }
            terminal.draw(|f| app.draw(f))?;
            last_draw = Instant::now();
            dirty = false;
        }

        let timeout = tick
            .saturating_sub(last_draw.elapsed())
            .min(Duration::from_millis(100));
        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(k) => app.handle_key(k, &monitor),
                Event::Resize(_, _) => app.needs_clear = true,
                _ => {}
            }
            dirty = true;
        }
        if app.reconnect {
            app.reconnect = false;
            reconnect(terminal, &monitor)?;
        }
        if app.quit {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::parse_status;
    use image::{Rgb, RgbImage};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui_image::picker::ProtocolType;
    use std::path::Path;

    fn sim(dir: &Path, text: &str) -> Sim {
        let mut s = Sim::new(dir.to_path_buf());
        s.status = Some(parse_status(text).unwrap());
        s
    }

    fn render(app: &mut App) -> String {
        let mut term = Terminal::new(TestBackend::new(140, 40)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let buf = term.backend().buffer();
        let mut s = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                s.push_str(buf[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    fn app(dir: &Path, picker: Option<Picker>) -> App {
        let mut app = App::new(
            Config::default(),
            picker,
            Box::new(|_, _, _| Err("no".into())),
        );
        let now = Utc::now().to_rfc3339();
        app.sims = vec![
            sim(
                &dir.join("a"),
                &format!(
                    r#"name = "bbh-q1"
status = "running"
updated = "{now}"
message = "all horizons found"
mystery = 42
[progress]
iteration = 1234
time = 50.0
time_end = 200.0
time_unit = "M"
speed = 100.0
[[black_holes]]
name = "BH1"
mass = 0.5
spin = [0, 0, 0.6]
[[images]]
file = "track.png"
title = "Tracks"
"#
                ),
            ),
            sim(&dir.join("b"), "status = \"finished\"\niteration = 7"),
        ];
        app.sources[0].scan.finish();
        app.sources[0].read.finish();
        app
    }

    #[test]
    fn views() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app(tmp.path(), None);

        let list = render(&mut app);
        assert!(list.contains("bbh-q1"), "{list}");
        assert!(list.contains("running"));
        assert!(list.contains("50/200 M"));
        assert!(list.contains("25%"));
        assert!(list.contains("(b)"));
        assert!(list.contains("finished"));
        assert!(list.contains("2 sims"));

        app.view = View::Cards;
        let cards = render(&mut app);
        assert!(cards.contains("all horizons found"), "{cards}");
        assert!(cards.contains("BH1 M=0.5 χ=0.6"));
        assert!(cards.contains("mystery=42"));

        app.view = View::List;
        app.selected = Some((None, tmp.path().join("b")));
        app.set_view(View::List, true);
        let detail = render(&mut app);
        assert!(detail.contains("(missing simulation name)"), "{detail}");
        assert!(detail.contains("iteration"));

        app.hide_done = true;
        app.set_view(View::List, false);
        let list = render(&mut app);
        assert!(!list.contains("(b)"));
        assert!(list.contains("1/2 sims"));

        app.help = true;
        assert!(render(&mut app).contains("scan for new simulations now"));
    }

    #[test]
    fn group_summary_history() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app(tmp.path(), None);
        // Without groups or summaries there are no such columns
        let list = render(&mut app);
        assert!(
            !list.contains("Group") && !list.contains("Summary"),
            "{list}"
        );

        app.sims.push(sim(
            &tmp.path().join("c"),
            r#"name = "row-3"
group = "octant"
summary = ["shells.r2.ham_l2"]
[shells.r2]
ham_l2 = 4e-6
[slurm]
job_id = 30
previous_job_ids = [10, 20]
[history]
time = [1, 2, 3, 4]
"shells.r2.ham_l2" = [1e-6, 2e-6, 3e-6, 4e-6]
"#,
        ));
        let list = render(&mut app);
        assert!(list.contains("Group") && list.contains("Summary"), "{list}");
        assert!(list.contains("octant"));
        assert!(list.contains("ham_l2 4e-6↑"), "{list}");
        assert!(list.contains("30 #3"), "{list}");

        app.view = View::Cards;
        let cards = render(&mut app);
        assert!(cards.contains("· octant"), "{cards}");
        assert!(cards.contains("ham_l2 4e-6 ▁"), "{cards}");

        app.view = View::List;
        app.selected = Some((None, tmp.path().join("c")));
        app.set_view(View::List, true);
        let detail = render(&mut app);
        assert!(detail.contains("Summary"), "{detail}");
        assert!(detail.contains("History"));
        assert!(detail.contains("rate +"), "{detail}");
        assert!(detail.contains("Earlier jobs    10, 20"), "{detail}");
        assert!(detail.contains("this is job 3"));

        // The group sorts and filters
        app.set_view(View::List, false);
        app.sort = Sort::Group;
        let vis = app.visible();
        assert_eq!(app.sims[vis[0].0].st().group.as_deref(), Some("octant"));
        app.filter = "octa".into();
        assert_eq!(app.visible().len(), 1);
    }

    #[test]
    fn sixel_image() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("a")).unwrap();
        RgbImage::from_pixel(80, 60, Rgb([255, 128, 0]))
            .save(tmp.path().join("a/track.png"))
            .unwrap();
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Sixel);
        let mut app = app(tmp.path(), Some(picker));
        app.selected = Some((None, tmp.path().join("a")));
        app.set_view(View::List, true);

        let first = render(&mut app);
        assert!(first.contains("1/1: Tracks"), "{first}");
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut screen = first;
        while !screen.contains("\x1bP") && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            app.receive_images();
            screen = render(&mut app);
        }
        assert!(screen.contains("\x1bP"), "no sixel sequence in {screen:?}");
    }
}
