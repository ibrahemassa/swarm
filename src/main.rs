// auto-deref suggestions break inference on rng.pick(); demo helpers take many knobs on purpose
#![allow(clippy::explicit_auto_deref, clippy::too_many_arguments, clippy::needless_range_loop)]

mod app;
mod demo;
mod demo_code;
mod fx;
mod orchestrator;
mod project;
mod prompts;
mod providers;
mod server;
mod session;
mod setup;
mod ui;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::Parser;
use ratatui::crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::DefaultTerminal;

use app::{App, Launch};
use orchestrator::{Config, State};
use project::Kind;
use providers::{AgentSpec, Provider};
use setup::{Choice, Open};

/// Run a swarm of Claude Code / Codex / Antigravity agents that build a web page, or any software
/// project, together and keep taking orders, on a dashboard that looks unreasonably busy. Run with
/// no arguments for the launcher.
#[derive(Parser, Debug, Clone)]
#[command(name = "swarm", version)]
pub struct Args {
    /// What to build or change, e.g. "landing page for a neon ramen bar". Leave out to open the launcher.
    pub task: Option<String>,
    /// Work on any kind of project (new or existing, see --out) instead of a single web page
    #[arg(long)]
    pub code: bool,
    /// Command that builds and tests the project; swarm runs it after every round and sends
    /// failures back to the team (code mode, e.g. "cargo test")
    #[arg(long)]
    pub check: Option<String>,
    /// Number of Claude Code workers
    #[arg(long, default_value_t = 4)]
    pub claude: usize,
    /// Number of Antigravity (agy) workers
    #[arg(long, default_value_t = 2)]
    pub agy: usize,
    /// Number of Codex workers (default: 1 if codex is logged in, else 0)
    #[arg(long)]
    pub codex: Option<usize>,
    /// Model for Claude workers
    #[arg(long, default_value = "sonnet")]
    pub model: String,
    /// Model for Antigravity workers
    #[arg(long, default_value = "gemini-3.8-flash-high")]
    pub agy_model: String,
    /// Model for Codex workers (empty = codex default)
    #[arg(long, default_value = "")]
    pub codex_model: String,
    /// Who leads the swarm: claude, agy or codex
    #[arg(long, default_value = "claude")]
    pub lead: String,
    /// Model for the lead (default: opus for claude, the family default otherwise)
    #[arg(long)]
    pub lead_model: Option<String>,
    /// Project folder (default: ./swarm-<task-slug>). Without a task: resume the project in it.
    #[arg(long, short)]
    pub out: Option<PathBuf>,
    /// Resume the project in --out, the current folder, or the most recent one
    #[arg(long, short = 'c')]
    pub resume: bool,
    /// Spending cap for this session in USD (Claude-reported cost; 0 = none)
    #[arg(long, default_value_t = 0.0)]
    pub budget: f64,
    /// Skip the lead's review pass after the first build
    #[arg(long)]
    pub no_review: bool,
    /// Open the live preview right away and watch the page build
    #[arg(long)]
    pub live: bool,
    /// Don't open the page in the browser when it ships
    #[arg(long)]
    pub no_open: bool,
    /// Scripted agents, real animations, zero tokens
    #[arg(long)]
    pub demo: bool,
}

/// Everything the dashboard needs to start, from flags, the launcher, or a saved project.
struct Start {
    task: String,
    dir: PathBuf,
    roster: Vec<AgentSpec>,
    review: bool,
    live: bool,
    auto_open: bool,
    demo: bool,
    budget: f64,
    relief_model: String,
    resume: Option<State>,
    kind: Kind,
    check: Option<String>,
}

fn from_args(a: &Args, task: String) -> std::io::Result<Start> {
    let lead_p = Provider::parse(&a.lead).unwrap_or(Provider::Claude);
    let lead_model =
        a.lead_model.clone().unwrap_or_else(|| if lead_p == Provider::Claude { "opus".into() } else { lead_p.models().remove(0) });
    let codex_n = a.codex.unwrap_or(if orchestrator::codex_ready() { 1 } else { 0 });
    let mut roster = vec![AgentSpec { name: orchestrator::name_word(lead_p, &lead_model), provider: lead_p, model: lead_model }];
    for (p, n, m) in
        [(Provider::Claude, a.claude, &a.model), (Provider::Agy, a.agy, &a.agy_model), (Provider::Codex, codex_n, &a.codex_model)]
    {
        let word = orchestrator::name_word(p, m);
        for k in 1..=n {
            roster.push(AgentSpec { name: format!("{word}-{k}"), provider: p, model: m.clone() });
        }
    }
    let kind = if a.code { Kind::Code } else { Kind::Page };
    let dir = a.out.clone().unwrap_or_else(|| orchestrator::default_dir(&task));
    let dir = orchestrator::prepare_dir(&dir, true, kind)?;
    Ok(Start {
        task,
        dir,
        roster,
        review: !a.no_review,
        live: a.live,
        auto_open: !a.no_open && !a.live,
        demo: a.demo,
        budget: a.budget,
        relief_model: a.model.clone(),
        resume: None,
        kind,
        check: a.check.clone().filter(|c| !c.trim().is_empty()),
    })
}

