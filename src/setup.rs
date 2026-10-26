//! The launcher `swarm` opens when you don't pass a task: what to build, where, who builds it,
//! how, and how much it may spend. It remembers your last squad and your recent projects.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use serde_json::{json, Value};

use crate::fx::{self, dim, hsv, lerp};
use crate::orchestrator::{self, codex_ready, home, name_word, tilde, State};
use crate::providers::{AgentSpec, Provider};
use crate::ui::{bold, centered, put, shimmer, span, st, trunc, AMBER, CYAN, FAINT, GOLD, GREEN, MUTED, NEON, RED, SOFT, TEXT, WHITE};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Open {
    Live,
    End,
    Never,
}

#[derive(Clone, Debug)]
pub struct Setup {
    pub task: String,
    pub dir: String,
    dir_edited: bool,
    pub lead: Provider,
    pub lead_model: String,
    /// Indexed like `Provider::ALL`: claude, antigravity, codex.
    pub counts: [usize; 3],
    pub models: [String; 3],
    pub review: bool,
    pub open: Open,
    pub budget: f64,
    pub demo: bool,
}

pub enum Choice {
    Launch(Setup),
    Resume(PathBuf),
    Quit,
}

const BUDGETS: &[f64] = &[0.0, 0.5, 1.0, 2.0, 3.0, 5.0, 10.0, 20.0, 50.0, 100.0];
const MAX_PER_TYPE: usize = 8;

const IDEAS: &[&str] = &[
    "landing page for a neon late-night ramen bar in Tokyo",
    "portfolio for a tattoo artist in Berlin who does fine-line botanicals",
    "launch page for a mechanical keyboard with hot-swappable switches",
    "event page for an underground synthwave festival on a rooftop",
    "SaaS pricing page for an AI meeting-notes app, three tiers and a FAQ",
    "menu and booking page for a specialty coffee roaster",
    "personal site for an indie game developer, with a devlog section",
    "conference page for a two-day Rust meetup in Istanbul",
    "product page for a minimalist titanium wallet",
    "landing page for a climbing gym with class schedule and pricing",
];

impl Setup {
    fn i(p: Provider) -> usize {
        Provider::ALL.iter().position(|x| *x == p).unwrap_or(0)
    }

    pub fn count(&self, p: Provider) -> usize {
        self.counts[Self::i(p)]
    }

    pub fn model(&self, p: Provider) -> &str {
        &self.models[Self::i(p)]
    }

    pub fn roster(&self) -> Vec<AgentSpec> {
        let mut v = vec![AgentSpec { name: name_word(self.lead, &self.lead_model), provider: self.lead, model: self.lead_model.clone() }];
        for p in Provider::ALL {
            let word = name_word(p, self.model(p));
            for k in 1..=self.count(p) {
                v.push(AgentSpec { name: format!("{word}-{k}"), provider: p, model: self.model(p).to_string() });
            }
        }
        v
    }

    pub fn dir_path(&self) -> PathBuf {
        expand(self.dir.trim())
    }

    fn workers(&self) -> usize {
        self.counts.iter().sum()
    }

    /// Very rough Claude spend for one full build. Antigravity and Codex don't report cost.
    fn estimate(&self) -> f64 {
        let per = |m: &str| match orchestrator::model_word(m).as_str() {
            "fable" => 2.4,
            "opus" => 1.1,
            "haiku" => 0.08,
            _ => 0.35,
        };
        let lead = if self.lead == Provider::Claude { per(&self.lead_model) * 1.6 } else { 0.0 };
        lead + self.count(Provider::Claude) as f64 * per(self.model(Provider::Claude))
    }

    fn sync_dir(&mut self) {
        if !self.dir_edited {
            let t = if self.task.trim().is_empty() { "page" } else { self.task.trim() };
            self.dir = tilde(&orchestrator::default_dir(t));
        }
    }
}

pub fn expand(s: &str) -> PathBuf {
    match s.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None if s == "~" => home(),
        None => PathBuf::from(s),
    }
}

// ── remembered settings and recent projects ───────────────────────────────────────────────

fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config")).join("swarm")
}

