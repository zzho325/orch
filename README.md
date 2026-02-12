# orch

Drop markdown files in `~/tasks/`. AI workers pick them up.

A Rust CLI that watches `~/tasks/` for markdown files and uses Claude to manage AI workers in tmux sessions. The Rust binary is the heartbeat. The AI is the brain. The filesystem is the database.

```bash
cargo install --path .
```

## Usage

```
orch daemon &                          # start watching ~/tasks/
echo "fix the auth bug" > ~/tasks/auth.md  # create a task
orch                                   # check status
orch jump auth                         # hop into the worker session
orch - close the auth task             # talk to the orchestrator
```

## Status example

```
$ orch
## Tasks

  check-recon-timing
    status: Pushed fix to ashley/ENG-23525. Extended recon time window
            so all daily batches fall within the correct day. Ready for PR.
    worker: running (task-check-recon)

  check-adjustments-review
    status: Produced detailed guide covering check adjustment cases.
            Still in progress.
    worker: running (task-check-adj)

## Workers

  task-check-adj: 2 windows (created Thu Feb 12 12:30:32 2026)
  task-check-recon: 1 windows (created Thu Feb 12 12:09:35 2026)
```

## How it works

Rust binary is the heartbeat. AI is the brain. Filesystem is the database.

1. You drop a `.md` file in `~/tasks/` — can be vague, a Linear link, whatever
2. The daemon detects the change — file watcher triggers
3. It runs `claude -p` with the orchestrator prompt — Claude reads all tasks, checks tmux sessions, decides what to do
4. Claude spins up a worker — a new tmux session running an interactive Claude Code instance, seeded with the task content
5. You check in when you want — `orch status`, `orch inbox`, `orch jump <name>`
6. Every 5 minutes — the daemon re-scans, peeks at worker panes, updates task files with progress

Edit `orchestrator.md` to change behavior — no rebuild needed.

Requires [Claude Code](https://docs.anthropic.com/en/docs/claude-code) and tmux.
