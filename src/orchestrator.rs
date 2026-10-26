//! The real thing: a lead plans the page, workers build sections in parallel, they message each
//! other through files in `.swarm/`, failed agents get replaced, the lead reviews, page ships.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinSet;

use crate::app::{AgentEv, Ev, Level, Phase, PlanItem};
use crate::providers::{self, AgentSpec, Parser, Provider};
use crate::Args;

pub type Tx = UnboundedSender<Ev>;

#[derive(Clone)]
pub struct Config {
    pub task: String,
    pub dir: PathBuf,
    pub roster: Vec<AgentSpec>,
    pub review: bool,
    pub live: bool,
    pub relief_model: String,
}

#[derive(Clone)]
pub struct Page {
    pub title: String,
    pub ids: Vec<String>,
    pub live: bool,
}

/// State the background watchers share with the run.
#[derive(Clone, Default)]
pub struct Shared {
    pub page: Arc<Mutex<Option<Page>>>,
    pub names: Arc<Mutex<Vec<String>>>,
    pub mail: Arc<Mutex<Vec<(String, String, String)>>>,
}

const PLAN_PROMPT: &str = r#"You are %LEAD%, the lead engineer of a swarm of AI coding agents that will build ONE static web page together, in parallel.

User request: "%TASK%"
Project folder (absolute path): %DIR%
Your team (each member gets exactly one task): %TEAM%

How the page gets assembled: index.html is generated automatically. It links styles/base.css, then for every task id in your order it links styles/<id>.css, inlines sections/<id>.html into <body>, and loads scripts/<id>.js if that file exists.

Do exactly two things:
1. Create %DIR%/styles/base.css: the shared design system. CSS custom properties on :root (palette, fonts, spacing scale, radius), an @import for any Google Fonts you pick, a modern reset, body and heading typography, and a .container helper. Make the art direction bold and specific to the request, never generic.
2. Reply with ONLY this JSON, no markdown fence and no commentary:
{"title": "<page title>", "vibe": "<one sentence of art direction>", "tasks": [{"agent": "<team member name>", "id": "<kebab-case section id>", "brief": "<2-3 sentences: content, layout, interactions>"}]}

Task rules: exactly one task per team member, listed in top-to-bottom page order. Ids are unique, short, kebab-case (hero, features, gallery, pricing, faq, footer...). The first task includes the site nav, the last one is the footer. If the page wants global JS (scroll reveals, cursor effects), give it to the owner of the first section.
Do not create any other files.
%TOOLS%"#;

const WORKER_PROMPT: &str = r#"You are %NAME%, one of %N% AI agents building a single web page in parallel. Your lead is %LEAD%.
Page: "%TITLE%" (%VIBE%)
Original request: "%TASK%"
Project folder (absolute path): %DIR%

YOUR TASK, section "%ID%": %BRIEF%

The full page, top to bottom: %ORDER%

Files you own. Create only these:
- %DIR%/sections/%ID%.html: an HTML fragment (no <html>, <head> or <body>) with ONE root element whose id="%ID%" (a <section>, or <header>/<nav>/<footer> if that fits better).
- %DIR%/styles/%ID%.css: every selector scoped under #%ID% so you never clash with teammates.
- %DIR%/scripts/%ID%.js: optional, only if your section needs JS. Wrap it in an IIFE.
index.html is assembled automatically: never create or edit it, and never touch other agents' files.
Start by reading %DIR%/styles/base.css and use its CSS variables everywhere.