pub fn load_prefs() -> Setup {
    let mut s = Setup {
        task: String::new(),
        dir: String::new(),
        dir_edited: false,
        lead: Provider::Claude,
        lead_model: "opus".into(),
        counts: [4, 2, if codex_ready() { 1 } else { 0 }],
        models: Provider::ALL.map(|p| p.models().remove(0)),
        review: true,
        open: Open::End,
        budget: 0.0,
        demo: false,
    };
    if let Some(v) = std::fs::read_to_string(config_dir().join("prefs.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
        if let Some(p) = v["lead"].as_str().and_then(Provider::parse) {
            s.lead = p;
        }
        if let Some(m) = v["lead_model"].as_str() {
            s.lead_model = m.into();
        }
        for (k, p) in Provider::ALL.iter().enumerate() {
            if let Some(n) = v["counts"][p.key()].as_u64() {
                s.counts[k] = (n as usize).min(MAX_PER_TYPE);
            }
            if let Some(m) = v["models"][p.key()].as_str() {
                s.models[k] = m.into();
            }
        }
        s.review = v["review"].as_bool().unwrap_or(true);
        s.open = match v["open"].as_str() {
            Some("live") => Open::Live,
            Some("never") => Open::Never,
            _ => Open::End,
        };
        s.budget = v["budget"].as_f64().unwrap_or(0.0);
    }
    s.sync_dir();
    s
}

pub fn save_prefs(s: &Setup) {
    let mut counts = serde_json::Map::new();
    let mut models = serde_json::Map::new();
    for p in Provider::ALL {
        counts.insert(p.key().into(), json!(s.count(p)));
        models.insert(p.key().into(), json!(s.model(p)));
    }
    let v = json!({
        "lead": s.lead.key(), "lead_model": s.lead_model, "counts": counts, "models": models,
        "review": s.review, "budget": s.budget,
        "open": match s.open { Open::Live => "live", Open::End => "end", Open::Never => "never" },
    });
    let _ = std::fs::create_dir_all(config_dir());
    let _ = std::fs::write(config_dir().join("prefs.json"), serde_json::to_string_pretty(&v).unwrap_or_default());
}

fn recent_dirs() -> Vec<PathBuf> {
    std::fs::read_to_string(config_dir().join("recent.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok())
        .unwrap_or_default()
        .into_iter()
        .map(PathBuf::from)
        .collect()
}

/// Put a project at the top of the recent list.
pub fn remember(dir: &Path) {
    let mut v: Vec<String> = recent_dirs().into_iter().map(|p| p.to_string_lossy().to_string()).collect();
    let d = dir.to_string_lossy().to_string();
    v.retain(|x| *x != d);
    v.insert(0, d);
    v.truncate(12);
    let _ = std::fs::create_dir_all(config_dir());
    let _ = std::fs::write(config_dir().join("recent.json"), serde_json::to_string_pretty(&v).unwrap_or_default());
}

/// The most recently used project that can still be resumed.
pub fn latest_project() -> Option<PathBuf> {
    recent_dirs().into_iter().find(|d| State::path(d).exists())
}

struct Recent {
    dir: PathBuf,
    title: String,
    round: u32,
    agents: usize,
    age: String,
}

fn ago(t: SystemTime) -> String {
    let s = SystemTime::now().duration_since(t).unwrap_or_default().as_secs();
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86_399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86_400),
    }
}

fn load_recent() -> Vec<Recent> {
    let mut dirs = recent_dirs();
    // a swarm project in the current folder counts too
    if let Ok(cwd) = std::env::current_dir() {
        if State::path(&cwd).exists() && !dirs.contains(&cwd) {
            dirs.insert(0, cwd);
        }
    }
    dirs.into_iter()
        .filter_map(|dir| {
            let st = State::load(&dir)?;
            let age = std::fs::metadata(State::path(&dir)).and_then(|m| m.modified()).map(ago).unwrap_or_default();
            let title = if st.title.is_empty() { st.task.clone() } else { st.title.clone() };
            Some(Recent { title, round: st.round.saturating_sub(1), agents: st.agents.len(), age, dir })
        })
        .take(6)
        .collect()
}

// ── the screen ──────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Field {
    Task,
    Dir,
    Lead,
    Team(usize),
    Review,
    Open,
    Budget,
    Mode,
    Launch,
    Recent(usize),
}

struct Wizard {
    s: Setup,
    focus: usize,
    cursor: usize,
    recent: Vec<Recent>,
    checks: [Option<Option<String>>; 3],
    check_rx: mpsc::Receiver<(Provider, Option<String>)>,
    err: Option<(String, Instant)>,
    start: Instant,
    frame: u64,
    idea: usize,
}

impl Wizard {
    fn fields(&self) -> Vec<Field> {
        let mut v = vec![
            Field::Task,
            Field::Dir,
            Field::Lead,
            Field::Team(0),
            Field::Team(1),
            Field::Team(2),
            Field::Review,
            Field::Open,
            Field::Budget,
            Field::Mode,
            Field::Launch,
        ];
        v.extend((0..self.recent.len()).map(Field::Recent));
        v
    }

    fn field(&self) -> Field {
        let f = self.fields();
        f[self.focus.min(f.len() - 1)]
    }

    fn text(&mut self) -> Option<&mut String> {
        match self.field() {
            Field::Task => Some(&mut self.s.task),
            Field::Dir => Some(&mut self.s.dir),
            _ => None,
        }
    }

