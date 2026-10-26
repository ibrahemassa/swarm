//! What kind of project the swarm is working on, which files belong to whom, and the check
//! command that decides whether a round actually works.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime};

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use crate::app::Ev;
use crate::session::Note;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    /// One static web page, assembled from per-agent sections.
    #[default]
    Page,
    /// Anything else: a new app, CLI, library or game, or changes to an existing codebase.
    Code,
}

impl Kind {
    pub fn key(self) -> &'static str {
        match self {
            Kind::Page => "page",
            Kind::Code => "code",
        }
    }

    pub fn parse(s: &str) -> Kind {
        if s.eq_ignore_ascii_case("code") {
            Kind::Code
        } else {
            Kind::Page
        }
    }
}

/// Folders that are never worth watching or scanning: dependencies, build output, caches.
const HEAVY: &[&str] = &["node_modules", "target", "dist", "build", "out", "__pycache__", "venv", "vendor", "coverage"];

pub fn skip_dir(name: &str) -> bool {
    name.starts_with('.') || HEAVY.contains(&name)
}

/// Every file under `dir` (relative path, mtime, size), skipping hidden and heavy folders.
pub fn scan(dir: &Path, limit: usize) -> Vec<(PathBuf, SystemTime, u64)> {
    let mut out = Vec::new();
    walk(dir, 0, limit, &mut out);
    out
}

fn walk(dir: &Path, depth: usize, limit: usize, out: &mut Vec<(PathBuf, SystemTime, u64)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if out.len() >= limit {
            return;
        }
        let name = e.file_name().to_string_lossy().to_string();
        let Ok(m) = e.metadata() else { continue };
        if m.is_dir() {
            if depth < 8 && !skip_dir(&name) {
                walk(&e.path(), depth + 1, limit, out);
            }
        } else if !name.starts_with('.') {
            out.push((e.path(), m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()));
        }
    }
}

fn glob(p: &[char], s: &[char]) -> bool {
    if p.is_empty() {
        return s.is_empty();
    }
    if p.len() >= 2 && p[0] == '*' && p[1] == '*' {
        let rest = &p[2..];
        let rest = rest.strip_prefix(&['/']).unwrap_or(rest);
        if rest.is_empty() {
            return true;
        }
        return (0..=s.len()).any(|i| (i == 0 || s[i - 1] == '/') && glob(rest, &s[i..]));
    }
    match p[0] {
        '*' => {
            for i in 0..=s.len() {
                if glob(&p[1..], &s[i..]) {
                    return true;
                }
                if i < s.len() && s[i] == '/' {
                    break;
                }
            }
            false
        }
        '?' => !s.is_empty() && s[0] != '/' && glob(&p[1..], &s[1..]),
        c => !s.is_empty() && s[0] == c && glob(&p[1..], &s[1..]),
    }
}

/// Does a project-relative path fall under an ownership pattern? `**` spans folders, `*` and `?`
/// stay inside one, and a plain path also covers everything below it.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pat = pattern.trim().trim_start_matches("./").trim_start_matches('/');
    let path = path.trim().trim_start_matches("./");
    if pat.is_empty() {
        return false;
    }
    if !pat.contains(['*', '?']) {
        let d = pat.trim_end_matches('/');
        return path == d || path.starts_with(&format!("{d}/"));
    }
    let p: Vec<char> = pat.chars().collect();
    let s: Vec<char> = path.chars().collect();
    glob(&p, &s)
}

pub fn owns(patterns: &[String], path: &str) -> bool {
    patterns.iter().any(|p| glob_match(p, path))
}

/// Did anything matching `patterns` change since `since`?
pub fn changed_since(dir: &Path, patterns: &[String], since: SystemTime) -> bool {
    scan(dir, 50_000)
        .into_iter()
        .any(|(p, m, _)| m >= since && p.strip_prefix(dir).map(|r| owns(patterns, &r.to_string_lossy())).unwrap_or(false))
}

fn has(dir: &Path, f: &str) -> bool {
    dir.join(f).exists()
}

