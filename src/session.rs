//! One long-lived agent. Claude and Antigravity keep a process open and take each new turn on
//! stdin; Codex gets a fresh `codex exec resume` per turn. Either way the orchestrator just sends
//! prompts and hears back when each turn ends.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::app::{AgentEv, Ev};
use crate::providers::{self, AgentSpec, Parser};

pub enum Input {
    Prompt(String),
    /// Kill whatever is running; the conversation can still be resumed by the next prompt.
    Stop,
}

/// What a session reports back to the orchestrator.
pub enum Note {
    Turn(usize, Result<String, String>),
    Session(usize, String),
    Cost(usize, f64),
}

#[derive(Clone)]
pub struct Handle(UnboundedSender<Input>);

impl Handle {
    pub fn send(&self, prompt: String) {
        let _ = self.0.send(Input::Prompt(prompt));
    }

    pub fn stop(&self) {
        let _ = self.0.send(Input::Stop);
    }
}

struct Proc {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Lines<BufReader<ChildStdout>>,
    stderr: JoinHandle<String>,
    parser: Parser,
}

pub fn spawn(
    idx: usize,
    spec: AgentSpec,
    dir: PathBuf,
    session: Option<String>,
    ev: UnboundedSender<Ev>,
    notes: UnboundedSender<Note>,
) -> Handle {
    let (tx, rx) = unbounded_channel();
    tokio::spawn(run(idx, spec, dir, session, ev, notes, rx));
    Handle(tx)
}

fn start(spec: &AgentSpec, dir: &Path, session: Option<&str>, prompt: Option<&str>) -> Result<Proc, String> {
    let mut child =
        providers::command(spec, dir, session, prompt).spawn().map_err(|e| format!("can't start {}: {e}", spec.provider.bin()))?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let mut err = child.stderr.take().ok_or("no stderr")?;
    let stderr = tokio::spawn(async move {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s).await;
        s
    });
    Ok(Proc { child, stdin, lines: BufReader::new(stdout).lines(), stderr, parser: Parser::new(spec.provider, dir) })
}

async fn next_line(p: &mut Option<Proc>) -> Option<String> {
    match p {
        Some(p) => p.lines.next_line().await.ok().flatten(),
        None => std::future::pending().await,
    }
}

enum Wake {
    Input(Option<Input>),
    Line(Option<String>),
}

async fn run(
    idx: usize,
    spec: AgentSpec,
    dir: PathBuf,
    mut session: Option<String>,
    ev: UnboundedSender<Ev>,
    notes: UnboundedSender<Note>,
    mut rx: UnboundedReceiver<Input>,
) {
    let send = |e: AgentEv| {
        let _ = ev.send(Ev::Agent(idx, e));
    };
    let mut proc: Option<Proc> = None;
    // turns handed to the CLI that haven't produced a result yet
    let mut open = 0usize;
    // Codex only: prompts waiting for the current turn to finish
    let mut queue: VecDeque<String> = VecDeque::new();

    loop {
        let wake = tokio::select! {
            i = rx.recv() => Wake::Input(i),
            l = next_line(&mut proc) => Wake::Line(l),
        };
        match wake {
            Wake::Input(None) => break,
            Wake::Input(Some(Input::Prompt(p))) => {
                if !spec.provider.persistent() {
                    queue.push_back(p);
                } else {
                    if proc.is_none() {
                        match start(&spec, &dir, session.as_deref(), None) {
                            Ok(p) => proc = Some(p),
                            Err(e) => {
                                send(AgentEv::Start);
                                send(AgentEv::Fail(e.clone()));
                                let _ = notes.send(Note::Turn(idx, Err(e)));
                                continue;
                            }
                        }
                    }
                    let line = providers::turn_line(spec.provider, &p);
                    let ok = match proc.as_mut().and_then(|p| p.stdin.as_mut()) {
                        Some(stdin) => stdin.write_all(line.as_bytes()).await.is_ok() && stdin.flush().await.is_ok(),
                        None => false,
                    };
                    open += 1;
                    if open == 1 {
                        send(AgentEv::Start);
                    }
                    if !ok {
                        // the process is gone; its EOF will fail the open turns
                        continue;
                    }
                }
            }
            Wake::Input(Some(Input::Stop)) => {
                queue.clear();
                if let Some(mut p) = proc.take() {
                    let _ = p.child.start_kill();
                    let _ = p.child.wait().await;
                }
                for _ in 0..open {
                    let _ = notes.send(Note::Turn(idx, Err("stopped".into())));
                }
                if open > 0 {
                    send(AgentEv::Stopped);
                }
                open = 0;
            }
            Wake::Line(Some(line)) => {
                let Some(p) = proc.as_mut() else { continue };
                for e in p.parser.feed(&line) {
                    if let AgentEv::Cost(c) = &e {
                        let _ = notes.send(Note::Cost(idx, *c));
                    }
                    if let AgentEv::Session(id) = &e {
                        if session.as_deref() != Some(id) {
                            session = Some(id.clone());
                            let _ = notes.send(Note::Session(idx, id.clone()));
                        }
                    }
                    send(e);
                }
                if let Some(r) = p.parser.take_turn() {
                    open = open.saturating_sub(1);
                    send(match &r {
                        Ok(t) => AgentEv::Done(t.clone()),
                        Err(e) => AgentEv::Fail(e.chars().take(140).collect()),
                    });
                    let _ = notes.send(Note::Turn(idx, r));
                    if open > 0 {
                        send(AgentEv::Start);
                    }
                }
            }
            Wake::Line(None) => {
                // process exited
                let Some(mut p) = proc.take() else { continue };
                let _ = p.child.wait().await;
                let stderr = p.stderr.await.unwrap_or_default();
                if open > 0 {
                    let e = p.parser.exit_error(&stderr);
                    send(AgentEv::Fail(e.chars().take(140).collect()));
                    for _ in 0..open {
                        let _ = notes.send(Note::Turn(idx, Err(e.clone())));
                    }
                    open = 0;
                }
            }
        }

        // Codex: start the next queued turn once nothing is running
        while !spec.provider.persistent() && proc.is_none() && open == 0 {
            let Some(p) = queue.pop_front() else { break };
            send(AgentEv::Start);
            match start(&spec, &dir, session.as_deref(), Some(&p)) {
                Ok(pr) => {
                    proc = Some(pr);
                    open = 1;
                }
                Err(e) => {
                    send(AgentEv::Fail(e.clone()));
                    let _ = notes.send(Note::Turn(idx, Err(e)));
                }
            }
        }
    }
}