    fn move_focus(&mut self, d: i32) {
        let n = self.fields().len() as i32;
        self.focus = ((self.focus as i32 + d).rem_euclid(n)) as usize;
        self.cursor = match self.field() {
            Field::Task => self.s.task.chars().count(),
            Field::Dir => self.s.dir.chars().count(),
            _ => 0,
        };
    }

    fn fail(&mut self, m: impl Into<String>) {
        self.err = Some((m.into(), Instant::now()));
    }

    fn check(&self, p: Provider) -> Option<&Option<String>> {
        self.checks[Setup::i(p)].as_ref()
    }

    fn validate(&mut self) -> bool {
        if self.s.demo {
            if self.s.task.trim().is_empty() {
                self.s.task = crate::demo::TASK.into();
                self.s.sync_dir();
            }
            if self.s.workers() == 0 {
                self.fail("the demo needs at least one worker");
                return false;
            }
            return true;
        }
        if self.s.task.trim().is_empty() {
            self.fail("tell the swarm what to build first");
            self.focus = 0;
            self.cursor = 0;
            return false;
        }
        if self.s.dir.trim().is_empty() {
            self.fail("pick a folder for the project");
            self.focus = 1;
            return false;
        }
        let mut used = vec![self.s.lead];
        used.extend(Provider::ALL.iter().copied().filter(|p| self.s.count(*p) > 0));
        for p in used {
            if let Some(None) = self.check(p) {
                self.fail(format!("{} isn't installed (no `{}` on PATH)", p.label(), p.bin()));
                return false;
            }
            if p == Provider::Codex && !codex_ready() {
                self.fail("codex isn't logged in · run `codex login` first");
                return false;
            }
        }
        true
    }

    fn change(&mut self, d: i32) {
        let f = self.field();
        let s = &mut self.s;
        match f {
            Field::Lead => {
                let k = Setup::i(s.lead) as i32;
                s.lead = Provider::ALL[(k + d).rem_euclid(3) as usize];
                s.lead_model = if s.lead == Provider::Claude { "opus".into() } else { s.lead.models().remove(0) };
            }
            Field::Team(k) => s.counts[k] = (s.counts[k] as i32 + d).clamp(0, MAX_PER_TYPE as i32) as usize,
            Field::Review => s.review = !s.review,
            Field::Open => {
                let all = [Open::Live, Open::End, Open::Never];
                let k = all.iter().position(|o| *o == s.open).unwrap_or(1) as i32;
                s.open = all[(k + d).rem_euclid(3) as usize];
            }
            Field::Budget => {
                let k = BUDGETS.iter().position(|b| (*b - s.budget).abs() < 1e-9).unwrap_or(0) as i32;
                s.budget = BUDGETS[(k + d).clamp(0, BUDGETS.len() as i32 - 1) as usize];
            }
            Field::Mode => s.demo = !s.demo,
            _ => {}
        }
    }

    fn cycle_model(&mut self, d: i32) {
        match self.field() {
            Field::Lead => {
                let ms = self.s.lead.models();
                let k = ms.iter().position(|m| *m == self.s.lead_model).map(|k| k as i32).unwrap_or(-1);
                self.s.lead_model = ms[(k + d).rem_euclid(ms.len() as i32) as usize].clone();
            }
            Field::Team(i) => {
                let ms = Provider::ALL[i].models();
                let k = ms.iter().position(|m| *m == self.s.models[i]).map(|k| k as i32).unwrap_or(-1);
                self.s.models[i] = ms[(k + d).rem_euclid(ms.len() as i32) as usize].clone();
            }
            _ => {}
        }
    }

    fn byte(s: &str, c: usize) -> usize {
        s.char_indices().nth(c).map(|(b, _)| b).unwrap_or(s.len())
    }

    fn insert(&mut self, t: &str) {
        let c = self.cursor;
        let is_task = self.field() == Field::Task;
        let Some(s) = self.text() else { return };
        let t: String = t.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        let b = Self::byte(s, c);
        s.insert_str(b, &t);
        self.cursor += t.chars().count();
        if is_task {
            self.s.sync_dir();
        } else {
            self.s.dir_edited = true;
        }
    }

