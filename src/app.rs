//! Dashboard state. Every event from the agents lands here and becomes something to look at.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::fx::{self, Packet, Particle, Rng};
use crate::providers::{AgentSpec, Provider};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    Plan,
    Build,
    Review,
    Ship,
}

impl Phase {
    pub const ALL: [Phase; 4] = [Phase::Plan, Phase::Build, Phase::Review, Phase::Ship];

    pub fn label(self) -> &'static str {
        match self {
            Phase::Plan => "PLAN",
            Phase::Build => "BUILD",
            Phase::Review => "REVIEW",
            Phase::Ship => "SHIP",
        }
    }
}

#[derive(Clone, Debug)]
pub enum AgentEv {
    Start,
    Text(String),
    Thinking(String),
    ToolStart(String),
    ToolDetail(String),
    /// File contents / command text streaming out of a tool call.
    Code(String),
    /// `true` when the tool call errored.
    ToolDone(bool),
    Tokens(u64),
    TokensTotal(u64),
    Cost(f64),
    Done(String),
    Fail(String),
}

#[derive(Clone, Debug)]
pub struct PlanItem {
    pub id: String,
    pub agent: String,
    pub brief: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Good,
    Warn,
    Bad,
    Msg,
}

#[derive(Debug)]
pub enum Ev {
    Spawn(AgentSpec),
    Agent(usize, AgentEv),
    Msg { from: String, to: String, text: String },
    Log(Level, String),
    File { path: String, bytes: u64, new: bool },
    Phase(Phase),
    Plan(String, Vec<PlanItem>),
    Shipped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AState {
    Queued,
    Booting,
    Thinking,
    Tool,
    Writing,
    Done,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Thinking,
    Code,
}

pub struct Agent {
    pub spec: AgentSpec,
    pub color: Color,
    pub role: String,
    pub state: AState,
    pub started: Option<Instant>,
    pub finished: Option<Instant>,
    pub tokens: u64,
    pub cost: f64,
    pub tool: String,
    pub detail: String,
    pub tools_used: u32,
    pub files: u32,
    pub lines: VecDeque<(Kind, String)>,
    pending: VecDeque<(Kind, String)>,
    pub rate: VecDeque<u64>,
    rate_acc: u64,
    pub flash: Option<(Instant, Color)>,
    pub bubble: Option<(String, String, Instant)>,
    pub verb: &'static str,
    verb_at: Instant,
    pub error: Option<String>,
    pub summary: String,
    tool_logged: bool,
}

impl Agent {
    fn new(spec: AgentSpec, color: Color, rng: &mut Rng) -> Self {
        Agent {
            spec,
            color,
            role: String::new(),
            state: AState::Queued,
            started: None,
            finished: None,
            tokens: 0,
            cost: 0.0,
            tool: String::new(),
            detail: String::new(),
            tools_used: 0,
            files: 0,
            lines: VecDeque::new(),
            pending: VecDeque::new(),
            rate: VecDeque::new(),
            rate_acc: 0,
            flash: None,
            bubble: None,
            verb: *rng.pick(fx::VERBS),
            verb_at: Instant::now(),
            error: None,
            summary: String::new(),
            tool_logged: false,
        }
    }

    pub fn working(&self) -> bool {
        matches!(self.state, AState::Booting | AState::Thinking | AState::Tool | AState::Writing)
    }

    pub fn elapsed(&self) -> Duration {
        match self.started {
            Some(s) => self.finished.unwrap_or_else(Instant::now).saturating_duration_since(s),
            None => Duration::ZERO,
        }
    }

    /// No agent reports real progress, so fake a believable curve from tool calls and time.
    pub fn progress(&self) -> f32 {
        match self.state {
            AState::Done => 1.0,
            AState::Queued => 0.0,
            _ => {
                let x = self.tools_used as f32 * 0.3 + self.elapsed().as_secs_f32() / 120.0;
                (1.0 - (-x).exp()).min(0.95)
            }
        }
    }

