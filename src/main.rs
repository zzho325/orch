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
    let beside_exe = exe
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("orchestrator.md");
    if beside_exe.exists() {
        return beside_exe;
    }
    // Fallback: ~/orchestrator/orchestrator.md
    dirs::home_dir()
        .unwrap_or_default()
        .join("orchestrator/orchestrator.md")
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

    use std::io::Write;

    let mut child = match Command::new("claude")
        .args(["-p", "--dangerously-skip-permissions"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
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
        Ok(s) => {
            if !s.success() {
                eprintln!("[orch] claude exited with {s}");
            }
        }
        Err(e) => eprintln!("[orch] claude wait failed: {e}"),
    }
}

fn run_inbox() {
    let inbox = tasks_dir().join(".inbox");
    if let Ok(msg) = fs::read_to_string(&inbox) {
        if !msg.trim().is_empty() {
            let _ = fs::remove_file(&inbox);
            run_orchestrator_with_message(&msg);
        }
    }
}

const SCAN_MSG: &str = "Scan ~/tasks/ and tmux sessions. For any unstarted task without a worker, \
    spin up an interactive tmux worker session. Update task files with status. Report what you did.";

fn run_new_task(files: &[String]) {
    run_orchestrator_with_message(&format!(
        "Task files changed: {}. Check these tasks and handle them — spin up workers for new tasks, update status for existing ones.",
        files.join(", ")
    ));
}

fn cmd_daemon() {
    let dir = tasks_dir();
    if !dir.exists() {
        fs::create_dir_all(&dir).expect("failed to create ~/tasks");
    }

    eprintln!("[orch] daemon started, watching {}", dir.display());
    // Process any pending inbox before scanning
    run_inbox();
    eprintln!("[orch] running initial scan...");
    run_orchestrator_with_message(SCAN_MSG);

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
                let has_inbox = events
                    .iter()
                    .any(|e| e.path.file_name().is_some_and(|f| f == ".inbox"));
                let has_md = events
                    .iter()
                    .any(|e| e.path.extension().is_some_and(|ext| ext == "md"));
                if has_inbox {
                    run_inbox();
                }
                if has_md {
                    let changed: Vec<String> = events
                        .iter()
                        .filter(|e| e.path.extension().is_some_and(|ext| ext == "md"))
                        .filter_map(|e| e.path.file_name().map(|f| f.to_string_lossy().to_string()))
                        .collect();
                    run_new_task(&changed);
                }
            }
            Ok(Err(e)) => eprintln!("[orch] watch error: {e:?}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Periodic check on workers
                eprintln!("[orch] periodic check...");
                run_orchestrator_with_message(SCAN_MSG);
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

    // Get all task-* tmux sessions
    let sessions: Vec<String> = Command::new("tmux")
        .arg("ls")
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| l.starts_with("task-"))
                .map(|l| l.to_string())
                .collect()
        })
        .unwrap_or_default();

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

                    // First non-empty line as description
                    let desc = content
                        .lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("")
                        .trim()
                        .trim_start_matches('#')
                        .trim();

                    // Find matching tmux session
                    let worker = sessions
                        .iter()
                        .find(|s| s.starts_with(&format!("task-{name}")))
                        .map(|s| s.as_str())
                        .unwrap_or("no worker");

                    println!("  {name}");
                    println!("    {desc}");
                    println!("    {worker}");
                    println!();
                }
            }
            if !found {
                println!("  (no tasks)");
            }
        }
        Err(_) => println!("  ~/tasks/ not found"),
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
        let _ = Command::new("tmux").arg("ls").status();
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
        Some(Cmd::Jump { name }) => cmd_jump(&name),
        Some(Cmd::Status) => cmd_status(),
        Some(Cmd::Scan) => {
            let inbox = tasks_dir().join(".inbox");
            fs::write(&inbox, SCAN_MSG).expect("failed to write to inbox");
            eprintln!("[orch] scan triggered — daemon will run it");
        }
        Some(Cmd::Msg { message }) => {
            let msg = message.join(" ");
            let inbox = tasks_dir().join(".inbox");
            fs::write(&inbox, &msg).expect("failed to write to inbox");
            eprintln!("[orch] message sent");
        }
        None => cmd_status(), // default: show status
    }
}