    /// Returns Some when the launcher is done.
    fn key(&mut self, k: KeyEvent) -> Option<Choice> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let f = self.field();
        let is_text = matches!(f, Field::Task | Field::Dir);
        match (k.code, ctrl) {
            (KeyCode::Char('c'), true) | (KeyCode::Char('q'), true) | (KeyCode::Esc, _) => return Some(Choice::Quit),
            (KeyCode::Char('g'), true) | (KeyCode::F(5), _) => {
                if self.validate() {
                    return Some(Choice::Launch(self.s.clone()));
                }
            }
            (KeyCode::Char('r'), true) => {
                self.idea = (self.idea + 1 + (self.frame as usize % 3)) % IDEAS.len();
                self.s.task = IDEAS[self.idea].to_string();
                self.s.sync_dir();
                self.focus = 0;
                self.cursor = self.s.task.chars().count();
            }
            (KeyCode::Char('u'), true) if is_text => {
                if let Some(s) = self.text() {
                    s.clear();
                }
                self.cursor = 0;
                if f == Field::Task {
                    self.s.sync_dir();
                }
            }
            (KeyCode::Up, _) | (KeyCode::BackTab, _) => self.move_focus(-1),
            (KeyCode::Down, _) | (KeyCode::Tab, _) => self.move_focus(1),
            (KeyCode::Enter, _) => match f {
                Field::Launch => {
                    if self.validate() {
                        return Some(Choice::Launch(self.s.clone()));
                    }
                }
                Field::Recent(i) => return Some(Choice::Resume(self.recent[i].dir.clone())),
                Field::Review | Field::Mode => self.change(1),
                _ => self.move_focus(1),
            },
            (KeyCode::Left, _) if is_text => self.cursor = self.cursor.saturating_sub(1),
            (KeyCode::Right, _) if is_text => {
                let n = self.text().map(|s| s.chars().count()).unwrap_or(0);
                self.cursor = (self.cursor + 1).min(n);
            }
            (KeyCode::Home, _) if is_text => self.cursor = 0,
            (KeyCode::End, _) if is_text => self.cursor = self.text().map(|s| s.chars().count()).unwrap_or(0),
            (KeyCode::Left, _) => self.change(-1),
            (KeyCode::Right, _) => self.change(1),
            (KeyCode::Backspace, _) if is_text => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    let c = self.cursor;
                    if let Some(s) = self.text() {
                        let b = Self::byte(s, c);
                        s.remove(b);
                    }
                    if f == Field::Task {
                        self.s.sync_dir();
                    } else {
                        self.s.dir_edited = true;
                    }
                }
            }
            (KeyCode::Delete, _) if is_text => {
                let c = self.cursor;
                if let Some(s) = self.text() {
                    if c < s.chars().count() {
                        let b = Self::byte(s, c);
                        s.remove(b);
                    }
                }
            }
            (KeyCode::Char(' '), false) if !is_text => match f {
                Field::Lead | Field::Team(_) => self.cycle_model(1),
                _ => self.change(1),
            },
            (KeyCode::Char('m'), false) if !is_text => self.cycle_model(1),
            (KeyCode::Char('M'), _) if !is_text => self.cycle_model(-1),
            (KeyCode::Char(c), false) if is_text => {
                let mut b = [0u8; 4];
                self.insert(c.encode_utf8(&mut b));
            }
            (KeyCode::Char(c), false) if c.is_ascii_digit() => {
                if let Field::Team(i) = f {
                    self.s.counts[i] = (c as usize - '0' as usize).min(MAX_PER_TYPE);
                }
            }
            _ => {}
        }
        None
    }
}

pub fn run(term: &mut DefaultTerminal, initial: Setup) -> std::io::Result<Choice> {
    let (tx, rx) = mpsc::channel();
    for p in Provider::ALL {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send((p, orchestrator::version(p)));
        });
    }
    let mut w = Wizard {
        cursor: initial.task.chars().count(),
        s: initial,
        focus: 0,
        recent: load_recent(),
        checks: [None, None, None],
        check_rx: rx,
        err: None,
        start: Instant::now(),
        frame: 0,
        idea: (SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as usize) % IDEAS.len(),
    };
    let frame = Duration::from_millis(33);
    loop {
        let started = Instant::now();
        while let Ok((p, v)) = w.check_rx.try_recv() {
            w.checks[Setup::i(p)] = Some(v);
        }
        w.frame += 1;
        if w.err.as_ref().is_some_and(|(_, at)| at.elapsed().as_secs_f32() > 5.0) {
            w.err = None;
        }
        term.draw(|f| draw(f, &w))?;
        if event::poll(frame.saturating_sub(started.elapsed()))? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    if let Some(c) = w.key(k) {
                        return Ok(c);
                    }
                }
                Event::Paste(s) => w.insert(&s),
                _ => {}
            }
        }
    }
}

// ── drawing ─────────────────────────────────────────────────────────────────────────────

const LABEL_W: usize = 15;

fn chip(s: impl Into<String>, fg: Color, bg: Color) -> Span<'static> {
    Span::styled(s.into(), Style::new().fg(fg).bg(bg).add_modifier(Modifier::BOLD))
}

