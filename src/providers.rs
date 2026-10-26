//! How to launch each agent CLI headless, and how to turn its JSON event stream into dashboard events.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;

use serde_json::{json, Value};
use tokio::process::Command;

use crate::app::AgentEv;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Provider {
    Claude,
    Codex,
    Agy,
}

impl Provider {
    pub const ALL: [Provider; 3] = [Provider::Claude, Provider::Agy, Provider::Codex];

    pub fn label(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
            Provider::Agy => "antigravity",
        }
    }

    pub fn bin(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
            Provider::Agy => "agy",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
            Provider::Agy => "agy",
        }
    }

    pub fn parse(s: &str) -> Option<Provider> {
        match s.trim().to_lowercase().as_str() {
            "claude" | "anthropic" => Some(Provider::Claude),
            "agy" | "antigravity" | "gemini" => Some(Provider::Agy),
            "codex" | "openai" | "gpt" => Some(Provider::Codex),
            _ => None,
        }
    }

    /// Claude and Antigravity keep one process alive and take new turns on stdin;
    /// Codex runs one process per turn and resumes its thread.
    pub fn persistent(self) -> bool {
        self != Provider::Codex
    }

    /// Models offered in the launcher; the first is the default.
    pub fn models(self) -> Vec<String> {
        let v: &[&str] = match self {
            Provider::Claude => &["sonnet", "opus", "haiku", "fable"],
            Provider::Agy => &[
                "gemini-3.8-flash-high",
                "gemini-3.8-flash-medium",
                "gemini-3.8-flash-low",
                "claude-sonnet-4-6",
                "claude-opus-4-6-thinking",
                "gpt-oss-120b-medium",
            ],
            Provider::Codex => return codex_models(),
        };
        v.iter().map(|s| s.to_string()).collect()
    }
}

/// Codex keeps the model list it fetched in ~/.codex/models_cache.json; "" means its configured default.
fn codex_models() -> Vec<String> {
    let mut out = vec![String::new()];
    let path = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default().join(".codex/models_cache.json");
    if let Some(v) = std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) {
        let list = v["models"].as_array().or(v.as_array()).cloned().unwrap_or_default();
        for m in list {
            if m["visibility"].as_str().is_some_and(|x| x != "list") {
                continue;
            }
            if let Some(slug) = m["slug"].as_str().or(m["id"].as_str()) {
                out.push(slug.to_string());
            }
        }
    }
    out
}

#[derive(Clone, Debug)]
pub struct AgentSpec {
    pub name: String,
    pub provider: Provider,
    pub model: String,
}

impl AgentSpec {
    pub fn model_label(&self) -> &str {
        if self.model.is_empty() {
            "default"
        } else {
            &self.model
        }
    }
}

/// How to launch an agent. Claude and Antigravity read their turns from stdin as JSON lines, so
/// `prompt` is only used by Codex. `session` resumes an earlier conversation.
pub fn command(spec: &AgentSpec, dir: &Path, session: Option<&str>, prompt: Option<&str>) -> Command {
    let mut c = Command::new(spec.provider.bin());
    match spec.provider {
        Provider::Claude => {
            c.arg("-p")
                .args(["--input-format", "stream-json", "--output-format", "stream-json"])
                .args(["--verbose", "--include-partial-messages"])
                .args(["--permission-mode", "acceptEdits", "--strict-mcp-config"])
                .args(["--allowedTools", "Read,Write,Edit,Glob,Grep"])
                .arg("--model")
                .arg(&spec.model);
            if let Some(s) = session {
                c.arg("--resume").arg(s);
            }
        }
        Provider::Codex => {
            c.args(["exec", "--json", "-s", "workspace-write", "--skip-git-repo-check", "-C"]).arg(dir);
            if !spec.model.is_empty() {
                c.arg("-m").arg(&spec.model);
            }
            if let Some(s) = session {
                c.arg("resume").arg(s);
            }
            c.arg(prompt.unwrap_or(""));
        }
        Provider::Agy => {
            c.args(["-p", "", "--input-format", "stream-json", "--output-format", "stream-json"])
                .args(["--mode", "accept-edits"])
                .arg("--model")
                .arg(&spec.model)
                .arg("--add-dir")
                .arg(dir);
            if let Some(s) = session {
                c.arg("--conversation").arg(s);
            }
        }
    }
    let stdin = if spec.provider.persistent() { Stdio::piped() } else { Stdio::null() };
    c.current_dir(dir).stdin(stdin).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    c
}

