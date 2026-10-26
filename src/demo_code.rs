//! Demo mode, code edition: scripted agents build `csvjson`, a small Rust CLI. The files are real
//! (the project compiles and its tests pass), the first `cargo test` fails on a real bug, the lead
//! routes the fix to the parser's owner, the re-check passes and the round ships. Zero tokens.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::sleep;

use crate::app::{AgentEv, Ev, Level, Phase, PlanItem};
use crate::demo::{ms, quick_tool, send, stream, write_file, Demo, Out};
use crate::fx::Rng;
use crate::orchestrator::{self, msg, Cmd, Config, Shared, Tx};
use crate::project::Kind;

pub const TASK: &str = "csvjson: a Rust CLI that streams CSV to JSON, with column selection and tests";
const TITLE: &str = "csvjson";
const CHECK: &str = "cargo test";

pub const LEAD_PINGS: &[&str] = &[
    "std only, no crates: the check has to run offline",
    "keep the signatures from main.rs, they're the contract",
    "tests live next to the code they test",
    "halfway there, nice and small",
];
const REPLIES: &[&str] = &["copy that", "on it", "makes sense, matching it", "good catch, adjusting", "done, synced"];

struct Job {
    id: &'static str,
    owns: &'static [&'static str],
    brief: &'static str,
    think: &'static str,
    files: &'static [(&'static str, &'static str)],
    /// (task id of the teammate, message)
    peer: Option<(&'static str, &'static str)>,
    summary: &'static str,
}

const JOBS: &[Job] = &[
    Job {
        id: "parser",
        owns: &["src/csv.rs"],
        brief: "csv::parse_row(&str) -> Vec<String>: commas, quoted fields, commas inside quotes, doubled quotes. Unit tests in the same file.",
        think: "parse_row splits on commas outside quotes.\nOne flag for 'inside quotes', push a field on every bare comma.\nTests: plain row, quoted comma, escaped quote, empty fields.\n",
        files: &[("src/csv.rs", CSV_BUGGY)],
        peer: Some(("writer", "parse_row hands you owned Strings, one per column; blank fields are empty strings")),
        summary: "#parser done: parse_row with quoted fields, 4 tests",
    },
    Job {
        id: "writer",
        owns: &["src/json.rs"],
        brief: "json::string escapes for JSON; json::object(keep, header, row, pretty) builds one object per row. Tests in the file.",
        think: "Escaping first: quotes, backslashes, \\n \\r \\t, other control chars as \\u00XX.\nobject() takes column indexes, so filtering stays somebody else's job.\n",
        files: &[("src/json.rs", JSON_RS)],
        peer: Some(("cli", "object() takes pretty: bool, wire --pretty straight into it")),
        summary: "#writer done: escaping + object(), 3 tests",
    },
    Job {
        id: "cli",
        owns: &["src/cli.rs"],
        brief: "cli::parse(args) -> Result<Opts, String> with --select a,b and --pretty; --help prints usage.",
        think: "No clap, std only: a tiny hand-rolled parser.\nOpts { select, pretty }; errors as plain strings for main to print.\n",
        files: &[("src/cli.rs", CLI_RS)],
        peer: Some(("filter", "select arrives as column names, in the order the user typed them")),
        summary: "#cli done: --select / --pretty parsing, 2 tests",
    },
    Job {
        id: "filter",
        owns: &["src/filter.rs"],
        brief: "filter::columns(header, select) -> Vec<usize>: every column, or the selected names in the order given.",
        think: "Map names to header indexes, keep the user's order, skip unknown names.\n",
        files: &[("src/filter.rs", FILTER_RS)],
        peer: None,
        summary: "#filter done: columns(), 2 tests",
    },
    Job {
        id: "e2e",
        owns: &["tests/cli.rs"],
        brief: "End-to-end tests that pipe CSV through the built binary and check the JSON that comes out.",
        think: "Integration tests drive the real binary through CARGO_BIN_EXE_csvjson.\nOne happy path, one --select case.\n",
        files: &[("tests/cli.rs", TESTS_CLI)],
        peer: Some(("parser", "my tests feed plain rows like 1,Ada; ping me if quoting changes the output shape")),
        summary: "#e2e done: 2 tests through the binary",
    },
    Job {
        id: "docs",
        owns: &["README.md", "examples/**"],
        brief: "README with the one command people need and both flags, plus an example CSV that shows the hard quoting cases.",
        think: "README: one command, two flags, what quoting it handles.\nThe example CSV gets a quoted comma and doubled quotes so the docs show the hard case.\n",
        files: &[("README.md", README_MD), ("examples/people.csv", PEOPLE_CSV)],
        peer: None,
        summary: "#docs done: README + examples/people.csv",
    },
    Job {
        id: "roundtrip",
        owns: &["tests/roundtrip.rs"],
        brief: "A test that punctuation-heavy fields come out of the binary exactly as they went in.",
        think: "Fields full of commas and quotes must survive CSV → JSON unchanged.\n",
        files: &[("tests/roundtrip.rs", TESTS_ROUNDTRIP)],
        peer: Some(("parser", "my roundtrip feeds \"say \"\"hi\"\"\": doubled quotes have to collapse to one")),
        summary: "#roundtrip done: tricky-field survival test",
    },
];