fn provider_color(p: Provider) -> Color {
    match p {
        Provider::Claude => Color::Rgb(255, 140, 90),
        Provider::Agy => Color::Rgb(110, 170, 255),
        Provider::Codex => Color::Rgb(90, 230, 150),
    }
}

fn draw(f: &mut Frame, w: &Wizard) {
    let area = f.area();
    let t = w.start.elapsed().as_secs_f32();
    let big = area.height >= 38 && area.width >= 72;
    let head_h = if big { 9 } else { 2 };
    // the form is ~32 rows; don't stretch it down an empty tall terminal
    let body_h = area.height.saturating_sub(head_h + 2).min(30);
    let [head, body, foot, _] =
        Layout::vertical([Constraint::Length(head_h), Constraint::Length(body_h), Constraint::Length(2), Constraint::Min(0)]).areas(area);

    draw_head(f, w, head, big, t);

    let wide = body.width >= 112;
    let (form, side) = if wide {
        let [a, b] = Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(body);
        (a, Some(b))
    } else {
        (body, None)
    };
    draw_form(f, w, form, t);
    if let Some(s) = side {
        draw_side(f, w, s, t);
    }

    // footer: error or key hints
    let line = match &w.err {
        Some((m, _)) => Line::from(vec![bold(" ✖ ", RED), bold(m.clone(), RED)]),
        None => {
            let k = |s: &str| bold(s.to_string(), GOLD);
            let d = |s: &str| span(s.to_string(), MUTED);
            Line::from(vec![
                k(" ↑↓"),
                d(" move  "),
                k("←→"),
                d(" change  "),
                k("space"),
                d(" model  "),
                k("ctrl-r"),
                d(" random idea  "),
                k("ctrl-g"),
                d(" launch  "),
                k("esc"),
                d(" quit"),
            ])
        }
    };
    f.render_widget(Paragraph::new(line).centered(), Rect { y: foot.y + 1, height: 1, ..foot });
}

fn draw_head(f: &mut Frame, w: &Wizard, area: Rect, big: bool, t: f32) {
    let buf = f.buffer_mut();
    if !big {
        let mut s = vec![];
        for (i, ch) in " ◢◤ S W A R M ◢◤ ".chars().enumerate() {
            s.push(bold(ch.to_string(), hsv(i as f32 * 12.0 + t * 90.0, 0.7, 1.0)));
        }
        s.push(span("  launch a swarm of coding agents", MUTED));
        centered(buf, area, area.y, &Line::from(s));
        return;
    }
    let logo = fx::banner("SWARM");
    let lw = logo.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
    let x0 = area.x + area.width.saturating_sub(lw) / 2;
    let y0 = area.y + 1;
    for (r, line) in logo.iter().enumerate() {
        for (c, ch) in line.chars().enumerate() {
            if ch == ' ' {
                continue;
            }
            let reveal = 0.05 + fx::hash(r as u32 + 1, c as u32 + 1) * 0.6;
            let (x, y) = (x0 + c as u16, y0 + r as u16);
            if t >= reveal {
                // a bright band sweeps across the letters every few seconds
                let sweep = ((t * 0.35).fract() * (lw as f32 + 40.0)) - 20.0;
                let near = (1.0 - ((c as f32 - sweep).abs() / 6.0)).max(0.0);
                let col = lerp(hsv(c as f32 * 5.0 + t * 60.0, 0.65, 1.0), WHITE, near * 0.8);
                put(buf, x, y, ch, col);
            } else {
                let gi = (fx::hash(c as u32, w.frame as u32) * fx::GLYPHS.len() as f32) as usize % fx::GLYPHS.len();
                put(buf, x, y, fx::GLYPHS[gi], dim(CYAN, 0.6));
            }
        }
    }
    let sub = "multi-agent web forge  ·  claude × codex × antigravity  ·  what are we building?";
    let shown = ((t - 0.4).max(0.0) * 80.0) as usize;
    centered(buf, area, y0 + 7, &Line::from(span(sub.chars().take(shown).collect::<String>(), MUTED)));
}

fn section(title: &str, num: &str, w: usize, active: bool, t: f32) -> Line<'static> {
    let c = if active { lerp(NEON, CYAN, 0.5 + 0.5 * (t * 2.0).sin()) } else { FAINT };
    let label = format!(" {num}  {title} ");
    let rest = w.saturating_sub(label.chars().count() + 2);
    Line::from(vec![span("━━", c), bold(label, if active { TEXT } else { MUTED }), span("━".repeat(rest), dim(c, 0.6))])
}

