//! Cross-platform process and path helpers.
//!
//! Everything in this crate that resolves a Unix-style home path goes through
//! [`home_dir`] so the same code runs on Windows, where `HOME` is usually
//! unset and the user directory lives in `USERPROFILE`.

use std::path::PathBuf;

/// The user's home directory.
///
/// `HOME` wins when set — it is the Unix convention and the override the
/// tests rely on. On Windows, `USERPROFILE` is the fallback, and the
/// `directories` crate is the last resort for the exotic case where neither
/// variable survives into the process.
///
/// On Windows a `HOME` that is not an absolute Windows path is ignored: a
/// process started from Git Bash/MSYS can inherit `/c/Users/me`, which Rust
/// would resolve as `C:\c\Users\me` and miss every agent's data.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| !cfg!(windows) || path.is_absolute())
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()))
}

/// Build the shell invocation for a user-owned command string.
///
/// Unix runs the command through `sh -c`. Windows has no `sh` outside Git
/// Bash, and the agent harnesses there execute statusLine commands through
/// `cmd.exe`, so the command string is passed to `cmd /S /C` instead.
pub fn shell_command(command: &str) -> std::process::Command {
    if cfg!(windows) {
        let mut builder = std::process::Command::new("cmd");
        builder.args(["/S", "/C", command]);
        builder
    } else {
        let mut builder = std::process::Command::new("sh");
        builder.args(["-c", command]);
        builder
    }
}

/// Quote a path for use inside a user-owned shell command.
///
/// POSIX single quotes, or double quotes on Windows: `cmd.exe` only
/// understands double quotes, and a Windows path cannot contain one.
pub fn shell_quote(path: &std::path::Path) -> String {
    if cfg!(windows) {
        format!("\"{}\"", path.display())
    } else {
        format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
    }
}

/// Kill a spawned process and every descendant it started.
///
/// Unix callers put the child in its own process group and signal the
/// group. Windows has no such groups, so the tree is torn down with
/// `taskkill /T /F`; a bare `kill()` would leave grandchildren holding
/// the stdout pipe open.
#[cfg(windows)]
pub fn kill_process_tree(child: &std::process::Child) {
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &child.id().to_string(), "/T", "/F"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_wraps_for_the_platform_shell() {
        // `Path::display` keeps the separators it was given; only the
        // quoting differs between the shells.
        let quoted = shell_quote(std::path::Path::new("/tmp/a b"));
        if cfg!(windows) {
            assert_eq!(quoted, "\"/tmp/a b\"");
        } else {
            assert_eq!(quoted, "'/tmp/a b'");
        }
    }
}