Team chat: to message someone, create a NEW file %DIR%/.swarm/outbox/%NAME%-<n>.msg (n = 1, 2, 3...). First line: "to: <name>" where name is a teammate, %LEAD%, or all. Then one or two short sentences. Messages for you are appended to %DIR%/.swarm/inbox/%NAME%.md. Message teammates when it matters to them: anchor ids the nav should link to, a shared visual motif, a variable you rely on. Before you finish, read your inbox (it may not exist yet, that's fine) and adapt, then message %LEAD% a one-line summary of what you shipped.

Work fast and decisively. Make it look premium: strong typography, responsive down to 375px wide, tasteful motion that respects prefers-reduced-motion. End with a one-line summary.
%TOOLS%"#;

const REVIEW_PROMPT: &str = r#"You are %LEAD%, lead of the swarm. Every agent has finished. Project folder (absolute path): %DIR%
index.html is generated automatically, do not edit it. It links styles/base.css, then styles/<id>.css, inlines sections/<id>.html and loads scripts/<id>.js for these ids, in order: %IDS%.

Messages your team sent you:
%MAIL%

Review the files quickly and fix only real problems by editing the section, style and script files directly: nav links must point to ids that exist, selectors stay scoped to their section, base.css variables are used consistently, no broken markup, nothing overflows at 375px wide. Leave sections that are fine alone. Reply with a two-line summary of what you changed.
%TOOLS%"#;

/// Headless agents can't answer permission prompts, so steer each one to tools that won't be denied.
fn tool_note(p: Provider) -> &'static str {
    match p {
        Provider::Claude => "Shell commands are not available in this run. Use only Read, Write, Edit, Glob and Grep, with absolute paths.",
        Provider::Agy => "IMPORTANT: shell commands are blocked in this session (run_command is always denied). Use only your file tools (read, write and edit files) with absolute paths.",
        Provider::Codex => "",
    }
}

const FALLBACK_BASE_CSS: &str = r#"@import url('https://fonts.googleapis.com/css2?family=Space+Grotesk:wght@400;500;700&display=swap');
:root{--bg:#0b0b12;--surface:#151522;--ink:#f1f1f7;--muted:#9a9ab0;--accent:#7c5cff;--accent-2:#00e0c6;--line:#262638;--radius:16px;--space:clamp(64px,10vw,128px);--font:'Space Grotesk',system-ui,sans-serif}
*,*::before,*::after{box-sizing:border-box;margin:0;padding:0}
html{scroll-behavior:smooth}
body{background:var(--bg);color:var(--ink);font-family:var(--font);line-height:1.6;-webkit-font-smoothing:antialiased}
img{max-width:100%;display:block}
a{color:inherit}
h1,h2,h3{line-height:1.08;letter-spacing:-.02em}
.container{width:min(1120px,92vw);margin-inline:auto}
"#;

const FALLBACK_IDS: &[&str] = &["hero", "features", "showcase", "testimonials", "pricing", "faq", "cta", "gallery", "stats", "team"];

fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    vars.iter().fold(template.to_string(), |s, (k, v)| s.replace(k, v))
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 32 {
            break;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "page".into()
    } else {
        out
    }
}

