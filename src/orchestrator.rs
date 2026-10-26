//! The real thing: a lead plans the page, workers build sections in parallel and message each other
//! through files in `.swarm/`, failed agents get replaced, the lead reviews, the page ships. Then the
//! swarm stays up: you talk to the lead (who re-plans and hands out work) or DM any agent directly,
//! and every round of changes ships again.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::app::{AgentEv, Ev, Level, Phase, PlanItem};
use crate::providers::{AgentSpec, Provider};
use crate::session::{self, Handle, Note};

pub type Tx = UnboundedSender<Ev>;

/// Orders from the command bar.
#[derive(Debug)]
pub enum Cmd {
    /// `to` is an agent name, the lead's name, or "all".
    Say {
        to: String,
        text: String,
    },
    Add(Provider, String),
    Stop(Option<String>),
    Review,
}

#[derive(Clone)]
pub struct Config {
    pub task: String,
    pub dir: PathBuf,
    pub roster: Vec<AgentSpec>,
    pub review: bool,
    pub live: bool,
    pub relief_model: String,
    pub resume: Option<State>,
}

#[derive(Clone)]
pub struct Page {
    pub title: String,
    pub ids: Vec<String>,
    /// Show "agent at work" stand-ins for sections that don't exist yet.
    pub placeholders: bool,
}

/// State the background watchers share with the run.
#[derive(Clone, Default)]
pub struct Shared {
    pub page: Arc<Mutex<Option<Page>>>,
    pub names: Arc<Mutex<Vec<String>>>,
    pub mail: Arc<Mutex<Vec<(String, String, String)>>>,
    /// Bumped on every file change so the preview page knows to reload.
    pub version: Arc<AtomicU64>,
}

// ── saved project ────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct SavedAgent {
    pub spec: AgentSpec,
    pub session: Option<String>,
    pub cost: f64,
}

/// `.swarm/state.json`: everything needed to pick a project back up later.
#[derive(Clone, Debug, Default)]
pub struct State {
    pub task: String,
    pub title: String,
    pub vibe: String,
    pub round: u32,
    pub review: bool,
    pub agents: Vec<SavedAgent>,
    pub plan: Vec<PlanItem>,
}

impl State {
    pub fn path(dir: &Path) -> PathBuf {
        dir.join(".swarm/state.json")
    }

    pub fn load(dir: &Path) -> Option<State> {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(Self::path(dir)).ok()?).ok()?;
        let s = |v: &Value| v.as_str().unwrap_or("").to_string();
        let agents = v["agents"]
            .as_array()?
            .iter()
            .filter_map(|a| {
                Some(SavedAgent {
                    spec: AgentSpec { name: s(&a["name"]), provider: Provider::parse(a["provider"].as_str()?)?, model: s(&a["model"]) },
                    session: a["session"].as_str().map(String::from),
                    cost: a["cost"].as_f64().unwrap_or(0.0),
                })
            })
            .collect::<Vec<_>>();
        if agents.is_empty() {
            return None;
        }
        let plan = v["plan"]
            .as_array()
            .map(|p| p.iter().map(|i| PlanItem { id: s(&i["id"]), agent: s(&i["agent"]), brief: s(&i["brief"]) }).collect())
            .unwrap_or_default();
        Some(State {
            task: s(&v["task"]),
            title: s(&v["title"]),
            vibe: s(&v["vibe"]),
            round: v["round"].as_u64().unwrap_or(1) as u32,
            review: v["review"].as_bool().unwrap_or(true),
            agents,
            plan,
        })
    }

    fn save(&self, dir: &Path) {
        let v = json!({
            "task": self.task,
            "title": self.title,
            "vibe": self.vibe,
            "round": self.round,
            "review": self.review,
            "agents": self.agents.iter().map(|a| json!({
                "name": a.spec.name, "provider": a.spec.provider.key(), "model": a.spec.model,
                "session": a.session, "cost": a.cost,
            })).collect::<Vec<_>>(),
            "plan": self.plan.iter().map(|i| json!({"id": i.id, "agent": i.agent, "brief": i.brief})).collect::<Vec<_>>(),
        });
        let _ = std::fs::write(Self::path(dir), serde_json::to_string_pretty(&v).unwrap_or_default());
    }
}

// ── prompts ──────────────────────────────────────────────────────────────────────────────

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
Do not create any other files. Later the user will send you more requests as new messages in this conversation.
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

