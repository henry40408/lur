//! The user config file (spec §5/§12): `default_profile` plus standing
//! `[allow]` grants, which the CLI unions with per-run flags.

use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// Deny by default; the shipped default.
    Strict,
    /// Full access.
    Loose,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    pub default_profile: Option<Profile>,
    pub net: Vec<String>,
    /// May contain `~`.
    pub fs_read: Vec<PathBuf>,
    /// May contain `~`.
    pub fs_write: Vec<PathBuf>,
    pub env: Vec<String>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read config {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Malformed TOML or a bad `default_profile`.
    #[error("invalid config {}: {message}", path.display())]
    Parse { path: PathBuf, message: String },
}

impl Config {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Only malformed TOML or a bad `default_profile` errors; unknown keys and
    /// non-string `[allow]` entries are silently ignored.
    pub fn parse(src: &str) -> Result<Self, String> {
        let table: toml::Table = toml::from_str(src).map_err(|e| e.message().to_string())?;

        let default_profile = match table.get("default_profile") {
            None => None,
            Some(v) => match v.as_str() {
                Some("strict") => Some(Profile::Strict),
                Some("loose") => Some(Profile::Loose),
                _ => {
                    return Err(format!(
                        "default_profile must be \"strict\" or \"loose\", got {v}"
                    ));
                }
            },
        };

        let allow = table.get("allow").and_then(|v| v.as_table());
        let strings = |key: &str| -> Vec<String> {
            allow
                .and_then(|a| a.get(key))
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        };

        Ok(Self {
            default_profile,
            net: strings("net"),
            fs_read: strings("fs_read").into_iter().map(PathBuf::from).collect(),
            fs_write: strings("fs_write").into_iter().map(PathBuf::from).collect(),
            env: strings("env"),
        })
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let src = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&src).map_err(|message| ConfigError::Parse {
            path: path.to_path_buf(),
            message,
        })
    }
}

/// Expand a leading `~` / `~/…`; unchanged if there is none or `home` is `None`.
pub fn expand_tilde(path: &Path, home: Option<&Path>) -> PathBuf {
    let Some(home) = home else {
        return path.to_path_buf();
    };
    let Some(s) = path.to_str() else {
        return path.to_path_buf();
    };
    if s == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = s.strip_prefix("~/") {
        return home.join(rest);
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reads_profile_and_allow_table() {
        let cfg = Config::parse(
            "default_profile = \"loose\"\n\
             [allow]\n\
             net = [\"api.github.com\", \"10.0.0.5:6379\"]\n\
             fs_read = [\"~/data\"]\n\
             fs_write = [\"./out\"]\n\
             env = [\"API_KEY\"]\n",
        )
        .expect("parses");
        assert_eq!(cfg.default_profile, Some(Profile::Loose));
        assert_eq!(cfg.net, vec!["api.github.com", "10.0.0.5:6379"]);
        assert_eq!(cfg.fs_read, vec![PathBuf::from("~/data")]);
        assert_eq!(cfg.fs_write, vec![PathBuf::from("./out")]);
        assert_eq!(cfg.env, vec!["API_KEY"]);
    }

    #[test]
    fn parse_tolerates_missing_allow_table() {
        let cfg = Config::parse("default_profile = \"strict\"\n").expect("parses");
        assert_eq!(cfg.default_profile, Some(Profile::Strict));
        assert!(cfg.net.is_empty() && cfg.env.is_empty());
    }

    #[test]
    fn parse_rejects_a_bad_profile_value() {
        assert!(Config::parse("default_profile = \"yolo\"\n").is_err());
    }

    #[test]
    fn parse_rejects_malformed_toml() {
        assert!(Config::parse("default_profile = ").is_err());
    }

    #[test]
    fn expand_tilde_resolves_against_home() {
        let home = PathBuf::from("/home/me");
        assert_eq!(
            expand_tilde(Path::new("~/data"), Some(&home)),
            PathBuf::from("/home/me/data")
        );
        assert_eq!(expand_tilde(Path::new("~"), Some(&home)), home);
        // No tilde, or unknown home → unchanged.
        assert_eq!(
            expand_tilde(Path::new("./out"), Some(&home)),
            PathBuf::from("./out")
        );
        assert_eq!(
            expand_tilde(Path::new("~/data"), None),
            PathBuf::from("~/data")
        );
    }
}