pub fn model_word(model: &str) -> String {
    let m = model.to_lowercase();
    ["fable", "opus", "sonnet", "haiku"]
        .iter()
        .find(|w| m.contains(*w))
        .map(|w| w.to_string())
        .unwrap_or_else(|| "claude".into())
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

pub fn codex_ready() -> bool {
    home().join(".codex/auth.json").exists()
        || std::env::var_os("OPENAI_API_KEY").is_some()
        || std::env::var_os("CODEX_API_KEY").is_some()
}

pub fn prepare_dir(out: Option<PathBuf>, task: &str) -> std::io::Result<PathBuf> {
    let dir = out.unwrap_or_else(|| PathBuf::from(format!("swarm-{}", slug(task))));
    std::fs::create_dir_all(&dir)?;
    let dir = dir.canonicalize()?;
    // .swarm/ is our own scratch space; stale mail from a previous run would replay
    let meta = dir.join(".swarm");
    if meta.exists() {
        std::fs::remove_dir_all(&meta)?;
    }
    for sub in [".swarm/outbox", ".swarm/inbox", "sections", "styles", "scripts"] {
        std::fs::create_dir_all(dir.join(sub))?;
    }
    Ok(dir)
}

pub fn roster(args: &Args) -> Vec<AgentSpec> {
    let lead = match args.lead.to_lowercase().as_str() {
        "agy" | "antigravity" | "gemini" => AgentSpec { name: "gemini".into(), provider: Provider::Agy, model: args.agy_model.clone() },
        "codex" => AgentSpec { name: "codex".into(), provider: Provider::Codex, model: args.codex_model.clone() },
        _ => AgentSpec { name: model_word(&args.lead_model), provider: Provider::Claude, model: args.lead_model.clone() },
    };
    let mut v = vec![lead];
    let word = model_word(&args.model);
    for i in 1..=args.claude {
        v.push(AgentSpec { name: format!("{word}-{i}"), provider: Provider::Claude, model: args.model.clone() });
    }
    let codex_n = args.codex.unwrap_or(if codex_ready() { 1 } else { 0 });
    for i in 1..=codex_n {
        v.push(AgentSpec { name: format!("codex-{i}"), provider: Provider::Codex, model: args.codex_model.clone() });
    }
    for i in 1..=args.agy {
        v.push(AgentSpec { name: format!("gemini-{i}"), provider: Provider::Agy, model: args.agy_model.clone() });
    }
    v
}

pub fn preflight(roster: &[AgentSpec], dir: &Path, demo: bool) -> Vec<(String, String, bool)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for s in roster {
        if !seen.insert(s.provider) {
            continue;
        }
        let v = std::process::Command::new(s.provider.bin()).arg("--version").output();
        let (status, ok) = match v {
            Ok(o) if o.status.success() => {
                let ver = String::from_utf8_lossy(&o.stdout);
                let ver = ver.lines().next().unwrap_or("").split_whitespace().find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit())).unwrap_or("?").to_string();
                (format!("v{ver} · LINKED"), true)
            }
            _ => ("NOT FOUND".to_string(), false),
        };
        let (status, ok) = if s.provider == Provider::Codex && !codex_ready() && !demo {
            ("NO AUTH · run codex login".to_string(), false)
        } else {
            (status, ok)
        };
        out.push((s.provider.label().to_string(), status, ok));
    }
    out.push(("swarm".into(), format!("1 lead + {} workers · ARMED", roster.len().saturating_sub(1)), true));
    let d = dir.to_string_lossy().replace(&home().to_string_lossy().to_string(), "~");
    out.push(("workspace".into(), trunc(&d, 34), true));
    if demo {
        out.push(("mode".into(), "DEMO · zero tokens burned".into(), true));
    }
    out
}

fn log(tx: &Tx, l: Level, s: impl Into<String>) {
    let _ = tx.send(Ev::Log(l, s.into()));
}

pub fn msg(tx: &Tx, from: &str, to: &str, text: impl Into<String>) {
    let _ = tx.send(Ev::Msg { from: from.into(), to: to.into(), text: text.into() });
}

pub async fn run_agent(idx: usize, spec: AgentSpec, prompt: String, dir: PathBuf, tx: Tx) -> Result<String, String> {
    let _ = tx.send(Ev::Agent(idx, AgentEv::Start));
    let mut child = match providers::command(&spec, &prompt, &dir).spawn() {
        Ok(c) => c,
        Err(e) => {
            let m = format!("can't start {}: {e}", spec.provider.bin());
            let _ = tx.send(Ev::Agent(idx, AgentEv::Fail(m.clone())));
            return Err(m);
        }
    };
    let stdout = child.stdout.take().expect("stdout is piped");
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let err_task = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s).await;
        s
    });
    let mut parser = Parser::new(spec.provider, &dir);
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        for ev in parser.feed(&line) {
            let _ = tx.send(Ev::Agent(idx, ev));
        }
    }
    let ok = child.wait().await.map(|s| s.success()).unwrap_or(false);
    let stderr = err_task.await.unwrap_or_default();
    let result = parser.finish(ok, &stderr);
    let ev = match &result {
        Ok(t) => AgentEv::Done(t.clone()),
        Err(e) => AgentEv::Fail(trunc(e, 140)),
    };
    let _ = tx.send(Ev::Agent(idx, ev));
    result
}