fn from_setup(s: setup::Setup) -> std::io::Result<Start> {
    setup::save_prefs(&s);
    let kind = s.kind;
    let dir = orchestrator::prepare_dir(&s.dir_path(), true, kind)?;
    Ok(Start {
        task: s.task.trim().to_string(),
        roster: s.roster(),
        review: s.review,
        live: s.open == Open::Live,
        auto_open: s.open == Open::End,
        demo: s.demo,
        budget: s.budget,
        relief_model: if s.model(Provider::Claude).is_empty() { "sonnet".into() } else { s.model(Provider::Claude).to_string() },
        resume: None,
        check: (kind == Kind::Code).then(|| s.check.trim().to_string()).filter(|c| !c.is_empty()),
        kind,
        dir,
    })
}

fn from_saved(dir: PathBuf, a: &Args) -> std::io::Result<Option<Start>> {
    let Some(st) = State::load(&dir) else { return Ok(None) };
    let dir = orchestrator::prepare_dir(&dir, false, st.kind)?;
    Ok(Some(Start {
        task: st.task.clone(),
        roster: st.agents.iter().map(|a| a.spec.clone()).collect(),
        review: st.review,
        live: false,
        auto_open: false,
        demo: false,
        budget: a.budget,
        relief_model: a.model.clone(),
        dir,
        kind: st.kind,
        check: st.check.clone(),
        resume: Some(st),
    }))
}

/// Decide what to run: flags first, then a saved project, then the launcher.
fn choose(a: &Args, term: &mut DefaultTerminal) -> std::io::Result<Option<Start>> {
    if let Some(t) = &a.task {
        return from_args(a, t.clone()).map(Some);
    }
    if a.demo {
        let task = if a.code { demo_code::TASK } else { demo::TASK };
        return from_args(a, task.to_string()).map(Some);
    }
    if a.resume || a.out.is_some() {
        let cwd = std::env::current_dir().ok().filter(|d| State::path(d).exists());
        let dir = a.out.clone().or(cwd).or_else(setup::latest_project);
        if let Some(d) = dir {
            if let Some(s) = from_saved(d.clone(), a)? {
                return Ok(Some(s));
            }
        }
    }
    let mut initial = setup::load_prefs();
    if a.budget > 0.0 {
        initial.budget = a.budget;
    }
    loop {
        match setup::run(term, initial.clone())? {
            Choice::Quit => return Ok(None),
            Choice::Launch(s) => return from_setup(s).map(Some),
            Choice::Resume(d) => {
                if let Some(s) = from_saved(d, a)? {
                    return Ok(Some(s));
                }
            }
        }
    }
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();
    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableBracketedPaste);
    let result = run(&args, &mut terminal);
    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    match result {
        Ok(Some(summary)) => println!("{summary}"),
        Ok(None) => {}
        Err(e) => {
            eprintln!("swarm: {e}");
            std::process::exit(1);
        }
    }
    Ok(())
}

fn run(args: &Args, terminal: &mut DefaultTerminal) -> std::io::Result<Option<String>> {
    let Some(start) = choose(args, terminal)? else { return Ok(None) };
    if !start.demo {
        setup::remember(&start.dir);
    }
    let preflight = orchestrator::preflight(&start.roster, &start.dir, start.demo);

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let cfg = Config {
        task: start.task.clone(),
        dir: start.dir.clone(),
        roster: start.roster.clone(),
        review: start.review,
        live: start.live,
        relief_model: start.relief_model.clone(),
        resume: start.resume.clone(),
        kind: start.kind,
        check: start.check.clone(),
    };
    if start.demo {
        rt.spawn(demo::run(cfg, tx, cmd_rx));
    } else {
        rt.spawn(orchestrator::run(cfg, tx, cmd_rx));
    }

    let mut app = App::new(
        Launch {
            task: start.task,
            dir: start.dir,
            roster: start.roster,
            preflight,
            demo: start.demo,
            auto_open: start.auto_open,
            budget: start.budget,
            kind: start.kind,
            check: start.check,
        },
        cmd_tx,
    );
    let frame = Duration::from_millis(33);
    let result = (|| -> std::io::Result<()> {
        loop {
            let started = Instant::now();
            while let Ok(ev) = rx.try_recv() {
                app.apply(ev);
            }
            app.tick();
            terminal.draw(|f| ui::draw(f, &mut app))?;
            if event::poll(frame.saturating_sub(started.elapsed()))? {
                match event::read()? {
                    Event::Key(k) if k.kind == KeyEventKind::Press => {
                        if app.key(k) {
                            break;
                        }
                    }
                    Event::Paste(s) => app.insert(&s),
                    _ => {}
                }
            }
        }
        Ok(())
    })();
    // dropping the runtime kills every agent process (kill_on_drop); sessions stay resumable
    rt.shutdown_timeout(Duration::from_millis(400));
    result?;
    Ok(Some(app.summary()))
}
