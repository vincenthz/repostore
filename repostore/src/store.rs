use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::stream::{self, Stream, StreamExt};
use tokio::fs;

use crate::RepoId;
use crate::error::{Error, Result};
use crate::git::Git;

/// Environment variable overriding the default cache location.
pub const CACHE_DIR_ENV: &str = "REPOSTORE_DIR";

const LAST_SYNC_KEY: &str = "repostore.lastsync";
const UPSTREAM_REMOTE: &str = "origin";

/// A local cache of bare repositories.
///
/// `Store` is a cheap handle (just the cache root): clone it freely, share it
/// between tasks. All methods are async and their futures are `Send`.
///
/// Dropping a future cancels the operation: running `git` processes are
/// killed and partially created clones are removed (best effort).
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

/// A bare repository present in the cache.
///
/// Obtained from [`Store::get`], [`Store::list`], [`Store::add`]...
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CachedRepo {
    /// Identity of the repository.
    pub id: RepoId,
    /// Path of the bare repository.
    pub path: PathBuf,
}

/// Result of [`Store::add`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Added {
    /// The cached repository.
    pub repo: CachedRepo,
    /// `false` when the repository was already cached (nothing was fetched).
    pub created: bool,
}

/// Result of [`Store::stats`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RepoStats {
    /// Url the cache fetches from.
    pub upstream: Option<String>,
    /// Apparent size of the bare repository, in bytes.
    pub disk_size: u64,
    /// Number of refs (branches and tags).
    pub refs: usize,
    /// Last successful add or sync done through repostore.
    pub last_sync: Option<SystemTime>,
}

/// Options for [`Store::clone_repo`].
///
/// ```
/// let opts = repostore::CloneOptions::new().branch("dev").offline(true);
/// ```
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CloneOptions {
    /// Branch to check out instead of the remote default.
    pub branch: Option<String>,
    /// Do not fetch an already cached repository before cloning.
    pub offline: bool,
}

impl CloneOptions {
    /// Default options: check out the remote default branch, fetch first.
    pub fn new() -> Self {
        Self::default()
    }

    /// Check out `branch` instead of the remote default.
    pub fn branch(mut self, branch: impl Into<String>) -> Self {
        self.branch = Some(branch.into());
        self
    }

    /// Skip fetching an already cached repository before cloning.
    pub fn offline(mut self, offline: bool) -> Self {
        self.offline = offline;
        self
    }
}

/// Result of [`Store::adjust_remote`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AdjustOutcome {
    /// The remote pointed at the cache and now points at the upstream url.
    Updated {
        /// Previous url (a path into the cache).
        from: String,
        /// Upstream url now configured.
        to: String,
    },
    /// The remote does not point into the cache; left as is.
    Unchanged {
        /// Current url of the remote.
        url: String,
    },
}

impl Store {
    /// Store rooted at `root`. Nothing is created until a repository is added.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Store at `$REPOSTORE_DIR`, or `<user cache dir>/repostore`.
    pub fn from_env() -> Result<Self> {
        if let Some(dir) = std::env::var_os(CACHE_DIR_ENV).filter(|d| !d.is_empty()) {
            return Ok(Self::new(dir));
        }
        dirs::cache_dir()
            .map(|d| Self::new(d.join("repostore")))
            .ok_or(Error::NoCacheDir)
    }

    /// Cache root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the bare repository for `id` lives (whether or not it exists).
    pub fn repo_path(&self, id: &RepoId) -> PathBuf {
        self.root.join(id.cache_rel_path())
    }

    /// The cached repository for `id`, if present.
    pub async fn get(&self, id: &RepoId) -> Result<Option<CachedRepo>> {
        let path = self.repo_path(id);
        Ok(is_bare_repo(&path).await.then(|| CachedRepo {
            id: id.clone(),
            path,
        }))
    }

    /// Like [`Store::get`] but fails with [`Error::NotCached`].
    pub async fn require(&self, id: &RepoId) -> Result<CachedRepo> {
        self.get(id)
            .await?
            .ok_or_else(|| Error::NotCached(id.clone()))
    }

