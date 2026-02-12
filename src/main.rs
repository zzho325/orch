use clap::{Parser, Subcommand};
use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

fn tasks_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join("tasks")
}

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
    /// Show tasks that need your input
    Inbox,
    /// Attach to a task's tmux session
    Jump {
        /// Task name (matches task-<name> tmux session)
        name: String,
    },
    /// Trigger a one-shot orchestrator scan
    Scan,
    /// Send a message to the orchestrator: orch - close the recon task
    #[command(name = "-")]
    Msg {
        /// Your message
        message: Vec<String>,
    },
}

fn prompt_file() -> PathBuf {
    // Look next to the binary first, then fall back to compile-time path
    let exe = std::env::current_exe().unwrap_or_default();
    let beside_exe = exe.parent().unwrap_or(std::path::Path::new(".")).join("orchestrator.md");
    if beside_exe.exists() {
        return beside_exe;
    }
    // Fallback: ~/orchestrator/orchestrator.md
    dirs::home_dir().unwrap_or_default().join("orchestrator/orchestrator.md")
}

fn run_orchestrator_with_message(message: &str) {
    eprintln!("[orch] {message}");

    let prompt_path = prompt_file();
    let system_prompt = match fs::read_to_string(&prompt_path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[orch] failed to read {}: {e}", prompt_path.display());
            return;
        }
    };

    let prompt = format!("{system_prompt}\n\n---\n\n{message}");

    let status = Command::new("claude")
        .args(["-p", "--dangerously-skip-permissions", &prompt])
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status();

    match status {
        Ok(s) => {
            if !s.success() {
                eprintln!("[orch] claude exited with {s}");
            }
        }
        Err(e) => eprintln!("[orch] failed to run claude: {e}"),
    }
}

fn run_orchestrator() {
    run_orchestrator_with_message("Scan ~/tasks/ and tmux sessions. For any unstarted task without a worker, spin up an interactive tmux worker session. Update task files with status. Report what you did.");
}

fn cmd_daemon() {
    let dir = tasks_dir();
    if !dir.exists() {
        fs::create_dir_all(&dir).expect("failed to create ~/tasks");
    }

    eprintln!("[orch] daemon started, watching {}", dir.display());
    eprintln!("[orch] running initial scan...");
    run_orchestrator();

    let (tx, rx) = mpsc::channel();
    let mut debouncer =
        new_debouncer(Duration::from_secs(3), tx).expect("failed to create watcher");

    debouncer
        .watcher()
        .watch(&dir, RecursiveMode::NonRecursive)
        .expect("failed to watch ~/tasks");

    let poll_interval = Duration::from_secs(5 * 60); // check workers every 5 min
    eprintln!("[orch] watching for changes (polling every 5m)...");

    loop {
        match rx.recv_timeout(poll_interval) {
            Ok(Ok(events)) => {
                let has_md = events
                    .iter()
                    .any(|e| e.path.extension().is_some_and(|ext| ext == "md"));
                if has_md {
                    run_orchestrator();
                }
            }
            Ok(Err(e)) => eprintln!("[orch] watch error: {e:?}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Periodic check on workers
                eprintln!("[orch] periodic check...");
                run_orchestrator();
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                eprintln!("[orch] channel closed");
                break;
            }
        }
    }
}

fn cmd_status() {
    let dir = tasks_dir();

    // Read task files
    println!("## Tasks\n");
    match fs::read_dir(&dir) {
        Ok(entries) => {
            let mut found = false;
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "md") {
                    found = true;
                    let name = path.file_stem().unwrap_or_default().to_string_lossy();
                    let content = fs::read_to_string(&path).unwrap_or_default();

                    // Check for status section
                    let status = if content.contains("## Status") {
                        // Extract last status line
                        content
                            .lines()
                            .rev()
                            .find(|l| !l.trim().is_empty())
                            .unwrap_or("unknown")
                            .trim()
                    } else {
                        "new"
                    };

                    // Extract session name from status section if present
                    let session_name = content
                        .lines()
                        .find_map(|l| {
                            // Look for backtick-quoted session names like `task-foo`
                            let rest = l.find("`task-")?;
                            let start = rest + 1;
                            let end = l[start..].find('`')? + start;
                            Some(l[start..end].to_string())
                        })
                        .unwrap_or_else(|| format!("task-{name}"));

                    let has_session = Command::new("tmux")
                        .args(["has-session", "-t", &session_name])
                        .status()
                        .is_ok_and(|s| s.success());

                    let worker = if has_session {
                        format!("running ({session_name})")
                    } else {
                        "none".to_string()
                    };

                    println!("  {name}");
                    println!("    status: {status}");
                    println!("    worker: {worker}");
                    println!();
                }
            }
            if !found {
                println!("  (no tasks)");
            }
        }
        Err(_) => println!("  ~/tasks/ not found"),
    }

    // Show tmux sessions
    println!("## Workers\n");
    let output = Command::new("tmux").arg("ls").output();
    match output {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let mut found = false;
            for line in stdout.lines() {
                if line.starts_with("task-") {
                    found = true;
                    println!("  {line}");
                }
            }
            if !found {
                println!("  (no active workers)");
            }
        }
        Err(_) => println!("  tmux not running"),
    }
}

fn cmd_inbox() {
    let dir = tasks_dir();

    println!("## Needs Your Attention\n");
    let mut found = false;

    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "md") {
                let content = fs::read_to_string(&path).unwrap_or_default();
                let lower = content.to_lowercase();

                if lower.contains("waiting for input")
                    || lower.contains("needs input")
                    || lower.contains("needs decision")
                    || lower.contains("blocked")
                    || lower.contains("question")
                {
                    found = true;
                    let name = path.file_stem().unwrap_or_default().to_string_lossy();

                    // Find the relevant line
                    let context = content
                        .lines()
                        .find(|l| {
                            let ll = l.to_lowercase();
                            ll.contains("waiting")
                                || ll.contains("needs input")
                                || ll.contains("blocked")
                                || ll.contains("question")
                        })
                        .unwrap_or("");

                    println!("  {name}");
                    println!("    {context}");
                    println!("    -> orch jump {name}");
                    println!();
                }
            }
        }
    }

    if !found {
        println!("  Nothing needs your attention right now.");
    }
}

fn cmd_jump(name: &str) {
    let session = if name.starts_with("task-") {
        name.to_string()
    } else {
        format!("task-{name}")
    };

    // Check if session exists
    let exists = Command::new("tmux")
        .args(["has-session", "-t", &session])
        .status()
        .is_ok_and(|s| s.success());

    if !exists {
        eprintln!("No tmux session '{session}' found.");
        eprintln!("Active task sessions:");
        let _ = Command::new("tmux")
            .arg("ls")
            .status();
        return;
    }

    // If already inside tmux, switch client instead of nesting
    if std::env::var("TMUX").is_ok() {
        let _ = Command::new("tmux")
            .args(["switch-client", "-t", &session])
            .status();
    } else {
        let _ = Command::new("tmux")
            .args(["attach-session", "-t", &session])
            .status();
    }
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Some(Cmd::Daemon) => cmd_daemon(),
        Some(Cmd::Status) => cmd_status(),
        Some(Cmd::Inbox) => cmd_inbox(),
        Some(Cmd::Jump { name }) => cmd_jump(&name),
        Some(Cmd::Scan) => run_orchestrator(),
        Some(Cmd::Msg { message }) => run_orchestrator_with_message(&message.join(" ")),
        None => cmd_status(), // default: show status
    }
}
