//! Dashboard state. Every event from the agents lands here and becomes something to look at,
//! and every key you press becomes either an edit to the command bar or an order for the swarm.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Color;
use tokio::sync::mpsc::UnboundedSender;

use crate::fx::{self, Packet, Particle, Rng};
use crate::orchestrator::Cmd;
use crate::providers::{AgentSpec, Provider};

/// Stand-in agent index for the human at the command bar.
pub const YOU: usize = usize::MAX;

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
    /// A turn started running.
    Start,
    /// What the next turn is for; drawn as a divider in the agent's log.
    Turn(String),
    Session(String),
    Text(String),
    Thinking(String),
    ToolStart(String),
    ToolDetail(String),
    /// File contents / command text streaming out of a tool call.
    Code(String),
    /// `true` when the tool call errored.
    ToolDone(bool),
    Tokens(u64),
    /// Real output-token count for the turn that just ended.
    TokensTotal(u64),
    Cost(f64),
    Done(String),
    Fail(String),
    Stopped,
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
    Spawn(AgentSpec, String),
    Agent(usize, AgentEv),
    Msg {
        from: String,
        to: String,
        text: String,
    },
    Log(Level, String),
    File {
        path: String,
        bytes: u64,
        new: bool,
    },
    Phase(Phase),
    Plan(String, Vec<PlanItem>),
    /// A round of work finished and the page is assembled.
    Shipped(u32),
    /// Live preview server is up.
    Server(String),
    /// A saved project was picked back up at this round.
    Resumed(u32),
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
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Thinking,
    Code,
    Divider,
    You,
}

pub struct Agent {
    pub spec: AgentSpec,
    pub color: Color,
    pub role: String,
    pub state: AState,
    pub started: Option<Instant>,
    pub finished: Option<Instant>,
    pub tokens: u64,
    turn_tokens: u64,
    pub cost: f64,
    pub tool: String,
    pub detail: String,
    pub tools_used: u32,
    turn_tools: u32,
    pub turns: u32,
    pub files: u32,
    pub session: Option<String>,
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
            turn_tokens: 0,
            cost: 0.0,
            tool: String::new(),
            detail: String::new(),
            tools_used: 0,
            turn_tools: 0,
            turns: 0,
            files: 0,
            session: None,
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

    /// Time spent on the current (or last) turn.
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
                let x = self.turn_tools as f32 * 0.3 + self.elapsed().as_secs_f32() / 120.0;
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

