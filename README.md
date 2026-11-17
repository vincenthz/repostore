# repostore

repostore allow to clone repository locally and maintain the cache. it clones
the bare repository in a local cache directory and clone the local repository
on demand from this local cache adjusting the local clone to use the proper
remote.

when cloning a project that is already present, a fetch is done first to the local
cache and then it clone it as usual.

## Library

* async function that allow to maintain git clones, clone a new repository using the cache strategy, sync

The workspace has two crates:

* `repostore/`: the library. Depends only on tokio, futures, thiserror and
  dirs; it doesn't pick a runtime flavor and never prints.
* `repostore-cli/`: the `repostore` binary, a thin layer over the library.

```toml
[dependencies]
repostore = { path = "../repostore/repostore" }
```

```rust
use repostore::{CloneOptions, Store};

let store = Store::from_env()?; // or Store::new("/path/to/cache")
store.clone_repo("https://github.com/rust-lang/log", "log", &CloneOptions::new()).await?;
for (repo, res) in store.sync_all(8).await? { /* ... */ }
```

All futures are `Send` (they can go into `tokio::spawn`). Dropping one cancels
the operation, kills git and removes the partial clone. See
`repostore/examples/mirror.rs` and `cargo doc -p repostore --open`.

## Binary

* list and stats for repositories cache
* sync all repositories or individual repository
* add / remove repo
* adjust remote

## Design

* Rust + tokio; every git operation shells out to the `git` executable, so ssh
  config and credential helpers behave exactly as with plain git.
* Cache location: `--cache-dir`, else `$REPOSTORE_DIR`, else
  `<user cache dir>/repostore` (`~/Library/Caches/repostore` on macOS,
  `~/.cache/repostore` on Linux).
* Repositories are keyed by normalized `host/path`, so
  `https://github.com/a/b.git`, `ssh://git@github.com/a/b` and
  `git@github.com:a/b` share the entry `<cache>/github.com/a/b.git`.
  Local paths / `file://` urls are stored under `local/`.
* Cache entries are bare clones fetching `refs/heads/*` and `refs/tags/*`
  (with pruning). If the upstream default branch changes, the cache `HEAD`
  follows it.
* Working clones are full, independent local clones of the cache (objects are
  hardlinked when on the same filesystem). Their `origin` is then set to the
  url given on the command line.

## Usage

```
repostore clone <url> [dest] [-b branch] [--offline]
repostore add <url>...
repostore remove <repo>...         # alias: rm
repostore sync [repo...] [-j jobs] # all repositories when none given
repostore list                     # alias: ls
repostore stats [repo...]
repostore adjust-remote [path] [-r remote]
```

`<repo>` is either a url or an id as shown by `list` (e.g. `github.com/a/b`).

## License

Licensed under either of

* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
* MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
