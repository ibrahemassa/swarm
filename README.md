# swarm

A lead agent plans a web page, a team of Claude Code / Antigravity / Codex agents build it in parallel
and message each other, and a terminal dashboard makes the whole thing look unreasonably intense.

```
swarm --demo                                   # scripted agents, real animations, zero tokens
swarm "landing page for a neon ramen bar"      # the real thing: opus lead + 4 sonnet + 2 gemini
swarm "portfolio for a tattoo artist" --claude 5 --agy 2 --codex 3 --live
```

Keys: `q` quit (kills the agents), `o` open the page, `f` toggle heavy effects, any key skips the boot screen.

## How it works

1. **Plan** — the lead writes `styles/base.css` (the shared design system) and answers with a JSON plan:
   one section per worker, in page order.
2. **Build** — every worker runs headless (`claude -p --output-format stream-json`, `agy -p`, `codex exec --json`)
   and owns `sections/<id>.html`, `styles/<id>.css` and optionally `scripts/<id>.js`.
   `index.html` is re-assembled from those files whenever one changes (`--live` opens it with auto-refresh).
3. **Team chat** — an agent messages a teammate by writing `.swarm/outbox/<name>-<n>.msg` (`to: <name|all>` on line 1).
   swarm animates the message across the screen and appends it to `.swarm/inbox/<name>.md`.
4. **Backup** — a worker that fails, or finishes without writing its section, is replaced once by a fresh Claude worker.
5. **Review** — the lead reads the team's messages and fixes cross-section problems (skip with `--no-review`).

## Options

| flag | default | |
|---|---|---|
| `--claude N` | 4 | Claude Code workers (`--model`, default `sonnet`) |
| `--agy N` | 2 | Antigravity workers (`--agy-model`, default `gemini-3.8-flash-high`) |
| `--codex N` | 1 if logged in | Codex workers (`--codex-model`); needs `codex login` |
| `--lead` | `claude` | `claude`, `agy` or `codex` (`--lead-model`, default `opus`) |
| `-o DIR` | `./swarm-<task>` | output folder |
| `--live` | off | open the page at build start and auto-refresh it |
| `--no-review` / `--no-open` | | skip the review pass / don't open the browser at the end |

Headless agents can't answer permission prompts: Claude workers only get file tools
(`Read,Write,Edit,Glob,Grep`), Antigravity runs in `accept-edits` mode and is told shell commands are blocked,
Codex runs `--full-auto` (sandboxed to the project folder).
