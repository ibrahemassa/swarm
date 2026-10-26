mod app;
mod demo;
mod fx;
mod orchestrator;
mod providers;
mod ui;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::Parser;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use app::App;

/// Run a swarm of Claude Code / Codex / Antigravity agents that build a web page together,
/// on a dashboard that looks unreasonably busy.
#[derive(Parser, Debug, Clone)]
#[command(name = "swarm", version)]
pub struct Args {
    /// What to build, e.g. "landing page for a neon ramen bar"
    pub task: Option<String>,
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
    /// Model for a Claude lead
    #[arg(long, default_value = "opus")]
    pub lead_model: String,
    /// Output folder (default: ./swarm-<task-slug>)
    #[arg(long, short)]
    pub out: Option<PathBuf>,
    /// Skip the lead's final review pass
    #[arg(long)]
    pub no_review: bool,
    /// Open the page right away and auto-refresh it while the agents build
    #[arg(long)]
    pub live: bool,
    /// Don't open the finished page in the browser
    #[arg(long)]
    pub no_open: bool,
    /// Scripted agents, real animations, zero tokens
    #[arg(long)]
    pub demo: bool,
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();
    let task = match (&args.task, args.demo) {
        (Some(t), _) => t.clone(),
        (None, true) => demo::TASK.to_string(),
        (None, false) => {
            eprintln!("usage: swarm \"what to build\"     (or: swarm --demo)");
            std::process::exit(2);
        }
    };
    let dir = orchestrator::prepare_dir(args.out.clone(), &task)?;
    let roster = orchestrator::roster(&args);
    let preflight = orchestrator::preflight(&roster, &dir, args.demo);

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let cfg = orchestrator::Config {
        task: task.clone(),
        dir: dir.clone(),
        roster: roster.clone(),
        review: !args.no_review,
        live: args.live,
        relief_model: args.model.clone(),
    };
    if args.demo {
        rt.spawn(demo::run(cfg, tx));
    } else {
        rt.spawn(orchestrator::run(cfg, tx));
    }

    let mut app = App::new(task, dir, roster, preflight, args.demo, !args.no_open);
    let mut terminal = ratatui::init();
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
                if let Event::Key(k) = event::read()? {
                    if k.kind != KeyEventKind::Press {
                        continue;
                    }
                    match k.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => break,
                        KeyCode::Char('o') => app.open_page(),
                        KeyCode::Char('f') => app.fx = !app.fx,
                        _ => app.skip_boot(),
                    }
                }
            }
        }
        Ok(())
    })();
    ratatui::restore();
    // dropping the runtime kills any agent still running (kill_on_drop)
    rt.shutdown_timeout(Duration::from_millis(300));
    result?;
    println!("{}", app.summary());
    Ok(())
}