/// One form row: focus marker, label, then the value spans.
fn row(label: &str, focused: bool, t: f32, value: Vec<Span<'static>>) -> Line<'static> {
    let marker = if focused { bold("▌", lerp(NEON, GOLD, 0.5 + 0.5 * (t * 5.0).sin())) } else { span(" ", FAINT) };
    let lab = format!(" {:<w$}", label, w = LABEL_W);
    let mut v = vec![marker, if focused { bold(lab, WHITE) } else { span(lab, MUTED) }];
    v.extend(value);
    Line::from(v)
}

fn arrows(focused: bool, inner: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let c = if focused { GOLD } else { FAINT };
    let mut v = vec![bold("◂ ", c)];
    v.extend(inner);
    v.push(bold(" ▸", c));
    v
}

/// Text field with a blinking cursor, scrolled so the cursor stays visible.
fn text_field(s: &str, cursor: usize, focused: bool, width: usize, placeholder: &str, frame: u64) -> Vec<Line<'static>> {
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() {
        let mut v = vec![span("› ", if focused { CYAN } else { FAINT })];
        if focused && (frame / 15).is_multiple_of(2) {
            v.push(Span::styled("▌", st(CYAN)));
        }
        v.push(Span::styled(placeholder.to_string(), st(FAINT).add_modifier(Modifier::ITALIC)));
        return vec![Line::from(v)];
    }
    let w = width.saturating_sub(3).max(8);
    let mut lines = Vec::new();
    let rows: Vec<&[char]> = chars.chunks(w).collect();
    let cur_row = cursor / w;
    let total_rows = rows.len().max(cur_row + 1);
    for r in 0..total_rows {
        let seg: &[char] = rows.get(r).copied().unwrap_or(&[]);
        let mut v = vec![span(if r == 0 { "› " } else { "  " }, if focused { CYAN } else { FAINT })];
        let col = if r == cur_row && focused { Some(cursor - r * w) } else { None };
        let text_c = if focused { WHITE } else { SOFT };
        match col {
            Some(cx) => {
                let before: String = seg.iter().take(cx).collect();
                let at: String = seg.get(cx).map(|c| c.to_string()).unwrap_or_else(|| " ".into());
                let after: String = seg.iter().skip(cx + 1).collect();
                v.push(span(before, text_c));
                let blink = (frame / 15).is_multiple_of(2);
                v.push(Span::styled(at, if blink { Style::new().fg(Color::Black).bg(CYAN) } else { st(text_c) }));
                v.push(span(after, text_c));
            }
            None => v.push(span(seg.iter().collect::<String>(), text_c)),
        }
        lines.push(Line::from(v));
    }
    lines
}

