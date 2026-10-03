//! The capability policy: fs, env, and network allowlists (spec §5).
//!
//! Fs read and write are separate lists. Requested paths are canonicalized
//! before the prefix check, so `..` or a symlink cannot escape a granted root;
//! a directory root grants its subtree, a file root only that file.
//!
//! Known limitation: canonicalize-then-open has a TOCTOU window (a symlink
//! swapped after the check); mitigation is deferred to §5 Layer-B OS hardening.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};

use thiserror::Error;

/// `host` (any port) or `host:port`; `*` matches any host (still subject to
/// the private-network deny).
///
/// IPv6 hosts are stored without brackets. A host that parses as an IP is
/// compared by address, so `::1`, `[::1]`, and `0:0:0:0:0:0:0:1` all match.
#[derive(Clone, Debug)]
struct HostRule {
    host: String,
    ip: Option<IpAddr>,
    port: Option<u16>,
}

impl HostRule {
    fn parse(s: &str) -> Self {
        let mut rule = Self::parse_parts(s);
        rule.ip = rule.host.parse().ok();
        rule
    }

    fn parse_parts(s: &str) -> Self {
        // Bracketed IPv6, optionally with a port: [::1] or [::1]:6379.
        if let Some(rest) = s.strip_prefix('[') {
            if let Some((h, p)) = rest.split_once("]:") {
                return Self {
                    host: h.to_lowercase(),
                    ip: None,
                    port: p.parse().ok(),
                };
            }
            if let Some(h) = rest.strip_suffix(']') {
                return Self {
                    host: h.to_lowercase(),
                    ip: None,
                    port: None,
                };
            }
        }
        // A bare IPv6 literal contains colons but no port.
        if s.parse::<Ipv6Addr>().is_ok() {
            return Self {
                host: s.to_lowercase(),
                ip: None,
                port: None,
            };
        }
        // host:port (single colon) or a bare host.
        if let Some((h, p)) = s.rsplit_once(':')
            && let Ok(port) = p.parse::<u16>()
        {
            return Self {
                host: h.to_lowercase(),
                ip: None,
                port: Some(port),
            };
        }
        Self {
            host: s.to_lowercase(),
            ip: None,
            port: None,
        }
    }

    /// `host` is lowercased with any IPv6 brackets stripped; `ip` is its parse.
    fn matches(&self, host: &str, ip: Option<IpAddr>, port: u16) -> bool {
        let host_ok = self.host == "*"
            || match (self.ip, ip) {
                (Some(a), Some(b)) => a == b,
                _ => self.host == host,
            };
        host_ok && self.port.is_none_or(|p| p == port)
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

    /// `host` may be a name, an IPv4 literal, or an IPv6 literal with or
    /// without brackets (`Url::host_str` yields `[::1]`). IP literals match a
    /// rule by address equality, not by spelling.
    pub fn allows_net(&self, host: &str, port: u16) -> bool {
        let host = host.to_lowercase();
        let host = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(&host);
        let ip = host.parse::<IpAddr>().ok();
        self.net_allow.iter().any(|r| r.matches(host, ip, port))
    }

    pub fn allows_private_net(&self) -> bool {
        self.allow_private_net
    }

    /// The SSRF deny set (§5): addresses a script must not reach without
    /// `--allow-private`.
    ///
    /// This is a curated list of ranges that lead to the local host, internal
    /// networks, or cloud metadata services — not the full IANA special-purpose
    /// registry. IPv6 forms that embed an IPv4 address (mapped `::ffff:a.b.c.d`,
    /// compatible `::a.b.c.d`, NAT64 `64:ff9b::/96`, 6to4 `2002::/16`) are judged
    /// by the embedded IPv4 address.
    pub fn is_private_ip(ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => is_private_v4(v4),
            IpAddr::V6(v6) => is_private_v6(v6),
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

fn is_private_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    // 127/8, 10/8, 172.16/12, 192.168/16, 169.254/16 (AWS/GCP/Azure metadata).
    ip.is_loopback() || ip.is_private() || ip.is_link_local()
        // 0.0.0.0/8 "this network"; 0.0.0.0 reaches localhost on most stacks.
        || a == 0
        // 100.64.0.0/10 CGNAT shared space (Alibaba Cloud metadata: 100.100.100.200).
        || (a == 100 && b & 0xc0 == 64)
        // 192.0.0.0/24 IETF protocol assignments (Oracle Cloud metadata: 192.0.0.192).
        || (a == 192 && b == 0 && c == 0)
        // 198.18.0.0/15 benchmarking, often used as internal space.
        || (a == 198 && b & 0xfe == 18)
        // 224.0.0.0/4 multicast; 240.0.0.0/4 reserved, incl. 255.255.255.255.
        || ip.is_multicast()
        || a >= 240
}

fn is_private_v6(ip: Ipv6Addr) -> bool {
    // Checked before `embedded_ipv4`, whose compatible form `::/96` covers them.
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    if let Some(v4) = embedded_ipv4(ip) {
        return is_private_v4(v4);
    }
    let s = ip.segments();
    is_unique_local(ip)
        || is_link_local(ip)
        // fec0::/10 deprecated site-local; some stacks still route it locally.
        || s[0] & 0xffc0 == 0xfec0
        // 64:ff9b:1::/48 local-use NAT64; the embedded IPv4 position varies.
        || (s[0] == 0x0064 && s[1] == 0xff9b && s[2] == 0x0001)
        // ff00::/8 multicast.
        || ip.is_multicast()
}

/// The IPv4 address an IPv6 address stands in for, if any: mapped
/// `::ffff:0:0/96`, compatible `::/96`, NAT64 `64:ff9b::/96`, or 6to4
/// `2002::/16`. Callers must handle `::` and `::1` first (compatible form).
///
/// Teredo (`2001::/32`) is not unwrapped: its client address is obfuscated
/// and reaching it needs a Teredo relay, not a direct connection.
fn embedded_ipv4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let ([0, 0, 0, 0, 0, 0 | 0xffff, hi, lo]
    | [0x0064, 0xff9b, 0, 0, 0, 0, hi, lo]
    | [0x2002, hi, lo, ..]) = ip.segments()
    else {
        return None;
    };
    Some(Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo)))
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
