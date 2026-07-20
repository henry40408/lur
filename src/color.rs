//! Shared decision for whether to emit ANSI color, honoring `NO_COLOR` and TTY
//! state. Used by both the diagnostics renderer (stderr) and `lur docs` (stdout).

use std::ffi::OsStr;
use std::io::IsTerminal;

/// Color is on only when the target stream is a TTY and `NO_COLOR` is unset or
/// empty (the de-facto standard: a non-empty `NO_COLOR` disables color
/// regardless of its value).
pub fn color_from_env(no_color: Option<&OsStr>, stream_is_tty: bool) -> bool {
    stream_is_tty && no_color.is_none_or(std::ffi::OsStr::is_empty)
}

/// Whether diagnostics written to stderr should be colorized.
pub fn stderr_color() -> bool {
    color_from_env(
        std::env::var_os("NO_COLOR").as_deref(),
        std::io::stderr().is_terminal(),
    )
}

/// Whether `lur docs` output written to stdout should be colorized.
pub fn stdout_color() -> bool {
    color_from_env(
        std::env::var_os("NO_COLOR").as_deref(),
        std::io::stdout().is_terminal(),
    )
}

#[cfg(test)]
mod tests {
    use super::color_from_env;
    use std::ffi::OsStr;

    #[test]
    fn color_from_env_truth_table() {
        assert!(color_from_env(None, true));
        assert!(!color_from_env(Some(OsStr::new("1")), true));
        assert!(color_from_env(Some(OsStr::new("")), true));
        assert!(!color_from_env(None, false));
        assert!(!color_from_env(Some(OsStr::new("1")), false));
    }
}