fn draw_form(f: &mut Frame, w: &Wizard, area: Rect, t: f32) {
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(st(FAINT)).title(Line::from(vec![
        span(" ◈ ", CYAN),
        bold("NEW SWARM", TEXT),
        span(" ", MUTED),
    ]));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let width = inner.width as usize;
    let field = w.field();
    let focused = |x: Field| x == field;
    let mut lines: Vec<Line> = Vec::new();
    let vw = width.saturating_sub(LABEL_W + 2);

    // 01 WHAT
    lines.push(section("WHAT", "01", width, matches!(field, Field::Task), t));
    let task = text_field(&w.s.task, w.cursor, focused(Field::Task), vw, "describe the page… (ctrl-r for an idea)", w.frame);
    for (k, l) in task.into_iter().take(3).enumerate() {
        let label = if k == 0 { "build me" } else { "" };
        let mut r = row(label, focused(Field::Task), t, Vec::new());
        r.spans.extend(l.spans);
        lines.push(r);
    }
    lines.push(Line::default());

    // 02 WHERE
    lines.push(section("WHERE", "02", width, focused(Field::Dir), t));
    let dir_line = text_field(&w.s.dir, w.cursor, focused(Field::Dir), vw, "~/sites/my-page", w.frame).remove(0);
    let mut r = row("folder", focused(Field::Dir), t, Vec::new());
    r.spans.extend(dir_line.spans);
    lines.push(r);
    let has_project = !w.s.dir.trim().is_empty() && State::path(&w.s.dir_path()).exists();
    if has_project {
        lines.push(row("", false, t, vec![span("⚠ already a swarm project · launching starts it over · resume it from RECENT", AMBER)]));
    } else if w.s.dir_path().exists() {
        lines.push(row("", false, t, vec![span("folder exists · the swarm adds its files to it", MUTED)]));
    } else {
        lines.push(row("", false, t, vec![span("new folder", MUTED)]));
    }
    lines.push(Line::default());

    // 03 WHO
    let who_active = matches!(field, Field::Lead | Field::Team(_));
    lines.push(section("WHO", "03", width, who_active, t));
    {
        let fo = focused(Field::Lead);
        let pc = provider_color(w.s.lead);
        let mut v = arrows(fo, vec![bold(format!("{:<11}", w.s.lead.label()), pc)]);
        v.push(span("  ★ model ", MUTED));
        v.push(bold(w.s.lead_model.clone(), GOLD));
        if fo {
            v.push(span("  space ↻", FAINT));
        }
        lines.push(row("lead", fo, t, v));
    }
    for (k, p) in Provider::ALL.iter().enumerate() {
        let fo = focused(Field::Team(k));
        let n = w.s.counts[k];
        let pc = provider_color(*p);
        let mut v = arrows(fo, vec![bold(format!("{n:>2}"), if n > 0 { WHITE } else { FAINT })]);
        v.push(span(" ", FAINT));
        for i in 0..MAX_PER_TYPE {
            let on = i < n;
            let c = if on { lerp(pc, WHITE, 0.25 * (0.5 + 0.5 * (t * 4.0 - i as f32 * 0.6).sin())) } else { FAINT };
            v.push(span(if on { "■" } else { "·" }, c));
        }
        let model = if w.s.models[k].is_empty() { "default".to_string() } else { w.s.models[k].clone() };
        v.push(span("  model ", MUTED));
        v.push(bold(model, if n > 0 { pc } else { FAINT }));
        if fo {
            v.push(span("  space ↻", FAINT));
        }
        let label = match p {
            Provider::Claude => "claude code",
            Provider::Agy => "antigravity",
            Provider::Codex => "codex",
        };
        lines.push(row(label, fo, t, v));
    }
    lines.push(Line::default());

    // 04 HOW
    lines.push(section("HOW", "04", width, matches!(field, Field::Review | Field::Open | Field::Mode), t));
    {
        let fo = focused(Field::Review);
        let (mark, c) = if w.s.review { ("[✔]", GREEN) } else { ("[ ]", FAINT) };
        lines.push(row("review pass", fo, t, vec![bold(mark, c), span(" lead checks the whole page after each build", MUTED)]));
        let fo = focused(Field::Open);
        let (label, desc) = match w.s.open {
            Open::Live => ("live", "open the preview as it builds"),
            Open::End => ("when done", "open the page once it ships"),
            Open::Never => ("never", "you open it with ctrl-o"),
        };
        let mut v = arrows(fo, vec![bold(format!("{label:<9}"), CYAN)]);
        v.push(span(format!("  {desc}"), MUTED));
        lines.push(row("browser", fo, t, v));
        let fo = focused(Field::Mode);
        let mut v = if w.s.demo {
            arrows(fo, vec![chip(" DEMO ", Color::Black, NEON)])
        } else {
            arrows(fo, vec![chip(" REAL ", Color::Black, GREEN)])
        };
        v.push(span(if w.s.demo { "  scripted agents build a sample page, zero tokens" } else { "  real agents, real tokens" }, MUTED));
        lines.push(row("mode", fo, t, v));
    }
    lines.push(Line::default());

    // 05 HOW MUCH
    lines.push(section("HOW MUCH", "05", width, focused(Field::Budget), t));
    {
        let fo = focused(Field::Budget);
        let b = if w.s.budget == 0.0 { "no cap".to_string() } else { format!("${:.2}", w.s.budget) };
        let mut v = arrows(fo, vec![bold(format!("{b:<7}"), if w.s.budget == 0.0 { SOFT } else { GOLD })]);
        if w.s.demo {
            v.push(span("  demo is free", MUTED));
        } else {
            v.push(span(format!("  ≈ ${:.2} per build", w.s.estimate()), MUTED));
            v.push(span(" (claude only, rough)", FAINT));
        }
        lines.push(row("budget", fo, t, v));
    }
    lines.push(Line::default());

    // LAUNCH
    let n = w.s.workers() + 1;
    let fo = focused(Field::Launch);
    let label = format!("  ▶  LAUNCH SWARM  ·  {n} agent{}  ", if n == 1 { "" } else { "s" });
    let pad = width.saturating_sub(label.chars().count()) / 2;
    let mut v = vec![span(" ".repeat(pad), FAINT)];
    if fo {
        for (i, ch) in label.chars().enumerate() {
            let bg = hsv(i as f32 * 6.0 + t * 160.0, 0.75, 1.0);
            v.push(Span::styled(ch.to_string(), Style::new().fg(Color::Black).bg(bg).add_modifier(Modifier::BOLD)));
        }
    } else {
        v.push(Span::styled(label, Style::new().fg(GOLD).bg(Color::Rgb(40, 32, 12)).add_modifier(Modifier::BOLD)));
    }
    lines.push(Line::from(v));
    let hint = if fo { "press enter" } else { "ctrl-g from anywhere" };
    let pad = width.saturating_sub(hint.len()) / 2;
    lines.push(Line::from(vec![span(" ".repeat(pad), FAINT), span(hint, FAINT)]));

    // keep the focused row in view on short terminals
    let focus_line = lines.iter().position(|l| l.spans.first().is_some_and(|s| s.content == "▌")).unwrap_or(0);
    let h = inner.height as usize;
    let skip = if lines.len() > h { focus_line.saturating_sub(h / 2).min(lines.len() - h) } else { 0 };
    let shown: Vec<Line> = lines.into_iter().skip(skip).take(h).collect();
    f.render_widget(Paragraph::new(shown), inner);
}

