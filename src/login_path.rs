//! The login shell's PATH, for an app started from Finder or the Dock.
//!
//! Those get only the system's PATH, so a host with `ssh = "local"` wouldn't
//! find tmux in Homebrew's `/opt/homebrew/bin`. The login shell sets up
//! where things are installed.

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Starts the line with the PATH, past whatever the shell's profile prints.
const MARK: &str = "lemur-path:";

/// Adds the directories of the login shell's PATH that lemur's lacks, after
/// lemur's own. Call it before any other thread starts.
pub fn add() {
    let Some(shell) = std::env::var_os("SHELL") else {
        return;
    };
    let Some(login) = login_path(&shell, Duration::from_secs(2)) else {
        return;
    };
    let current = std::env::var_os("PATH").unwrap_or_default();
    if let Some(path) = merge(&current, &login) {
        // SAFETY: lemur has no other threads yet, so none reads the
        // environment meanwhile.
        unsafe { std::env::set_var("PATH", path) };
    }
}

/// The PATH `shell` sets up as a login shell, or None if it fails or takes
/// longer than `timeout`.
fn login_path(shell: &OsStr, timeout: Duration) -> Option<OsString> {
    // Quoted, fish joins its PATH list with colons too.
    let probe = format!(r#"printf "\n{MARK}%s\n" "$PATH""#);
    let mut child = Command::new(shell)
        .args(["-l", "-c", &probe])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        out
    });
    let deadline = Instant::now() + timeout;
    while !reader.is_finished() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = child.wait();
    parse(&reader.join().ok()?)
}

/// The PATH from the shell's output: the rest of the line with the mark.
fn parse(out: &[u8]) -> Option<OsString> {
    out.split(|&b| b == b'\n')
        .find_map(|line| line.strip_prefix(MARK.as_bytes()))
        .filter(|path| !path.is_empty())
        .map(|path| OsString::from_vec(path.to_vec()))
}

/// `current` followed by the absolute directories of `login` it lacks, or
/// None if it lacks none.
fn merge(current: &OsStr, login: &OsStr) -> Option<OsString> {
    let mut dirs: Vec<PathBuf> = std::env::split_paths(current)
        .filter(|dir| !dir.as_os_str().is_empty())
        .collect();
    let before = dirs.len();
    for dir in std::env::split_paths(login) {
        if dir.is_absolute() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    if dirs.len() == before {
        return None;
    }
    std::env::join_paths(dirs).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    /// An executable `sh` script standing in for the login shell.
    fn fake_shell(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("shell");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn adds_missing_directories_after_its_own() {
        let path = merge(
            OsStr::new("/usr/bin:/bin"),
            OsStr::new("/opt/homebrew/bin:/usr/bin:/bin:/opt/homebrew/sbin"),
        );
        assert_eq!(
            path.as_deref(),
            Some(OsStr::new(
                "/usr/bin:/bin:/opt/homebrew/bin:/opt/homebrew/sbin"
            ))
        );
    }

    #[test]
    fn leaves_the_path_alone_when_nothing_is_missing() {
        assert_eq!(merge(OsStr::new("/usr/bin:/bin"), OsStr::new("/bin")), None);
    }

    #[test]
    fn skips_relative_and_empty_directories() {
        let path = merge(OsStr::new(""), OsStr::new("::bin:/usr/bin"));
        assert_eq!(path.as_deref(), Some(OsStr::new("/usr/bin")));
    }

    #[test]
    fn finds_the_path_past_what_the_profile_prints() {
        let out = b"Welcome!\nlemur-path:/a:/b\nbye\n";
        assert_eq!(parse(out).as_deref(), Some(OsStr::new("/a:/b")));
        assert_eq!(parse(b"Welcome!\n"), None);
    }

    #[test]
    fn asks_the_shell_for_its_login_path() {
        let dir = TempDir::new("login-path");
        let shell = fake_shell(
            &dir,
            r#"[ "$1" = -l ] && PATH=/from/profile:/usr/bin; echo hi; exec /bin/sh -c "$3""#,
        );
        assert_eq!(
            login_path(shell.as_os_str(), Duration::from_secs(5)).as_deref(),
            Some(OsStr::new("/from/profile:/usr/bin"))
        );
    }

    #[test]
    fn gives_up_on_a_slow_shell() {
        let dir = TempDir::new("login-path-slow");
        let shell = fake_shell(&dir, "sleep 5");
        let start = Instant::now();
        assert_eq!(
            login_path(shell.as_os_str(), Duration::from_millis(200)),
            None
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