const CARGO_TOML: &str = r####"[package]
name = "csvjson"
version = "0.1.0"
edition = "2021"
description = "Stream CSV in, get JSON out"

[dependencies]
"####;

const MAIN_RS: &str = r####"//! csvjson: stream CSV in, get JSON out. One module per job:
//! csv (parse rows) → filter (pick columns) → json (write objects); cli reads the flags.
mod cli;
mod csv;
mod filter;
mod json;

use std::io::{self, BufRead, Write};

fn main() {
    let opts = match cli::parse(std::env::args().skip(1)) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("csvjson: {e}");
            std::process::exit(2);
        }
    };
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines().map_while(Result::ok);
    let Some(header) = lines.next() else { return };
    let header = csv::parse_row(&header);
    let keep = filter::columns(&header, &opts.select);
    let mut out = io::stdout().lock();
    let _ = write!(out, "[");
    for (i, line) in lines.enumerate() {
        let row = csv::parse_row(&line);
        let sep = if i == 0 { "\n" } else { ",\n" };
        let _ = write!(out, "{sep}{}", json::object(&keep, &header, &row, opts.pretty));
    }
    let _ = writeln!(out, "\n]");
}
"####;

const CSV_BUGGY: &str = r####"//! RFC 4180-style rows: commas, quoted fields, doubled quotes.

/// Split one CSV line into its fields.
pub fn parse_row(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => fields.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    fields.push(cur);
    fields
}

#[cfg(test)]
mod tests {
    use super::parse_row;

    #[test]
    fn plain_row() {
        assert_eq!(parse_row("a,b,c"), ["a", "b", "c"]);
    }

    #[test]
    fn quoted_comma() {
        assert_eq!(parse_row("\"x, y\",z"), ["x, y", "z"]);
    }

    #[test]
    fn escaped_quote() {
        assert_eq!(parse_row("\"say \"\"hi\"\"\""), ["say \"hi\""]);
    }

    #[test]
    fn empty_fields() {
        assert_eq!(parse_row(",,"), ["", "", ""]);
    }
}
"####;

const CSV_FIXED: &str = r####"//! RFC 4180-style rows: commas, quoted fields, doubled quotes.

/// Split one CSV line into its fields.
pub fn parse_row(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // a doubled quote inside a quoted field is one literal quote
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => fields.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    fields.push(cur);
    fields
}

#[cfg(test)]
mod tests {
    use super::parse_row;

    #[test]
    fn plain_row() {
        assert_eq!(parse_row("a,b,c"), ["a", "b", "c"]);
    }

    #[test]
    fn quoted_comma() {
        assert_eq!(parse_row("\"x, y\",z"), ["x, y", "z"]);
    }

    #[test]
    fn escaped_quote() {
        assert_eq!(parse_row("\"say \"\"hi\"\"\""), ["say \"hi\""]);
    }

