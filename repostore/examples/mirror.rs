//! Keep a set of repositories mirrored in a cache, using only the library.
//!
//! ```sh
//! cargo run -p repostore --example mirror -- /tmp/cache https://github.com/rust-lang/log
//! ```

use repostore::Store;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let root = args.next().ok_or("usage: mirror <cache-dir> <url>...")?;
    let store = Store::new(root);

    for url in args {
        let added = store.add(&url).await?;
        if added.created {
            println!("added  {}", added.repo.id);
        }
    }

    for (repo, res) in store.sync_all(4).await? {
        match res {
            Ok(()) => {
                let stats = store.stats(&repo).await?;
                println!(
                    "synced {} ({} refs, {} bytes)",
                    repo.id, stats.refs, stats.disk_size
                );
            }
            Err(e) => eprintln!("failed {}: {e}", repo.id),
        }
    }
    Ok(())
}