fn sanitize_id(s: &str) -> String {
    let s = slug(s);
    s.chars().take(24).collect::<String>().trim_matches('-').to_string()
}

fn parse_plan(text: &str, workers: &[(usize, AgentSpec)]) -> Option<(String, String, Vec<PlanItem>)> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    let v: Value = serde_json::from_str(text.get(start..=end)?).ok()?;
    let title = v["title"].as_str().unwrap_or("Swarm build").to_string();
    let vibe = v["vibe"].as_str().unwrap_or("").to_string();
    let mut items = Vec::new();
    let mut used_agents = HashSet::new();
    let mut used_ids = HashSet::new();
    for t in v["tasks"].as_array()? {
        let Some(agent) = t["agent"].as_str() else { continue };
        let Some((_, spec)) = workers.iter().find(|(_, s)| s.name.eq_ignore_ascii_case(agent.trim())) else { continue };
        if !used_agents.insert(spec.name.clone()) {
            continue;
        }
        let mut id = sanitize_id(t["id"].as_str().unwrap_or("section"));
        if id.is_empty() {
            id = "section".into();
        }
        let base = id.clone();
        let mut k = 2;
        while !used_ids.insert(id.clone()) {
            id = format!("{base}-{k}");
            k += 1;
        }
        items.push(PlanItem { id, agent: spec.name.clone(), brief: t["brief"].as_str().unwrap_or("").to_string() });
    }
    if items.is_empty() {
        return None;
    }
    // anyone the lead forgot still gets work
    let mut spare = FALLBACK_IDS.iter().filter(|i| !used_ids.contains(**i));
    for (_, s) in workers {
        if used_agents.contains(&s.name) {
            continue;
        }
        let id = spare.next().map(|s| s.to_string()).unwrap_or_else(|| format!("extra-{}", items.len() + 1));
        let brief = format!("Add a {id} section that fits the rest of the page.");
        let at = items.len().saturating_sub(1).max(1).min(items.len());
        items.insert(at, PlanItem { id, agent: s.name.clone(), brief });
    }
    Some((title, vibe, items))
}

fn fallback_plan(task: &str, workers: &[(usize, AgentSpec)]) -> (String, String, Vec<PlanItem>) {
    let n = workers.len();
    let mut ids: Vec<String> = FALLBACK_IDS.iter().take(n.saturating_sub(1)).map(|s| s.to_string()).collect();
    while ids.len() + 1 < n {
        ids.push(format!("extra-{}", ids.len() + 1));
    }
    if n > 0 {
        ids.push("footer".into());
    }
    if n == 1 {
        ids = vec!["page".into()];
    }
    let items = workers
        .iter()
        .zip(ids)
        .map(|((_, s), id)| PlanItem { brief: format!("Build the {id} section for: {task}"), id, agent: s.name.clone() })
        .collect();
    (trunc(task, 60), "bold, modern, dark".into(), items)
}