/// The usual "build and test everything" command for a project, from its manifests.
pub fn detect_check(dir: &Path) -> Option<String> {
    if has(dir, "Cargo.toml") {
        return Some("cargo test".into());
    }
    if has(dir, "package.json") {
        let pkg = std::fs::read_to_string(dir.join("package.json")).unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&pkg).unwrap_or_default();
        let test = v["scripts"]["test"].as_str().unwrap_or("");
        if !test.is_empty() && !test.contains("no test specified") {
            let pm = if has(dir, "pnpm-lock.yaml") {
                "pnpm"
            } else if has(dir, "yarn.lock") {
                "yarn"
            } else if has(dir, "bun.lockb") || has(dir, "bun.lock") {
                "bun run"
            } else {
                "npm"
            };
            return Some(format!("{pm} test"));
        }
        if v["scripts"]["build"].is_string() {
            return Some("npm run build".into());
        }
    }
    if has(dir, "go.mod") {
        return Some("go test ./...".into());
    }
    if has(dir, "pyproject.toml") || has(dir, "pytest.ini") || has(dir, "setup.py") || has(dir, "requirements.txt") || has(dir, "tests") {
        let pytest = std::process::Command::new("python3")
            .args(["-c", "import pytest"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        return Some(if pytest { "python3 -m pytest -q" } else { "python3 -m unittest discover -v" }.into());
    }
    if let Ok(mk) = std::fs::read_to_string(dir.join("Makefile")) {
        if mk.lines().any(|l| l.starts_with("test:")) {
            return Some("make test".into());
        }
    }
    None
}

/// One line about what's in a folder, for the launcher.
pub fn describe(dir: &Path) -> String {
    if !dir.exists() {
        return "new folder · the lead picks the stack".into();
    }
    let n = scan(dir, 5_000).len();
    if n == 0 {
        return "empty folder · the lead picks the stack".into();
    }
    let stack = [
        ("Cargo.toml", "Rust"),
        ("package.json", "JavaScript/TypeScript"),
        ("go.mod", "Go"),
        ("pyproject.toml", "Python"),
        ("requirements.txt", "Python"),
        ("setup.py", "Python"),
        ("pom.xml", "Java"),
        ("build.gradle", "Java/Kotlin"),
        ("Gemfile", "Ruby"),
        ("composer.json", "PHP"),
        ("CMakeLists.txt", "C/C++"),
        ("index.html", "static site"),
    ]
    .iter()
    .find(|(f, _)| has(dir, f))
    .map(|(f, s)| format!("{s} ({f})"))
    .unwrap_or_else(|| "files".into());
    let more = if n >= 5_000 { "+" } else { "" };
    format!("existing project · {stack} · {n}{more} files")
}

/// What the lead gets told about the folder before it plans.
pub fn overview(dir: &Path) -> String {
    let mut top: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    if name.starts_with('.') {
                        return None;
                    }
                    Some(if e.path().is_dir() { format!("{name}/") } else { name })
                })
                .collect()
        })
        .unwrap_or_default();
    if top.is_empty() {
        return "it's empty, you are starting from scratch".into();
    }
    top.sort();
    let shown = top.iter().take(40).cloned().collect::<Vec<_>>().join(", ");
    let more = if top.len() > 40 { format!(" and {} more", top.len() - 40) } else { String::new() };
    format!("it already contains a project (top level: {shown}{more}). Explore it with your file tools before planning and build on what is there; don't rewrite working code")
}

/// Keep swarm's scratch folder out of `git status` in an existing repo (local-only exclude file).
pub fn git_exclude(dir: &Path) {
    let info = dir.join(".git/info");
    if !dir.join(".git").is_dir() {
        return;
    }
    let path = info.join("exclude");
    let cur = std::fs::read_to_string(&path).unwrap_or_default();
    if cur.lines().any(|l| l.trim() == ".swarm/" || l.trim() == ".swarm") {
        return;
    }
    let _ = std::fs::create_dir_all(&info);
    let sep = if cur.is_empty() || cur.ends_with('\n') { "" } else { "\n" };
    let _ = std::fs::write(&path, format!("{cur}{sep}# swarm's scratch space\n.swarm/\n"));
}

/// `git diff --shortstat` for the working tree, if the folder is a repo.
pub fn diff_stat(dir: &Path) -> Option<String> {
    let o = std::process::Command::new("git").args(["diff", "--shortstat"]).current_dir(dir).stderr(Stdio::null()).output().ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (o.status.success() && !s.is_empty()).then_some(s)
}