    fn feed(&mut self, k: Kind, s: &str) {
        let s = s.replace('\t', "  ").replace('\r', "");
        if s.is_empty() {
            return;
        }
        if let Some((pk, ps)) = self.pending.back_mut() {
            if *pk == k {
                ps.push_str(&s);
                return;
            }
        }
        self.pending.push_back((k, s));
    }

    /// Typewriter: move a slice of pending text onto the screen each frame.
    fn reveal(&mut self) {
        let total: usize = self.pending.iter().map(|(_, s)| s.len()).sum();
        if total == 0 {
            return;
        }
        let mut budget = (total / 6).max(4);
        while budget > 0 {
            let Some((k, s)) = self.pending.front_mut() else { break };
            let k = *k;
            let chunk = if s.len() <= budget {
                let c = std::mem::take(s);
                self.pending.pop_front();
                c
            } else {
                let mut cut = budget;
                while !s.is_char_boundary(cut) {
                    cut += 1;
                }
                s.drain(..cut).collect::<String>()
            };
            budget = budget.saturating_sub(chunk.len().max(1));
            self.push_stream(k, &chunk);
        }
    }

    fn push_stream(&mut self, k: Kind, s: &str) {
        for (i, part) in s.split('\n').enumerate() {
            let new_line = i > 0 || self.lines.back().is_none_or(|(lk, _)| *lk != k);
            if new_line {
                self.lines.push_back((k, String::new()));
            }
            if let Some((_, l)) = self.lines.back_mut() {
                if l.len() < 4000 {
                    l.push_str(part);
                }
            }
        }
        while self.lines.len() > 300 {
            self.lines.pop_front();
        }
    }
}

pub struct FileEntry {
    pub path: String,
    pub bytes: u64,
    pub at: Instant,
    pub new: bool,
}

pub struct App {
    pub task: String,
    pub dir: PathBuf,
    pub demo: bool,
    auto_open: bool,
    pub agents: Vec<Agent>,
    pub phase: Phase,
    pub title: String,
    pub plan: Vec<PlanItem>,
    pub logs: VecDeque<(Instant, Level, String)>,
    pub files: Vec<FileEntry>,
    pub comms: VecDeque<(String, String, String)>,
    pub msg_count: u32,
    pub packets: Vec<Packet>,
    pub particles: Vec<Particle>,
    pub rects: HashMap<usize, Rect>,
    pub screen: Rect,
    pub fx: bool,
    pub preflight: Vec<(String, String, bool)>,
    pub rate: VecDeque<u64>,
    pub start: Instant,
    pub end: Option<Instant>,
    pub t: f32,
    pub frame: u64,
    pub boot_start: Instant,
    pub boot_len: f32,
    boot_skipped: bool,
    pub dash_at: Option<Instant>,
    pub shipped: Option<Instant>,
    rng: Rng,
    rate_acc: u64,
    sample_at: Instant,
    last_tick: Instant,
    next_firework: Instant,
}

fn agent_color(provider: Provider, ordinal: usize, lead: bool) -> Color {
    if lead {
        return Color::Rgb(255, 196, 0);
    }
    // golden-ratio spread inside each provider's hue family
    let k = (ordinal as f32 * 0.618_034).fract();
    match provider {
        Provider::Claude => fx::hsv(4.0 + k * 52.0, 0.62, 1.0),
        Provider::Codex => fx::hsv(135.0 + k * 45.0, 0.6, 0.95),
        Provider::Agy => fx::hsv(195.0 + k * 100.0, 0.55, 1.0),
    }
}

fn center(r: &Rect) -> (f32, f32) {
    (r.x as f32 + r.width as f32 / 2.0, r.y as f32 + r.height as f32 / 2.0)
}

impl App {
    pub fn new(
        task: String,
        dir: PathBuf,
        roster: Vec<AgentSpec>,
        preflight: Vec<(String, String, bool)>,
        demo: bool,
        auto_open: bool,
    ) -> Self {
        let mut rng = Rng::seeded();
        let mut counts: HashMap<Provider, usize> = HashMap::new();
        let agents = roster
            .into_iter()
            .enumerate()
            .map(|(i, s)| {
                let o = counts.entry(s.provider).or_insert(0);
                let c = agent_color(s.provider, *o, i == 0);
                *o += 1;
                let mut a = Agent::new(s, c, &mut rng);
                if i == 0 {
                    a.role = "lead".into();
                }
                a
            })
            .collect();
        let now = Instant::now();
        let boot_len = (1.4 + 0.22 * preflight.len() as f32 + 0.9).min(4.5);
        App {
            task,
            dir,
            demo,
            auto_open,
            agents,
            phase: Phase::Plan,
            title: String::new(),
            plan: Vec::new(),
            logs: VecDeque::new(),
            files: Vec::new(),
            comms: VecDeque::new(),
            msg_count: 0,
            packets: Vec::new(),
            particles: Vec::new(),
            rects: HashMap::new(),
            screen: Rect::default(),
            fx: true,
            preflight,
            rate: VecDeque::new(),
            start: now,
            end: None,
            t: 0.0,
            frame: 0,
            boot_start: now,
            boot_len,
            boot_skipped: false,
            dash_at: None,
            shipped: None,
            rng,
            rate_acc: 0,
            sample_at: now,
            last_tick: now,
            next_firework: now,
        }
    }

