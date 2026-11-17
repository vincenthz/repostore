use std::path::{Path, PathBuf};
use std::process::Command;

use repostore::{AdjustOutcome, CloneOptions, Error, RepoId, Store};
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

struct Fixture {
    _tmp: TempDir,
    upstream: PathBuf,
    url: String,
    store: Store,
    work: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let upstream = root.join("upstream/project");
        std::fs::create_dir_all(&upstream).unwrap();
        git(&upstream, &["init", "--quiet", "-b", "master"]);
        let f = Self {
            url: upstream.to_string_lossy().into_owned(),
            upstream,
            store: Store::new(root.join("cache")),
            work: root.join("work"),
            _tmp: tmp,
        };
        f.commit("one");
        f
    }

    fn commit(&self, name: &str) -> String {
        std::fs::write(self.upstream.join(name), name).unwrap();
        git(&self.upstream, &["add", name]);
        git(&self.upstream, &["commit", "--quiet", "-m", name]);
        git(&self.upstream, &["rev-parse", "HEAD"])
    }

    fn id(&self) -> RepoId {
        RepoId::from_url(&self.url).unwrap()
    }
}

#[tokio::test]
async fn clone_adds_to_cache_and_points_to_upstream() {
    let f = Fixture::new();
    let dest = f.work.join("a");
    let repo = f
        .store
        .clone_repo(&f.url, &dest, &CloneOptions::default())
        .await
        .unwrap();

    assert_eq!(repo.path, f.store.root().join(f.id().cache_rel_path()));
    assert!(dest.join("one").exists());
    assert_eq!(git(&dest, &["remote", "get-url", "origin"]), f.url);
    assert_eq!(git(&dest, &["branch", "--show-current"]), "master");
    // Tracking refs match upstream branch names.
    git(&dest, &["rev-parse", "--verify", "origin/master"]);
    // Fetching from the real upstream works.
    git(&dest, &["fetch", "--quiet"]);
}

#[tokio::test]
async fn clone_of_cached_repo_fetches_first() {
    let f = Fixture::new();
    f.store.add(&f.url).await.unwrap();
    let head = f.commit("two");

    let dest = f.work.join("b");
    f.store
        .clone_repo(&f.url, &dest, &CloneOptions::default())
        .await
        .unwrap();
    assert_eq!(git(&dest, &["rev-parse", "HEAD"]), head);
}

#[tokio::test]
async fn offline_clone_uses_cache_as_is() {
    let f = Fixture::new();
    let first = git(&f.upstream, &["rev-parse", "HEAD"]);
    f.store.add(&f.url).await.unwrap();
    f.commit("two");

    let dest = f.work.join("c");
    let opts = CloneOptions::new().offline(true);
    f.store.clone_repo(&f.url, &dest, &opts).await.unwrap();
    assert_eq!(git(&dest, &["rev-parse", "HEAD"]), first);
}

#[tokio::test]
async fn clone_branch_and_refuse_non_empty_dest() {
    let f = Fixture::new();
    git(&f.upstream, &["branch", "feature"]);

    let dest = f.work.join("d");
    let opts = CloneOptions::new().branch("feature");
    f.store.clone_repo(&f.url, &dest, &opts).await.unwrap();
    assert_eq!(git(&dest, &["branch", "--show-current"]), "feature");

    let err = f
        .store
        .clone_repo(&f.url, &dest, &CloneOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::DestinationExists(_)), "{err}");
}

#[tokio::test]
async fn add_is_idempotent_and_failed_add_leaves_nothing() {
    let f = Fixture::new();
    assert!(f.store.add(&f.url).await.unwrap().created);
    assert!(!f.store.add(&f.url).await.unwrap().created);

    let missing = f.upstream.with_file_name("missing");
    assert!(f.store.add(missing.to_str().unwrap()).await.is_err());

    let listed = f.store.list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, f.id());
    // No leftover temp directories.
    let parent = listed[0].path.parent().unwrap();
    assert_eq!(std::fs::read_dir(parent).unwrap().count(), 1);
}

#[tokio::test]
async fn sync_prunes_and_follows_default_branch_rename() {
    let f = Fixture::new();
    git(&f.upstream, &["branch", "old"]);
    let repo = f.store.add(&f.url).await.unwrap().repo;

    git(&f.upstream, &["branch", "-D", "old"]);
    git(&f.upstream, &["branch", "-m", "master", "main"]);
    let head = f.commit("two");
    f.store.sync(&repo).await.unwrap();

    let refs = git(&repo.path, &["for-each-ref", "--format=%(refname)"]);
    assert_eq!(refs, "refs/heads/main");
    assert_eq!(
        git(&repo.path, &["symbolic-ref", "HEAD"]),
        "refs/heads/main"
    );

    let dest = f.work.join("e");
    f.store
        .clone_repo(&f.url, &dest, &CloneOptions::new().offline(true))
        .await
        .unwrap();
    assert_eq!(git(&dest, &["rev-parse", "HEAD"]), head);
}

#[tokio::test]
async fn sync_many_reports_each_repo() {
    use futures::StreamExt;
    let f = Fixture::new();
    f.store.add(&f.url).await.unwrap();
    let repos = f.store.list().await.unwrap();
    let results: Vec<_> = f.store.sync_many(repos, 4).collect().await;
    assert_eq!(results.len(), 1);
    assert!(results[0].1.is_ok());
}

#[tokio::test]
async fn stats_and_remove() {
    let f = Fixture::new();
    let repo = f.store.add(&f.url).await.unwrap().repo;

    let stats = f.store.stats(&repo).await.unwrap();
    assert_eq!(stats.upstream.as_deref(), Some(f.url.as_str()));
    assert_eq!(stats.refs, 1);
    assert!(stats.disk_size > 0);
    assert!(stats.last_sync.is_some());

    f.store.remove(&f.id()).await.unwrap();
    assert!(f.store.list().await.unwrap().is_empty());
    // Empty parent directories are pruned, the root is kept.
    assert_eq!(std::fs::read_dir(f.store.root()).unwrap().count(), 0);
    assert!(matches!(
        f.store.remove(&f.id()).await,
        Err(Error::NotCached(_))
    ));
}

#[tokio::test]
async fn adjust_remote_repoints_clones_of_the_cache() {
    let f = Fixture::new();
    let repo = f.store.add(&f.url).await.unwrap().repo;

    // A clone made directly from the cache points at the cache path.
    let dest = f.work.join("f");
    std::fs::create_dir_all(&f.work).unwrap();
    git(
        &f.work,
        &["clone", "--quiet", repo.path.to_str().unwrap(), "f"],
    );

    let out = f.store.adjust_remote(&dest, "origin").await.unwrap();
    assert!(
        matches!(out, AdjustOutcome::Updated { ref to, .. } if *to == f.url),
        "{out:?}"
    );
    assert_eq!(git(&dest, &["remote", "get-url", "origin"]), f.url);

    // Second run: already pointing at upstream.
    let out = f.store.adjust_remote(&dest, "origin").await.unwrap();
    assert_eq!(out, AdjustOutcome::Unchanged { url: f.url.clone() });

    assert!(matches!(
        f.store.adjust_remote(&dest, "nope").await,
        Err(Error::NoRemote { .. })
    ));
}
