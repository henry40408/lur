//! The capability policy: fs, env, and network allowlists.
//!
//! Fs read and write are separate lists. Requested paths are canonicalized
//! before the prefix check, so `..` or a symlink cannot escape a granted root;
//! a directory root grants its subtree, a file root only that file.
//!
//! Each root is held as an open directory handle (`cap-std`). `lur.fs` opens
//! files through [`Beneath`], so a symlink swapped in after the check cannot lead
//! out of the root: the check only picks which root, the OS enforces confinement.

use std::ffi::{OsStr, OsString};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cap_std::ambient_authority;
use cap_std::fs::Dir;
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
    fs_read: Vec<FsRoot>,
    fs_write: Vec<FsRoot>,
    /// Exact names.
    env_allow: Vec<String>,
    env_allow_all: bool,
    net_allow: Vec<HostRule>,
    /// Off by default (SSRF guard).
    allow_private_net: bool,
}

/// A granted root: its canonical path plus an open directory handle, so access
/// can be confined to it by the OS rather than by re-resolving a path string.
#[derive(Clone, Debug)]
struct FsRoot {
    /// Canonicalized.
    path: PathBuf,
    /// The root itself, or for a file root the file's parent directory.
    dir: Arc<Dir>,
    /// Set for a file root: the only name reachable through `dir`.
    file: Option<OsString>,
}

impl FsRoot {
    fn open(path: &Path) -> std::io::Result<Self> {
        let path = std::fs::canonicalize(path)?;
        let (dir_path, file) = if path.is_dir() {
            (path.as_path(), None)
        } else {
            let parent = path.parent().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "root has no parent")
            })?;
            (parent, path.file_name().map(OsStr::to_os_string))
        };
        let dir = Arc::new(Dir::open_ambient_dir(dir_path, ambient_authority())?);
        Ok(Self { path, dir, file })
    }

    fn beneath(&self, resolved: &Path) -> Beneath {
        Beneath {
            dir: Arc::clone(&self.dir),
            rel: self.relative(resolved),
        }
    }

    /// `resolved` (canonical, already known to be under this root) relative to
    /// the directory handle.
    fn relative(&self, resolved: &Path) -> PathBuf {
        match &self.file {
            Some(name) => PathBuf::from(name),
            None => match resolved.strip_prefix(&self.path) {
                Ok(rel) if !rel.as_os_str().is_empty() => rel.to_path_buf(),
                _ => PathBuf::from("."),
            },
        }
    }
}

/// A path confined beneath a granted root's directory handle. The OS refuses to
/// follow `..` or symlinks out of the root, so swapping a path component after
/// the policy check cannot reach outside it.
#[derive(Debug)]
pub struct Beneath {
    dir: Arc<Dir>,
    rel: PathBuf,
}

impl Beneath {
    pub fn read(&self) -> std::io::Result<Vec<u8>> {
        self.dir.read(&self.rel)
    }

    pub fn write(&self, data: &[u8]) -> std::io::Result<()> {
        self.dir.write(&self.rel, data)
    }
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
    /// No access at all (the default profile).
    pub fn strict() -> Self {
        Self::default()
    }

    /// Full fs read/write, every env var, any host, private IPs included.
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
            fs_read: open_roots(read)?,
            fs_write: open_roots(write)?,
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

    /// The SSRF deny set: addresses a script must not reach without
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

    /// Returns the canonicalized path that was checked.
    pub fn allows_read(&self, path: &Path) -> Result<PathBuf, PolicyError> {
        let resolved = resolve_existing(path)?;
        gate("read", &self.fs_read, resolved).map(|(resolved, _)| resolved)
    }

    /// A new file resolves through its parent. Returns the canonicalized path.
    pub fn allows_write(&self, path: &Path) -> Result<PathBuf, PolicyError> {
        let resolved = resolve_for_write(path)?;
        gate("write", &self.fs_write, resolved).map(|(resolved, _)| resolved)
    }

    /// Like [`allows_read`](Self::allows_read), but returns a handle that opens
    /// the file confined to the granting root (no check-then-open window).
    pub fn open_read(&self, path: &Path) -> Result<Beneath, PolicyError> {
        let resolved = resolve_existing(path)?;
        gate("read", &self.fs_read, resolved).map(|(resolved, root)| root.beneath(&resolved))
    }

    /// Write counterpart of [`open_read`](Self::open_read).
    pub fn open_write(&self, path: &Path) -> Result<Beneath, PolicyError> {
        let resolved = resolve_for_write(path)?;
        gate("write", &self.fs_write, resolved).map(|(resolved, root)| root.beneath(&resolved))
    }
}

/// `starts_with` is component-wise, so root `/a/b` does not match `/a/bc`.
fn gate<'a>(
    op: &'static str,
    roots: &'a [FsRoot],
    resolved: PathBuf,
) -> Result<(PathBuf, &'a FsRoot), PolicyError> {
    match roots.iter().find(|root| resolved.starts_with(&root.path)) {
        Some(root) => Ok((resolved, root)),
        None => Err(PolicyError::Denied { op, path: resolved }),
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

fn open_roots(roots: &[PathBuf]) -> std::io::Result<Vec<FsRoot>> {
    roots.iter().map(|r| FsRoot::open(r)).collect()
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
    // A dangling symlink lands here and is checked as the link itself; the
    // directory handle then refuses to follow it out of the root.
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
    fn write_through_dangling_symlink_out_of_root_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("escaped.txt");
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let p = Policy::from_roots(&[], &[root.path().to_path_buf()]).unwrap();
        let file = p.open_write(&link).unwrap();
        assert!(file.write(b"x").is_err());
        assert!(!target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn write_through_dangling_symlink_inside_root_is_allowed() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("made.txt");
        let link = root.path().join("link");
        std::os::unix::fs::symlink("made.txt", &link).unwrap();

        let p = Policy::from_roots(&[], &[root.path().to_path_buf()]).unwrap();
        p.open_write(&link).unwrap().write(b"x").unwrap();
        assert_eq!(std::fs::read(target).unwrap(), b"x");
    }

    /// The race the handle closes: a path that passed the check is swapped for a
    /// symlink out of the root before the open.
    #[cfg(unix)]
    #[test]
    fn symlink_swapped_in_after_the_check_cannot_escape() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), b"s").unwrap();
        let sub = root.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("f"), b"ok").unwrap();

        let p = Policy::from_roots(&[root.path().to_path_buf()], &[]).unwrap();
        let file = p.open_read(&sub.join("f")).unwrap(); // check passes here
        std::fs::remove_dir_all(&sub).unwrap();
        std::os::unix::fs::symlink(outside.path(), &sub).unwrap();
        std::fs::write(outside.path().join("f"), b"leaked").unwrap();

        assert!(file.read().is_err());
    }

    #[test]
    fn file_root_reaches_only_that_file() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();

        let p = Policy::from_roots(std::slice::from_ref(&a), &[]).unwrap();
        assert_eq!(p.open_read(&a).unwrap().read().unwrap(), b"a");
        assert!(matches!(p.open_read(&b), Err(PolicyError::Denied { .. })));
    }

    #[test]
    fn strict_profile_denies_by_default() {
        let p = Policy::strict();
        assert!(!p.allows_env("ANY_NAME"));
        assert!(!p.allows_net("example.com", 443));
        assert!(!p.allows_private_net());
    }
}
