mod actions;
mod attach;
mod browse;
mod clip;
mod config;
mod gui;
mod login_path;
mod notes;
mod remote;
mod snapshot;
#[cfg(test)]
mod test_support;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use config::Config;
use std::path::PathBuf;

/// Sidebar for Claude Code sessions running in tmux on remote hosts.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the window (the default)
    Gui,
    /// Print hosts, bases, worktrees and sessions
    Ls,
    /// Start a thread in a new worktree, or in an existing one with --worktree
    New {
        /// Base name (last path component unless set in the config)
        base: String,
        /// Label for the thread (shown in the sidebar; several threads can
        /// share one); also the worktree name for new worktrees
        name: String,
        /// Path of an existing worktree to start in
        #[arg(long)]
        worktree: Option<String>,
        /// Start just a shell instead of Claude (in an existing worktree)
        #[arg(long, requires = "worktree")]
        shell: bool,
        /// Branch to create for a new worktree; without one it's detached
        #[arg(long, conflicts_with = "worktree")]
        branch: Option<String>,
        /// What a new worktree starts from, like origin/main; the default
        /// branch if unset
        #[arg(long, conflicts_with = "worktree")]
        from: Option<String>,
    },
    /// Attach this terminal to a session
    Attach { host: String, session: String },
    /// Rename a thread; its base prefix stays
    Rename {
        host: String,
        session: String,
        name: String,
    },
    /// Upload files and paste their remote paths into the active session
    Upload {
        #[arg(required = true)]
        files: Vec<PathBuf>,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// Upload the clipboard image and paste its remote path into the active session
    Paste {
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = Config::load()?;
    match cli.cmd.unwrap_or(Cmd::Gui) {
        Cmd::Gui => {
            login_path::add();
            gui::run(cfg)
        }
        Cmd::Ls => {
            print_tree(&cfg, &snapshot::fetch(&cfg));
            Ok(())
        }
        Cmd::New {
            base,
            name,
            worktree,
            shell,
            branch,
            from,
        } => {
            let b = cfg
                .base_by_name(&base)
                .with_context(|| format!("unknown base {base}"))?;
            actions::validate_name(&name)?;
            let thread = actions::unique_thread(&name, |_| false);
            let (host, session) = match worktree {
                Some(w) => actions::new_thread_in(&cfg, b, &w, &thread, shell)?,
                None => {
                    let (started, note) = actions::new_worktree_thread(
                        &cfg,
                        b,
                        &name,
                        &thread,
                        branch.as_deref(),
                        from.as_deref(),
                    )?;
                    if let Some(note) = note {
                        eprintln!("{note}");
                    }
                    started
                }
            };
            println!("started {session} on {host}");
            Ok(())
        }
        Cmd::Attach { host, session } => {
            let h = cfg.host_or_err(&host)?;
            attach::attach_here(h, &session)
        }
        Cmd::Rename {
            host,
            session,
            name,
        } => {
            let new = actions::rename_session(&cfg, &host, &session, &name)?;
            println!("renamed {session} to {new}");
            Ok(())
        }
        Cmd::Upload {
            files,
            host,
            session,
        } => {
            for f in &files {
                if !f.is_file() {
                    bail!("{} is not a file", f.display());
                }
            }
            let (host, session) = actions::resolve_target(&cfg, host, session)?;
            let paths = actions::upload_and_type(&cfg, &host, &session, &files)?;
            println!("sent {} to {session}", paths.join(" "));
            Ok(())
        }
        Cmd::Paste { host, session } => {
            let (host, session) = actions::resolve_target(&cfg, host, session)?;
            let dir = std::env::temp_dir().join(format!("lemur-{}", std::process::id()));
            std::fs::create_dir_all(&dir)?;
            let png = dir.join("clipboard.png");
            let sent = clip::clipboard_png(&png)
                .and_then(|()| actions::upload_and_type(&cfg, &host, &session, &[png]));
            let _ = std::fs::remove_dir_all(&dir);
            println!("sent {} to {session}", sent?.join(" "));
            Ok(())
        }
    }
}

fn print_tree(cfg: &Config, snap: &snapshot::Snapshot) {
    for h in &snap.hosts {
        match &h.error {
            Some(e) => println!("{}  (error: {e})", h.name),
            None => println!("{}", h.name),
        }
        for b in &h.bases {
            let base = &cfg.bases[b.index];
            match &b.error {
                Some(e) => println!("  {}  {}  (error: {e})", base.name(), base.path),
                None => println!("  {}  {}", base.name(), base.path),
            }
            for w in &b.worktrees {
                println!(
                    "    {}  [{}]",
                    w.path,
                    w.branch.as_deref().unwrap_or("detached")
                );
                for s in &w.sessions {
                    print_session(s, "      ");
                }
            }
        }
        if !h.other.is_empty() {
            println!("  (no base)");
            for s in &h.other {
                print_session(s, "    ");
            }
        }
    }
}

fn print_session(s: &snapshot::Session, indent: &str) {
    let state = match s.activity {
        snapshot::Activity::NeedsInput => "needs input",
        snapshot::Activity::Working => "working",
        snapshot::Activity::Idle => "idle",
        snapshot::Activity::Unknown => "-",
    };
    let attached = if s.attached { ", attached" } else { "" };
    println!(
        "{indent}{}  ({state}, idle {}s{attached})",
        s.name, s.idle_secs
    );
}