/// Drop terminal escape sequences and carriage-return redraws from a line of tool output.
pub fn strip_ansi(s: &str) -> String {
    let s = s.rsplit('\r').find(|p| !p.is_empty()).unwrap_or("");
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                chars.next();
            }
        } else if c == '\t' {
            out.push_str("    ");
        } else if !c.is_control() {
            out.push(c);
        }
    }
    out
}

async fn pump<R: AsyncRead + Unpin>(r: R, tx: UnboundedSender<String>) {
    let mut lines = BufReader::new(r).lines();
    while let Ok(Some(l)) = lines.next_line().await {
        if tx.send(l).is_err() {
            break;
        }
    }
}

pub const CHECK_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Run a command through `sh -c` in the project, streaming its output to the dashboard, and report
/// pass/fail plus the tail of the output back to the orchestrator.
pub async fn run_check(dir: PathBuf, cmd: String, tx: UnboundedSender<Ev>, notes: UnboundedSender<Note>) {
    let started = Instant::now();
    let spawned = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .current_dir(&dir)
        .env("CI", "true")
        .env("NO_COLOR", "1")
        .env("CARGO_TERM_COLOR", "never")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("can't run `{cmd}`: {e}");
            let _ = tx.send(Ev::CheckLine(msg.clone()));
            let _ = tx.send(Ev::CheckDone { ok: false, secs: 0 });
            let _ = notes.send(Note::Check(false, msg));
            return;
        }
    };
    let (ltx, mut lrx) = unbounded_channel::<String>();
    if let Some(o) = child.stdout.take() {
        tokio::spawn(pump(o, ltx.clone()));
    }
    if let Some(e) = child.stderr.take() {
        tokio::spawn(pump(e, ltx.clone()));
    }
    drop(ltx);
    let mut tail: VecDeque<String> = VecDeque::new();
    let deadline = tokio::time::sleep(CHECK_TIMEOUT);
    tokio::pin!(deadline);
    let mut timed_out = false;
    loop {
        tokio::select! {
            l = lrx.recv() => match l {
                Some(l) => {
                    let l = strip_ansi(&l);
                    let _ = tx.send(Ev::CheckLine(l.clone()));
                    tail.push_back(l);
                    if tail.len() > 80 {
                        tail.pop_front();
                    }
                }
                None => break,
            },
            _ = &mut deadline => {
                timed_out = true;
                let _ = child.start_kill();
                let m = format!("⏱ gave up after {} minutes", CHECK_TIMEOUT.as_secs() / 60);
                let _ = tx.send(Ev::CheckLine(m.clone()));
                tail.push_back(m);
                break;
            }
        }
    }
    let ok = !timed_out && child.wait().await.map(|s| s.success()).unwrap_or(false);
    let _ = tx.send(Ev::CheckDone { ok, secs: started.elapsed().as_secs() });
    let tail: Vec<String> = tail.into_iter().collect();
    let _ = notes.send(Note::Check(ok, tail.join("\n")));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("src/api/**", "src/api/mod.rs"));
        assert!(glob_match("src/api/**", "src/api/v1/users.rs"));
        assert!(!glob_match("src/api/**", "src/apix/mod.rs"));
        assert!(glob_match("src/**/*.rs", "src/a.rs"));
        assert!(glob_match("src/**/*.rs", "src/x/y/a.rs"));
        assert!(!glob_match("src/*.rs", "src/x/a.rs"));
        assert!(glob_match("tests/test_api.py", "tests/test_api.py"));
        assert!(glob_match("src/api", "src/api/handlers.rs"));
        assert!(glob_match("src/api/", "src/api/handlers.rs"));
        assert!(!glob_match("src/api", "src/apis.rs"));
        assert!(glob_match("**/*.test.ts", "web/components/button.test.ts"));
        assert!(glob_match("./README.md", "README.md"));
        assert!(glob_match("**", "anything/at/all"));
        assert!(glob_match("src/?.rs", "src/a.rs"));
        assert!(!glob_match("", "src/a.rs"));
    }

    #[test]
    fn ansi() {
        assert_eq!(strip_ansi("\x1b[32mok\x1b[0m done"), "ok done");
        assert_eq!(strip_ansi("50%\r100%"), "100%");
        assert_eq!(strip_ansi("a\tb"), "a    b");
    }
}
