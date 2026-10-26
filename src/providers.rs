//! How to launch each agent CLI headless, and how to turn its JSON event stream into dashboard events.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;

use serde_json::Value;
use tokio::process::Command;

use crate::app::AgentEv;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Provider {
    Claude,
    Codex,
    Agy,
}

impl Provider {
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

pub fn command(spec: &AgentSpec, prompt: &str, dir: &Path) -> Command {
    let mut c = Command::new(spec.provider.bin());
    match spec.provider {
        Provider::Claude => {
            c.arg("-p")
                .arg(prompt)
                .args(["--output-format", "stream-json", "--verbose", "--include-partial-messages"])
                .args(["--permission-mode", "acceptEdits", "--strict-mcp-config"])
                .args(["--allowedTools", "Read,Write,Edit,Glob,Grep"])
                .arg("--model")
                .arg(&spec.model);
        }
        Provider::Codex => {
            c.args(["exec", "--json", "--full-auto", "--skip-git-repo-check", "-C"]).arg(dir);
            if !spec.model.is_empty() {
                c.arg("-m").arg(&spec.model);
            }
            c.arg(prompt);
        }
        Provider::Agy => {
            c.arg("-p")
                .arg(prompt)
                .args(["--output-format", "stream-json", "--mode", "accept-edits"])
                .arg("--model")
                .arg(&spec.model)
                .arg("--add-dir")
                .arg(dir);
        }
    }
    c.current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    c
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
                                if let Some(ch) = u32::from_str_radix(&self.raw[i + 2..i + 6], 16)
                                    .ok()
                                    .and_then(char::from_u32)
                                {
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

pub struct Parser {
    provider: Provider,
    dir: String,
    blocks: HashMap<u64, InputStream>,
    partial: bool,
    agy_tool_step: Option<u64>,
    final_text: Option<String>,
    error: Option<String>,
    last_text: String,
}

impl Parser {
    pub fn new(provider: Provider, dir: &Path) -> Self {
        Parser {
            provider,
            dir: dir.to_string_lossy().to_string(),
            blocks: HashMap::new(),
            partial: false,
            agy_tool_step: None,
            final_text: None,
            error: None,
            last_text: String::new(),
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

    pub fn finish(self, exit_ok: bool, stderr: &str) -> Result<String, String> {
        if let Some(e) = self.error {
            return Err(e);
        }
        if let Some(t) = self.final_text {
            return Ok(t);
        }
        if exit_ok {
            return Ok(self.last_text);
        }
        let tail = stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("process exited")
            .trim();
        Err(tail.chars().take(160).collect())
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
                    "message_start" => self.last_text.clear(),
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
            "system" => {
                if v["subtype"] == "thinking_tokens" {
                    if let Some(n) = v["estimated_tokens_delta"].as_u64() {
                        out.push(AgentEv::Tokens(n));
                    }
                }
            }
            "result" => {
                if let Some(c) = v["total_cost_usd"].as_f64() {
                    out.push(AgentEv::Cost(c));
                }
                if let Some(n) = v["usage"]["output_tokens"].as_u64() {
                    out.push(AgentEv::TokensTotal(n));
                }
                let text = v["result"].as_str().unwrap_or("").to_string();
                let failed = v["is_error"].as_bool().unwrap_or(false)
                    || v["subtype"].as_str().is_some_and(|s| s != "success");
                if failed {
                    let why = if text.is_empty() {
                        v["subtype"].as_str().unwrap_or("error").to_string()
                    } else {
                        text
                    };
                    self.error = Some(why);
                } else {
                    self.final_text = Some(text);
                }
            }
            _ => {}
        }
    }

    fn codex(&mut self, v: &Value, out: &mut Vec<AgentEv>) {
        let m = &v["msg"];
        match m["type"].as_str().unwrap_or("") {
            "agent_message" => {
                let t = m["message"].as_str().unwrap_or("").to_string();
                self.last_text.clear();
                self.text(out, &format!("{t}\n"), true);
            }
            "agent_message_delta" => {
                let t = m["delta"].as_str().unwrap_or("").to_string();
                self.text(out, &t, true);
            }
            "agent_reasoning" | "agent_reasoning_delta" => {
                let t = m["text"].as_str().or(m["delta"].as_str()).unwrap_or("");
                if !t.is_empty() {
                    out.push(AgentEv::Thinking(t.to_string()));
                }
            }
            "exec_command_begin" => {
                out.push(AgentEv::ToolStart("Bash".into()));
                let cmd = match &m["command"] {
                    Value::Array(parts) => parts.last().and_then(|p| p.as_str()).unwrap_or("").to_string(),
                    Value::String(s) => s.clone(),
                    _ => String::new(),
                };
                out.push(AgentEv::ToolDetail(short(&cmd, &self.dir)));
            }
            "exec_command_end" => out.push(AgentEv::ToolDone(m["exit_code"].as_i64().unwrap_or(0) != 0)),
            "patch_apply_begin" => {
                out.push(AgentEv::ToolStart("Edit".into()));
                if let Some((path, change)) = m["changes"].as_object().and_then(|c| c.iter().next()) {
                    out.push(AgentEv::ToolDetail(short(path, &self.dir)));
                    let body = change["add"]["content"]
                        .as_str()
                        .or(change["update"]["unified_diff"].as_str())
                        .unwrap_or("");
                    if !body.is_empty() {
                        out.push(AgentEv::Code(body.to_string()));
                    }
                }
            }
            "patch_apply_end" => out.push(AgentEv::ToolDone(!m["success"].as_bool().unwrap_or(true))),
            "token_count" => {
                let n = m["info"]["total_token_usage"]["output_tokens"]
                    .as_u64()
                    .or(m["output_tokens"].as_u64());
                if let Some(n) = n {
                    out.push(AgentEv::TokensTotal(n));
                }
            }
            "task_complete" => {
                let t = m["last_agent_message"].as_str().map(String::from).unwrap_or_else(|| self.last_text.clone());
                self.final_text = Some(t);
                self.error = None;
            }
            "error" => self.error = Some(m["message"].as_str().unwrap_or("codex error").to_string()),
            "stream_error" => {
                let t = m["message"].as_str().unwrap_or("stream error");
                out.push(AgentEv::Thinking(format!("⚠ {t}\n")));
            }
            _ => {}
        }
    }

    fn agy(&mut self, v: &Value, out: &mut Vec<AgentEv>) {
        match v["event"].as_str().unwrap_or("") {
            "step_update" => {
                let s = &v["step_update"];
                let state = s["state"].as_str().unwrap_or("");
                let step = s["step_index"].as_u64().unwrap_or(0);
                match s["step_type"].as_str().unwrap_or("") {
                    "tool" => {
                        let raw = s["tool_name"].as_str().unwrap_or("tool");
                        let params = &s["tool_info"]["parameters"];
                        let first = params
                            .as_object()
                            .and_then(|o| o.values().find_map(|v| v.as_str()))
                            .unwrap_or("")
                            .to_string();
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
                if r["status"] == "SUCCESS" {
                    self.final_text = Some(resp);
                } else {
                    let status = r["status"].as_str().unwrap_or("FAILED");
                    self.error = Some(format!("{status} {resp}").trim().to_string());
                }
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
