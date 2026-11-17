//! Checks of the public API as seen by a dependent crate.

use std::time::Duration;

use repostore::{CachedRepo, CloneOptions, RepoId, Store};

fn assert_send<T: Send>(_: &T) {}
fn assert_send_sync<T: Send + Sync + 'static>() {}

/// Never called: type-checks that every operation's future is `Send`, so
/// applications can `tokio::spawn` them.
#[allow(dead_code)]
fn futures_are_send(store: Store, repo: CachedRepo, id: RepoId) {
    assert_send_sync::<Store>();
    assert_send_sync::<CachedRepo>();
    assert_send_sync::<RepoId>();
    assert_send_sync::<repostore::Error>();

    assert_send(&store.add("x"));
    assert_send(&store.get(&id));
    assert_send(&store.require(&id));
    assert_send(&store.sync(&repo));
    assert_send(&store.sync_many(vec![repo.clone()], 2));
    assert_send(&store.sync_all(2));
    assert_send(&store.remove(&id));
    assert_send(&store.list());
    assert_send(&store.stats(&repo));
    assert_send(&repo.upstream());
    assert_send(&store.clone_repo("x", "dest", &CloneOptions::new()));
    assert_send(&store.adjust_remote(".", "origin"));
}

#[tokio::test]
async fn spawnable() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = Store::new(tmp.path());
    let listed = tokio::spawn(async move { store.list().await })
        .await
        .unwrap();
    assert!(listed.unwrap().is_empty());
}

/// Dropping an `add` mid-clone kills git and removes the partial clone.
#[tokio::test]
async fn cancelled_add_leaves_nothing() {
    // A git:// "server" that accepts connections and never answers.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let _server = tokio::spawn(async move {
        let mut conns = Vec::new();
        while let Ok((c, _)) = listener.accept().await {
            conns.push(c);
        }
    });

    let tmp = tempfile::TempDir::new().unwrap();
    let store = Store::new(tmp.path());
    let url = format!("git://127.0.0.1:{port}/hang");
    let parent = store
        .repo_path(&RepoId::from_url(&url).unwrap())
        .parent()
        .unwrap()
        .to_owned();
    let entries = || std::fs::read_dir(&parent).map_or(0, |d| d.count());

    tokio::select! {
        res = store.add(&url) => panic!("add finished: {res:?}"),
        _ = async {
            while entries() == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        } => {}
    }
    // The add future is dropped here; cleanup runs on the blocking pool.
    for _ in 0..500 {
        if entries() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(entries(), 0, "partial clone left in {}", parent.display());
    assert!(store.list().await.unwrap().is_empty());
}