    #[test]
    fn empty_fields() {
        assert_eq!(parse_row(",,"), ["", "", ""]);
    }
}
"####;

const FIX_SNIPPET: &str = r####"    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // a doubled quote inside a quoted field is one literal quote
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
"####;

const JSON_RS: &str = r####"//! JSON output: string escaping and one object per row.

/// Quote and escape a string for JSON.
pub fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// One row as a JSON object with only the `keep` columns.
pub fn object(keep: &[usize], header: &[String], row: &[String], pretty: bool) -> String {
    let fields: Vec<String> = keep
        .iter()
        .map(|&i| {
            let v = row.get(i).map(String::as_str).unwrap_or("");
            format!("{}: {}", string(&header[i]), string(v))
        })
        .collect();
    if pretty {
        format!("  {{\n    {}\n  }}", fields.join(",\n    "))
    } else {
        format!("  {{{}}}", fields.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_quotes_and_newlines() {
        assert_eq!(string("a\"b\nc"), r#""a\"b\nc""#);
    }

    #[test]
    fn escapes_control_chars() {
        assert_eq!(string("\u{1}"), r#""\u0001""#);
    }

    #[test]
    fn object_keeps_selected_columns() {
        let h = vec!["id".to_string(), "name".to_string()];
        let r = vec!["7".to_string(), "Ada".to_string()];
        assert_eq!(object(&[1], &h, &r, false), r#"  {"name": "Ada"}"#);
    }
}
"####;

const CLI_RS: &str = r####"//! Command-line flags.

#[derive(Debug, Default, PartialEq)]
pub struct Opts {
    /// Columns to keep, by name, in output order; empty keeps all of them.
    pub select: Vec<String>,
    pub pretty: bool,
}

pub fn parse(args: impl Iterator<Item = String>) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut args = args.peekable();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--pretty" | "-p" => o.pretty = true,
            "--select" | "-s" => {
                let v = args.next().ok_or("--select needs a comma-separated list of columns")?;
                o.select = v.split(',').map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect();
            }
            "--help" | "-h" => return Err("usage: csvjson [--select a,b] [--pretty] < input.csv".into()),
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> impl Iterator<Item = String> + '_ {
        s.split_whitespace().map(String::from)
    }

    #[test]
    fn select_and_pretty() {
        let o = parse(args("--select id,name -p")).unwrap();
        assert_eq!(o, Opts { select: vec!["id".into(), "name".into()], pretty: true });
    }

    #[test]
    fn rejects_unknown_flags() {
        assert!(parse(args("--nope")).is_err());
    }
}
"####;

const FILTER_RS: &str = r####"//! Which columns make it into the output.

/// Indexes of the columns to keep: all of them, or the selected names in the order given.
pub fn columns(header: &[String], select: &[String]) -> Vec<usize> {
    if select.is_empty() {
        return (0..header.len()).collect();
    }
    select.iter().filter_map(|name| header.iter().position(|h| h == name)).collect()
}

#[cfg(test)]
mod tests {
    use super::columns;

    fn header() -> Vec<String> {
        ["id", "name", "city"].map(String::from).to_vec()
    }

    #[test]
    fn keeps_everything_by_default() {
        assert_eq!(columns(&header(), &[]), [0, 1, 2]);
    }

    #[test]
    fn follows_the_selected_order() {
        assert_eq!(columns(&header(), &["city".into(), "id".into()]), [2, 0]);
    }
}
"####;

const TESTS_CLI: &str = r####"use std::io::Write;
use std::process::{Command, Stdio};

fn run(args: &[&str], input: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_csvjson"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("the binary runs");
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap()
}

#[test]
fn converts_rows_to_objects() {
    let out = run(&[], "id,name\n1,Ada\n2,Linus\n");
    assert!(out.contains(r#"{"id": "1", "name": "Ada"}"#), "{out}");
    assert!(out.trim_end().ends_with(']'), "{out}");
}

#[test]
fn select_keeps_only_named_columns() {
    let out = run(&["--select", "name"], "id,name\n1,Ada\n");
    assert!(out.contains(r#"{"name": "Ada"}"#), "{out}");
    assert!(!out.contains("\"id\""), "{out}");
}
"####;

const TESTS_ROUNDTRIP: &str = r####"use std::io::Write;
use std::process::{Command, Stdio};

/// Fields full of punctuation come out exactly as they went in.
#[test]
fn tricky_fields_survive() {
    let input = "k,v\n\"a,b\",\"say \"\"hi\"\"\"\n";
    let mut child = Command::new(env!("CARGO_BIN_EXE_csvjson")).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap();
    assert!(out.contains(r#""k": "a,b""#), "{out}");
    assert!(out.contains(r#""v": "say \"hi\"""#), "{out}");
}
"####;

const README_MD: &str = r####"# csvjson

Stream CSV in, get JSON out. No dependencies.

```sh
cargo run -q -- --select name,city --pretty < examples/people.csv
```

| flag | |
|---|---|
| `-s, --select a,b` | keep these columns, in this order |
| `-p, --pretty` | one field per line |

Quoted fields, commas inside quotes and doubled quotes (`""`) all work.
"####;

const PEOPLE_CSV: &str = r####"id,name,city
1,Ada,"London, UK"
2,Linus,Helsinki
3,"Grace ""Amazing"" Hopper",Arlington
"####;

/// Real `cargo test` output from this exact project, before the fix ({DIR} is the project folder).
const CHECK_FAIL: &str = r####"   Compiling csvjson v0.1.0 ({DIR})
    Finished `test` profile [unoptimized + debuginfo] target(s) in 2.84s
     Running unittests src/main.rs (target/debug/deps/csvjson-e7f33143014f0e98)

running 11 tests
test cli::tests::rejects_unknown_flags ... ok
test cli::tests::select_and_pretty ... ok
test csv::tests::empty_fields ... ok
test csv::tests::plain_row ... ok
test csv::tests::quoted_comma ... ok
test filter::tests::follows_the_selected_order ... ok
test filter::tests::keeps_everything_by_default ... ok
test csv::tests::escaped_quote ... FAILED
test json::tests::escapes_control_chars ... ok
test json::tests::escapes_quotes_and_newlines ... ok
test json::tests::object_keeps_selected_columns ... ok

failures:

---- csv::tests::escaped_quote stdout ----

thread 'csv::tests::escaped_quote' panicked at src/csv.rs:35:9:
assertion `left == right` failed
  left: ["say hi"]
 right: ["say \"hi\""]
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    csv::tests::escaped_quote

test result: FAILED. 10 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

error: test failed, to rerun pass `--bin csvjson`"####;

/// And after it.
const CHECK_PASS: &str = r####"   Compiling csvjson v0.1.0 ({DIR})
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.61s
     Running unittests src/main.rs (target/debug/deps/csvjson-e7f33143014f0e98)

running 11 tests
test cli::tests::select_and_pretty ... ok
test cli::tests::rejects_unknown_flags ... ok
test csv::tests::empty_fields ... ok
test csv::tests::escaped_quote ... ok
test csv::tests::plain_row ... ok
test csv::tests::quoted_comma ... ok
test filter::tests::follows_the_selected_order ... ok
test filter::tests::keeps_everything_by_default ... ok
test json::tests::escapes_control_chars ... ok
test json::tests::escapes_quotes_and_newlines ... ok
test json::tests::object_keeps_selected_columns ... ok

test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/cli.rs (target/debug/deps/cli-051536fc7552718f)

running 2 tests
test converts_rows_to_objects ... ok
test select_keeps_only_named_columns ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/roundtrip.rs (target/debug/deps/roundtrip-4dac20e48796a75c)

running 1 test
test tricky_fields_survive ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s"####;

#[allow(clippy::too_many_arguments)]
async fn worker(
    idx: usize,
    name: String,
    lead: String,
    mine: Vec<&'static Job>,
    owners: HashMap<&'static str, String>,
    dir: PathBuf,
    tx: Tx,
    seed: u64,
) {
    let mut rng = Rng::new(seed);
    let mut tokens = 0u64;
    let cps = rng.range(110.0, 170.0);
    sleep(ms(rng.range(300.0, 2500.0))).await;
    for (k, j) in mine.iter().enumerate() {
        send(&tx, idx, AgentEv::Turn(format!("build #{}", j.id)));
        if k == 0 {
            send(&tx, idx, AgentEv::Start);
            sleep(ms(rng.range(900.0, 1800.0))).await;
            quick_tool(&tx, idx, "Read", "Cargo.toml", rng.range(400.0, 900.0)).await;
        }
        stream(&tx, idx, Out::Thinking, j.think, 60.0, &mut rng, &mut tokens).await;
        quick_tool(&tx, idx, "Read", "src/main.rs", rng.range(500.0, 1100.0)).await;
        for (path, body) in j.files {
            if let Some(parent) = Path::new(path).parent() {
                let _ = std::fs::create_dir_all(dir.join(parent));
            }
            write_file(&tx, idx, &dir, path, body, cps, &mut rng, &mut tokens).await;
        }
        if let Some((peer_job, text)) = j.peer {
            if let Some(peer) = owners.get(peer_job).filter(|p| **p != name) {
                msg(&tx, &name, peer, text);
                let (tx2, me, peer2, reply) = (tx.clone(), name.clone(), peer.clone(), rng.pick(REPLIES).to_string());
                let wait = rng.range(1200.0, 2800.0);
                tokio::spawn(async move {
                    sleep(ms(wait)).await;
                    msg(&tx2, &peer2, &me, reply);
                });
            }
        }
    }
    quick_tool(&tx, idx, "Read", &format!(".swarm/inbox/{name}.md"), rng.range(500.0, 1000.0)).await;
    let summary = mine.iter().map(|j| j.summary).collect::<Vec<_>>().join("; ");
    stream(&tx, idx, Out::Text, &format!("{summary}\n"), 120.0, &mut rng, &mut tokens).await;
    msg(&tx, &name, &lead, summary.clone());
    send(&tx, idx, AgentEv::Cost(tokens as f64 * 15e-6));
    sleep(ms(250.0)).await;
    send(&tx, idx, AgentEv::Done(summary));
}

/// Play a `cargo test` run on the dashboard: the real output of this project, failing or passing.
pub(crate) async fn check(tx: &Tx, dir: &Path, pass: bool, label: &str) {
    let started = Instant::now();
    let _ = tx.send(Ev::CheckStart { cmd: CHECK.into(), label: label.into() });
    let _ = tx.send(Ev::Log(Level::Info, format!("$ {CHECK}")));
    let out = if pass { CHECK_PASS } else { CHECK_FAIL }.replace("{DIR}", &orchestrator::tilde(dir));
    for (i, line) in out.lines().enumerate() {
        let _ = tx.send(Ev::CheckLine(line.to_string()));
        let pause = if i == 0 {
            if pass {
                700.0
            } else {
                1600.0
            }
        } else if line.is_empty() {
            20.0
        } else {
            55.0
        };
        sleep(ms(pause)).await;
    }
    let _ = tx.send(Ev::CheckDone { ok: pass, secs: started.elapsed().as_secs() });
    let _ = tx
        .send(Ev::Log(if pass { Level::Good } else { Level::Bad }, if pass { "check passed".into() } else { "check failed".to_string() }));
}

pub(crate) async fn review(d: &mut Demo) {
    let mut tokens = 0;
    let _ = d.tx.send(Ev::Phase(Phase::Review));
    send(&d.tx, 0, AgentEv::Turn("review the work".into()));
    send(&d.tx, 0, AgentEv::Start);
    sleep(ms(600.0)).await;
    stream(
        &d.tx,
        0,
        Out::Thinking,
        "Checking the seams: parse_row → columns → object match the calls in main.rs,\nand the README's flags match cli.rs.\n",
        90.0,
        &mut d.rng,
        &mut tokens,
    )
    .await;
    quick_tool(&d.tx, 0, "Grep", "pub fn in src/", 800.0).await;
    quick_tool(&d.tx, 0, "Read", "src/main.rs", 700.0).await;
    stream(&d.tx, 0, Out::Text, "Interfaces line up, README matches the flags. Over to the check.\n", 110.0, &mut d.rng, &mut tokens).await;
    send(&d.tx, 0, AgentEv::Done("review clean".into()));
}

/// End of the first round: review, a failing check, the fix loop, a passing check, ship.
pub(crate) async fn finish_first(d: &mut Demo) {
    if d.cfg.review {
        d.review().await;
    }
    let (tx, dir, lead) = (d.tx.clone(), d.cfg.dir.clone(), d.lead());
    let _ = tx.send(Ev::Phase(Phase::Check));
    check(&tx, &dir, false, "check").await;

    let owner = d.items.iter().find(|i| i.id == "parser").map(|i| i.agent.clone()).unwrap_or_else(|| lead.clone());
    let oi = d.names.iter().position(|n| *n == owner).unwrap_or(0);
    let mut tokens = 0;
    let _ = tx.send(Ev::Log(Level::Warn, "check failed · fix round 1/3".into()));
    send(&tx, 0, AgentEv::Turn("fix the check · 1/3".into()));
    send(&tx, 0, AgentEv::Start);
    sleep(ms(700.0)).await;
    stream(
        &tx,
        0,
        Out::Thinking,
        "1 of 14 failed: csv::tests::escaped_quote.\n\"say \"\"hi\"\"\" parses as [\"say hi\"]: doubled quotes inside a quoted field\nshould collapse to one quote, not toggle quoting twice. That's src/csv.rs.\n",
        95.0,
        &mut d.rng,
        &mut tokens,
    )
    .await;
    stream(&tx, 0, Out::Text, &format!("Routing the fix to {owner}.\n"), 110.0, &mut d.rng, &mut tokens).await;
    send(&tx, 0, AgentEv::Done(format!("fix routed to {owner}")));
    msg(&tx, &lead, "you", format!("1 test failed: doubled quotes inside quoted fields. {owner} is on it."));
    msg(&tx, &lead, &owner, "escaped_quote fails (src/csv.rs:35): a doubled quote inside quotes must become one literal quote");
    let _ = tx.send(Ev::Phase(Phase::Build));

    let mut rng = Rng::seeded();
    send(&tx, oi, AgentEv::Turn(format!("task from {lead}")));
    send(&tx, oi, AgentEv::Start);
    sleep(ms(900.0)).await;
    stream(
        &tx,
        oi,
        Out::Thinking,
        "Right: the toggle eats both quotes of a \"\" pair.\nPeek at the next char; if it's another quote inside a quoted field, push one and skip it.\n",
        70.0,
        &mut rng,
        &mut tokens,
    )
    .await;
    quick_tool(&tx, oi, "Read", "src/csv.rs", 700.0).await;
    send(&tx, oi, AgentEv::ToolStart("Edit".into()));
    send(&tx, oi, AgentEv::ToolDetail("src/csv.rs".into()));
    sleep(ms(400.0)).await;
    stream(&tx, oi, Out::Code, FIX_SNIPPET, 120.0, &mut rng, &mut tokens).await;
    let _ = std::fs::write(dir.join("src/csv.rs"), CSV_FIXED);
    send(&tx, oi, AgentEv::ToolDone(false));
    let done = "Fixed: \"\" inside quotes is one literal quote now. escaped_quote and roundtrip both cover it.";
    stream(&tx, oi, Out::Text, &format!("{done}\n"), 120.0, &mut rng, &mut tokens).await;
    msg(&tx, &owner, &lead, "fixed the doubled-quote case in parse_row");
    send(&tx, oi, AgentEv::Cost(tokens as f64 * 15e-6));
    send(&tx, oi, AgentEv::Done(done.into()));
    sleep(ms(500.0)).await;

    let _ = tx.send(Ev::Phase(Phase::Check));
    check(&tx, &dir, true, "re-check · after fix 1/3").await;
    sleep(ms(300.0)).await;
    d.ship();
}

pub async fn run(cfg: Config, tx: Tx, cmds: UnboundedReceiver<Cmd>) {
    let shared = Shared::default();
    *shared.names.lock().unwrap() = cfg.roster.iter().map(|s| s.name.clone()).collect();
    tokio::spawn(orchestrator::watch_files(cfg.dir.clone(), tx.clone(), shared.clone(), Kind::Code));
    let _ = tx.send(Ev::CheckSet(Some(CHECK.into())));
    sleep(ms(2200.0)).await; // let the boot sequence breathe

    let mut rng = Rng::seeded();
    let mut tokens = 0u64;
    let names: Vec<String> = cfg.roster.iter().map(|s| s.name.clone()).collect();
    let lead = names[0].clone();
    let workers: Vec<(usize, String)> = names.iter().cloned().enumerate().skip(1).collect();
    if workers.is_empty() {
        let _ = tx.send(Ev::Log(Level::Bad, "demo needs at least one worker".into()));
        return;
    }

    let _ = tx.send(Ev::Phase(Phase::Plan));
    send(&tx, 0, AgentEv::Turn("plan the work".into()));
    send(&tx, 0, AgentEv::Start);
    sleep(ms(900.0)).await;
    let think = "Request: csvjson, a Rust CLI that streams CSV to JSON.\nstd only, no crates: it builds offline and `cargo test` is the whole check.\nPipeline: csv::parse_row → filter::columns → json::object; cli::parse reads the flags.\nI write Cargo.toml and main.rs with those signatures, then one owner per module.\nTests live next to the code they test.\n";
    stream(&tx, 0, Out::Thinking, think, 90.0, &mut rng, &mut tokens).await;
    let _ = std::fs::create_dir_all(cfg.dir.join("src"));
    write_file(&tx, 0, &cfg.dir, "Cargo.toml", CARGO_TOML, 260.0, &mut rng, &mut tokens).await;
    write_file(&tx, 0, &cfg.dir, "src/main.rs", MAIN_RS, 260.0, &mut rng, &mut tokens).await;
    stream(
        &tx,
        0,
        Out::Text,
        &format!("Plan locked: {} tasks, split by file. Check: {CHECK}.\n", JOBS.len()),
        110.0,
        &mut rng,
        &mut tokens,
    )
    .await;
    send(&tx, 0, AgentEv::Cost(tokens as f64 * 75e-6));
    send(&tx, 0, AgentEv::Done("plan locked".into()));

    // every job gets an owner; with fewer than seven workers some own more than one
    let mut owners: HashMap<&'static str, String> = HashMap::new();
    let mut per_worker: Vec<Vec<&'static Job>> = vec![Vec::new(); workers.len()];
    for (k, j) in JOBS.iter().enumerate() {
        let w = k % workers.len();
        owners.insert(j.id, workers[w].1.clone());
        per_worker[w].push(j);
    }
    let items: Vec<PlanItem> = JOBS
        .iter()
        .map(|j| PlanItem {
            id: j.id.into(),
            agent: owners[j.id].clone(),
            brief: j.brief.into(),
            owns: j.owns.iter().map(|o| o.to_string()).collect(),
        })
        .collect();
    let _ = tx.send(Ev::Plan(TITLE.into(), items.clone()));
    for it in &items {
        let brief: String = it.brief.chars().take(80).collect();
        msg(&tx, &lead, &it.agent, format!("you own #{} [{}]: {brief}", it.id, it.owns.join(", ")));
        sleep(ms(150.0)).await;
    }

    let mut d = Demo::new(cfg.clone(), tx.clone(), names.clone(), items);
    d.start_round();
    for ((idx, name), mine) in workers.into_iter().zip(per_worker) {
        let seed = rng.next();
        let (lead, owners, dir, tx) = (lead.clone(), owners.clone(), cfg.dir.clone(), tx.clone());
        let h = d.work.spawn(async move {
            worker(idx, name, lead, mine, owners, dir, tx, seed).await;
            idx
        });
        d.running.insert(idx, h);
    }
    d.serve(cmds, shared).await;
}
