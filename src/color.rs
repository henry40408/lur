//! Whether to emit ANSI color, honoring `NO_COLOR` and TTY state.

use std::ffi::OsStr;
use std::io::IsTerminal;

/// On only for a TTY with `NO_COLOR` unset or empty (per no-color.org).
pub fn color_from_env(no_color: Option<&OsStr>, stream_is_tty: bool) -> bool {
    stream_is_tty && no_color.is_none_or(std::ffi::OsStr::is_empty)
}

pub fn stderr_color() -> bool {
    color_from_env(
        std::env::var_os("NO_COLOR").as_deref(),
        std::io::stderr().is_terminal(),
    )
}

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