Team chat: to message someone, create a NEW file %DIR%/.swarm/outbox/%NAME%-<n>.msg (n = 1, 2, 3... never reuse a number). First line: "to: <name>" where name is a teammate, %LEAD%, all, or you (the human running the swarm). Then one or two short sentences. Messages for you are appended to %DIR%/.swarm/inbox/%NAME%.md. Message teammates when it matters to them: anchor ids the nav should link to, a shared visual motif, a variable you rely on. Before you finish, read your inbox (it may not exist yet, that's fine) and adapt, then message %LEAD% a one-line summary of what you shipped.

Work fast and decisively. Make it look premium: strong typography, responsive down to 375px wide, tasteful motion that respects prefers-reduced-motion. End with a one-line summary. Later you may get follow-up tasks from %LEAD% or messages from the user in this same conversation.
%TOOLS%"#;

const REVIEW_PROMPT: &str = r#"Review time. You are %LEAD%, lead of the swarm. Project folder (absolute path): %DIR%
index.html is generated automatically, do not edit it. It links styles/base.css, then styles/<id>.css, inlines sections/<id>.html and loads scripts/<id>.js for these ids, in order: %IDS%.

Messages your team sent you:
%MAIL%

Review the files quickly and fix only real problems by editing the section, style and script files directly: nav links must point to ids that exist, selectors stay scoped to their section, base.css variables are used consistently, no broken markup, nothing overflows at 375px wide. Leave sections that are fine alone. Reply with a two-line summary of what you changed.
%TOOLS%"#;

const ROUTE_PROMPT: &str = r#"Message from the user (the human running this swarm): "%TEXT%"

Project folder (absolute path): %DIR%. Page "%TITLE%" (%VIBE%). index.html is assembled automatically from styles/base.css (yours) plus, in this order: %ORDER%. Each section #<id> lives in sections/<id>.html, styles/<id>.css and optionally scripts/<id>.js, owned by one agent.
Team right now: %TEAM%

Decide how to handle this. Small global tweaks (palette, fonts, spacing in styles/base.css) or one-line fixes you can make yourself right now with your file tools. Changes inside a section go to its owner. Then reply with ONLY this JSON, no markdown fence and no commentary:
{"reply": "<one or two sentences back to the user>", "tasks": [{"agent": "<name>", "task": "<the concrete change>"}], "new_sections": [{"id": "<kebab-case id>", "after": "<existing id it goes after, or \"\" to go before the footer>", "agent": "<name>", "brief": "<2-3 sentences>"}], "remove_sections": ["<id>"], "review": false}
Rules: at most one task per agent (merge them). Never list yourself in tasks. Give new sections to agents that own nothing first, then to whoever has the least work. Set "review" to true only after big multi-section changes. If the user only asked a question or chatted, answer in "reply" and leave every array empty.
%TOOLS%"#;

const INTRO: &str = r#"You are %NAME%, an AI agent in a swarm building one web page together (lead: %LEAD%). Project folder (absolute path): %DIR%. Page "%TITLE%". Sections, top to bottom: %ORDER%.
index.html is assembled automatically from styles/base.css (the lead's design system) plus sections/<id>.html, styles/<id>.css and scripts/<id>.js for each section: never edit index.html or files of sections you don't own, and scope every selector under your section's #id. Read styles/base.css before you style anything.
Team chat: create a NEW file %DIR%/.swarm/outbox/%NAME%-<n>.msg with "to: <name|all|you>" on the first line and a short message after it. Your inbox is %DIR%/.swarm/inbox/%NAME%.md.
%TOOLS%

"#;

const TASK_PROMPT: &str = r#"%INTRO%Task from %LEAD%: %TASK%
You own %OWNS%. Make the change in your own files now, then reply with a one-line summary."#;

const CHAT_PROMPT: &str = r#"%INTRO%Message from the user (the human running this swarm): "%TEXT%"
You own %OWNS%. If they're asking for a change, make it in your own files now. Then answer them in one or two short sentences."#;

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

pub fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}

pub fn slug(s: &str) -> String {
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
    ["fable", "opus", "sonnet", "haiku"].iter().find(|w| m.contains(*w)).map(|w| w.to_string()).unwrap_or_else(|| "claude".into())
}

/// Name stem for an agent: claude models go by model, the others by family.
pub fn name_word(p: Provider, model: &str) -> String {
    match p {
        Provider::Claude => model_word(model),
        Provider::Agy => "gemini".into(),
        Provider::Codex => "codex".into(),
    }
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

pub fn tilde(p: &Path) -> String {
    let s = p.to_string_lossy().to_string();
    let h = home().to_string_lossy().to_string();
    if !h.is_empty() && s.starts_with(&h) {
        format!("~{}", &s[h.len()..])
    } else {
        s
    }
}

pub fn codex_ready() -> bool {
    home().join(".codex/auth.json").exists() || std::env::var_os("OPENAI_API_KEY").is_some() || std::env::var_os("CODEX_API_KEY").is_some()
}

pub fn default_dir(task: &str) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| home());
    cwd.join(format!("swarm-{}", slug(task)))
}

/// Create the project folder. A fresh build clears `.swarm/` (old mail would replay); a resume keeps it.
pub fn prepare_dir(dir: &Path, fresh: bool) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let dir = dir.canonicalize()?;
    let meta = dir.join(".swarm");
    if fresh && meta.exists() {
        std::fs::remove_dir_all(&meta)?;
    }
    for sub in [".swarm/outbox", ".swarm/inbox", "sections", "styles", "scripts"] {
        std::fs::create_dir_all(dir.join(sub))?;
    }
    Ok(dir)
}

pub fn preflight(roster: &[AgentSpec], dir: &Path, demo: bool) -> Vec<(String, String, bool)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for s in roster {
        if !seen.insert(s.provider) {
            continue;
        }
        let (status, ok) = match version(s.provider) {
            Some(v) => (format!("v{v} · LINKED"), true),
            None => ("NOT FOUND".to_string(), false),
        };
        let (status, ok) = if s.provider == Provider::Codex && !codex_ready() && !demo {
            ("NO AUTH · run codex login".to_string(), false)
        } else {
            (status, ok)
        };
        out.push((s.provider.label().to_string(), status, ok));
    }
    out.push(("swarm".into(), format!("1 lead + {} workers · ARMED", roster.len().saturating_sub(1)), true));
    out.push(("workspace".into(), trunc(&tilde(dir), 34), true));
    if demo {
        out.push(("mode".into(), "DEMO · zero tokens burned".into(), true));
    }
    out
}