/// One user turn, in the stdin format the persistent CLIs expect.
pub fn turn_line(provider: Provider, text: &str) -> String {
    let msg = json!({"role": "user", "content": text});
    let v = match provider {
        Provider::Agy => json!({"event": "user", "message": msg}),
        _ => json!({"type": "user", "message": msg}),
    };
    format!("{v}\n")
}

/// Rough output-token estimate for streamed text (the real count arrives at the end).
fn tok(bytes: usize) -> u64 {
    bytes.div_ceil(4) as u64
}

/// Path relative to the project folder, squashed onto one line.
pub fn short(s: &str, dir: &str) -> String {
    let s = s.strip_prefix(dir).map(|r| r.trim_start_matches('/')).unwrap_or(s);
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > 70 {
        format!("{}…", s.chars().take(69).collect::<String>())
    } else {
        s
    }
}

fn detail(input: &Value, dir: &str) -> String {
    for k in ["file_path", "path", "pattern", "command", "url", "query"] {
        if let Some(s) = input[k].as_str() {
            return short(s, dir);
        }
    }
    String::new()
}

/// Decodes a tool call's JSON arguments while they are still streaming in, so we can show
/// the file contents being typed live instead of waiting for the call to finish.
#[derive(Default)]
struct InputStream {
    raw: String,
    path_sent: bool,
    start: Option<usize>,
    pos: usize,
    done: bool,
}

/// Complete string value for `key` in partial JSON, if its closing quote has arrived.
fn json_str(raw: &str, key: &str) -> Option<String> {
    let k = raw.find(&format!("\"{key}\""))?;
    let rest = &raw[k + key.len() + 2..];
    let q = rest.find('"')?;
    if rest[..q].trim() != ":" {
        return None;
    }
    let body = &rest[q + 1..];
    let mut out = String::new();
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    None
}

impl InputStream {
    fn push(&mut self, delta: &str) -> (Option<String>, String) {
        self.raw.push_str(delta);
        let mut path = None;
        if !self.path_sent {
            if let Some(p) = json_str(&self.raw, "file_path").or_else(|| json_str(&self.raw, "path")) {
                path = Some(p);
                self.path_sent = true;
            }
        }
        if self.start.is_none() {
            for key in ["\"content\"", "\"new_string\"", "\"command\""] {
                let Some(k) = self.raw.find(key) else { continue };
                let after = k + key.len();
                let rest = &self.raw[after..];
                if let Some(q) = rest.find('"') {
                    if rest[..q].trim() == ":" {
                        self.start = Some(after + q + 1);
                        self.pos = after + q + 1;
                        break;
                    }
                }
            }
        }
        let mut out = String::new();
        if self.start.is_some() && !self.done {
            let bytes = self.raw.as_bytes();
            let mut i = self.pos;
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' => {
                        if i + 1 >= bytes.len() {
                            break;
                        }
                        match bytes[i + 1] {
                            b'n' => out.push('\n'),
                            b't' => out.push_str("  "),
                            b'r' => {}
                            b'u' => {
                                if i + 6 > bytes.len() {
                                    break;
                                }
                                if let Some(ch) = u32::from_str_radix(&self.raw[i + 2..i + 6], 16).ok().and_then(char::from_u32) {
                                    out.push(ch);
                                }
                                i += 6;
                                continue;
                            }
                            other => out.push(other as char),
                        }
                        i += 2;
                    }
                    b'"' => {
                        self.done = true;
                        i += 1;
                        break;
                    }
                    _ => {
                        let ch = self.raw[i..].chars().next().unwrap_or(' ');
                        out.push(ch);
                        i += ch.len_utf8();
                    }
                }
            }
            self.pos = i;
        }
        (path, out)
    }
}

/// Turns one agent process's JSON lines into dashboard events and tells when a turn has ended.
pub struct Parser {
    provider: Provider,
    dir: String,
    blocks: HashMap<u64, InputStream>,
    partial: bool,
    agy_tool_step: Option<u64>,
    last_text: String,
    /// Claude reports cost as a running total for the process; we hand out deltas.
    cost_seen: f64,
    turn: Option<Result<String, String>>,
    codex_error: Option<String>,
}

impl Parser {
    pub fn new(provider: Provider, dir: &Path) -> Self {
        Parser {
            provider,
            dir: dir.to_string_lossy().to_string(),
            blocks: HashMap::new(),
            partial: false,
            agy_tool_step: None,
            last_text: String::new(),
            cost_seen: 0.0,
            turn: None,
            codex_error: None,
        }
    }

