//! Per-chunk transcript cache and markdown rendering.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Piece {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// Chunk results saved to disk after every chunk, so a crash loses at most one.
pub struct Cache {
    path: PathBuf,
    pieces: BTreeMap<usize, Piece>,
}

impl Cache {
    pub fn load(path: &Path) -> Result<Cache> {
        let pieces = match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s)
                .with_context(|| format!("corrupt cache {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e.into()),
        };
        Ok(Cache {
            path: path.to_path_buf(),
            pieces,
        })
    }

    pub fn get(&self, i: usize) -> Option<&Piece> {
        self.pieces.get(&i)
    }

    pub fn put(&mut self, i: usize, piece: Piece) -> Result<()> {
        self.pieces.insert(i, piece);
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(&self.pieces)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

pub fn fmt_ts(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    let (h, m, sec) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m:02}:{sec:02}")
    }
}

pub fn render_markdown(title: &str, source: &Path, duration: f64, pieces: &[Piece]) -> String {
    let mut md = String::new();
    let _ = writeln!(md, "---");
    let _ = writeln!(md, "title: {title:?}");
    let _ = writeln!(md, "source: {:?}", source.display().to_string());
    let _ = writeln!(md, "duration: \"{}\"", fmt_ts(duration));
    let _ = writeln!(md, "transcribed_with: ark-asr-3B-mlx");
    let _ = writeln!(md, "---\n");
    let _ = writeln!(md, "# {title}\n");
    let _ = writeln!(
        md,
        "> Video transcript. `[mm:ss]` marks where each passage starts in the source video.\n"
    );
    for p in pieces.iter().filter(|p| !p.text.trim().is_empty()) {
        let _ = writeln!(md, "**[{}]** {}\n", fmt_ts(p.start), p.text.trim());
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(t: f64, s: &str) -> Piece {
        Piece {
            start: t,
            end: t + 28.0,
            text: s.into(),
        }
    }

    #[test]
    fn fmt_ts_formats_minutes_and_hours() {
        assert_eq!(fmt_ts(75.0), "01:15");
        assert_eq!(fmt_ts(3725.0), "1:02:05");
    }

    #[test]
    fn render_skips_empty_pieces() {
        let md = render_markdown(
            "T",
            Path::new("/c/v.mp4"),
            60.0,
            &[piece(0.0, "a"), piece(28.0, "  "), piece(56.0, "b")],
        );
        assert!(md.contains("**[00:00]** a"));
        assert!(!md.contains("**[00:28]**"));
        assert!(md.contains("**[00:56]** b"));
        assert!(md.contains("source: \"/c/v.mp4\""));
        assert!(md.contains("duration: \"01:00\""));
    }

    #[test]
    fn cache_survives_reload() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("x/c.json");
        let mut c = Cache::load(&p).unwrap();
        c.put(0, piece(0.0, "a")).unwrap();
        c.put(1, piece(28.0, "b")).unwrap();
        let c2 = Cache::load(&p).unwrap();
        assert_eq!(c2.get(1).unwrap().text, "b");
    }
}