/// `<bin> --version`, reduced to the version number.
pub fn version(p: Provider) -> Option<String> {
    let o = std::process::Command::new(p.bin()).arg("--version").output().ok()?;
    if !o.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&o.stdout);
    Some(
        s.lines()
            .next()
            .unwrap_or("")
            .split_whitespace()
            .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()))
            .unwrap_or("?")
            .to_string(),
    )
}

pub fn log(tx: &Tx, l: Level, s: impl Into<String>) {
    let _ = tx.send(Ev::Log(l, s.into()));
}

pub fn msg(tx: &Tx, from: &str, to: &str, text: impl Into<String>) {
    let _ = tx.send(Ev::Msg { from: from.into(), to: to.into(), text: text.into() });
}

fn sanitize_id(s: &str) -> String {
    let s = slug(s);
    s.chars().take(24).collect::<String>().trim_matches('-').to_string()
}

/// The JSON object in an agent's answer (they sometimes wrap it in prose or a fence).
fn json_in(text: &str) -> Option<Value> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    serde_json::from_str(text.get(start..=end)?).ok()
}

fn parse_plan(text: &str, workers: &[String]) -> Option<(String, String, Vec<PlanItem>)> {
    let v = json_in(text)?;
    let title = v["title"].as_str().unwrap_or("Swarm build").to_string();
    let vibe = v["vibe"].as_str().unwrap_or("").to_string();
    let mut items = Vec::new();
    let mut used_agents = HashSet::new();
    let mut used_ids = HashSet::new();
    for t in v["tasks"].as_array()? {
        let Some(agent) = t["agent"].as_str() else { continue };
        let Some(name) = workers.iter().find(|s| s.eq_ignore_ascii_case(agent.trim())) else { continue };
        if !used_agents.insert(name.clone()) {
            continue;
        }
        let id = unique_id(t["id"].as_str().unwrap_or("section"), &mut used_ids);
        items.push(PlanItem { id, agent: name.clone(), brief: t["brief"].as_str().unwrap_or("").to_string() });
    }
    if items.is_empty() {
        return None;
    }
    // anyone the lead forgot still gets work
    let mut spare = FALLBACK_IDS.iter().filter(|i| !used_ids.contains(**i));
    for name in workers {
        if used_agents.contains(name) {
            continue;
        }
        let id = spare.next().map(|s| s.to_string()).unwrap_or_else(|| format!("extra-{}", items.len() + 1));
        let brief = format!("Add a {id} section that fits the rest of the page.");
        let at = items.len().saturating_sub(1).max(1).min(items.len());
        items.insert(at, PlanItem { id, agent: name.clone(), brief });
    }
    Some((title, vibe, items))
}

fn unique_id(raw: &str, used: &mut HashSet<String>) -> String {
    let mut id = sanitize_id(raw);
    if id.is_empty() {
        id = "section".into();
    }
    let base = id.clone();
    let mut k = 2;
    while !used.insert(id.clone()) {
        id = format!("{base}-{k}");
        k += 1;
    }
    id
}

fn fallback_plan(task: &str, workers: &[String]) -> (String, String, Vec<PlanItem>) {
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
        .map(|(s, id)| PlanItem { brief: format!("Build the {id} section for: {task}"), id, agent: s.clone() })
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

/// Stitch the agents' fragments into index.html.
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
            Err(_) if page.placeholders => {
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
    let html = format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n  <meta charset=\"utf-8\">\n  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n  <title>{}</title>\n  <link rel=\"stylesheet\" href=\"styles/base.css\">\n{links}{scripts}</head>\n<body>\n{body}</body>\n</html>\n",
        escape(&page.title)
    );
    let path = dir.join("index.html");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(html.as_str()) {
        let _ = std::fs::write(&path, html);
    }
}

