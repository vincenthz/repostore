//! Thin async wrapper over the `git` command line.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;

use tokio::process::Command;

use crate::error::{Error, Result};

pub(crate) struct Git {
    cmd: Command,
    display: Vec<String>,
}

impl Git {
    pub fn new() -> Self {
        let mut cmd = Command::new("git");
        cmd.stdin(Stdio::null()).kill_on_drop(true);
        Self {
            cmd,
            display: Vec::new(),
        }
    }

    /// Operate on a bare repository. Uses an explicit `--git-dir` so it works
    /// even with `safe.bareRepository=explicit`.
    pub fn bare(git_dir: &Path) -> Self {
        Self::new().arg("--git-dir").arg(git_dir)
    }

    /// Operate on a working tree.
    pub fn work(dir: &Path) -> Self {
        Self::new().arg("-C").arg(dir)
    }

    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        let arg = arg.as_ref();
        self.display.push(arg.to_string_lossy().into_owned());
        self.cmd.arg(arg);
        self
    }

    pub fn args<I, S>(self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        args.into_iter().fold(self, Self::arg)
    }

    /// Run the command, returning stdout on success.
    pub async fn run(mut self) -> Result<String> {
        let out = self.cmd.output().await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::GitNotFound
            } else {
                e.into()
            }
        })?;
        if !out.status.success() {
            return Err(Error::Git {
                args: self.display.join(" "),
                status: out.status,
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// `git config --get <key>`, mapping "key not set" to `None`.
    pub async fn config_get(self, key: &str) -> Result<Option<String>> {
        match self.args(["config", "--get", key]).run().await {
            Ok(v) => Ok(Some(v.trim().to_owned())),
            Err(Error::Git { status, .. }) if status.code() == Some(1) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