    /// Lines that skip the typewriter (dividers, your own messages).
    fn note(&mut self, k: Kind, s: String) {
        for (pk, ps) in self.pending.drain(..).collect::<Vec<_>>() {
            self.push_stream(pk, &ps);
        }
        self.lines.push_back((k, s));
        self.lines.push_back((Kind::Text, String::new()));
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
        while self.lines.len() > 1500 {
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

pub const COMMANDS: &[(&str, &str)] = &[
    ("/help", "keys and commands"),
    ("/add", "hire an agent: /add claude|agy|codex [model]"),
    ("/stop", "interrupt everyone, or /stop <name>"),
    ("/review", "the lead reviews the whole page now"),
    ("/open", "open the live page in your browser"),
    ("/budget", "spending cap for this session in USD (0 = none)"),
    ("/fx", "toggle heavy effects"),
    ("/quit", "save and exit (agents can be resumed later)"),
];

pub struct App {
    pub task: String,
    pub dir: PathBuf,
    pub demo: bool,
    auto_open: bool,
    cmd: UnboundedSender<Cmd>,
    pub agents: Vec<Agent>,
    pub phase: Phase,
    pub round: u32,
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
    pub t: f32,
    pub frame: u64,
    pub boot_start: Instant,
    pub boot_len: f32,
    boot_skipped: bool,
    pub dash_at: Option<Instant>,
    /// Big SHIP IT banner (first round only) until this moment.
    pub banner_until: Option<Instant>,
    pub shipped_at: Option<Instant>,
    fireworks_until: Instant,
    pub server: Option<String>,
    // command bar
    pub input: String,
    pub cursor: usize,
    history: Vec<String>,
    hist_pos: Option<usize>,
    pub sugg_sel: usize,
    pub selected: Option<usize>,
    pub zoom: bool,
    pub scroll: usize,
    pub help: bool,
    pub toast: Option<(String, Level, Instant)>,
    pub reply: Option<(String, String, Instant)>,
    quit_armed: Option<Instant>,
    // money
    pub budget: f64,
    pub cost_base: f64,
    budget_tripped: bool,
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

pub struct Launch {
    pub task: String,
    pub dir: PathBuf,
    pub roster: Vec<AgentSpec>,
    pub preflight: Vec<(String, String, bool)>,
    pub demo: bool,
    pub auto_open: bool,
    pub budget: f64,
}

impl App {
    pub fn new(l: Launch, cmd: UnboundedSender<Cmd>) -> Self {
        let mut rng = Rng::seeded();
        let mut counts: HashMap<Provider, usize> = HashMap::new();
        let agents = l
            .roster
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
        let boot_len = (1.4 + 0.22 * l.preflight.len() as f32 + 0.9).min(4.5);
        App {
            task: l.task,
            dir: l.dir,
            demo: l.demo,
            auto_open: l.auto_open,
            cmd,
            agents,
            phase: Phase::Plan,
            round: 1,
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
            preflight: l.preflight,
            rate: VecDeque::new(),
            start: now,
            t: 0.0,
            frame: 0,
            boot_start: now,
            boot_len,
            boot_skipped: false,
            dash_at: None,
            banner_until: None,
            shipped_at: None,
            fireworks_until: now,
            server: None,
            input: String::new(),
            cursor: 0,
            history: Vec::new(),
            hist_pos: None,
            sugg_sel: 0,
            selected: None,
            zoom: false,
            scroll: 0,
            help: false,
            toast: None,
            reply: None,
            quit_armed: None,
            budget: l.budget,
            cost_base: 0.0,
            budget_tripped: false,
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

    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    pub fn busy(&self) -> bool {
        self.agents.iter().any(|a| a.working())
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

    pub fn toast(&mut self, level: Level, text: impl Into<String>) {
        self.toast = Some((text.into(), level, Instant::now()));
    }

    /// Spend this session (resumed projects start with what they already cost).
    pub fn spent(&self) -> f64 {
        (self.total_cost() - self.cost_base).max(0.0)
    }

    pub fn over_budget(&self) -> bool {
        self.budget > 0.0 && self.spent() >= self.budget
    }

    pub fn apply(&mut self, ev: Ev) {
        match ev {
            Ev::Spawn(spec, why) => {
                let o = self.agents.iter().filter(|a| a.spec.provider == spec.provider).count();
                let c = agent_color(spec.provider, o, false);
                let name = spec.name.clone();
                let a = Agent::new(spec, c, &mut self.rng);
                self.agents.push(a);
                self.log(Level::Warn, format!("{name} {why}"));
                if let Some(r) = self.rects.get(&0).copied() {
                    let (x, y) = center(&r);
                    fx::burst(&mut self.particles, &mut self.rng, x, y, c, 40, 16.0);
                }
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
                        if new {
                            self.log(Level::Good, format!("+ {path}"));
                        }
                        self.files.push(FileEntry { path, bytes, at: now, new });
                    }
                }
            }
            Ev::Phase(p) => {
                if p != self.phase {
                    self.log(Level::Info, format!("━━ round {} · {} ━━", self.round, p.label()));
                }
                self.phase = p;
            }
            Ev::Plan(title, items) => {
                self.title = title;
                let mut roles: HashMap<String, Vec<String>> = HashMap::new();
                for it in &items {
                    roles.entry(it.agent.clone()).or_default().push(it.id.clone());
                }
                for a in self.agents.iter_mut().skip(1) {
                    a.role = match roles.get(&a.spec.name) {
                        Some(ids) if ids.len() > 1 => format!("#{} +{}", ids[0], ids.len() - 1),
                        Some(ids) => format!("#{}", ids[0]),
                        None => String::new(),
                    };
                }
                if self.plan.is_empty() && !items.is_empty() {
                    self.log(Level::Good, format!("plan locked: {} sections", items.len()));
                }
                self.plan = items;
            }
            Ev::Shipped(round) => {
                let now = Instant::now();
                self.round = round + 1;
                self.shipped_at = Some(now);
                self.fireworks_until = now + Duration::from_secs_f32(if round == 1 { 9.0 } else { 3.5 });
                if round == 1 {
                    self.banner_until = Some(now + Duration::from_secs(7));
                    if self.auto_open {
                        self.open_page();
                    }
                } else {
                    self.toast(Level::Good, format!("✔ round {round} shipped · the page is updated"));
                }
                self.log(Level::Good, format!("SHIPPED round {round}"));
            }
            Ev::Resumed(round) => {
                self.round = round;
                self.cost_base = self.total_cost();
                self.boot_skipped = true;
            }
            Ev::Server(url) => {
                self.log(Level::Info, format!("live preview at {url}"));
                self.server = Some(url);
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
                a.started = Some(now);
                a.finished = None;
                a.error = None;
                a.turn_tools = 0;
                a.turn_tokens = 0;
                a.turns += 1;
                a.flash = Some((now, a.color));
                if a.turns == 1 {
                    log = Some((Level::Info, format!("{} spawned · {} {}", a.spec.name, a.spec.provider.label(), a.spec.model_label())));
                }
            }
            AgentEv::Turn(label) => a.note(Kind::Divider, label),
            AgentEv::Session(id) => a.session = Some(id),
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
                a.turn_tools += 1;
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
                a.turn_tokens += n;
                a.rate_acc += n;
                self.rate_acc += n;
            }
            AgentEv::TokensTotal(n) => {
                if n > a.turn_tokens {
                    let d = n - a.turn_tokens;
                    a.rate_acc += d;
                    self.rate_acc += d;
                    a.tokens += d;
                    a.turn_tokens = n;
                }
            }
            AgentEv::Cost(c) => a.cost += c,
            AgentEv::Done(summary) => {
                a.state = AState::Done;
                a.finished = Some(now);
                a.summary = summary.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
                a.flash = Some((now, Color::Rgb(80, 250, 123)));
                burst = Some(Color::Rgb(80, 250, 123));
                log = Some((Level::Good, format!("{} finished a turn in {}s", a.spec.name, a.elapsed().as_secs())));
            }
            AgentEv::Fail(err) => {
                a.state = AState::Failed;
                a.finished = Some(now);
                a.error = Some(err.clone());
                a.flash = Some((now, Color::Rgb(255, 85, 85)));
                burst = Some(Color::Rgb(255, 85, 85));
                log = Some((Level::Bad, format!("{} failed: {err}", a.spec.name)));
            }
            AgentEv::Stopped => {
                a.state = AState::Stopped;
                a.finished = Some(now);
                a.flash = Some((now, Color::Rgb(255, 203, 107)));
                log = Some((Level::Warn, format!("{} stopped", a.spec.name)));
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

    fn index_of(&self, name: &str) -> usize {
        if name == "you" {
            YOU
        } else {
            self.agent_index(name).unwrap_or(0)
        }
    }

    fn message(&mut self, from: String, to: String, text: String) {
        let fi = self.index_of(&from);
        let targets: Vec<usize> =
            if to.eq_ignore_ascii_case("all") { (1..self.agents.len()).filter(|&j| j != fi).collect() } else { vec![self.index_of(&to)] };
        let color = if fi == YOU { Color::Rgb(255, 255, 255) } else { self.agents[fi].color };
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
        while self.comms.len() > 200 {
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
        if self.toast.as_ref().is_some_and(|(_, _, at)| now.duration_since(*at).as_secs_f32() > 4.5) {
            self.toast = None;
        }
        if self.reply.as_ref().is_some_and(|(_, _, at)| now.duration_since(*at).as_secs_f32() > 12.0) {
            self.reply = None;
        }
        if self.quit_armed.is_some_and(|at| now.duration_since(at).as_secs_f32() > 2.5) {
            self.quit_armed = None;
        }
        if self.banner_until.is_some_and(|u| now >= u) {
            self.banner_until = None;
        }

        if self.over_budget() && !self.budget_tripped {
            self.budget_tripped = true;
            let _ = self.cmd.send(Cmd::Stop(None));
            self.toast(Level::Bad, format!("budget cap hit (${:.2}) · agents stopped · /budget <usd> to raise it", self.budget));
            self.log(Level::Bad, "budget cap hit, everyone stopped");
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
            if to == YOU {
                self.reply = Some((from, text, now));
            } else if let Some(a) = self.agents.get_mut(to) {
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
        let launching = self.shipped_at.is_some_and(|s| now.duration_since(s).as_secs_f32() > 0.6) && now < self.fireworks_until;
        if launching && self.fx && now >= self.next_firework {
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

    pub fn page_target(&self) -> String {
        match &self.server {
            Some(url) => url.clone(),
            None => self.dir.join("index.html").display().to_string(),
        }
    }

    pub fn open_page(&mut self) {
        if self.server.is_none() && !self.dir.join("index.html").exists() {
            self.toast(Level::Warn, "no page yet, the agents are still on it");
            return;
        }
        let target = self.page_target();
        if crate::orchestrator::open(&target) {
            self.log(Level::Info, format!("opened {target}"));
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
        let mut s = format!(
            "swarm · {} agents · {} · {} tokens · {} messages · ${:.2} this session\n  page:    {}\n",
            self.agents.len(),
            crate::ui::fmt_dur(self.elapsed()),
            crate::ui::fmt_k(self.total_tokens()),
            self.msg_count,
            self.spent(),
            index.display(),
        );
        if !self.demo {
            s += &format!("  resume:  swarm -o {}   (or run `swarm` and pick it under RECENT)", self.dir.display());
        }
        s
    }

    // ── command bar ──────────────────────────────────────────────────────────────────────

    /// Who a message typed right now would go to.
    pub fn target(&self) -> String {
        let t = self.input.trim_start();
        if let Some(rest) = t.strip_prefix('@') {
            let name: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
            if name.eq_ignore_ascii_case("all") {
                return "all".into();
            }
            if let Some(i) = self.agent_index(&name) {
                return self.agents[i].spec.name.clone();
            }
            return name;
        }
        match self.selected {
            Some(i) if i > 0 => self.agents[i].spec.name.clone(),
            _ => self.agents[0].spec.name.clone(),
        }
    }

    /// Completions for whatever is typed: commands, agent names, providers, models.
    pub fn suggestions(&self) -> Vec<(String, String)> {
        let t = &self.input;
        if t.is_empty() {
            return Vec::new();
        }
        let lower = t.to_lowercase();
        if t.starts_with('/') {
            if let Some(rest) = lower.strip_prefix("/add ") {
                let parts: Vec<&str> = rest.split_whitespace().collect();
                let ends_space = rest.ends_with(' ');
                return match (parts.len(), ends_space) {
                    (0, _) | (1, false) => {
                        let p = parts.first().copied().unwrap_or("");
                        Provider::ALL
                            .iter()
                            .filter(|pr| pr.key().starts_with(p))
                            .map(|pr| (format!("/add {} ", pr.key()), pr.label().to_string()))
                            .collect()
                    }
                    (1, true) | (2, false) => {
                        let Some(pr) = Provider::parse(parts[0]) else { return Vec::new() };
                        let m = parts.get(1).copied().unwrap_or("");
                        pr.models()
                            .into_iter()
                            .filter(|x| !x.is_empty() && x.starts_with(m))
                            .map(|x| (format!("/add {} {x}", pr.key()), "model".to_string()))
                            .collect()
                    }
                    _ => Vec::new(),
                };
            }
            if let Some(rest) = lower.strip_prefix("/stop ") {
                return self
                    .agents
                    .iter()
                    .filter(|a| a.spec.name.starts_with(rest.trim()))
                    .map(|a| (format!("/stop {}", a.spec.name), a.spec.provider.label().to_string()))
                    .collect();
            }
            if t.contains(' ') {
                return Vec::new();
            }
            return COMMANDS
                .iter()
                .filter(|(c, _)| c.starts_with(lower.as_str()) && *c != lower)
                .map(|(c, d)| (c.to_string(), d.to_string()))
                .collect();
        }
        if let Some(rest) = lower.strip_prefix('@') {
            if rest.contains(' ') {
                return Vec::new();
            }
            let mut out: Vec<(String, String)> = self
                .agents
                .iter()
                .enumerate()
                .filter(|(_, a)| a.spec.name.to_lowercase().starts_with(rest))
                .map(|(i, a)| {
                    let d = if i == 0 { "lead".to_string() } else { format!("{} {}", a.spec.provider.label(), a.role) };
                    (format!("@{} ", a.spec.name), d)
                })
                .collect();
            if "all".starts_with(rest) {
                out.push(("@all ".into(), "every worker".into()));
            }
            return out;
        }
        Vec::new()
    }

    fn set_input(&mut self, s: String) {
        self.cursor = s.chars().count();
        self.input = s;
        self.sugg_sel = 0;
    }

    fn byte_at(&self, char_idx: usize) -> usize {
        self.input.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(self.input.len())
    }

    pub fn insert(&mut self, s: &str) {
        let s: String = s.chars().map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c }).collect();
        let b = self.byte_at(self.cursor);
        self.input.insert_str(b, &s);
        self.cursor += s.chars().count();
        self.sugg_sel = 0;
        self.hist_pos = None;
    }

    /// Returns true when the app should exit.
    pub fn key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if self.booting() {
            self.boot_skipped = true;
            if !(ctrl && k.code == KeyCode::Char('c')) {
                return false;
            }
        }
        if self.banner_until.is_some() && !ctrl {
            self.banner_until = None;
            if !matches!(k.code, KeyCode::Char(_)) {
                return false;
            }
        }
        if self.help && !ctrl {
            self.help = false;
            return false;
        }
        match (k.code, ctrl) {
            (KeyCode::Char('c'), true) | (KeyCode::Char('q'), true) => {
                if self.busy() && self.quit_armed.is_none() {
                    self.quit_armed = Some(Instant::now());
                    self.toast(Level::Warn, "agents are still working · press ctrl-c again to quit");
                    return false;
                }
                return true;
            }
            (KeyCode::Char('o'), true) => self.open_page(),
            (KeyCode::Char('f'), true) => self.fx = !self.fx,
            (KeyCode::Char('u'), true) => self.set_input(String::new()),
            (KeyCode::Char('w'), true) => {
                let b = self.byte_at(self.cursor);
                let head = self.input[..b].trim_end();
                let cut = head.rfind(' ').map(|i| i + 1).unwrap_or(0);
                let tail = self.input[b..].to_string();
                self.input = format!("{}{}", &self.input[..cut], tail);
                self.cursor = self.input[..cut].chars().count();
            }
            (KeyCode::F(1), _) => self.help = true,
            (KeyCode::Esc, _) => {
                if !self.input.is_empty() {
                    self.set_input(String::new());
                } else if self.zoom {
                    self.zoom = false;
                    self.scroll = 0;
                } else {
                    self.selected = None;
                }
            }
            (KeyCode::Enter, _) => {
                if self.input.trim().is_empty() {
                    if self.selected.is_some() {
                        self.zoom = !self.zoom;
                        self.scroll = 0;
                    }
                } else {
                    let s = self.suggestions();
                    // complete a half-typed command or name first, like a shell would
                    if let Some((c, _)) = s.get(self.sugg_sel) {
                        if !self.input.contains(' ') && (self.input.starts_with('/') || self.input.starts_with('@')) {
                            let c = c.clone();
                            if c.ends_with(' ') || self.input.starts_with('@') {
                                self.set_input(c);
                                return false;
                            }
                            self.set_input(c);
                        }
                    }
                    let line = std::mem::take(&mut self.input);
                    self.cursor = 0;
                    self.sugg_sel = 0;
                    self.hist_pos = None;
                    if self.history.last() != Some(&line) {
                        self.history.push(line.clone());
                    }
                    return self.submit(line.trim());
                }
            }
            (KeyCode::Tab, _) => {
                let s = self.suggestions();
                if let Some((c, _)) = s.get(self.sugg_sel.min(s.len().saturating_sub(1))) {
                    self.set_input(c.clone());
                } else if self.input.is_empty() {
                    let n = self.agents.len();
                    self.selected = match self.selected {
                        None => Some(0),
                        Some(i) if i + 1 < n => Some(i + 1),
                        _ => None,
                    };
                    self.scroll = 0;
                }
            }
            (KeyCode::BackTab, _) => {
                let n = self.agents.len();
                self.selected = match self.selected {
                    None => Some(n - 1),
                    Some(0) => None,
                    Some(i) => Some(i - 1),
                };
                self.scroll = 0;
            }
            (KeyCode::Up, _) => {
                let s = self.suggestions();
                if !s.is_empty() {
                    self.sugg_sel = (self.sugg_sel + s.len() - 1) % s.len();
                } else if !self.history.is_empty() {
                    let p = match self.hist_pos {
                        None => self.history.len() - 1,
                        Some(p) => p.saturating_sub(1),
                    };
                    self.hist_pos = Some(p);
                    let h = self.history[p].clone();
                    self.set_input(h);
                    self.hist_pos = Some(p);
                }
            }
            (KeyCode::Down, _) => {
                let s = self.suggestions();
                if !s.is_empty() {
                    self.sugg_sel = (self.sugg_sel + 1) % s.len();
                } else if let Some(p) = self.hist_pos {
                    if p + 1 < self.history.len() {
                        let h = self.history[p + 1].clone();
                        self.set_input(h);
                        self.hist_pos = Some(p + 1);
                    } else {
                        self.set_input(String::new());
                    }
                }
            }
            (KeyCode::PageUp, _) => self.scroll += 10,
            (KeyCode::PageDown, _) => self.scroll = self.scroll.saturating_sub(10),
            (KeyCode::Left, _) => self.cursor = self.cursor.saturating_sub(1),
            (KeyCode::Right, _) => self.cursor = (self.cursor + 1).min(self.input.chars().count()),
            (KeyCode::Home, _) => self.cursor = 0,
            (KeyCode::End, _) => self.cursor = self.input.chars().count(),
            (KeyCode::Backspace, _) => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    let b = self.byte_at(self.cursor);
                    self.input.remove(b);
                    self.sugg_sel = 0;
                }
            }
            (KeyCode::Delete, _) => {
                if self.cursor < self.input.chars().count() {
                    let b = self.byte_at(self.cursor);
                    self.input.remove(b);
                }
            }
            (KeyCode::Char(c), false) => {
                let mut b = [0u8; 4];
                self.insert(c.encode_utf8(&mut b));
            }
            _ => {}
        }
        false
    }

    fn send(&self, c: Cmd) {
        let _ = self.cmd.send(c);
    }

    /// Run a line from the command bar. Returns true to quit.
    fn submit(&mut self, line: &str) -> bool {
        if line.is_empty() {
            return false;
        }
        if let Some(cmd) = line.strip_prefix('/') {
            let mut parts = cmd.split_whitespace();
            let name = parts.next().unwrap_or("").to_lowercase();
            let args: Vec<&str> = parts.collect();
            match name.as_str() {
                "help" | "h" | "?" => self.help = true,
                "quit" | "q" | "exit" => return true,
                "open" | "o" => self.open_page(),
                "fx" => self.fx = !self.fx,
                "review" => {
                    self.send(Cmd::Review);
                    self.toast(Level::Info, "lead is reviewing the page");
                }
                "stop" => {
                    let who = args.first().map(|s| s.to_string());
                    if let Some(w) = &who {
                        if self.agent_index(w).is_none() {
                            self.toast(Level::Warn, format!("no agent called {w}"));
                            return false;
                        }
                    }
                    self.send(Cmd::Stop(who.clone()));
                    self.toast(Level::Warn, format!("stopping {}", who.unwrap_or_else(|| "everyone".into())));
                }
                "budget" => match args.first().and_then(|a| a.trim_start_matches('$').parse::<f64>().ok()) {
                    Some(b) if b >= 0.0 => {
                        self.budget = b;
                        self.budget_tripped = false;
                        if b == 0.0 {
                            self.toast(Level::Good, "no spending cap");
                        } else {
                            self.toast(Level::Good, format!("spending cap ${b:.2} · spent ${:.2} so far", self.spent()));
                        }
                    }
                    _ => self.toast(Level::Info, format!("spent ${:.2} this session · usage: /budget 5", self.spent())),
                },
                "add" | "hire" => {
                    let Some(p) = args.first().and_then(|a| Provider::parse(a)) else {
                        self.toast(Level::Warn, "usage: /add claude|agy|codex [model]");
                        return false;
                    };
                    let model = args.get(1).map(|s| s.to_string()).unwrap_or_else(|| p.models().remove(0));
                    self.send(Cmd::Add(p, model));
                }
                _ => self.toast(Level::Warn, format!("unknown command /{name} · /help lists them")),
            }
            return false;
        }
        if self.over_budget() {
            self.toast(Level::Bad, format!("over budget (${:.2}) · /budget <usd> to raise the cap", self.budget));
            return false;
        }
        // "@name text", or whoever is selected, or the lead
        let (to, text) = match line.strip_prefix('@') {
            Some(rest) => {
                let (name, text) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                let to = if name.eq_ignore_ascii_case("all") {
                    "all".to_string()
                } else if let Some(i) = self.agent_index(name) {
                    self.agents[i].spec.name.clone()
                } else {
                    self.toast(Level::Warn, format!("no agent called {name}"));
                    return false;
                };
                (to, text.trim().to_string())
            }
            None => (self.target(), line.to_string()),
        };
        if text.is_empty() {
            return false;
        }
        if to == "all" {
            for a in self.agents.iter_mut().skip(1) {
                a.note(Kind::You, format!("you → all: {text}"));
            }
        } else if let Some(i) = self.agent_index(&to) {
            self.agents[i].note(Kind::You, format!("you: {text}"));
        }
        self.message("you".into(), to.clone(), text.clone());
        self.send(Cmd::Say { to, text });
        false
    }
}
