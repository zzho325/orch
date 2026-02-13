use clap::{Parser, Subcommand};
use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const SCAN_MSG: &str = "\
    Scan ~/tasks/ and tmux sessions. For any unstarted task without a worker, \
    spin up an interactive tmux worker session. Update task files with status. \
    Report what you did.";

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "orch", about = "Task orchestrator for Claude Code workers")]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the background watcher daemon
    Daemon,
    /// Show status of all tasks and workers
    Status,
    /// Attach to a task's tmux session
    Jump { name: String },
    /// Trigger a one-shot orchestrator scan
    Scan,
    /// Send a message to the orchestrator
    #[command(name = "-")]
    Msg { message: Vec<String> },
}

// ---------------------------------------------------------------------------
// Paths & helpers
// ---------------------------------------------------------------------------

fn tasks_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join("tasks")
}

fn prompt_file() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        let beside = exe.with_file_name("orchestrator.md");
        if beside.exists() {
            return beside;
        }
    }
    dirs::home_dir()
        .unwrap_or_default()
        .join("orchestrator/orchestrator.md")
}

fn log_file() -> fs::File {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(tasks_dir().join(".orch.log"))
        .expect("failed to open log file")
}

fn write_inbox(msg: &str) {
    fs::write(tasks_dir().join(".inbox"), msg).expect("failed to write to inbox");
}

fn has_tmux_session(name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", name])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

// ---------------------------------------------------------------------------
// Claude invocation
// ---------------------------------------------------------------------------

fn run_orchestrator(message: &str) {
    eprintln!("[orch] {message}");

    let system_prompt = match fs::read_to_string(prompt_file()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[orch] failed to read prompt: {e}");
            return;
        }
    };

    let prompt = format!("{system_prompt}\n\n---\n\n{message}");
    let log = log_file();

    let mut child = match Command::new("claude")
        .args(["-p", "--dangerously-skip-permissions"])
        .stdin(Stdio::piped())
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[orch] failed to run claude: {e}");
            return;
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes());
    }

    match child.wait() {
        Ok(s) if !s.success() => eprintln!("[orch] claude exited with {s}"),
        Err(e) => eprintln!("[orch] claude wait failed: {e}"),
        _ => {}
    }
}

fn drain_inbox() {
    let inbox = tasks_dir().join(".inbox");
    if let Ok(msg) = fs::read_to_string(&inbox) {
        if !msg.trim().is_empty() {
            let _ = fs::remove_file(&inbox);
            run_orchestrator(&msg);
        }
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn cmd_status() {
    let dir = tasks_dir();

    println!("## Tasks\n");
    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => {
            println!("  ~/tasks/ not found");
            return;
        }
    };

    let mut found = false;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.extension().is_some_and(|e| e == "md") {
            continue;
        }
        found = true;

        let name = path.file_stem().unwrap_or_default().to_string_lossy();
        let content = fs::read_to_string(&path).unwrap_or_default();
        let summary = extract_section(&content, "## Summary");

        let session = format!("task-{name}");
        let worker = if has_tmux_session(&session) {
            format!("running ({session})")
        } else {
            "none".into()
        };

        println!("  {name}  [worker: {worker}]");
        if summary.is_empty() {
            let desc = content
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim()
                .trim_start_matches('#')
                .trim();
            println!("    {desc}");
        } else {
            for line in &summary {
                println!("    {line}");
            }
        }
        println!();
    }

    if !found {
        println!("  (no tasks)");
    }
}

fn cmd_jump(name: &str) {
    let session = if name.starts_with("task-") {
        name.to_string()
    } else {
        format!("task-{name}")
    };

    if !has_tmux_session(&session) {
        eprintln!("No tmux session '{session}' found.");
        let _ = Command::new("tmux").arg("ls").status();
        return;
    }

    let action = if std::env::var("TMUX").is_ok() {
        "switch-client"
    } else {
        "attach-session"
    };
    let _ = Command::new("tmux").args([action, "-t", &session]).status();
}

fn cmd_daemon() {
    let dir = tasks_dir();
    fs::create_dir_all(&dir).ok();

    eprintln!("[orch] daemon started, watching {}", dir.display());
    drain_inbox();
    eprintln!("[orch] running initial scan...");
    run_orchestrator(SCAN_MSG);

    let (tx, rx) = mpsc::channel();
    let mut debouncer =
        new_debouncer(Duration::from_secs(3), tx).expect("failed to create watcher");
    debouncer
        .watcher()
        .watch(&dir, RecursiveMode::NonRecursive)
        .expect("failed to watch ~/tasks");

    eprintln!("[orch] watching for changes (polling every 5m)...");

    loop {
        match rx.recv_timeout(Duration::from_secs(5 * 60)) {
            Ok(Ok(events)) => {
                if events.iter().any(|e| e.path.file_name().is_some_and(|f| f == ".inbox")) {
                    drain_inbox();
                }

                let changed: Vec<_> = events
                    .iter()
                    .filter(|e| e.path.extension().is_some_and(|ext| ext == "md"))
                    .filter_map(|e| e.path.file_name().map(|f| f.to_string_lossy().into_owned()))
                    .collect();

                if !changed.is_empty() {
                    run_orchestrator(&format!(
                        "Task files changed: {}. Check these tasks and handle them — \
                         spin up workers for new tasks, update status for existing ones.",
                        changed.join(", ")
                    ));
                }
            }
            Ok(Err(e)) => eprintln!("[orch] watch error: {e:?}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                eprintln!("[orch] periodic check...");
                run_orchestrator(SCAN_MSG);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// Extract lines between a `## Heading` and the next `## ` (or EOF).
fn extract_section<'a>(content: &'a str, heading: &str) -> Vec<&'a str> {
    let mut inside = false;
    content
        .lines()
        .filter(move |line| {
            if line.trim().starts_with(heading) {
                inside = true;
                return false;
            }
            if inside && line.trim().starts_with("## ") {
                inside = false;
            }
            inside && !line.trim().is_empty()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Some(Cmd::Status) | None => cmd_status(),
        Some(Cmd::Jump { name }) => cmd_jump(&name),
        Some(Cmd::Daemon) => cmd_daemon(),
        Some(Cmd::Scan) => {
            write_inbox(SCAN_MSG);
            eprintln!("[orch] scan triggered");
        }
        Some(Cmd::Msg { message }) => {
            write_inbox(&message.join(" "));
            eprintln!("[orch] message sent");
        }
    }
}