fn ensure_base_css(dir: &Path) {
    let p = dir.join("styles/base.css");
    if !p.exists() {
        let _ = std::fs::write(p, FALLBACK_BASE_CSS);
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Stitch the agents' fragments into index.html. With `live`, the page auto-refreshes and shows
/// placeholders for sections still being built.
pub fn assemble(dir: &Path, page: &Page) {
    let mut links = String::new();
    let mut body = String::new();
    let mut scripts = String::new();
    for id in &page.ids {
        if dir.join(format!("styles/{id}.css")).exists() {
            links += &format!("  <link rel=\"stylesheet\" href=\"styles/{id}.css\">\n");
        }
        match std::fs::read_to_string(dir.join(format!("sections/{id}.html"))) {
            Ok(s) => {
                body += s.trim();
                body += "\n";
            }
            Err(_) if page.live => {
                body += &format!(
                    "<section style=\"padding:72px 24px;text-align:center;font:14px ui-monospace,monospace;opacity:.55\">⧗ #{id}: agent at work…</section>\n"
                );
            }
            Err(_) => {}
        }
        if dir.join(format!("scripts/{id}.js")).exists() {
            scripts += &format!("  <script src=\"scripts/{id}.js\" defer></script>\n");
        }
    }
    let refresh = if page.live { "  <meta http-equiv=\"refresh\" content=\"4\">\n" } else { "" };
    let html = format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n  <meta charset=\"utf-8\">\n  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n  <title>{}</title>\n{refresh}  <link rel=\"stylesheet\" href=\"styles/base.css\">\n{links}{scripts}</head>\n<body>\n{body}</body>\n</html>\n",
        escape(&page.title)
    );
    let path = dir.join("index.html");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(html.as_str()) {
        let _ = std::fs::write(&path, html);
    }
}

pub fn open(path: &Path) {
    let _ = std::process::Command::new("xdg-open")
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<(PathBuf, SystemTime, u64)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let Ok(m) = e.metadata() else { continue };
        if m.is_dir() {
            if depth < 4 {
                walk(&p, depth + 1, out);
            }
        } else {
            out.push((p, m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()));
        }
    }
}

/// Polls the project folder: reports new/changed files and re-assembles index.html.
pub async fn watch_files(dir: PathBuf, tx: Tx, shared: Shared) {
    let mut seen: HashMap<PathBuf, (SystemTime, u64)> = HashMap::new();
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let mut files = Vec::new();
        walk(&dir, 0, &mut files);
        let mut src_changed = false;
        for (path, mtime, len) in files {
            let prev = seen.get(&path).copied();
            if prev == Some((mtime, len)) {
                continue;
            }
            seen.insert(path.clone(), (mtime, len));
            let rel = path.strip_prefix(&dir).map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
            if ["sections/", "styles/", "scripts/"].iter().any(|p| rel.starts_with(p)) {
                src_changed = true;
            }
            let _ = tx.send(Ev::File { path: rel, bytes: len, new: prev.is_none() });
        }
        if src_changed {
            let page = shared.page.lock().unwrap().clone();
            if let Some(p) = page {
                assemble(&dir, &p);
            }
        }
    }
}

fn parse_mail(body: &str, names: &[String]) -> (String, String) {
    let lead = names.first().cloned().unwrap_or_else(|| "lead".into());
    let mut lines = body.lines();
    let first = lines.next().unwrap_or("").trim();
    let (to, text) = if first.to_lowercase().starts_with("to:") {
        let to = first[3..].trim().trim_start_matches('@').trim().to_string();
        (to, lines.collect::<Vec<_>>().join(" "))
    } else {
        (lead.clone(), body.to_string())
    };
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let to = if to.eq_ignore_ascii_case("all") {
        "all".to_string()
    } else {
        names.iter().find(|n| n.eq_ignore_ascii_case(&to)).cloned().unwrap_or(lead)
    };
    (to, trunc(&text, 280))
}

/// Picks up `.swarm/outbox/*.msg`, animates them, and delivers them into recipients' inboxes.
pub async fn watch_outbox(dir: PathBuf, tx: Tx, shared: Shared) {
    let outbox = dir.join(".swarm/outbox");
    let mut done: HashSet<String> = HashSet::new();
    loop {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let Ok(rd) = std::fs::read_dir(&outbox) else { continue };
        let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
        files.sort();
        for p in files {
            let fname = p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
            if done.contains(&fname) {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(&p) else { continue };
            if body.trim().is_empty() {
                continue;
            }
            done.insert(fname.clone());
            let names = shared.names.lock().unwrap().clone();
            let from = names
                .iter()
                .filter(|n| fname.starts_with(n.as_str()))
                .max_by_key(|n| n.len())
                .cloned()
                .unwrap_or_else(|| fname.trim_end_matches(".msg").to_string());
            let (to, text) = parse_mail(&body, &names);
            let targets: Vec<&String> = if to == "all" { names.iter().filter(|n| **n != from).collect() } else { names.iter().filter(|n| **n == to).collect() };
            for t in targets {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(format!(".swarm/inbox/{t}.md"))) {
                    let _ = writeln!(f, "- {from}: {text}");
                }
            }
            shared.mail.lock().unwrap().push((from.clone(), to.clone(), text.clone()));
            msg(&tx, &from, &to, text);
        }
    }
}

