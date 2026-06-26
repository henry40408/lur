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

use std::path::{Path, PathBuf};

use thiserror::Error;

/// A resolved capability policy.
#[derive(Clone, Debug, Default)]
pub struct Policy {
    /// Canonicalized readable roots.
    fs_read: Vec<PathBuf>,
    /// Canonicalized writable roots.
    fs_write: Vec<PathBuf>,
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
        })
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