fn draw_side(f: &mut Frame, w: &Wizard, area: Rect, t: f32) {
    let roster = w.s.roster();
    let squad_h = (roster.len() as u16 + 2).min(area.height / 2).max(4);
    let [squad, sys, rec] = Layout::vertical([Constraint::Length(squad_h), Constraint::Length(5), Constraint::Min(3)]).areas(area);

    // squad preview
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(st(FAINT)).title(Line::from(vec![
        span(" ⚡ ", GOLD),
        bold("SQUAD", TEXT),
        span(format!(" · {} ", roster.len()), MUTED),
    ]));
    let inner = block.inner(squad);
    f.render_widget(block, squad);
    let mut lines = Vec::new();
    let h = inner.height as usize;
    for (i, a) in roster.iter().enumerate() {
        if lines.len() + 1 >= h && i + 1 < roster.len() {
            lines.push(Line::from(span(format!("  … and {} more", roster.len() - i), MUTED)));
            break;
        }
        let c = if i == 0 { GOLD } else { provider_color(a.provider) };
        let fr = fx::SPINNER[((w.frame / 3) as usize + i) % fx::SPINNER.len()];
        let mut v = vec![span(format!(" {fr} "), c), bold(format!("{:<12}", trunc(&a.name, 12)), c)];
        if i == 0 {
            v.push(chip(" LEAD ", Color::Black, GOLD));
            v.push(span(" ", FAINT));
        }
        v.push(span(format!("{} {}", a.provider.label(), a.model_label()), MUTED));
        lines.push(Line::from(v));
    }
    f.render_widget(Paragraph::new(lines), inner);

    // systems check
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(st(FAINT)).title(Line::from(vec![
        span(" ⟐ ", CYAN),
        bold("SYSTEMS", TEXT),
        span(" ", MUTED),
    ]));
    let inner = block.inner(sys);
    f.render_widget(block, sys);
    let mut lines = Vec::new();
    for p in Provider::ALL {
        let mut v = vec![span(" ", FAINT), bold(format!("{:<12}", p.label()), provider_color(p))];
        match w.check(p) {
            None => {
                let fr = fx::SPINNER[(w.frame / 2) as usize % fx::SPINNER.len()];
                v.push(span(format!("{fr} checking…"), MUTED));
            }
            Some(None) => v.push(bold("✖ not installed", RED)),
            Some(Some(ver)) => {
                v.push(bold(format!("✔ v{ver}"), GREEN));
                if p == Provider::Codex && !codex_ready() {
                    v.push(span(" · not logged in", AMBER));
                }
            }
        }
        lines.push(Line::from(v));
    }
    f.render_widget(Paragraph::new(lines), inner);

    // recent projects
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(st(FAINT)).title(Line::from(vec![
        span(" ↺ ", PINK_SOFT),
        bold("RECENT", TEXT),
        span(" · enter to resume ", MUTED),
    ]));
    let inner = block.inner(rec);
    f.render_widget(block, rec);
    if w.recent.is_empty() {
        f.render_widget(Paragraph::new(Line::from(span(" nothing yet · your projects show up here", FAINT))), inner);
        return;
    }
    let mut lines = Vec::new();
    let iw = inner.width as usize;
    for (i, r) in w.recent.iter().enumerate() {
        let fo = w.field() == Field::Recent(i);
        let marker = if fo { bold("▌", lerp(NEON, GOLD, 0.5 + 0.5 * (t * 5.0).sin())) } else { span(" ", FAINT) };
        lines.push(Line::from(vec![marker, bold(trunc(&r.title, iw.saturating_sub(3)), if fo { WHITE } else { SOFT })]));
        lines.push(Line::from(vec![
            span("  ", FAINT),
            span(
                trunc(
                    &format!(
                        "{} · {} agents · {} round{} · {}",
                        tilde(&r.dir),
                        r.agents,
                        r.round,
                        if r.round == 1 { "" } else { "s" },
                        r.age
                    ),
                    iw.saturating_sub(2),
                ),
                if fo { MUTED } else { FAINT },
            ),
        ]));
        if lines.len() + 2 > inner.height as usize {
            break;
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
    let _ = shimmer;
}

const PINK_SOFT: Color = Color::Rgb(255, 121, 198);
