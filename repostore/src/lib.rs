//! Local cache of git repositories.
//!
//! A [`Store`] keeps bare clones of repositories under a cache directory,
//! keyed by a normalized [`RepoId`] (`host/path`). Working clones are made
//! from the cache, after fetching it, and their `origin` is then pointed at
//! the real upstream. Cloning a repository you already have only transfers
//! the new objects over the network.
//!
//! ```no_run
//! use repostore::{CloneOptions, Store};
//!
//! # async fn demo() -> repostore::Result<()> {
//! let store = Store::from_env()?; // or Store::new("/path/to/cache")
//!
//! // Adds github.com/rust-lang/log to the cache (or fetches it), then
//! // clones ./log from it with origin = the url below.
//! store
//!     .clone_repo("https://github.com/rust-lang/log", "log", &CloneOptions::new())
//!     .await?;
//!
//! // Refresh everything, 8 fetches at a time.
//! for (repo, res) in store.sync_all(8).await? {
//!     if let Err(e) = res {
//!         eprintln!("{}: {e}", repo.id);
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Runtime
//!
//! Operations are async and need a Tokio runtime with I/O enabled (the
//! default for `#[tokio::main]`, or `Builder::enable_all`). Either flavor
//! works; all futures are `Send`, so they can be passed to `tokio::spawn`.
//!
//! # Git
//!
//! Git operations run the `git` executable (which must be on `PATH`), so the
//! user's ssh configuration and credential helpers apply exactly as with
//! plain git. Nothing is printed: git's output is captured and failures are
//! reported as [`Error::Git`] with git's stderr.
//!
//! # Cancellation
//!
//! Dropping a future stops the operation: the running `git` process is killed
//! and a partially created cache entry or working clone is removed.

#![warn(missing_docs)]

mod error;
mod git;
mod id;
mod store;

pub use error::{Error, Result};
pub use id::{LOCAL_HOST, RepoId};
pub use store::{Added, AdjustOutcome, CACHE_DIR_ENV, CachedRepo, CloneOptions, RepoStats, Store};