    pub fn booting(&self) -> bool {
        !self.boot_skipped && self.boot_age() < self.boot_len
    }

    pub fn boot_age(&self) -> f32 {
        self.boot_start.elapsed().as_secs_f32()
    }

    pub fn skip_boot(&mut self) {
        self.boot_skipped = true;
    }

    pub fn elapsed(&self) -> Duration {
        self.end.unwrap_or_else(Instant::now).saturating_duration_since(self.start)
    }

    pub fn agent_index(&self, name: &str) -> Option<usize> {
        let name = name.trim().trim_start_matches('@');
        self.agents
            .iter()
            .position(|a| a.spec.name == name)
            .or_else(|| self.agents.iter().position(|a| a.spec.name.eq_ignore_ascii_case(name)))
            .or_else(|| name.eq_ignore_ascii_case("lead").then_some(0))
    }

    pub fn log(&mut self, level: Level, text: impl Into<String>) {
        self.logs.push_back((Instant::now(), level, text.into()));
        while self.logs.len() > 300 {
            self.logs.pop_front();
        }
    }

    pub fn apply(&mut self, ev: Ev) {
        match ev {
            Ev::Spawn(spec) => {
                let o = self.agents.iter().filter(|a| a.spec.provider == spec.provider).count();
                let c = agent_color(spec.provider, o, false);
                let name = spec.name.clone();
                let a = Agent::new(spec, c, &mut self.rng);
                self.agents.push(a);
                self.log(Level::Warn, format!("{name} dropped in as backup"));
            }
            Ev::Agent(i, e) => self.agent_event(i, e),
            Ev::Msg { from, to, text } => self.message(from, to, text),
            Ev::Log(l, s) => self.log(l, s),
            Ev::File { path, bytes, new } => {
                let now = Instant::now();
                match self.files.iter_mut().find(|f| f.path == path) {
                    Some(f) => {
                        f.bytes = bytes;
                        f.at = now;
                        f.new = false;
                    }
                    None => {
                        self.log(Level::Good, format!("+ {path}"));
                        self.files.push(FileEntry { path, bytes, at: now, new });
                    }
                }
            }
            Ev::Phase(p) => {
                self.phase = p;
                self.log(Level::Info, format!("━━ phase {} ━━", p.label()));
                if p == Phase::Ship {
                    self.end = Some(Instant::now());
                }
            }
            Ev::Plan(title, items) => {
                self.title = title;
                for it in &items {
                    if let Some(i) = self.agent_index(&it.agent) {
                        self.agents[i].role = it.id.clone();
                    }
                }
                if self.plan.is_empty() {
                    self.log(Level::Good, format!("plan locked: {} sections", items.len()));
                }
                self.plan = items;
            }
            Ev::Shipped => {
                self.shipped = Some(Instant::now());
                self.log(Level::Good, "SHIPPED. index.html is ready");
                if self.auto_open {
                    self.open_page();
                }
            }
        }
    }