/// Open a URL or file in the default browser.
pub fn open(target: &str) -> bool {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    std::process::Command::new(opener).arg(target).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().is_ok()
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

/// Polls the project folder: reports new/changed files, re-assembles index.html, bumps the
/// preview version.
pub async fn watch_files(dir: PathBuf, tx: Tx, shared: Shared) {
    let mut seen: HashMap<PathBuf, (SystemTime, u64)> = HashMap::new();
    let mut first = true;
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let mut files = Vec::new();
        walk(&dir, 0, &mut files);
        let mut src_changed = false;
        let mut any = false;
        for (path, mtime, len) in files {
            let prev = seen.get(&path).copied();
            if prev == Some((mtime, len)) {
                continue;
            }
            seen.insert(path.clone(), (mtime, len));
            any = true;
            let rel = path.strip_prefix(&dir).map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
            if ["sections/", "styles/", "scripts/"].iter().any(|p| rel.starts_with(p)) {
                src_changed = true;
            }
            let _ = tx.send(Ev::File { path: rel, bytes: len, new: prev.is_none() && !first });
        }
        first = false;
        if src_changed {
            let page = shared.page.lock().unwrap().clone();
            if let Some(p) = page {
                assemble(&dir, &p);
            }
        }
        if any {
            shared.version.fetch_add(1, Ordering::Relaxed);
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
    } else if ["you", "user", "human"].iter().any(|w| to.eq_ignore_ascii_case(w)) {
        "you".to_string()
    } else {
        names.iter().find(|n| n.eq_ignore_ascii_case(&to)).cloned().unwrap_or(lead)
    };
    (to, trunc(&text, 280))
}

/// Picks up `.swarm/outbox/*.msg`, animates them, and delivers them into recipients' inboxes.
pub async fn watch_outbox(dir: PathBuf, tx: Tx, shared: Shared) {
    let outbox = dir.join(".swarm/outbox");
    let mut done: HashSet<String> = HashSet::new();
    // a resumed project has old mail lying around; it was delivered last time
    if let Ok(rd) = std::fs::read_dir(&outbox) {
        done.extend(rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()));
    }
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
            let targets: Vec<&String> = match to.as_str() {
                "all" => names.iter().filter(|n| **n != from).collect(),
                "you" => Vec::new(),
                _ => names.iter().filter(|n| **n == to).collect(),
            };
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

// ── the run ──────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
enum Job {
    Plan,
    Review,
    Route,
    Build(String),
    Task,
    Chat,
}

impl Job {
    /// Work the current round waits for before it can ship.
    fn gates(&self) -> bool {
        matches!(self, Job::Build(_) | Job::Task)
    }
}

struct Slot {
    spec: AgentSpec,
    handle: Handle,
    jobs: VecDeque<Job>,
    session: Option<String>,
    cost: f64,
    /// The agent's conversation already knows what the project is.
    briefed: bool,
    relief: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    Idle,
    Planning,
    Routing,
    Building,
    Reviewing,
}

struct Orch {
    cfg: Config,
    tx: Tx,
    shared: Shared,
    notes: UnboundedSender<Note>,
    slots: Vec<Slot>,
    title: String,
    vibe: String,
    items: Vec<PlanItem>,
    round: u32,
    stage: Stage,
    waiting: HashSet<usize>,
    review_after: bool,
    stopping: bool,
    mail_seen: usize,
    preview: Option<String>,
}

impl Orch {
    fn dir_s(&self) -> String {
        self.cfg.dir.to_string_lossy().to_string()
    }

    fn lead(&self) -> &AgentSpec {
        &self.slots[0].spec
    }

    fn workers(&self) -> Vec<String> {
        self.slots.iter().skip(1).map(|s| s.spec.name.clone()).collect()
    }

    fn index(&self, name: &str) -> Option<usize> {
        self.slots.iter().position(|s| s.spec.name.eq_ignore_ascii_case(name.trim().trim_start_matches('@')))
    }

    fn add_slot(&mut self, spec: AgentSpec, session: Option<String>, cost: f64, relief: bool) -> usize {
        let idx = self.slots.len();
        let handle = session::spawn(idx, spec.clone(), self.cfg.dir.clone(), session.clone(), self.tx.clone(), self.notes.clone());
        self.shared.names.lock().unwrap().push(spec.name.clone());
        let briefed = session.is_some();
        self.slots.push(Slot { spec, handle, jobs: VecDeque::new(), session, cost, briefed, relief });
        idx
    }

    fn dispatch(&mut self, idx: usize, job: Job, label: String, prompt: String) {
        let _ = self.tx.send(Ev::Agent(idx, AgentEv::Turn(label)));
        if job.gates() {
            self.waiting.insert(idx);
        }
        let s = &mut self.slots[idx];
        s.jobs.push_back(job);
        s.briefed = true;
        s.handle.send(prompt);
    }

    fn order(&self) -> String {
        if self.items.is_empty() {
            return "(nothing yet)".into();
        }
        self.items.iter().map(|p| format!("#{} ({})", p.id, p.agent)).collect::<Vec<_>>().join(" → ")
    }

    fn owns(&self, name: &str) -> String {
        let ids: Vec<String> = self.items.iter().filter(|i| i.agent == name).map(|i| format!("#{}", i.id)).collect();
        if ids.is_empty() {
            "no section yet".into()
        } else {
            format!("{} (sections/<id>.html, styles/<id>.css, scripts/<id>.js)", ids.join(", "))
        }
    }

    fn team(&self) -> String {
        self.slots
            .iter()
            .skip(1)
            .map(|s| {
                let ids: Vec<String> = self.items.iter().filter(|i| i.agent == s.spec.name).map(|i| format!("#{}", i.id)).collect();
                let owns = if ids.is_empty() { "owns nothing".to_string() } else { format!("owns {}", ids.join(", ")) };
                let busy = if s.jobs.is_empty() { "idle" } else { "busy" };
                format!("{} ({} {}): {owns}, {busy}", s.spec.name, s.spec.provider.label(), s.spec.model_label())
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn intro(&self, idx: usize) -> String {
        if self.slots[idx].briefed {
            return String::new();
        }
        let s = &self.slots[idx].spec;
        fill(
            INTRO,
            &[
                ("%NAME%", &s.name),
                ("%LEAD%", &self.lead().name),
                ("%DIR%", &self.dir_s()),
                ("%TITLE%", &self.title),
                ("%ORDER%", &self.order()),
                ("%TOOLS%", tool_note(s.provider)),
            ],
        )
    }

    fn build_prompt(&self, idx: usize, it: &PlanItem) -> String {
        let me = &self.slots[idx].spec;
        let n = self.slots.len().to_string();
        let mut p = fill(
            WORKER_PROMPT,
            &[
                ("%NAME%", &me.name),
                ("%N%", &n),
                ("%LEAD%", &self.lead().name),
                ("%TITLE%", &self.title),
                ("%VIBE%", &self.vibe),
                ("%TASK%", &self.cfg.task),
                ("%DIR%", &self.dir_s()),
                ("%ID%", &it.id),
                ("%BRIEF%", &it.brief),
                ("%ORDER%", &self.order()),
                ("%TOOLS%", tool_note(me.provider)),
            ],
        );
        let others: Vec<String> = self.items.iter().filter(|i| i.agent == me.name && i.id != it.id).map(|i| format!("#{}", i.id)).collect();
        if !others.is_empty() {
            p = format!("{p}\nYou still own {} as well; this is an additional section.", others.join(", "));
        }
        p
    }

    fn page(&self, placeholders: bool) -> Page {
        Page { title: self.title.clone(), ids: self.items.iter().map(|i| i.id.clone()).collect(), placeholders }
    }

    fn publish_plan(&mut self, placeholders: bool) {
        let page = self.page(placeholders);
        *self.shared.page.lock().unwrap() = Some(page.clone());
        assemble(&self.cfg.dir, &page);
        let _ = self.tx.send(Ev::Plan(self.title.clone(), self.items.clone()));
        self.save();
    }

    fn save(&self) {
        let st = State {
            task: self.cfg.task.clone(),
            title: self.title.clone(),
            vibe: self.vibe.clone(),
            round: self.round,
            review: self.cfg.review,
            agents: self.slots.iter().map(|s| SavedAgent { spec: s.spec.clone(), session: s.session.clone(), cost: s.cost }).collect(),
            plan: self.items.clone(),
        };
        st.save(&self.cfg.dir);
    }

    fn phase(&self, p: Phase) {
        let _ = self.tx.send(Ev::Phase(p));
    }

    fn log(&self, l: Level, s: impl Into<String>) {
        log(&self.tx, l, s);
    }

    fn msg(&self, from: &str, to: &str, text: impl Into<String>) {
        msg(&self.tx, from, to, text);
    }

    // ── starting ────────────────────────────────────────────────────────────────────────

    fn start_fresh(&mut self) {
        let lead = self.lead().clone();
        let workers = self.workers();
        if workers.is_empty() {
            // solo run: the lead builds the whole page itself
            ensure_base_css(&self.cfg.dir);
            self.title = trunc(&self.cfg.task, 60);
            self.items =
                vec![PlanItem { id: "page".into(), agent: lead.name.clone(), brief: format!("Build the whole page: {}", self.cfg.task) }];
            self.publish_plan(true);
            self.go_build();
            let it = self.items[0].clone();
            let p = self.build_prompt(0, &it);
            self.dispatch(0, Job::Build(it.id.clone()), format!("build #{}", it.id), p);
            return;
        }
        self.stage = Stage::Planning;
        self.phase(Phase::Plan);
        self.log(Level::Info, format!("{} is drafting the plan", lead.name));
        let team = self
            .slots
            .iter()
            .skip(1)
            .map(|s| format!("{} ({} {})", s.spec.name, s.spec.provider.label(), s.spec.model_label()))
            .collect::<Vec<_>>()
            .join(", ");
        let prompt = fill(
            PLAN_PROMPT,
            &[
                ("%LEAD%", &lead.name),
                ("%TASK%", &self.cfg.task),
                ("%DIR%", &self.dir_s()),
                ("%TEAM%", &team),
                ("%TOOLS%", tool_note(lead.provider)),
            ],
        );
        self.dispatch(0, Job::Plan, "plan the page".into(), prompt);
    }

    fn resume(&mut self, st: State) {
        self.title = st.title;
        self.vibe = st.vibe;
        self.items = st.plan;
        self.round = st.round.max(1);
        for (i, s) in self.slots.iter().enumerate() {
            if s.cost > 0.0 {
                let _ = self.tx.send(Ev::Agent(i, AgentEv::Cost(s.cost)));
            }
            if let Some(id) = &s.session {
                let _ = self.tx.send(Ev::Agent(i, AgentEv::Session(id.clone())));
            }
        }
        let _ = self.tx.send(Ev::Resumed(self.round));
        self.publish_plan(false);
        self.phase(Phase::Ship);
        self.stage = Stage::Idle;
        self.log(Level::Good, format!("resumed \"{}\" · round {} · talk to {} below", self.title, self.round, self.lead().name));
        self.msg(&self.lead().name.clone(), "you", "Back online with the whole team. What should we change?");
    }

    fn go_build(&mut self) {
        self.stage = Stage::Building;
        self.phase(Phase::Build);
        if self.cfg.live {
            if let Some(url) = &self.preview {
                open(url);
            }
            self.cfg.live = false;
        }
    }

    // ── turn results ────────────────────────────────────────────────────────────────────

    async fn note(&mut self, n: Note) {
        match n {
            Note::Session(idx, id) => {
                if let Some(s) = self.slots.get_mut(idx) {
                    s.session = Some(id);
                }
                self.save();
            }
            Note::Cost(idx, c) => {
                if let Some(s) = self.slots.get_mut(idx) {
                    s.cost += c;
                }
                self.save();
            }
            Note::Turn(idx, result) => {
                let Some(job) = self.slots.get_mut(idx).and_then(|s| s.jobs.pop_front()) else { return };
                self.turn(idx, job, result).await;
                if !self.slots[idx].jobs.iter().any(Job::gates) {
                    self.waiting.remove(&idx);
                }
                self.maybe_finish();
            }
        }
    }

    async fn turn(&mut self, idx: usize, job: Job, result: Result<String, String>) {
        let name = self.slots[idx].spec.name.clone();
        let stopped = result.as_ref().err().is_some_and(|e| e == "stopped");
        match job {
            Job::Plan if stopped => {
                self.log(Level::Warn, "planning stopped · message the lead to start again");
                self.idle();
            }
            Job::Plan => self.planned(result).await,
            Job::Build(id) => {
                // "success" without the section file on disk is still a failure
                let result = result.and_then(|t| {
                    if self.cfg.dir.join(format!("sections/{id}.html")).exists() {
                        Ok(t)
                    } else {
                        let e = format!("finished without writing sections/{id}.html");
                        let _ = self.tx.send(Ev::Agent(idx, AgentEv::Fail(e.clone())));
                        Err(e)
                    }
                });
                if let Err(e) = result {
                    if !stopped && !self.stopping {
                        self.relieve(idx, &id, &e).await;
                    }
                }
            }
            Job::Task => {
                if let Err(e) = result {
                    if !stopped {
                        self.msg(&name, &self.lead().name.clone(), format!("couldn't finish my task: {}", trunc(&e, 80)));
                    }
                }
            }
            Job::Chat => match result {
                Ok(t) if !t.trim().is_empty() => self.msg(&name, "you", trunc(t.trim(), 400)),
                Ok(_) => {}
                Err(e) if !stopped => self.msg(&name, "you", format!("sorry, that failed: {}", trunc(&e, 120))),
                Err(_) => {}
            },
            Job::Route => self.routed(result),
            Job::Review => {
                if let Ok(t) = &result {
                    let first = t.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
                    if !first.is_empty() {
                        self.log(Level::Good, format!("review: {}", trunc(first, 120)));
                    }
                }
                if self.stage == Stage::Reviewing {
                    if self.waiting.is_empty() {
                        self.ship();
                    } else {
                        self.stage = Stage::Building;
                        self.review_after = false;
                        self.phase(Phase::Build);
                    }
                }
            }
        }
    }

    async fn planned(&mut self, result: Result<String, String>) {
        let workers = self.workers();
        let (title, vibe, items) = match result.as_deref().ok().and_then(|t| parse_plan(t, &workers)) {
            Some(p) => p,
            None => {
                self.log(Level::Warn, "plan unreadable, using the default section split");
                fallback_plan(&self.cfg.task, &workers)
            }
        };
        ensure_base_css(&self.cfg.dir);
        self.title = title;
        self.vibe = vibe;
        self.items = items.clone();
        self.publish_plan(true);
        let lead = self.lead().name.clone();
        for it in &items {
            self.msg(&lead, &it.agent, format!("you own #{}: {}", it.id, trunc(&it.brief, 110)));
            tokio::time::sleep(Duration::from_millis(140)).await;
        }
        self.go_build();
        self.review_after = self.cfg.review;
        for it in &items {
            let Some(idx) = self.index(&it.agent) else { continue };
            let p = self.build_prompt(idx, it);
            self.dispatch(idx, Job::Build(it.id.clone()), format!("build #{}", it.id), p);
        }
    }

    /// A dead worker gets replaced once by a fresh Claude worker.
    async fn relieve(&mut self, idx: usize, id: &str, err: &str) {
        let spec = self.slots[idx].spec.clone();
        if self.slots[idx].relief {
            self.log(Level::Bad, format!("#{id} lost: backup agent failed too"));
            return;
        }
        let word = model_word(&self.cfg.relief_model);
        let name = self.fresh_name(&word);
        let rs = AgentSpec { name: name.clone(), provider: Provider::Claude, model: self.cfg.relief_model.clone() };
        let _ = self.tx.send(Ev::Spawn(rs.clone(), "dropped in as backup".into()));
        let ridx = self.add_slot(rs, None, 0.0, true);
        let lead = self.lead().name.clone();
        self.msg(&spec.name, &lead, format!("I'm down ({}). Need backup on #{id}", trunc(err, 60)));
        tokio::time::sleep(Duration::from_millis(400)).await;
        self.msg(&lead, &name, format!("take over #{id} from {}", spec.name));
        for p in self.items.iter_mut() {
            if p.id == id {
                p.agent = name.clone();
            }
        }
        self.publish_plan(true);
        let Some(it) = self.items.iter().find(|i| i.id == id).cloned() else { return };
        let p = self.build_prompt(ridx, &it);
        self.dispatch(ridx, Job::Build(id.to_string()), format!("take over #{id}"), p);
    }

    fn fresh_name(&self, word: &str) -> String {
        let mut n = 1;
        loop {
            let name = format!("{word}-{n}");
            if !self.slots.iter().any(|s| s.spec.name == name) {
                return name;
            }
            n += 1;
        }
    }

    /// The lead answered a message from the user: relay the reply, hand out the work.
    fn routed(&mut self, result: Result<String, String>) {
        let lead = self.lead().name.clone();
        let text = match result {
            Ok(t) => t,
            Err(e) => {
                if e != "stopped" {
                    self.msg(&lead, "you", format!("I hit an error: {}", trunc(&e, 120)));
                }
                if self.stage == Stage::Routing {
                    self.idle();
                }
                return;
            }
        };
        let Some(v) = json_in(&text) else {
            // no JSON: treat the whole answer as a chat reply
            self.msg(&lead, "you", trunc(text.trim(), 400));
            if self.stage == Stage::Routing {
                self.idle();
            }
            return;
        };
        let reply = v["reply"].as_str().unwrap_or("").trim().to_string();
        if !reply.is_empty() {
            self.msg(&lead, "you", trunc(&reply, 400));
        }

        let mut changed_plan = false;
        if let Some(rm) = v["remove_sections"].as_array() {
            let ids: Vec<String> = rm.iter().filter_map(|x| x.as_str()).map(sanitize_id).collect();
            let before = self.items.len();
            self.items.retain(|i| !ids.contains(&i.id));
            if self.items.len() != before {
                changed_plan = true;
                self.log(Level::Warn, format!("removed {}", ids.iter().map(|i| format!("#{i}")).collect::<Vec<_>>().join(", ")));
            }
        }

        let mut builds: Vec<(usize, PlanItem)> = Vec::new();
        let mut used: HashSet<String> = self.items.iter().map(|i| i.id.clone()).collect();
        for ns in v["new_sections"].as_array().cloned().unwrap_or_default() {
            let id = unique_id(ns["id"].as_str().unwrap_or("section"), &mut used);
            let Some(idx) = self.pick_owner(ns["agent"].as_str().unwrap_or(""), &builds) else { continue };
            let it = PlanItem {
                id: id.clone(),
                agent: self.slots[idx].spec.name.clone(),
                brief: ns["brief"].as_str().unwrap_or("").to_string(),
            };
            let after = ns["after"].as_str().map(sanitize_id).unwrap_or_default();
            let at = match self.items.iter().position(|i| i.id == after) {
                Some(p) if !after.is_empty() => p + 1,
                _ => match self.items.iter().position(|i| i.id == "footer") {
                    Some(f) => f,
                    None => self.items.len(),
                },
            };
            self.items.insert(at, it.clone());
            builds.push((idx, it));
            changed_plan = true;
        }

        let mut tasks: Vec<(usize, String)> = Vec::new();
        for t in v["tasks"].as_array().cloned().unwrap_or_default() {
            let (Some(agent), Some(task)) = (t["agent"].as_str(), t["task"].as_str()) else { continue };
            let Some(idx) = self.index(agent).filter(|&i| i != 0) else { continue };
            match tasks.iter_mut().find(|(i, _)| *i == idx) {
                Some((_, prev)) => {
                    prev.push(' ');
                    prev.push_str(task);
                }
                None => tasks.push((idx, task.to_string())),
            }
        }

        if changed_plan {
            self.publish_plan(true);
        }
        if builds.is_empty() && tasks.is_empty() {
            if self.stage == Stage::Routing {
                self.idle();
            }
            return;
        }
        self.stopping = false;
        if matches!(self.stage, Stage::Idle | Stage::Routing) {
            self.go_build();
            self.review_after = false;
        }
        if v["review"].as_bool().unwrap_or(false) {
            self.review_after = true;
        }
        for (idx, it) in builds {
            let name = self.slots[idx].spec.name.clone();
            self.msg(&lead, &name, format!("new section #{}: {}", it.id, trunc(&it.brief, 100)));
            let p = self.build_prompt(idx, &it);
            self.dispatch(idx, Job::Build(it.id.clone()), format!("build #{}", it.id), p);
        }
        for (idx, task) in tasks {
            let name = self.slots[idx].spec.name.clone();
            self.msg(&lead, &name, trunc(&task, 140));
            let owns = self.owns(&name);
            let p = fill(TASK_PROMPT, &[("%INTRO%", &self.intro(idx)), ("%LEAD%", &lead), ("%TASK%", &task), ("%OWNS%", &owns)]);
            self.dispatch(idx, Job::Task, format!("task from {lead}"), p);
        }
    }

    /// Who builds a new section: the lead's pick if it names a worker, else an idle worker with the
    /// fewest sections.
    fn pick_owner(&self, wanted: &str, planned: &[(usize, PlanItem)]) -> Option<usize> {
        if let Some(i) = self.index(wanted).filter(|&i| i != 0) {
            return Some(i);
        }
        let load = |i: usize| {
            let s = &self.slots[i];
            self.items.iter().filter(|it| it.agent == s.spec.name).count() * 2
                + s.jobs.len() * 3
                + planned.iter().filter(|(p, _)| *p == i).count() * 3
        };
        (1..self.slots.len()).min_by_key(|&i| load(i))
    }

    fn maybe_finish(&mut self) {
        if self.stage != Stage::Building || !self.waiting.is_empty() {
            return;
        }
        let page = self.page(false);
        *self.shared.page.lock().unwrap() = Some(page.clone());
        assemble(&self.cfg.dir, &page);
        if self.stopping {
            self.log(Level::Warn, format!("round {} stopped", self.round));
            self.idle();
            return;
        }
        if self.review_after && self.slots.len() > 1 {
            self.review();
        } else {
            self.ship();
        }
    }

    fn review(&mut self) {
        self.stage = Stage::Reviewing;
        self.review_after = false;
        self.phase(Phase::Review);
        let lead = self.lead().clone();
        let mail: Vec<String> = {
            let all = self.shared.mail.lock().unwrap();
            let fresh: Vec<String> = all[self.mail_seen.min(all.len())..]
                .iter()
                .filter(|(_, to, _)| *to == lead.name || to == "all")
                .map(|(f, _, t)| format!("- {f}: {t}"))
                .collect();
            self.mail_seen = all.len();
            fresh
        };
        let mail = if mail.is_empty() { "(none)".to_string() } else { mail.join("\n") };
        let ids = self.items.iter().map(|i| i.id.clone()).collect::<Vec<_>>().join(", ");
        let prompt = fill(
            REVIEW_PROMPT,
            &[("%LEAD%", &lead.name), ("%DIR%", &self.dir_s()), ("%IDS%", &ids), ("%MAIL%", &mail), ("%TOOLS%", tool_note(lead.provider))],
        );
        self.dispatch(0, Job::Review, "review the page".into(), prompt);
    }

    fn ship(&mut self) {
        let page = self.page(false);
        *self.shared.page.lock().unwrap() = Some(page.clone());
        assemble(&self.cfg.dir, &page);
        self.phase(Phase::Ship);
        let _ = self.tx.send(Ev::Shipped(self.round));
        self.round += 1;
        self.stage = Stage::Idle;
        self.stopping = false;
        self.save();
    }

    fn idle(&mut self) {
        self.stage = Stage::Idle;
        self.stopping = false;
        self.phase(Phase::Ship);
        self.save();
    }

    // ── orders from the command bar ────────────────────────────────────────────────────

    fn command(&mut self, c: Cmd) {
        match c {
            Cmd::Say { to, text } => {
                self.stopping = false;
                let lead = self.lead().name.clone();
                if to.eq_ignore_ascii_case("all") {
                    for idx in 1..self.slots.len() {
                        self.chat(idx, &text);
                    }
                    if self.slots.len() == 1 {
                        self.route(&text);
                    }
                } else if to.eq_ignore_ascii_case(&lead) || to.eq_ignore_ascii_case("lead") {
                    self.route(&text);
                } else if let Some(idx) = self.index(&to) {
                    self.chat(idx, &text);
                }
            }
            Cmd::Add(provider, model) => {
                let name = self.fresh_name(&name_word(provider, &model));
                let spec = AgentSpec { name: name.clone(), provider, model };
                let _ =
                    self.tx.send(Ev::Spawn(spec.clone(), format!("joined the swarm ({} {})", spec.provider.label(), spec.model_label())));
                self.add_slot(spec, None, 0.0, false);
                let lead = self.lead().name.clone();
                self.msg(&lead, &name, "welcome aboard. Stand by for a section.");
                self.save();
            }
            Cmd::Stop(who) => match who.and_then(|w| self.index(&w)) {
                Some(idx) => self.slots[idx].handle.stop(),
                None => {
                    self.stopping = true;
                    for s in &self.slots {
                        s.handle.stop();
                    }
                }
            },
            Cmd::Review => match self.stage {
                Stage::Idle => self.review(),
                Stage::Building => self.review_after = true,
                _ => {}
            },
        }
    }

    fn route(&mut self, text: &str) {
        if self.stage == Stage::Idle {
            self.stage = Stage::Routing;
            self.phase(Phase::Plan);
        }
        let lead = self.lead().clone();
        let prompt = fill(
            ROUTE_PROMPT,
            &[
                ("%TEXT%", text),
                ("%DIR%", &self.dir_s()),
                ("%TITLE%", &self.title),
                ("%VIBE%", &self.vibe),
                ("%ORDER%", &self.order()),
                ("%TEAM%", &self.team()),
                ("%TOOLS%", tool_note(lead.provider)),
            ],
        );
        self.dispatch(0, Job::Route, "message from you".into(), prompt);
    }

    fn chat(&mut self, idx: usize, text: &str) {
        let name = self.slots[idx].spec.name.clone();
        let owns = self.owns(&name);
        let p = fill(CHAT_PROMPT, &[("%INTRO%", &self.intro(idx)), ("%TEXT%", text), ("%OWNS%", &owns)]);
        self.dispatch(idx, Job::Chat, "message from you".into(), p);
    }
}

pub async fn run(cfg: Config, tx: Tx, mut cmds: UnboundedReceiver<Cmd>) {
    let shared = Shared::default();
    let (notes_tx, mut notes) = unbounded_channel();
    let preview = crate::server::start(cfg.dir.clone(), shared.version.clone()).await;
    if let Some(url) = &preview {
        let _ = tx.send(Ev::Server(url.clone()));
    }
    tokio::spawn(watch_files(cfg.dir.clone(), tx.clone(), shared.clone()));
    tokio::spawn(watch_outbox(cfg.dir.clone(), tx.clone(), shared.clone()));

    let mut o = Orch {
        cfg: cfg.clone(),
        tx: tx.clone(),
        shared,
        notes: notes_tx,
        slots: Vec::new(),
        title: String::new(),
        vibe: String::new(),
        items: Vec::new(),
        round: 1,
        stage: Stage::Idle,
        waiting: HashSet::new(),
        review_after: false,
        stopping: false,
        mail_seen: 0,
        preview,
    };
    match cfg.resume.clone() {
        Some(st) => {
            for a in &st.agents {
                o.add_slot(a.spec.clone(), a.session.clone(), a.cost, false);
            }
            o.resume(st);
        }
        None => {
            for s in &cfg.roster {
                o.add_slot(s.clone(), None, 0.0, false);
            }
            o.start_fresh();
        }
    }

    loop {
        tokio::select! {
            c = cmds.recv() => match c {
                Some(c) => o.command(c),
                None => break,
            },
            Some(n) = notes.recv() => o.note(n).await,
        }
    }
}
