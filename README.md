# orch

A Rust CLI that watches `~/tasks/` for markdown files and uses Claude to manage AI workers in tmux sessions.

## How it works

1. Drop a `.md` file in `~/tasks/` — can be vague, a Linear link, whatever
2. The daemon detects the change and runs Claude with the orchestrator prompt
3. Claude reads all tasks, checks tmux sessions, and spins up interactive Claude Code workers
4. You check in when you want — or the daemon re-scans every 5 minutes

All state lives in the task files and tmux sessions. The orchestrator is stateless — every scan reconstructs the world from scratch.

## Install

```bash
cargo install --path .
cp orchestrator.md ~/bin/orchestrator.md  # prompt file goes next to binary
```

## Commands

```
orch                    # show task status
orch status             # same
orch inbox              # tasks needing your attention
orch jump <name>        # switch to a worker's tmux session
orch scan               # trigger a one-shot scan
orch daemon             # run the background watcher
orch - <message>        # talk to the orchestrator in natural language
```

## Example

```bash
# Start the daemon
orch daemon &

# Create a task
echo "# Fix auth race condition\nCheck session.ts for timing issues" > ~/tasks/fix-auth.md

# The daemon picks it up, spins up a Claude worker in tmux

# Check status
orch status

# Jump into the worker session
orch jump fix-auth

# Tell the orchestrator to close it
orch - close the fix-auth task
```

## Task files

Tasks are freeform markdown in `~/tasks/`. No schema required. The orchestrator appends a `## Status` section as work progresses.

```markdown
# Fix Auth Race Condition

Check the logic in session.ts. We're seeing a race condition
where the token isn't set before the dashboard fetches data.
```

## Architecture

```
~/tasks/*.md              task queue (source of truth)
~/bin/orch                rust binary (watcher + CLI)
~/bin/orchestrator.md     prompt file (the orchestrator's brain)
tmux task-* sessions      workers (interactive Claude Code sessions)
```

The Rust binary is the heartbeat. The AI is the brain. The filesystem is the database.

## Configuration

Edit `orchestrator.md` to customize the orchestrator's behavior — it's just a prompt. The binary reads it at runtime, no rebuild needed.

## Requirements

- [Claude Code](https://docs.anthropic.com/en/docs/claude-code)
- tmux
- Rust (to build)