    /// Clone `url` as a bare repository into the cache, unless already present.
    pub async fn add(&self, url: &str) -> Result<Added> {
        let id = RepoId::from_url(url)?;
        if let Some(repo) = self.get(&id).await? {
            return Ok(Added {
                repo,
                created: false,
            });
        }

        let path = self.repo_path(&id);
        let parent = path.parent().expect("cache path has a parent");
        fs::create_dir_all(parent).await?;

        // Clone next to the final location and rename into place, so an
        // interrupted clone never leaves a half-populated cache entry.
        let tmp = tmp_path(&path);
        let cleanup = Cleanup::new(tmp.clone(), false);
        let res = async {
            Git::new()
                .args(["clone", "--bare", "--quiet", "--"])
                .arg(url)
                .arg(&tmp)
                .run()
                .await?;
            configure_cache_repo(&tmp).await?;
            fs::rename(&tmp, &path).await.map_err(Error::from)
        }
        .await;

        let repo = CachedRepo { id, path };
        match res {
            Ok(()) => {
                cleanup.disarm();
                Ok(Added {
                    repo,
                    created: true,
                })
            }
            Err(e) => {
                cleanup.remove().await;
                if matches!(e, Error::Io(_)) && is_bare_repo(&repo.path).await {
                    // Lost a race with a concurrent add of the same repository.
                    Ok(Added {
                        repo,
                        created: false,
                    })
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Fetch the cached repository from its upstream.
    pub async fn sync(&self, repo: &CachedRepo) -> Result<()> {
        Git::bare(&repo.path)
            .args(["fetch", "--prune", "--quiet", UPSTREAM_REMOTE])
            .run()
            .await?;
        fix_head(&repo.path).await?;
        record_sync(&repo.path).await
    }

    /// Sync several repositories, at most `jobs` at a time, yielding results
    /// as they complete.
    pub fn sync_many(
        &self,
        repos: Vec<CachedRepo>,
        jobs: usize,
    ) -> impl Stream<Item = (CachedRepo, Result<()>)> + '_ {
        stream::iter(repos)
            .map(move |repo| async move {
                let res = self.sync(&repo).await;
                (repo, res)
            })
            .buffer_unordered(jobs.max(1))
    }

    /// Sync every cached repository, at most `jobs` at a time.
    ///
    /// Fails only if the cache cannot be listed; per-repository results are
    /// returned sorted by id.
    pub async fn sync_all(&self, jobs: usize) -> Result<Vec<(CachedRepo, Result<()>)>> {
        let repos = self.list().await?;
        let mut results: Vec<_> = self.sync_many(repos, jobs).collect().await;
        results.sort_by(|a, b| a.0.id.cmp(&b.0.id));
        Ok(results)
    }

    /// Delete a repository from the cache.
    pub async fn remove(&self, id: &RepoId) -> Result<()> {
        let repo = self.require(id).await?;
        fs::remove_dir_all(&repo.path).await?;
        // Prune now-empty parent directories, up to the root.
        let mut dir = repo.path.parent();
        while let Some(d) = dir {
            if d == self.root || fs::remove_dir(d).await.is_err() {
                break;
            }
            dir = d.parent();
        }
        Ok(())
    }

    /// All cached repositories, sorted by id.
    pub async fn list(&self) -> Result<Vec<CachedRepo>> {
        let mut repos = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            let mut entries = match fs::read_dir(&dir).await {
                Ok(e) => e,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            while let Some(entry) = entries.next_entry().await? {
                if !entry.file_type().await?.is_dir() {
                    continue;
                }
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if is_tmp_name(name) {
                    continue;
                }
                let path = entry.path();
                if name.ends_with(".git") && is_bare_repo(&path).await {
                    let rel = path.strip_prefix(&self.root).expect("under root");
                    if let Some(id) = RepoId::from_cache_rel_path(rel) {
                        repos.push(CachedRepo { id, path });
                    }
                } else {
                    stack.push(path);
                }
            }
        }
        repos.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(repos)
    }

    /// Size, ref count and last sync time of a cached repository.
    pub async fn stats(&self, repo: &CachedRepo) -> Result<RepoStats> {
        let upstream = repo.upstream().await?;
        let refs = Git::bare(&repo.path)
            .args(["for-each-ref", "--format=%(refname)"])
            .run()
            .await?
            .lines()
            .count();
        let last_sync = Git::bare(&repo.path)
            .config_get(LAST_SYNC_KEY)
            .await?
            .and_then(|v| v.parse().ok())
            .map(|secs| UNIX_EPOCH + Duration::from_secs(secs));
        let path = repo.path.clone();
        let disk_size = tokio::task::spawn_blocking(move || dir_size(&path))
            .await
            .expect("dir_size panicked")?;
        Ok(RepoStats {
            upstream,
            disk_size,
            refs,
            last_sync,
        })
    }

    /// Clone `url` into `dest` through the cache.
    ///
    /// The repository is added to the cache if missing, or fetched first if
    /// already present (unless [`CloneOptions::offline`]). The working clone
    /// is then made from the cache, and its `origin` remote set to `url`.
    ///
    /// `dest` must be missing or an empty directory. On failure or
    /// cancellation, whatever was created in `dest` is removed.
    pub async fn clone_repo(
        &self,
        url: &str,
        dest: impl AsRef<Path>,
        opts: &CloneOptions,
    ) -> Result<CachedRepo> {
        let dest = dest.as_ref();
        let existed = match fs::read_dir(dest).await {
            Ok(mut entries) => {
                if entries.next_entry().await?.is_some() {
                    return Err(Error::DestinationExists(dest.to_owned()));
                }
                true
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.into()),
        };
        let id = RepoId::from_url(url)?;
        let repo = match self.get(&id).await? {
            Some(repo) => {
                if !opts.offline {
                    self.sync(&repo).await?;
                }
                repo
            }
            None => self.add(url).await?.repo,
        };

        let cleanup = Cleanup::new(dest.to_owned(), existed);
        let res = async {
            let mut clone = Git::new().args(["clone", "--quiet"]);
            if let Some(branch) = &opts.branch {
                clone = clone.arg("--branch").arg(branch);
            }
            clone.arg("--").arg(&repo.path).arg(dest).run().await?;
            Git::work(dest)
                .args(["remote", "set-url", UPSTREAM_REMOTE])
                .arg(url)
                .run()
                .await
        }
        .await;
        match res {
            Ok(_) => {
                cleanup.disarm();
                Ok(repo)
            }
            Err(e) => {
                cleanup.remove().await;
                Err(e)
            }
        }
    }

    /// If `remote` of the working clone at `worktree` points into the cache,
    /// re-point it to the upstream url of that cached repository.
    pub async fn adjust_remote(
        &self,
        worktree: impl AsRef<Path>,
        remote: &str,
    ) -> Result<AdjustOutcome> {
        let worktree = worktree.as_ref();
        let current = Git::work(worktree)
            .config_get(&format!("remote.{remote}.url"))
            .await?
            .ok_or_else(|| Error::NoRemote {
                path: worktree.to_owned(),
                remote: remote.to_owned(),
            })?;

        let Some(repo) = self.find_by_local_url(&current).await? else {
            return Ok(AdjustOutcome::Unchanged { url: current });
        };
        let upstream = repo.upstream().await?.ok_or_else(|| Error::NoRemote {
            path: repo.path.clone(),
            remote: UPSTREAM_REMOTE.to_owned(),
        })?;
        Git::work(worktree)
            .args(["remote", "set-url", remote])
            .arg(&upstream)
            .run()
            .await?;
        Ok(AdjustOutcome::Updated {
            from: current,
            to: upstream,
        })
    }

    /// Map a local path / `file://` url to the cached repository it points to.
    async fn find_by_local_url(&self, url: &str) -> Result<Option<CachedRepo>> {
        let path = Path::new(url.strip_prefix("file://").unwrap_or(url));
        if !path.is_absolute() {
            return Ok(None);
        }
        let (Ok(path), Ok(root)) = (
            fs::canonicalize(path).await,
            fs::canonicalize(&self.root).await,
        ) else {
            return Ok(None);
        };
        let Some(id) = path
            .strip_prefix(&root)
            .ok()
            .and_then(RepoId::from_cache_rel_path)
        else {
            return Ok(None);
        };
        self.get(&id).await
    }
}

impl CachedRepo {
    /// Upstream url the cache fetches from.
    pub async fn upstream(&self) -> Result<Option<String>> {
        Git::bare(&self.path)
            .config_get(&format!("remote.{UPSTREAM_REMOTE}.url"))
            .await
    }
}

async fn configure_cache_repo(git_dir: &Path) -> Result<()> {
    // `clone --bare` sets no fetch refspec; mirror branches and tags so that
    // `git fetch` keeps them up to date (but skip e.g. refs/pull/*).
    Git::bare(git_dir)
        .args([
            "config",
            "--replace-all",
            "remote.origin.fetch",
            "+refs/heads/*:refs/heads/*",
        ])
        .run()
        .await?;
    Git::bare(git_dir)
        .args([
            "config",
            "--add",
            "remote.origin.fetch",
            "+refs/tags/*:refs/tags/*",
        ])
        .run()
        .await?;
    record_sync(git_dir).await
}

/// `fetch` never updates HEAD of a bare repository. If HEAD now points to a
/// missing branch (e.g. upstream renamed `master` to `main` and we pruned),
/// re-point it at the upstream default branch so clones get a checkout.
async fn fix_head(git_dir: &Path) -> Result<()> {
    if let Ok(head) = Git::bare(git_dir)
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .run()
        .await
    {
        let valid = Git::bare(git_dir)
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("{}^{{commit}}", head.trim()))
            .run()
            .await
            .is_ok();
        if valid {
            return Ok(());
        }
    }
    let out = Git::bare(git_dir)
        .args(["ls-remote", "--symref", UPSTREAM_REMOTE, "HEAD"])
        .run()
        .await?;
    let target = out.lines().find_map(|l| {
        let (target, name) = l.strip_prefix("ref: ")?.split_once('\t')?;
        (name == "HEAD").then_some(target)
    });
    if let Some(target) = target {
        Git::bare(git_dir)
            .args(["symbolic-ref", "HEAD", target])
            .run()
            .await?;
    }
    Ok(())
}

async fn record_sync(git_dir: &Path) -> Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Git::bare(git_dir)
        .args(["config", LAST_SYNC_KEY, &now.to_string()])
        .run()
        .await?;
    Ok(())
}

async fn is_bare_repo(path: &Path) -> bool {
    fs::metadata(path.join("HEAD")).await.is_ok()
        && fs::metadata(path.join("objects")).await.is_ok()
}

/// Removes a path created by an operation unless disarmed.
///
/// Error paths call [`Cleanup::remove`] and wait for it. If the operation's
/// future is dropped (cancelled) instead, removal happens from `Drop`, on the
/// blocking pool when inside a Tokio runtime.
struct Cleanup {
    path: Option<PathBuf>,
    /// Empty the directory but keep it (it existed before the operation).
    keep_dir: bool,
}

impl Cleanup {
    fn new(path: PathBuf, keep_dir: bool) -> Self {
        Self {
            path: Some(path),
            keep_dir,
        }
    }

