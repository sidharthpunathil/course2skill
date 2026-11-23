//! Visual index: the moments in a video worth looking at, found from the
//! transcript timestamps (spoken visual cues) and from scene changes in the
//! picture, each paired with what is being said at that moment.

use crate::transcript::fmt_ts;
use anyhow::Result;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Phrases that usually mean the speaker is pointing at something on screen.
const CUES: &[&str] = &[
    "as you can see",
    "you can see",
    "you'll see",
    "you will see",
    "take a look",
    "look at this",
    "look at that",
    "look here",
    "right here",
    "over here",
    "up here",
    "down here",
    "this page",
    "this screen",
    "this tab",
    "this column",
    "this row",
    "this sheet",
    "this tool",
    "this chart",
    "this graph",
    "this report",
    "this button",
    "click",
    "scroll",
    "on the screen",
    "on screen",
    "on the left",
    "on the right",
    "at the top",
    "at the bottom",
    "highlighted",
    "zoom in",
    "let me show",
    "i'll show",
    "showing you",
    "here's what",
    "here is the",
    "here we have",
    "here's the",
    "this is the",
    "type in",
    "go to",
    "open up",
    "pull up",
    "drop down",
    "dropdown",
];

/// Scene-change sensitivity (0-1). Lower catches subtler changes like overlays.
const SCENE_THRESHOLD: f64 = 0.25;

#[derive(Debug, Clone, PartialEq)]
pub struct Moment {
    pub at: f64,
    pub why: String,
    pub said: String,
}

/// `[mm:ss] sentence` lines from a transcript, in order.
pub fn transcript_lines(md: &str) -> Vec<(f64, String)> {
    md.lines()
        .filter_map(|l| {
            let rest = l.strip_prefix('[')?;
            let (ts, text) = rest.split_once("] ")?;
            Some((crate::media::parse_ts(ts).ok()?, text.trim().to_string()))
        })
        .collect()
}

/// Builds the index from transcript lines and scene-change times.
pub fn moments(lines: &[(f64, String)], scenes: &[f64]) -> Vec<Moment> {
    let said_at = |t: f64| -> String {
        let i = lines.partition_point(|(s, _)| *s <= t + 0.5);
        i.checked_sub(1)
            .map(|i| lines[i].1.clone())
            .unwrap_or_default()
    };
    let mut out: Vec<Moment> = Vec::new();
    for (t, text) in lines {
        let lower = text.to_lowercase();
        if let Some(cue) = CUES.iter().find(|c| lower.contains(*c)) {
            out.push(Moment {
                at: *t,
                why: format!("spoken cue \"{cue}\""),
                said: text.clone(),
            });
        }
    }
    for &t in scenes {
        out.push(Moment {
            at: t,
            why: "screen changes".into(),
            said: said_at(t),
        });
    }
    out.sort_by(|a, b| a.at.total_cmp(&b.at));
    // Merge events within 2 seconds of each other into one moment.
    let mut merged: Vec<Moment> = Vec::new();
    for m in out {
        match merged.last_mut() {
            Some(last) if m.at - last.at < 2.0 => {
                if !last.why.contains(&m.why) {
                    last.why = format!("{} + {}", last.why, m.why);
                }
            }
            _ => merged.push(m),
        }
    }
    merged
}

pub fn render(video_name: &str, video_no: usize, moments: &[Moment]) -> String {
    let mut md = format!(
        "# Visual index: {video_name} (v{video_no})\n\nMoments worth looking at: every scene change in the picture, and every sentence where the speaker points at something on screen. `said` is the transcript sentence at that moment.\n\n| time | why | said |\n|---|---|---|\n"
    );
    for m in moments {
        let said: String = m.said.chars().take(160).collect();
        let _ = writeln!(
            md,
            "| {} | {} | {} |",
            fmt_ts(m.at),
            m.why,
            said.replace('|', "\\|")
        );
    }
    if moments.is_empty() {
        md.push_str("| - | no scene changes or spoken cues found | - |\n");
    }
    md
}

/// Scene changes for a video, cached next to the speech cache.
pub fn cached_scenes(cache_dir: &Path, video: &Path) -> Result<Vec<f64>> {
    let key: String = video
        .display()
        .to_string()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    let path: PathBuf = cache_dir.join("scenes").join(format!("{key}.json"));
    if let Ok(s) = std::fs::read_to_string(&path)
        && let Ok(v) = serde_json::from_str(&s)
    {
        return Ok(v);
    }
    let scenes = crate::media::scene_changes(video, SCENE_THRESHOLD)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, serde_json::to_string(&scenes)?)?;
    Ok(scenes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_lines_parse() {
        let l =
            transcript_lines("# T\n\n[00:05] Hello there.\n[01:02] As you can see, the report.\n");
        assert_eq!(
            l,
            vec![
                (5.0, "Hello there.".to_string()),
                (62.0, "As you can see, the report.".to_string())
            ]
        );
    }

    #[test]
    fn moments_combine_cues_and_scenes() {
        let lines = vec![
            (5.0, "Hello.".to_string()),
            (62.0, "As you can see, the report.".to_string()),
            (90.0, "Next.".to_string()),
        ];
        let m = moments(&lines, &[63.0, 95.0]);
        assert_eq!(m.len(), 2);
        assert!(
            m[0].why.contains("as you can see") && m[0].why.contains("screen changes"),
            "{:?}",
            m[0]
        );
        assert_eq!(m[1].said, "Next.");
    }
}
