//! Per-chunk transcript cache and markdown rendering with sentence timestamps.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Piece {
    pub start: f64,
    pub end: f64,
    pub text: String,
    #[serde(default)]
    pub words: Vec<Word>,
}

/// A sentence with the time its first word is spoken.
#[derive(Debug, Clone, PartialEq)]
pub struct Sentence {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// Chunk results saved after every chunk, so a crash loses at most one chunk.
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
    let s = secs.max(0.0).floor() as u64;
    let (h, m, sec) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m:02}:{sec:02}")
    }
}

/// Joins aligned words into sentences. The aligner drops punctuation, so the
/// sentence breaks come from the chunk text: words are matched to it in order.
pub fn sentences(pieces: &[Piece]) -> Vec<Sentence> {
    let mut out = Vec::new();
    for p in pieces.iter().filter(|p| !p.text.trim().is_empty()) {
        if p.words.is_empty() {
            out.push(Sentence {
                start: p.start,
                end: p.end,
                text: p.text.trim().to_string(),
            });
            continue;
        }
        let mut wi = 0;
        let mut cur = String::new();
        let mut cur_start: Option<f64> = None;
        let mut cur_end = p.start;
        for token in p.text.split_whitespace() {
            let norm = normalize(token);
            // Advance to the aligned word that matches this token (aligners may split or skip).
            let mut matched = None;
            for (k, w) in p.words.iter().enumerate().skip(wi).take(4) {
                if !norm.is_empty() && normalize(&w.text) == norm {
                    matched = Some(k);
                    break;
                }
            }
            if let Some(k) = matched {
                cur_start.get_or_insert(p.words[k].start);
                cur_end = p.words[k].end;
                wi = k + 1;
            } else if cur_start.is_none() {
                cur_start = p.words.get(wi).map(|w| w.start);
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(token);
            if token.ends_with(['.', '?', '!']) {
                out.push(Sentence {
                    start: cur_start.unwrap_or(p.start),
                    end: cur_end,
                    text: std::mem::take(&mut cur),
                });
                cur_start = None;
            }
        }
        if !cur.is_empty() {
            out.push(Sentence {
                start: cur_start.unwrap_or(p.start),
                end: cur_end.max(p.end),
                text: cur,
            });
        }
    }
    out
}

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Paragraph break when the speaker pauses this long, or a paragraph gets this long.
const PAUSE_BREAK: f64 = 1.5;
const PARAGRAPH_SPAN: f64 = 45.0;

pub fn render_markdown(title: &str, source: &Path, duration: f64, pieces: &[Piece]) -> String {
    let mut md = String::new();
    let _ = writeln!(md, "---");
    let _ = writeln!(md, "title: {title:?}");
    let _ = writeln!(md, "source: {:?}", source.display().to_string());
    let _ = writeln!(md, "duration: \"{}\"", fmt_ts(duration));
    let _ = writeln!(
        md,
        "transcribed_with: Qwen3-ASR (mlx-qwen3-asr), word timestamps from Qwen3-ForcedAligner"
    );
    let _ = writeln!(md, "---\n");
    let _ = writeln!(md, "# {title}\n");
    let _ = writeln!(
        md,
        "> Video transcript. Each sentence starts with `[mm:ss]`, the moment it is spoken in the source video.\n> To see the screen at that moment: `scripts/frame.sh \"{}\" mm:ss`\n",
        source
            .file_name()
            .map(|s| s.to_string_lossy())
            .unwrap_or_default()
    );
    let mut para_start: Option<f64> = None;
    let mut last_end = 0.0;
    for s in sentences(pieces) {
        let new_para = para_start
            .is_some_and(|ps| s.start - last_end >= PAUSE_BREAK || s.start - ps >= PARAGRAPH_SPAN);
        if new_para {
            md.push('\n');
            para_start = None;
        }
        para_start.get_or_insert(s.start);
        let _ = writeln!(md, "[{}] {}", fmt_ts(s.start), s.text);
        last_end = s.end;
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(t: &str, s: f64) -> Word {
        Word {
            text: t.into(),
            start: s,
            end: s + 0.3,
        }
    }

    #[test]
    fn fmt_ts_formats_minutes_and_hours() {
        assert_eq!(fmt_ts(75.4), "01:15");
        assert_eq!(fmt_ts(3725.0), "1:02:05");
    }

    #[test]
    fn sentences_take_time_of_first_word() {
        let p = Piece {
            start: 100.0,
            end: 110.0,
            text: "Hello world. Use Screaming Frog!".into(),
            words: vec![
                w("Hello", 100.5),
                w("world", 101.0),
                w("Use", 104.2),
                w("Screaming", 104.5),
                w("Frog", 105.0),
            ],
        };
        let s = sentences(&[p]);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].text, "Hello world.");
        assert!((s[1].start - 104.2).abs() < 1e-9);
    }

    #[test]
    fn sentences_survive_missing_words() {
        let p = Piece {
            start: 0.0,
            end: 5.0,
            text: "One two three.".into(),
            words: vec![w("one", 0.1), w("three", 2.0)],
        };
        let s = sentences(&[p]);
        assert_eq!(s[0].text, "One two three.");
        assert!((s[0].start - 0.1).abs() < 1e-9);
    }

    #[test]
    fn render_puts_timestamp_on_each_sentence_and_skips_empty() {
        let pieces = vec![
            Piece {
                start: 0.0,
                end: 2.0,
                text: "A.".into(),
                words: vec![w("A", 0.0)],
            },
            Piece {
                start: 2.0,
                end: 4.0,
                text: "  ".into(),
                words: vec![],
            },
            Piece {
                start: 60.0,
                end: 62.0,
                text: "B.".into(),
                words: vec![w("B", 61.0)],
            },
        ];
        let md = render_markdown("T", Path::new("/c/v.mp4"), 62.0, &pieces);
        assert!(md.contains("[00:00] A.\n\n[01:01] B.\n"), "{md}");
        assert!(md.contains("scripts/frame.sh \"v.mp4\""));
        assert!(md.contains("duration: \"01:02\""));
    }

    #[test]
    fn cache_survives_reload() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("x/c.json");
        let mut c = Cache::load(&p).unwrap();
        c.put(
            1,
            Piece {
                start: 0.0,
                end: 1.0,
                text: "b".into(),
                words: vec![],
            },
        )
        .unwrap();
        assert_eq!(Cache::load(&p).unwrap().get(1).unwrap().text, "b");
    }
}
