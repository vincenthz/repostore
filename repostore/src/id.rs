//! Normalized repository identity.
//!
//! Different spellings of the same remote map to the same [`RepoId`], so a
//! repository is cached only once:
//!
//! * `https://github.com/owner/repo.git`
//! * `ssh://git@github.com/owner/repo`
//! * `git@github.com:owner/repo.git`
//!
//! all become `github.com/owner/repo`, stored at `<cache>/github.com/owner/repo.git`.
//! Local repositories (`/abs/path` or `file:///abs/path`) live under the
//! pseudo host `local`.

use std::fmt;
use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};

/// Host used for repositories referenced by a local path or `file://` url.
pub const LOCAL_HOST: &str = "local";

/// Normalized identity of a repository, displayed as `host/path`.
///
/// ```
/// use repostore::RepoId;
///
/// let a = RepoId::from_url("git@github.com:owner/repo.git")?;
/// let b: RepoId = "https://github.com/owner/repo".parse()?;
/// assert_eq!(a, b);
/// assert_eq!(a.to_string(), "github.com/owner/repo");
/// # Ok::<(), repostore::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RepoId {
    host: String,
    path: String,
}

impl RepoId {
    /// Derive the identity of a repository from its remote url.
    pub fn from_url(url: &str) -> Result<Self> {
        let invalid = || Error::InvalidUrl(url.to_owned());
        let url = url.trim();

        let (host, path) = if let Some((scheme, rest)) = url.split_once("://") {
            match scheme.to_ascii_lowercase().as_str() {
                "file" => (LOCAL_HOST.to_owned(), rest),
                "http" | "https" | "ssh" | "git" | "git+ssh" | "ssh+git" => {
                    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
                    (normalize_host(authority).ok_or_else(invalid)?, path)
                }
                _ => return Err(invalid()),
            }
        } else if url.starts_with('/') {
            (LOCAL_HOST.to_owned(), url)
        } else if let Some((authority, path)) = url.split_once(':') {
            // scp-like syntax: `[user@]host:path`. Like git, a `/` before the
            // first `:` means it is a path, not a host.
            if authority.contains('/') {
                return Err(invalid());
            }
            (normalize_host(authority).ok_or_else(invalid)?, path)
        } else {
            return Err(invalid());
        };

        let path = normalize_path(path).ok_or_else(invalid)?;
        Ok(Self { host, path })
    }

    /// Parse either a url (see [`RepoId::from_url`]) or an id in its display
    /// form `host/path` (e.g. `github.com/owner/repo`).
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        if s.contains(':') || s.starts_with('/') {
            return Self::from_url(s);
        }
        let invalid = || Error::InvalidId(s.to_owned());
        let (host, path) = s.split_once('/').ok_or_else(invalid)?;
        if host.is_empty() || host.starts_with('.') {
            return Err(invalid());
        }
        Ok(Self {
            host: host.to_ascii_lowercase(),
            path: normalize_path(path).ok_or_else(invalid)?,
        })
    }

    /// Rebuild an id from a path relative to the cache root
    /// (`host/owner/repo.git`).
    pub(crate) fn from_cache_rel_path(rel: &Path) -> Option<Self> {
        let mut parts = Vec::new();
        for c in rel.components() {
            match c {
                Component::Normal(s) => parts.push(s.to_str()?),
                _ => return None,
            }
        }
        let (host, rest) = parts.split_first()?;
        let (last, dirs) = rest.split_last()?;
        let last = last.strip_suffix(".git")?;
        let mut path = dirs.join("/");
        if !path.is_empty() {
            path.push('/');
        }
        path.push_str(last);
        Some(Self {
            host: (*host).to_owned(),
            path: normalize_path(&path)?,
        })
    }

    /// Normalized host (lowercase; `host_port` when a port is given;
    /// [`LOCAL_HOST`] for local repositories).
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Repository path on the host, without `.git` suffix (e.g. `owner/repo`).
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Last path component, the conventional directory name for a clone.
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// Location of the bare repository relative to the cache root.
    pub fn cache_rel_path(&self) -> PathBuf {
        let mut p = PathBuf::from(&self.host);
        p.push(format!("{}.git", self.path));
        p
    }
}

impl std::str::FromStr for RepoId {
    type Err = Error;

    /// Same as [`RepoId::parse`].
    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

impl fmt::Display for RepoId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.host, self.path)
    }
}

/// Strip user info, lowercase, and make ports filesystem friendly.
fn normalize_host(authority: &str) -> Option<String> {
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = host.trim_start_matches('[').replace("]", "");
    let host = host.to_ascii_lowercase().replace(':', "_");
    let ok = !host.is_empty()
        && !host.starts_with('.')
        && host != LOCAL_HOST
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    ok.then_some(host)
}

/// Collapse slashes, drop a trailing `.git`, reject anything that could
/// escape the cache directory.
fn normalize_path(path: &str) -> Option<String> {
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty()
        || parts
            .iter()
            .any(|p| *p == "." || *p == ".." || p.contains('\\'))
    {
        return None;
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(url: &str) -> String {
        RepoId::from_url(url).unwrap().to_string()
    }

    #[test]
    fn equivalent_urls_share_an_id() {
        for url in [
            "https://github.com/owner/repo",
            "https://github.com/owner/repo.git",
            "https://user:token@GitHub.com/owner/repo/",
            "ssh://git@github.com/owner/repo.git",
            "git@github.com:owner/repo.git",
            "github.com:owner/repo",
            "git://github.com/owner/repo",
        ] {
            assert_eq!(id(url), "github.com/owner/repo", "{url}");
        }
    }

    #[test]
    fn nested_paths_and_ports() {
        assert_eq!(
            id("https://gitlab.com/group/sub/repo.git"),
            "gitlab.com/group/sub/repo"
        );
        assert_eq!(
            id("ssh://git@example.com:2222/repo"),
            "example.com_2222/repo"
        );
    }

    #[test]
    fn dot_prefixed_components_are_allowed() {
        assert_eq!(
            id("https://github.com/owner/.github"),
            "github.com/owner/.github"
        );
    }

    #[test]
    fn local_paths() {
        assert_eq!(id("/srv/git/project.git"), "local/srv/git/project");
        assert_eq!(id("file:///srv/git/project"), "local/srv/git/project");
    }

    #[test]
    fn rejects_bad_urls() {
        for url in [
            "",
            "relative/path",
            "ftp://host/repo",
            "https://host/",
            "https://host/../etc",
            "./repo:thing",
            "https://local/repo",
        ] {
            assert!(RepoId::from_url(url).is_err(), "{url}");
        }
    }

    #[test]
    fn parse_accepts_ids_and_urls() {
        let a = RepoId::parse("github.com/owner/repo").unwrap();
        let b = RepoId::parse("git@github.com:owner/repo").unwrap();
        assert_eq!(a, b);
        assert!(RepoId::parse("repo").is_err());
    }

    #[test]
    fn cache_path_roundtrip() {
        let a = RepoId::from_url("https://gitlab.com/group/sub/repo").unwrap();
        assert_eq!(
            a.cache_rel_path(),
            PathBuf::from("gitlab.com/group/sub/repo.git")
        );
        assert_eq!(RepoId::from_cache_rel_path(&a.cache_rel_path()).unwrap(), a);
        assert_eq!(a.name(), "repo");
    }
}