    pub fn feed(&mut self, line: &str) -> Vec<AgentEv> {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        match self.provider {
            Provider::Claude => self.claude(&v, &mut out),
            Provider::Codex => self.codex(&v, &mut out),
            Provider::Agy => self.agy(&v, &mut out),
        }
        out
    }

    /// The result of the turn that just ended, once its final event has arrived.
    pub fn take_turn(&mut self) -> Option<Result<String, String>> {
        let t = self.turn.take();
        if t.is_some() {
            self.last_text.clear();
            self.agy_tool_step = None;
        }
        t
    }

    /// Why the process died with a turn still open.
    pub fn exit_error(&self, stderr: &str) -> String {
        if let Some(e) = &self.codex_error {
            return e.clone();
        }
        let tail = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("process exited").trim();
        tail.chars().take(160).collect()
    }

    fn text(&mut self, out: &mut Vec<AgentEv>, t: &str, count: bool) {
        if t.is_empty() {
            return;
        }
        self.last_text.push_str(t);
        if count {
            out.push(AgentEv::Tokens(tok(t.len())));
        }
        out.push(AgentEv::Text(t.to_string()));
    }

    fn claude(&mut self, v: &Value, out: &mut Vec<AgentEv>) {
        match v["type"].as_str().unwrap_or("") {
            "stream_event" => {
                if !v["parent_tool_use_id"].is_null() {
                    return;
                }
                self.partial = true;
                let e = &v["event"];
                let idx = e["index"].as_u64().unwrap_or(0);
                match e["type"].as_str().unwrap_or("") {
                    "message_start" => {
                        self.last_text.clear();
                        self.blocks.clear();
                    }
                    "content_block_start" => {
                        let b = &e["content_block"];
                        if b["type"] == "tool_use" {
                            self.blocks.insert(idx, InputStream::default());
                            out.push(AgentEv::ToolStart(b["name"].as_str().unwrap_or("tool").to_string()));
                        }
                    }
                    "content_block_delta" => {
                        let d = &e["delta"];
                        match d["type"].as_str().unwrap_or("") {
                            "text_delta" => {
                                let t = d["text"].as_str().unwrap_or("").to_string();
                                self.text(out, &t, true);
                            }
                            "thinking_delta" => {
                                let t = d["thinking"].as_str().unwrap_or("");
                                if !t.is_empty() {
                                    out.push(AgentEv::Tokens(tok(t.len())));
                                    out.push(AgentEv::Thinking(t.to_string()));
                                }
                            }
                            "input_json_delta" => {
                                let p = d["partial_json"].as_str().unwrap_or("");
                                if let Some(s) = self.blocks.get_mut(&idx) {
                                    let (path, code) = s.push(p);
                                    if let Some(path) = path {
                                        out.push(AgentEv::ToolDetail(short(&path, &self.dir)));
                                    }
                                    if !code.is_empty() {
                                        out.push(AgentEv::Tokens(tok(code.len())));
                                        out.push(AgentEv::Code(code));
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            "assistant" => {
                if !v["parent_tool_use_id"].is_null() {
                    return;
                }
                let Some(content) = v["message"]["content"].as_array() else { return };
                for b in content {
                    match b["type"].as_str().unwrap_or("") {
                        "tool_use" => {
                            if !self.partial {
                                out.push(AgentEv::ToolStart(b["name"].as_str().unwrap_or("tool").to_string()));
                            }
                            let d = detail(&b["input"], &self.dir);
                            if !d.is_empty() {
                                out.push(AgentEv::ToolDetail(d));
                            }
                        }
                        "text" if !self.partial => {
                            let t = b["text"].as_str().unwrap_or("").to_string();
                            self.text(out, &t, true);
                        }
                        _ => {}
                    }
                }
            }
            "user" => {
                let Some(content) = v["message"]["content"].as_array() else { return };
                for b in content {
                    if b["type"] == "tool_result" {
                        out.push(AgentEv::ToolDone(b["is_error"].as_bool().unwrap_or(false)));
                    }
                }
            }
            "system" => match v["subtype"].as_str().unwrap_or("") {
                "init" => {
                    if let Some(id) = v["session_id"].as_str() {
                        out.push(AgentEv::Session(id.to_string()));
                    }
                }
                "thinking_tokens" => {
                    if let Some(n) = v["estimated_tokens_delta"].as_u64() {
                        out.push(AgentEv::Tokens(n));
                    }
                }
                _ => {}
            },
            "result" => {
                if let Some(c) = v["total_cost_usd"].as_f64() {
                    if c > self.cost_seen {
                        out.push(AgentEv::Cost(c - self.cost_seen));
                        self.cost_seen = c;
                    }
                }
                if let Some(n) = v["usage"]["output_tokens"].as_u64() {
                    out.push(AgentEv::TokensTotal(n));
                }
                let text = v["result"].as_str().unwrap_or("").to_string();
                let failed = v["is_error"].as_bool().unwrap_or(false) || v["subtype"].as_str().is_some_and(|s| s != "success");
                self.turn = Some(if failed {
                    Err(if text.is_empty() { v["subtype"].as_str().unwrap_or("error").to_string() } else { text })
                } else {
                    Ok(text)
                });
            }
            _ => {}
        }
    }

    fn codex(&mut self, v: &Value, out: &mut Vec<AgentEv>) {
        match v["type"].as_str().unwrap_or("") {
            "thread.started" => {
                if let Some(id) = v["thread_id"].as_str() {
                    out.push(AgentEv::Session(id.to_string()));
                }
            }
            "item.started" | "item.updated" | "item.completed" => {
                let done = v["type"] == "item.completed";
                let it = &v["item"];
                match it["type"].as_str().unwrap_or("") {
                    "agent_message" if done => {
                        let t = it["text"].as_str().unwrap_or("").to_string();
                        self.last_text.clear();
                        self.text(out, &format!("{t}\n"), true);
                    }
                    "reasoning" if done => {
                        let t = it["text"].as_str().unwrap_or("");
                        if !t.is_empty() {
                            out.push(AgentEv::Tokens(tok(t.len())));
                            out.push(AgentEv::Thinking(format!("{t}\n")));
                        }
                    }
                    "command_execution" => {
                        if v["type"] == "item.started" {
                            out.push(AgentEv::ToolStart("Bash".into()));
                            let cmd = it["command"].as_str().unwrap_or("");
                            // strip the `/usr/bin/zsh -lc '...'` wrapper
                            let cmd = cmd.split_once(" -lc ").map(|(_, c)| c.trim_matches('\'')).unwrap_or(cmd);
                            out.push(AgentEv::ToolDetail(short(cmd, &self.dir)));
                        } else if done {
                            // exit 1 is usually "no matches" from rg/grep, not a real failure
                            out.push(AgentEv::ToolDone(it["exit_code"].as_i64().is_some_and(|c| c > 1)));
                        }
                    }
                    "file_change" => {
                        let changes = it["changes"].as_array().cloned().unwrap_or_default();
                        let path = changes.first().and_then(|c| c["path"].as_str()).unwrap_or("").to_string();
                        if v["type"] == "item.started" {
                            let added = changes.first().is_some_and(|c| c["kind"] == "add");
                            out.push(AgentEv::ToolStart(if added { "Write" } else { "Edit" }.into()));
                            out.push(AgentEv::ToolDetail(short(&path, &self.dir)));
                        } else if done {
                            // Codex doesn't stream patch contents; replay what landed on disk
                            if let Ok(body) = std::fs::read_to_string(&path) {
                                out.push(AgentEv::Code(body.chars().take(6000).collect()));
                            }
                            out.push(AgentEv::ToolDone(it["status"] == "failed"));
                        }
                    }
                    "web_search" if v["type"] == "item.started" => {
                        out.push(AgentEv::ToolStart("WebSearch".into()));
                        out.push(AgentEv::ToolDetail(short(it["query"].as_str().unwrap_or(""), &self.dir)));
                    }
                    "web_search" if done => out.push(AgentEv::ToolDone(false)),
                    "mcp_tool_call" if v["type"] == "item.started" => {
                        out.push(AgentEv::ToolStart(it["tool"].as_str().unwrap_or("mcp").to_string()));
                    }
                    "mcp_tool_call" if done => out.push(AgentEv::ToolDone(it["status"] == "failed")),
                    "todo_list" if done => {
                        let items = it["items"].as_array().cloned().unwrap_or_default();
                        let list: String = items
                            .iter()
                            .map(|i| format!("{} {}\n", if i["completed"] == true { "☑" } else { "☐" }, i["text"].as_str().unwrap_or("")))
                            .collect();
                        out.push(AgentEv::Thinking(list));
                    }
                    "error" if done => {
                        let t = it["message"].as_str().unwrap_or("error");
                        out.push(AgentEv::Thinking(format!("⚠ {t}\n")));
                    }
                    _ => {}
                }
            }
            "turn.completed" => {
                let u = &v["usage"];
                let n = u["output_tokens"].as_u64().unwrap_or(0) + u["reasoning_output_tokens"].as_u64().unwrap_or(0);
                if n > 0 {
                    out.push(AgentEv::TokensTotal(n));
                }
                self.turn = Some(Ok(self.last_text.trim().to_string()));
            }
            "turn.failed" => {
                let e = v["error"]["message"].as_str().unwrap_or("codex turn failed").to_string();
                self.turn = Some(Err(e));
            }
            "error" => {
                let t = v["message"].as_str().unwrap_or("codex error").to_string();
                out.push(AgentEv::Thinking(format!("⚠ {t}\n")));
                self.codex_error = Some(t);
            }
            _ => {}
        }
    }

    fn agy(&mut self, v: &Value, out: &mut Vec<AgentEv>) {
        match v["event"].as_str().unwrap_or("") {
            "init" => {
                if let Some(id) = v["conversation_id"].as_str() {
                    out.push(AgentEv::Session(id.to_string()));
                }
            }
            "step_update" => {
                let s = &v["step_update"];
                let state = s["state"].as_str().unwrap_or("");
                let step = s["step_index"].as_u64().unwrap_or(0);
                match s["step_type"].as_str().unwrap_or("") {
                    "tool" => {
                        let raw = s["tool_name"].as_str().unwrap_or("tool");
                        let params = &s["tool_info"]["parameters"];
                        let first = params.as_object().and_then(|o| o.values().find_map(|v| v.as_str())).unwrap_or("").to_string();
                        if state == "ACTIVE" && self.agy_tool_step != Some(step) {
                            self.agy_tool_step = Some(step);
                            out.push(AgentEv::ToolStart(agy_tool(raw)));
                            if !first.is_empty() {
                                out.push(AgentEv::ToolDetail(short(&first, &self.dir)));
                            }
                        } else if state != "ACTIVE" && !state.is_empty() {
                            // Antigravity doesn't stream file contents, so read what it wrote and replay it.
                            let writes = ["write", "edit", "replace", "create"].iter().any(|w| raw.contains(w));
                            if writes && state == "DONE" && first.starts_with('/') {
                                if let Ok(body) = std::fs::read_to_string(&first) {
                                    let body: String = body.chars().take(6000).collect();
                                    out.push(AgentEv::Code(body));
                                }
                            }
                            out.push(AgentEv::ToolDone(state != "DONE"));
                        }
                    }
                    "agent_response" => {
                        if let Some(t) = s["thinking_delta"].as_str() {
                            if !t.is_empty() {
                                out.push(AgentEv::Thinking(t.to_string()));
                            }
                        }
                        if let Some(t) = s["text_delta"].as_str() {
                            let t = t.to_string();
                            self.text(out, &t, false);
                        }
                        if state == "DONE" {
                            let u = &s["usage"];
                            let n = u["output_tokens"].as_u64().unwrap_or(0) + u["thinking_tokens"].as_u64().unwrap_or(0);
                            if n > 0 {
                                out.push(AgentEv::Tokens(n));
                            }
                        }
                    }
                    _ => {}
                }
            }
            "result" => {
                let r = &v["result"];
                let resp = r["response"].as_str().unwrap_or("").to_string();
                self.turn = Some(if r["status"] == "SUCCESS" {
                    Ok(resp)
                } else {
                    let status = r["status"].as_str().unwrap_or("FAILED");
                    let err = r["error"].as_str().unwrap_or("");
                    Err(format!("{status} {err} {resp}").split_whitespace().collect::<Vec<_>>().join(" "))
                });
            }
            _ => {}
        }
    }
}

fn agy_tool(raw: &str) -> String {
    match raw {
        "write_file" | "write_to_file" => "Write".into(),
        "view_file" | "read_file" => "Read".into(),
        "replace_file_content" | "edit_file" | "multi_replace_file_content" => "Edit".into(),
        "run_command" => "Bash".into(),
        "list_dir" => "List".into(),
        "grep_search" | "find_by_name" => "Search".into(),
        other => {
            let mut s = other.replace('_', " ");
            if let Some(f) = s.get_mut(0..1) {
                f.make_ascii_uppercase();
            }
            s
        }
    }
}
