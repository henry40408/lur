//! The capability policy: fs, env, and network allowlists (spec §5).
//!
//! Fs read and write are separate lists. Requested paths are canonicalized
//! before the prefix check, so `..` or a symlink cannot escape a granted root;
//! a directory root grants its subtree, a file root only that file.
//!
//! Known limitation: canonicalize-then-open has a TOCTOU window (a symlink
//! swapped after the check); mitigation is deferred to §5 Layer-B OS hardening.

use std::net::{IpAddr, Ipv6Addr};
use std::path::{Path, PathBuf};

use thiserror::Error;

/// `host` (any port) or `host:port`; `*` matches any host (still subject to
/// the private-network deny).
#[derive(Clone, Debug)]
struct HostRule {
    host: String,
    port: Option<u16>,
}

impl HostRule {
    fn parse(s: &str) -> Self {
        // Bracketed IPv6, optionally with a port: [::1] or [::1]:6379.
        if let Some(rest) = s.strip_prefix('[') {
            if let Some((h, p)) = rest.split_once("]:") {
                return Self {
                    host: h.to_lowercase(),
                    port: p.parse().ok(),
                };
            }
            if let Some(h) = rest.strip_suffix(']') {
                return Self {
                    host: h.to_lowercase(),
                    port: None,
                };
            }
        }
        // A bare IPv6 literal contains colons but no port.
        if s.parse::<Ipv6Addr>().is_ok() {
            return Self {
                host: s.to_lowercase(),
                port: None,
            };
        }
        // host:port (single colon) or a bare host.
        if let Some((h, p)) = s.rsplit_once(':')
            && let Ok(port) = p.parse::<u16>()
        {
            return Self {
                host: h.to_lowercase(),
                port: Some(port),
            };
        }
        Self {
            host: s.to_lowercase(),
            port: None,
        }
    }

