//! The capability policy and allowlist matching (spec §5).
//!
//! v1 implements the filesystem allowlists. Read and write are **separate**
//! lists (like Deno). Matching is escape-proof by **canonicalizing** the
//! requested path (resolving `.`/`..`/symlinks to the real absolute path)
//! before the prefix check, so a granted root cannot be escaped via `..` or a
//! symlink. A directory root grants its whole subtree; a file root grants only
//! that file. Roots are canonicalized once at construction.
//!
//! Known limitation: canonicalize-then-open has a TOCTOU window (a symlink
//! swapped after the check). Full mitigation belongs to the reserved §5
//! Layer-B OS hardening, not v1.

use std::net::{IpAddr, Ipv6Addr};
use std::path::{Path, PathBuf};

use thiserror::Error;

/// A network allowlist entry: a host (any port) or a host with a fixed port.
/// The host `*` matches any host (still subject to the private-network deny).
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

/// A resolved capability policy.
#[derive(Clone, Debug, Default)]
pub struct Policy {
    /// Canonicalized readable roots.
    fs_read: Vec<PathBuf>,
    /// Canonicalized writable roots.
    fs_write: Vec<PathBuf>,
    /// Allowlisted environment-variable names (exact match).
    env_allow: Vec<String>,
    /// When set, every environment variable is readable (`-A`).
    env_allow_all: bool,
    /// Allowed network hosts (host or host:port).
    net_allow: Vec<HostRule>,
    /// When set, connections to loopback/private/link-local IPs are permitted
    /// (off by default — SSRF deny, §5).
    allow_private_net: bool,
}

/// Why a filesystem access was refused.
#[derive(Debug, Error)]
pub enum PolicyError {
    /// The path resolved fine but is not within any granted root.
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
    /// The shipped strict policy: no filesystem access at all.
    pub fn strict() -> Self {
        Self::default()
    }

    /// Build a policy from raw read/write roots, canonicalizing each to an
    /// absolute real path. Roots that do not exist are an error.
    pub fn from_roots(read: &[PathBuf], write: &[PathBuf]) -> std::io::Result<Self> {
        Ok(Self {
            fs_read: canonicalize_all(read)?,
            fs_write: canonicalize_all(write)?,
            ..Default::default()
        })
    }

    /// Add allowlisted environment-variable names (builder style).
    pub fn with_env(mut self, names: Vec<String>) -> Self {
        self.env_allow = names;
        self
    }

    /// Grant read access to every environment variable (`-A`).
    pub fn allow_all_env(mut self) -> Self {
        self.env_allow_all = true;
        self
    }

    /// Whether `name` may be read via `lur.env`.
    pub fn allows_env(&self, name: &str) -> bool {
        self.env_allow_all || self.env_allow.iter().any(|n| n == name)
    }

    /// Add allowlisted network hosts (`host`, `host:port`, or `*`).
    pub fn with_net(mut self, hosts: Vec<String>) -> Self {
        self.net_allow = hosts.iter().map(|h| HostRule::parse(h)).collect();
        self
    }

    /// Permit connections to private/loopback/link-local IPs (`--allow-private`).
    pub fn allow_private(mut self) -> Self {
        self.allow_private_net = true;
        self
    }

    /// Whether `host:port` is on the network allowlist.
    pub fn allows_net(&self, host: &str, port: u16) -> bool {
        let host = host.to_lowercase();
        self.net_allow.iter().any(|r| r.matches(&host, port))
    }

    /// Whether connections to private/loopback IPs are permitted.
    pub fn allows_private_net(&self) -> bool {
        self.allow_private_net
    }

    /// Whether `ip` is in a loopback / private / link-local / unique-local range
    /// (the SSRF deny set, §5). IPv4-mapped IPv6 addresses are unwrapped first.
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

    /// Check a read. On success, returns the canonicalized path to open.
    pub fn allows_read(&self, path: &Path) -> Result<PathBuf, PolicyError> {
        let resolved = resolve_existing(path)?;
        gate("read", &self.fs_read, resolved)
    }

    /// Check a write. A not-yet-existing file resolves through its parent. On
    /// success, returns the canonicalized path to write.
    pub fn allows_write(&self, path: &Path) -> Result<PathBuf, PolicyError> {
        let resolved = resolve_for_write(path)?;
        gate("write", &self.fs_write, resolved)
    }
}

/// Return `resolved` if it lies within any granted root, else `Denied`.
///
/// `starts_with` is component-wise, so a file root `/a/b` does not spuriously
/// match a sibling `/a/bc`, and a directory root matches its whole subtree.
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

/// Canonicalize an existing path.
fn resolve_existing(path: &Path) -> Result<PathBuf, PolicyError> {
    std::fs::canonicalize(path).map_err(|source| PolicyError::Resolve {
        path: path.to_path_buf(),
        source,
    })
}

/// Canonicalize a write target: the file if it exists, otherwise its parent
/// directory joined with the file name (so new files are allowed, but only
/// inside an already-real parent).
fn resolve_for_write(path: &Path) -> Result<PathBuf, PolicyError> {
    if let Ok(existing) = std::fs::canonicalize(path) {
        return Ok(existing);
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
