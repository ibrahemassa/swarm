# Changelog

## 0.2.0

- **Launcher**: `swarm` with no arguments opens a setup screen: what to build, project folder, lead and squad
  (agents per CLI and models), review pass, when to open the browser, demo or real, and a budget. It remembers your
  last squad, checks which CLIs are installed, and lists recent projects to resume.
- **The swarm stays up after it ships.** A command bar talks to the lead (it replies, re-plans and hands out tasks or
  new sections), DMs one agent with `@name`, or everyone with `@all`.
- **Persistent agent sessions**: Claude Code and Antigravity keep one live conversation per agent over
  stream-json stdin; Codex resumes its thread for every turn.
- Commands: `/add`, `/stop`, `/review`, `/budget`, `/open`, `/fx`, `/help`, `/quit`, with tab completion.
- Agent selection, a zoom view (full scrollable log, stats, owned sections, comms), help overlay, toasts, reply bubbles.
- Live preview server on `127.0.0.1:7700` that reloads the page on every change and keeps the scroll position.
- Budget cap that stops all agents once the Claude-reported spend crosses it.
- Projects are saved to `.swarm/state.json` and resumable with `swarm -c`, `swarm -o DIR`, or from the launcher.
- Agents can message the human (`to: you`).
- Codex support updated to the `thread.*` / `item.*` / `turn.*` JSON event stream.
- Demo mode plays along when you talk to it.

## 0.1.0

- One-shot build: a lead plans, Claude Code / Codex / Antigravity workers build sections in parallel, message each
  other through `.swarm/outbox`, failed workers get a Claude backup, the lead reviews, the page ships.
- Animated dashboard: boot sequence, live code streaming with syntax highlighting, matrix rain, message packets,
  comms mesh, sparklines, fireworks.
- Demo mode with scripted agents and zero tokens.