    fn agent_event(&mut self, i: usize, e: AgentEv) {
        let now = Instant::now();
        let rect = self.rects.get(&i).copied();
        let Some(a) = self.agents.get_mut(i) else { return };
        let mut log: Option<(Level, String)> = None;
        let mut burst: Option<Color> = None;
        match e {
            AgentEv::Start => {
                a.state = AState::Booting;
                a.started.get_or_insert(now);
                a.finished = None;
                a.flash = Some((now, a.color));
                log = Some((Level::Info, format!("{} spawned · {} {}", a.spec.name, a.spec.provider.label(), a.spec.model_label())));
            }
            AgentEv::Text(s) => {
                if !matches!(a.state, AState::Tool | AState::Writing) {
                    a.state = AState::Thinking;
                }
                a.feed(Kind::Text, &s);
            }
            AgentEv::Thinking(s) => {
                if !matches!(a.state, AState::Tool | AState::Writing) {
                    a.state = AState::Thinking;
                }
                a.feed(Kind::Thinking, &s);
            }
            AgentEv::ToolStart(name) => {
                a.state = AState::Tool;
                a.tool = name;
                a.detail.clear();
                a.tools_used += 1;
                a.tool_logged = false;
            }
            AgentEv::ToolDetail(d) => {
                a.detail = d;
                if !a.tool_logged {
                    a.tool_logged = true;
                    log = Some((Level::Info, format!("{} ⏺ {}({})", a.spec.name, a.tool, a.detail)));
                }
            }
            AgentEv::Code(s) => {
                a.state = AState::Writing;
                a.feed(Kind::Code, &s);
            }
            AgentEv::ToolDone(err) => {
                if err {
                    a.flash = Some((now, Color::Rgb(255, 85, 85)));
                } else if matches!(a.tool.as_str(), "Write" | "Edit") {
                    a.files += 1;
                }
                if a.working() {
                    a.state = AState::Thinking;
                }
            }
            AgentEv::Tokens(n) => {
                a.tokens += n;
                a.rate_acc += n;
                self.rate_acc += n;
            }
            AgentEv::TokensTotal(n) => {
                if n > a.tokens {
                    let d = n - a.tokens;
                    a.rate_acc += d;
                    self.rate_acc += d;
                    a.tokens = n;
                }
            }
            AgentEv::Cost(c) => a.cost += c,
            AgentEv::Done(summary) => {
                a.state = AState::Done;
                a.finished = Some(now);
                a.summary = summary.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
                a.flash = Some((now, Color::Rgb(80, 250, 123)));
                burst = Some(Color::Rgb(80, 250, 123));
                log = Some((Level::Good, format!("{} finished in {}s", a.spec.name, a.elapsed().as_secs())));
            }
            AgentEv::Fail(err) => {
                a.state = AState::Failed;
                a.finished = Some(now);
                a.error = Some(err.clone());
                a.flash = Some((now, Color::Rgb(255, 85, 85)));
                burst = Some(Color::Rgb(255, 85, 85));
                log = Some((Level::Bad, format!("{} failed: {err}", a.spec.name)));
            }
        }
        if let (Some(c), Some(r)) = (burst, rect) {
            let (x, y) = center(&r);
            fx::burst(&mut self.particles, &mut self.rng, x, y, c, 48, 14.0);
        }
        if let Some((l, s)) = log {
            self.log(l, s);
        }
    }

