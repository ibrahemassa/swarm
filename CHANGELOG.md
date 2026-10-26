# Changelog

## 0.3.0

- **Code mode** (`--code`, or CODE PROJECT in the launcher): the swarm works on any project, new or existing,
  not just one web page. The lead writes the skeleton (or reads the existing code), then splits the work into tasks
  that each own a set of files or globs.
- **Check command** (`--check`, the launcher, or `/check`): runs after every round and gates the ship. Failures go
  back to the lead, who sends fixes to the owners of the broken code; up to three fix rounds. The lead can suggest a
  check, but nothing runs until you accept it.
- The launcher detects the project's usual test command (cargo, npm/pnpm/yarn/bun, go, pytest or unittest, make) and
  describes an existing folder before you launch.
- `/run <cmd>` runs any command in the project and streams it to the new CHECK panel.
- CHECK step in the phase pipeline, a check chip in the header, live colored test output.
- Warnings when an agent edits a file another agent owns.
- Existing repos: `.swarm/` goes into `.git/info/exclude`, and a `git diff --shortstat` is logged when a round ships.
- The file watcher skips dependency and build folders (`node_modules`, `target`, `dist`, …).

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