fn worker_prompt(cfg: &Config, lead: &AgentSpec, me: &AgentSpec, it: &PlanItem, items: &[PlanItem], title: &str, vibe: &str) -> String {
    let order = items.iter().map(|p| format!("#{} ({})", p.id, p.agent)).collect::<Vec<_>>().join(" → ");
    let dir = cfg.dir.to_string_lossy();
    let n = items.len().to_string();
    fill(
        WORKER_PROMPT,
        &[
            ("%NAME%", &me.name),
            ("%N%", &n),
            ("%LEAD%", &lead.name),
            ("%TITLE%", title),
            ("%VIBE%", vibe),
            ("%TASK%", &cfg.task),
            ("%DIR%", &dir),
            ("%ID%", &it.id),
            ("%BRIEF%", &it.brief),
            ("%ORDER%", &order),
            ("%TOOLS%", tool_note(me.provider)),
        ],
    )
}

pub async fn run(cfg: Config, tx: Tx) {
    let shared = Shared::default();
    *shared.names.lock().unwrap() = cfg.roster.iter().map(|s| s.name.clone()).collect();
    tokio::spawn(watch_files(cfg.dir.clone(), tx.clone(), shared.clone()));
    tokio::spawn(watch_outbox(cfg.dir.clone(), tx.clone(), shared.clone()));

    let dir_s = cfg.dir.to_string_lossy().to_string();
    let lead = cfg.roster[0].clone();
    let workers: Vec<(usize, AgentSpec)> = cfg.roster.iter().cloned().enumerate().skip(1).collect();
    if workers.is_empty() {
        log(&tx, Level::Bad, "no workers — pass --claude / --agy / --codex");
        return;
    }

    let _ = tx.send(Ev::Phase(Phase::Plan));
    log(&tx, Level::Info, format!("{} is drafting the plan", lead.name));
    let team = workers
        .iter()
        .map(|(_, s)| format!("{} ({} {})", s.name, s.provider.label(), s.model_label()))
        .collect::<Vec<_>>()
        .join(", ");
    let prompt = fill(
        PLAN_PROMPT,
        &[("%LEAD%", &lead.name), ("%TASK%", &cfg.task), ("%DIR%", &dir_s), ("%TEAM%", &team), ("%TOOLS%", tool_note(lead.provider))],
    );
    let planned = run_agent(0, lead.clone(), prompt, cfg.dir.clone(), tx.clone()).await;
    let (title, vibe, mut items) = match planned.as_deref().ok().and_then(|t| parse_plan(t, &workers)) {
        Some(p) => p,
        None => {
            log(&tx, Level::Warn, "plan unreadable, using the default section split");
            fallback_plan(&cfg.task, &workers)
        }
    };
    ensure_base_css(&cfg.dir);
    let ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
    let page = Page { title: title.clone(), ids: ids.clone(), live: cfg.live };
    *shared.page.lock().unwrap() = Some(page.clone());
    assemble(&cfg.dir, &page);
    let plan_json = json!({
        "title": title,
        "vibe": vibe,
        "tasks": items.iter().map(|i| json!({"id": i.id, "agent": i.agent, "brief": i.brief})).collect::<Vec<_>>(),
    });
    let _ = std::fs::write(cfg.dir.join(".swarm/plan.json"), serde_json::to_string_pretty(&plan_json).unwrap_or_default());
    let _ = tx.send(Ev::Plan(title.clone(), items.clone()));
    if cfg.live {
        open(&cfg.dir.join("index.html"));
    }
    for it in &items {
        msg(&tx, &lead.name, &it.agent, format!("you own #{}: {}", it.id, trunc(&it.brief, 110)));
        tokio::time::sleep(Duration::from_millis(140)).await;
    }

    let _ = tx.send(Ev::Phase(Phase::Build));
    let mut set = JoinSet::new();
    for it in &items {
        let Some((idx, spec)) = workers.iter().find(|(_, s)| s.name == it.agent).cloned() else { continue };
        let prompt = worker_prompt(&cfg, &lead, &spec, it, &items, &title, &vibe);
        let (dir, tx2, it2) = (cfg.dir.clone(), tx.clone(), it.clone());
        set.spawn(async move {
            let r = run_agent(idx, spec.clone(), prompt, dir, tx2).await;
            (idx, spec, it2, r, false)
        });
    }

    // a dead agent gets replaced once by a fresh Claude worker
    let word = model_word(&cfg.relief_model);
    let mut relief_n = cfg.roster.iter().filter(|s| s.name.starts_with(&format!("{word}-"))).count();
    let mut next_idx = cfg.roster.len();
    while let Some(joined) = set.join_next().await {
        let Ok((idx, spec, it, res, was_relief)) = joined else { continue };
        // "success" without the section file on disk is still a failure
        let res = res.and_then(|t| {
            if cfg.dir.join(format!("sections/{}.html", it.id)).exists() {
                Ok(t)
            } else {
                let e = format!("finished without writing sections/{}.html", it.id);
                let _ = tx.send(Ev::Agent(idx, AgentEv::Fail(e.clone())));
                Err(e)
            }
        });
        let Err(e) = res else { continue };
        if was_relief {
            log(&tx, Level::Bad, format!("#{} lost: backup agent failed too", it.id));
            continue;
        }
        relief_n += 1;
        let rs = AgentSpec { name: format!("{word}-{relief_n}"), provider: Provider::Claude, model: cfg.relief_model.clone() };
        let ridx = next_idx;
        next_idx += 1;
        shared.names.lock().unwrap().push(rs.name.clone());
        let _ = tx.send(Ev::Spawn(rs.clone()));
        msg(&tx, &spec.name, &lead.name, format!("I'm down ({}). Need backup on #{}", trunc(&e, 60), it.id));
        tokio::time::sleep(Duration::from_millis(400)).await;
        msg(&tx, &lead.name, &rs.name, format!("take over #{} from {}", it.id, spec.name));
        for p in items.iter_mut() {
            if p.id == it.id {
                p.agent = rs.name.clone();
            }
        }
        let _ = tx.send(Ev::Plan(title.clone(), items.clone()));
        let mut it2 = it.clone();
        it2.agent = rs.name.clone();
        let prompt = worker_prompt(&cfg, &lead, &rs, &it2, &items, &title, &vibe);
        let (dir, tx2) = (cfg.dir.clone(), tx.clone());
        set.spawn(async move {
            let r = run_agent(ridx, rs.clone(), prompt, dir, tx2).await;
            (ridx, rs, it2, r, true)
        });
    }

    let page = Page { title: title.clone(), ids: ids.clone(), live: false };
    *shared.page.lock().unwrap() = Some(page.clone());
    assemble(&cfg.dir, &page);

    if cfg.review {
        let _ = tx.send(Ev::Phase(Phase::Review));
        let mail = shared
            .mail
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, to, _)| *to == lead.name || to == "all")
            .map(|(f, _, t)| format!("- {f}: {t}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mail = if mail.is_empty() { "(none)".to_string() } else { mail };
        let ids_s = ids.join(", ");
        let prompt = fill(
            REVIEW_PROMPT,
            &[("%LEAD%", &lead.name), ("%DIR%", &dir_s), ("%IDS%", &ids_s), ("%MAIL%", &mail), ("%TOOLS%", tool_note(lead.provider))],
        );
        let _ = run_agent(0, lead.clone(), prompt, cfg.dir.clone(), tx.clone()).await;
        assemble(&cfg.dir, &page);
    }

    let _ = tx.send(Ev::Phase(Phase::Ship));
    let _ = tx.send(Ev::Shipped);
}
