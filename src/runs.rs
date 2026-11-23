//! Each generation lives in its own timestamped folder under the base output
//! folder. `latest` points at the newest one. The speech cache in `.cache/` is
//! shared, so a new generation never transcribes the same audio twice.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

pub const LATEST: &str = "latest";

pub fn cache_dir(base: &Path) -> PathBuf {
    base.join(".cache")
}

/// Creates `base/<YYYY-MM-DD_HH-MM-SS>` and points `latest` at it.
pub fn new_run(base: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(base)?;
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let mut dir = base.join(&stamp);
    let mut n = 2;
    while dir.exists() {
        dir = base.join(format!("{stamp}-{n}"));
        n += 1;
    }
    std::fs::create_dir_all(&dir)?;
    point_latest(base, &dir)?;
    Ok(dir)
}

fn point_latest(base: &Path, dir: &Path) -> Result<()> {
    let link = base.join(LATEST);
    if link.is_symlink() {
        std::fs::remove_file(&link)?;
    }
    #[cfg(unix)]
    {
        let name = dir.file_name().context("run folder has no name")?;
        std::os::unix::fs::symlink(name, &link).context("updating the latest link")?;
    }
    Ok(())
}

/// Every run folder, oldest first (timestamps sort as text).
pub fn list(base: &Path) -> Vec<PathBuf> {
    let mut runs: Vec<PathBuf> = std::fs::read_dir(base)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| !p.is_symlink() && p.is_dir() && is_stamp(&crate::layout::file_name(p)))
        .collect();
    runs.sort();
    runs
}

fn is_stamp(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() >= 19 && b[..4].iter().all(u8::is_ascii_digit) && b[4] == b'-' && b[10] == b'_'
}

/// Resolves `latest` or a run folder name.
pub fn resolve(base: &Path, name: &str) -> Result<PathBuf> {
    if name == LATEST {
        return list(base)
            .pop()
            .with_context(|| format!("no runs yet in {}", base.display()));
    }
    let dir = base.join(name);
    if !dir.is_dir() {
        bail!("no run named {name:?} in {}", base.display());
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_runs_are_unique_and_latest_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        let a = new_run(tmp.path()).unwrap();
        let b = new_run(tmp.path()).unwrap();
        assert_ne!(a, b);
        assert_eq!(resolve(tmp.path(), LATEST).unwrap(), b);
        assert_eq!(
            std::fs::read_link(tmp.path().join(LATEST)).unwrap(),
            b.file_name().unwrap()
        );
    }

    #[test]
    fn cache_is_not_a_run() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(cache_dir(tmp.path())).unwrap();
        assert!(resolve(tmp.path(), LATEST).is_err());
    }
}
