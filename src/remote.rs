use crate::config::Host;
use anyhow::{Context, Result, bail};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// One shared SSH connection per host, so each command costs milliseconds.
const SSH_OPTS: &[&str] = &[
    "-o",
    "ControlMaster=auto",
    "-o",
    "ControlPath=~/.ssh/muxbox-%C",
    "-o",
    "ControlPersist=10m",
];

/// Options for non-interactive calls: never prompt, fail fast.
const BATCH_OPTS: &[&str] = &["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"];

pub struct Remote<'a> {
    pub host: &'a Host,
}

impl<'a> Remote<'a> {
    pub fn new(host: &'a Host) -> Self {
        Remote { host }
    }

    pub fn is_local(&self) -> bool {
        self.host.ssh == "local"
    }

    /// The `tmux` command for this host, as shell words.
    pub fn tmux(&self) -> String {
        match &self.host.tmux_socket {
            Some(s) => format!("tmux -L {}", quote(s)),
            None => "tmux".into(),
        }
    }

    /// Runs a POSIX sh script on the host and returns its stdout.
    ///
    /// The script goes over stdin to `sh -s`, so the remote login shell
    /// (bash, zsh, fish...) never has to parse it.
    pub fn run(&self, script: &str) -> Result<String> {
        let mut cmd = if self.is_local() {
            let mut c = Command::new("sh");
            c.arg("-s");
            c
        } else {
            let mut c = Command::new("ssh");
            c.args(SSH_OPTS)
                .args(BATCH_OPTS)
                .arg("-T")
                .arg(&self.host.ssh)
                .arg("sh -s");
            c
        };
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("starting a shell on {}", self.host.name))?;
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(script.as_bytes())?;
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!(
                "{}: {}",
                self.host.name,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Copies a local file to an absolute path on the host.
    pub fn copy_to(&self, local: &Path, remote_path: &str) -> Result<()> {
        if self.is_local() {
            std::fs::copy(local, remote_path)
                .with_context(|| format!("copying to {remote_path}"))?;
            return Ok(());
        }
        let out = Command::new("scp")
            .args(SSH_OPTS)
            .args(BATCH_OPTS)
            .arg("-q")
            .arg(local)
            .arg(format!("{}:{}", self.host.ssh, remote_path))
            .output()
            .context("starting scp")?;
        if !out.status.success() {
            bail!(
                "scp to {}: {}",
                self.host.name,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    /// argv for an interactive terminal that runs `script` on the host.
    pub fn interactive(&self, script: &str) -> Vec<String> {
        if self.is_local() {
            return vec!["sh".into(), "-c".into(), script.into()];
        }
        let mut argv: Vec<String> = vec!["ssh".into()];
        argv.extend(SSH_OPTS.iter().map(|s| s.to_string()));
        argv.push("-t".into());
        argv.push(self.host.ssh.clone());
        argv.push(format!("sh -c {}", quote(script)));
        argv
    }
}

/// Quotes a string as a single POSIX shell word.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Quotes a path for sh while keeping a leading `~/` expandable.
pub fn sh_path(p: &str) -> String {
    if p == "~" {
        "\"$HOME\"".into()
    } else if let Some(rest) = p.strip_prefix("~/") {
        format!("\"$HOME\"/{}", quote(rest))
    } else {
        quote(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_escapes_single_quotes() {
        assert_eq!(quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn sh_path_keeps_tilde_expandable() {
        assert_eq!(sh_path("~/code/api"), "\"$HOME\"/'code/api'");
        assert_eq!(sh_path("/srv/api"), "'/srv/api'");
        assert_eq!(sh_path(".claude/worktrees"), "'.claude/worktrees'");
    }
}