    fn disarm(mut self) {
        self.path = None;
    }

    async fn remove(mut self) {
        if let Some(path) = self.path.take() {
            let keep_dir = self.keep_dir;
            let _ = tokio::task::spawn_blocking(move || remove_path(&path, keep_dir)).await;
        }
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let Some(path) = self.path.take() else { return };
        let keep_dir = self.keep_dir;
        let work = move || {
            let _ = remove_path(&path, keep_dir);
        };
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => drop(rt.spawn_blocking(work)),
            Err(_) => work(),
        }
    }
}

fn remove_path(path: &Path, keep_dir: bool) -> std::io::Result<()> {
    if !keep_dir {
        return match std::fs::remove_dir_all(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            r => r,
        };
    }
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(entry.path())?;
        } else {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

const TMP_MARKER: &str = ".git.repostore-tmp-";

/// Hidden sibling path for an in-progress clone, skipped by [`Store::list`].
fn tmp_path(path: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let name = path.file_stem().expect("has file name").to_string_lossy();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(".{name}{TMP_MARKER}{}-{n}", std::process::id()))
}

fn is_tmp_name(name: &str) -> bool {
    name.starts_with('.') && name.contains(TMP_MARKER)
}

fn dir_size(path: &Path) -> std::io::Result<u64> {
    let mut total = 0;
    let mut stack = vec![path.to_owned()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                stack.push(entry.path());
            } else {
                total += entry.metadata()?.len();
            }
        }
    }
    Ok(total)
}