    fn message(&mut self, from: String, to: String, text: String) {
        let fi = self.agent_index(&from).unwrap_or(0);
        let targets: Vec<usize> = if to.eq_ignore_ascii_case("all") {
            (0..self.agents.len()).filter(|&j| j != fi && j != 0).collect()
        } else {
            vec![self.agent_index(&to).unwrap_or(0)]
        };
        let color = self.agents[fi].color;
        for t in targets {
            if t == fi {
                continue;
            }
            let speed = self.rng.range(0.75, 1.15);
            self.packets.push(Packet { from: fi, to: t, t: 0.0, speed, color, from_name: from.clone(), text: text.clone() });
        }
        self.msg_count += 1;
        self.log(Level::Msg, format!("{from} ➜ {to}: {text}"));
        self.comms.push_back((from, to, text));
        while self.comms.len() > 100 {
            self.comms.pop_front();
        }
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f32().min(0.1);
        self.last_tick = now;
        self.t = now.duration_since(self.start).as_secs_f32();
        self.frame += 1;
        if !self.booting() && self.dash_at.is_none() {
            self.dash_at = Some(now);
        }

        for a in &mut self.agents {
            a.reveal();
            if now.duration_since(a.verb_at).as_secs_f32() > 3.2 {
                a.verb = *self.rng.pick(fx::VERBS);
                a.verb_at = now;
            }
            if a.bubble.as_ref().is_some_and(|(_, _, at)| now.duration_since(*at).as_secs_f32() > 4.5) {
                a.bubble = None;
            }
        }

        if now.duration_since(self.sample_at) >= Duration::from_millis(250) {
            self.sample_at = now;
            for a in &mut self.agents {
                a.rate.push_back(a.rate_acc);
                a.rate_acc = 0;
                while a.rate.len() > 48 {
                    a.rate.pop_front();
                }
            }
            self.rate.push_back(self.rate_acc);
            self.rate_acc = 0;
            while self.rate.len() > 64 {
                self.rate.pop_front();
            }
        }

        // packets in flight
        let mut arrived = Vec::new();
        for p in &mut self.packets {
            p.t += dt * p.speed;
            if p.t >= 1.0 {
                arrived.push((p.to, p.from_name.clone(), p.text.clone(), p.color));
            }
        }
        self.packets.retain(|p| p.t < 1.0);
        for (to, from, text, color) in arrived {
            if let Some(a) = self.agents.get_mut(to) {
                a.bubble = Some((from, text, now));
                a.flash = Some((now, color));
            }
            if let Some(r) = self.rects.get(&to).copied() {
                let (x, y) = center(&r);
                fx::burst(&mut self.particles, &mut self.rng, x, y, color, 10, 7.0);
            }
        }

        // particles + fireworks
        let mut sparks = Vec::new();
        for p in &mut self.particles {
            p.step(dt);
            if p.rocket && p.life <= 0.0 {
                sparks.push((p.x, p.y));
            }
        }
        self.particles.retain(|p| p.life > 0.0);
        for (x, y) in sparks {
            let c = fx::hsv(self.rng.range(0.0, 360.0), 0.75, 1.0);
            fx::burst(&mut self.particles, &mut self.rng, x, y, c, 60, 18.0);
        }
        if self.shipped.is_some_and(|s| now.duration_since(s).as_secs_f32() > 0.6) && self.fx && now >= self.next_firework {
            let s = self.screen;
            let r = fx::rocket(&mut self.rng, s.x as f32, s.right() as f32, s.bottom() as f32);
            self.particles.push(r);
            self.next_firework = now + Duration::from_secs_f32(self.rng.range(0.25, 0.75));
        }
        if self.particles.len() > 2000 {
            let extra = self.particles.len() - 2000;
            self.particles.drain(..extra);
        }
    }

    pub fn open_page(&mut self) {
        let p = self.dir.join("index.html");
        if !p.exists() {
            return;
        }
        let spawned = std::process::Command::new("xdg-open")
            .arg(&p)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if spawned.is_ok() {
            self.log(Level::Info, format!("opened {}", p.display()));
        }
    }

    pub fn total_tokens(&self) -> u64 {
        self.agents.iter().map(|a| a.tokens).sum()
    }

    pub fn total_cost(&self) -> f64 {
        self.agents.iter().map(|a| a.cost).sum()
    }

    pub fn summary(&self) -> String {
        let index = self.dir.join("index.html");
        if self.shipped.is_some() {
            format!(
                "✔ swarm shipped · {} agents · {}s · {} tokens · {} messages\n  page: {}",
                self.agents.len(),
                self.elapsed().as_secs(),
                self.total_tokens(),
                self.msg_count,
                index.display()
            )
        } else {
            format!("✖ stopped early — whatever the agents finished is in {}", self.dir.display())
        }
    }
}