    fn matches(&self, host: &str, port: u16) -> bool {
        (self.host == "*" || self.host == host) && self.port.is_none_or(|p| p == port)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Policy {
    /// Canonicalized.
    fs_read: Vec<PathBuf>,
    /// Canonicalized.
    fs_write: Vec<PathBuf>,
    /// Exact names.
    env_allow: Vec<String>,
    env_allow_all: bool,
    net_allow: Vec<HostRule>,
    /// Off by default (SSRF guard, §5).
    allow_private_net: bool,
}

#[derive(Debug, Error)]
pub enum PolicyError {
    /// Resolved, but outside every granted root.
    #[error("{op} access to {} is not allowed by the policy", path.display())]
    Denied { op: &'static str, path: PathBuf },

    /// The path (or its parent, for writes) could not be resolved.
    #[error("cannot resolve {}: {source}", path.display())]
    Resolve {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl Policy {
    /// No access at all (the default profile, §5).
    pub fn strict() -> Self {
        Self::default()
    }

    /// Full fs read/write, every env var, any host, private IPs included (§5).
    pub fn loose() -> std::io::Result<Self> {
        let root = vec![PathBuf::from("/")];
        Ok(Self::from_roots(&root, &root)?
            .allow_all_env()
            .with_net(vec!["*".to_string()])
            .allow_private())
    }

    /// Roots are canonicalized; a missing root is an error.
    pub fn from_roots(read: &[PathBuf], write: &[PathBuf]) -> std::io::Result<Self> {
        Ok(Self {
            fs_read: canonicalize_all(read)?,
            fs_write: canonicalize_all(write)?,
            ..Default::default()
        })
    }

    /// Replaces the env allowlist.
    pub fn with_env(mut self, names: Vec<String>) -> Self {
        self.env_allow = names;
        self
    }

    pub fn allow_all_env(mut self) -> Self {
        self.env_allow_all = true;
        self
    }

    pub fn allows_env(&self, name: &str) -> bool {
        self.env_allow_all || self.env_allow.iter().any(|n| n == name)
    }

    /// Replaces the network allowlist (`host`, `host:port`, or `*`).
    pub fn with_net(mut self, hosts: Vec<String>) -> Self {
        self.net_allow = hosts.iter().map(|h| HostRule::parse(h)).collect();
        self
    }

    pub fn allow_private(mut self) -> Self {
        self.allow_private_net = true;
        self
    }

    pub fn allows_net(&self, host: &str, port: u16) -> bool {
        let host = host.to_lowercase();
        self.net_allow.iter().any(|r| r.matches(&host, port))
    }

    pub fn allows_private_net(&self) -> bool {
        self.allow_private_net
    }

    /// The SSRF deny set (§5): loopback, private, link-local, unique-local, and
    /// unspecified. IPv4-mapped IPv6 addresses are unwrapped first.
    pub fn is_private_ip(ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => {
                v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
            }
            IpAddr::V6(v6) => {
                if let Some(mapped) = v6.to_ipv4_mapped() {
                    return Self::is_private_ip(IpAddr::V4(mapped));
                }
                v6.is_loopback() || v6.is_unspecified() || is_unique_local(v6) || is_link_local(v6)
            }
        }
    }

    /// Returns the canonicalized path to open.
    pub fn allows_read(&self, path: &Path) -> Result<PathBuf, PolicyError> {
        let resolved = resolve_existing(path)?;
        gate("read", &self.fs_read, resolved)
    }

    /// A new file resolves through its parent. Returns the canonicalized path.
    pub fn allows_write(&self, path: &Path) -> Result<PathBuf, PolicyError> {
        let resolved = resolve_for_write(path)?;
        gate("write", &self.fs_write, resolved)
    }
}

/// `starts_with` is component-wise, so root `/a/b` does not match `/a/bc`.
fn gate(op: &'static str, roots: &[PathBuf], resolved: PathBuf) -> Result<PathBuf, PolicyError> {
    if roots.iter().any(|root| resolved.starts_with(root)) {
        Ok(resolved)
    } else {
        Err(PolicyError::Denied { op, path: resolved })
    }
}

/// `fc00::/7` — unique local addresses.
fn is_unique_local(ip: Ipv6Addr) -> bool {
    ip.segments()[0] & 0xfe00 == 0xfc00
}

/// `fe80::/10` — link-local addresses.
fn is_link_local(ip: Ipv6Addr) -> bool {
    ip.segments()[0] & 0xffc0 == 0xfe80
}

fn canonicalize_all(roots: &[PathBuf]) -> std::io::Result<Vec<PathBuf>> {
    roots.iter().map(std::fs::canonicalize).collect()
}

fn resolve_existing(path: &Path) -> Result<PathBuf, PolicyError> {
    std::fs::canonicalize(path).map_err(|source| PolicyError::Resolve {
        path: path.to_path_buf(),
        source,
    })
}

/// The file if it exists, else its canonical parent joined with the file name.
fn resolve_for_write(path: &Path) -> Result<PathBuf, PolicyError> {
    if let Ok(existing) = std::fs::canonicalize(path) {
        return Ok(existing);
    }
    // A dangling symlink fails canonicalize, but writing through it would
    // create its target outside the granted root.
    if std::fs::symlink_metadata(path).is_ok() {
        return Err(PolicyError::Resolve {
            path: path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path is a dangling symlink",
            ),
        });
    }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let file_name = path.file_name();
    match (parent, file_name) {
        (Some(parent), Some(name)) => {
            let parent = std::fs::canonicalize(parent).map_err(|source| PolicyError::Resolve {
                path: parent.to_path_buf(),
                source,
            })?;
            Ok(parent.join(name))
        }
        _ => Err(PolicyError::Resolve {
            path: path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path has no resolvable parent",
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loose_profile_is_permissive() {
        let p = Policy::loose().expect("loose builds");
        assert!(p.allows_env("ANY_NAME"));
        assert!(p.allows_net("example.com", 443));
        assert!(p.allows_private_net());
    }

    #[cfg(unix)]
    #[test]
    fn write_through_dangling_symlink_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("escaped.txt");
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let p = Policy::from_roots(&[], &[root.path().to_path_buf()]).unwrap();
        assert!(matches!(
            p.allows_write(&link),
            Err(PolicyError::Resolve { .. })
        ));
    }

    #[test]
    fn strict_profile_denies_by_default() {
        let p = Policy::strict();
        assert!(!p.allows_env("ANY_NAME"));
        assert!(!p.allows_net("example.com", 443));
        assert!(!p.allows_private_net());
    }
}
