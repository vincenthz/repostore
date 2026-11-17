use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::SystemTime;

use anyhow::Result;
use clap::{Parser, Subcommand};
use futures::StreamExt;
use repostore::{AdjustOutcome, CachedRepo, CloneOptions, RepoId, Store};

/// Keep a local cache of git repositories and clone from it.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Cache directory [default: $REPOSTORE_DIR or <user cache dir>/repostore]
    #[arg(long, global = true, value_name = "DIR")]
    cache_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Clone a repository through the cache (adding or fetching it first)
    Clone {
        url: String,
        /// Destination directory [default: repository name]
        dest: Option<PathBuf>,
        /// Check out this branch instead of the remote default
        #[arg(short, long)]
        branch: Option<String>,
        /// Don't fetch an already cached repository before cloning
        #[arg(long)]
        offline: bool,
    },
    /// Add repositories to the cache
    Add {
        #[arg(required = true)]
        urls: Vec<String>,
    },
    /// Remove repositories from the cache
    #[command(alias = "rm")]
    Remove {
        /// Repository url or id (host/path)
        #[arg(required = true)]
        repos: Vec<String>,
    },
    /// Fetch cached repositories from their upstream (all if none given)
    Sync {
        /// Repository url or id (host/path)
        repos: Vec<String>,
        /// Number of concurrent fetches
        #[arg(short, long, default_value_t = 4)]
        jobs: usize,
    },
    /// List cached repositories
    #[command(alias = "ls")]
    List,
    /// Show size, ref count and last sync of cached repositories
    Stats {
        /// Repository url or id (host/path) [default: all]
        repos: Vec<String>,
    },
    /// Point a working clone's remote back at the upstream if it targets the cache
    AdjustRemote {
        /// Working clone
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(short, long, default_value = "origin")]
        remote: String,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<ExitCode> {
    let store = match cli.cache_dir {
        Some(dir) => Store::new(dir),
        None => Store::from_env()?,
    };

    match cli.command {
        Command::Clone {
            url,
            dest,
            branch,
            offline,
        } => {
            let url = resolve_url(&url);
            let id = RepoId::from_url(&url)?;
            let dest = dest.unwrap_or_else(|| PathBuf::from(id.name()));
            let mut opts = CloneOptions::new().offline(offline);
            if let Some(branch) = branch {
                opts = opts.branch(branch);
            }
            store.clone_repo(&url, &dest, &opts).await?;
            eprintln!("cloned {id} into {}", dest.display());
        }
        Command::Add { urls } => {
            let mut failed = false;
            for url in urls {
                let url = resolve_url(&url);
                match store.add(&url).await {
                    Ok(a) if a.created => println!("added {}", a.repo.id),
                    Ok(a) => println!("already cached {}", a.repo.id),
                    Err(e) => {
                        failed = true;
                        eprintln!("error: {url}: {e}");
                    }
                }
            }
            return Ok(exit_code(failed));
        }
        Command::Remove { repos } => {
            for r in repos {
                let id = RepoId::parse(&r)?;
                store.remove(&id).await?;
                println!("removed {id}");
            }
        }
        Command::Sync { repos, jobs } => {
            let repos = select(&store, &repos).await?;
            let mut failed = false;
            let mut results = store.sync_many(repos, jobs);
            while let Some((repo, res)) = results.next().await {
                match res {
                    Ok(()) => println!("synced {}", repo.id),
                    Err(e) => {
                        failed = true;
                        eprintln!("error: {}: {e}", repo.id);
                    }
                }
            }
            return Ok(exit_code(failed));
        }
        Command::List => {
            for repo in store.list().await? {
                let upstream = repo.upstream().await?.unwrap_or_default();
                println!("{}\t{upstream}", repo.id);
            }
        }
        Command::Stats { repos } => {
            let repos = select(&store, &repos).await?;
            let mut rows = Vec::new();
            let mut total = 0;
            for repo in &repos {
                let s = store.stats(repo).await?;
                total += s.disk_size;
                rows.push([
                    repo.id.to_string(),
                    human_size(s.disk_size),
                    s.refs.to_string(),
                    s.last_sync.map_or_else(|| "never".into(), age),
                ]);
            }
            print_table(["REPOSITORY", "SIZE", "REFS", "SYNCED"], &rows);
            println!(
                "\n{} repositories, {} in {}",
                repos.len(),
                human_size(total),
                store.root().display()
            );
        }
        Command::AdjustRemote { path, remote } => {
            match store.adjust_remote(&path, &remote).await? {
                AdjustOutcome::Updated { from, to } => println!("{remote}: {from} -> {to}"),
                AdjustOutcome::Unchanged { url } => {
                    println!("{remote}: {url} (not pointing to the cache, unchanged)")
                }
                other => println!("{remote}: {other:?}"),
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Selected repositories, or all of them when none is given.
async fn select(store: &Store, args: &[String]) -> Result<Vec<CachedRepo>> {
    if args.is_empty() {
        return Ok(store.list().await?);
    }
    let mut repos = Vec::new();
    for a in args {
        let id = RepoId::parse(a)?;
        repos.push(store.require(&id).await?);
    }
    Ok(repos)
}

/// Turn an existing relative local path into an absolute one, so the cache
/// records a usable upstream.
fn resolve_url(arg: &str) -> String {
    if !arg.contains("://")
        && Path::new(arg).exists()
        && let Ok(p) = std::fs::canonicalize(arg)
    {
        return p.to_string_lossy().into_owned();
    }
    arg.to_owned()
}

fn exit_code(failed: bool) -> ExitCode {
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn age(t: SystemTime) -> String {
    let secs = SystemTime::now()
        .duration_since(t)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

fn print_table<const N: usize>(header: [&str; N], rows: &[[String; N]]) {
    let mut widths = header.map(str::len);
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.len());
        }
    }
    let line = |cells: [&str; N]| {
        let mut out = String::new();
        for (i, (cell, w)) in cells.iter().zip(widths).enumerate() {
            // Left-align the first column, right-align numbers.
            if i == 0 {
                out.push_str(&format!("{cell:<w$}"));
            } else {
                out.push_str(&format!("  {cell:>w$}"));
            }
        }
        println!("{}", out.trim_end());
    };
    line(header);
    for row in rows {
        line(row.each_ref().map(String::as_str));
    }
}
