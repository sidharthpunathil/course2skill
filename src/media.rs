//! ffmpeg and ffprobe wrappers. Paths are always passed as separate args.

use crate::chunk::{Span, parse_silences};
use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::{Command, Output};

fn run(cmd: &mut Command, what: &str) -> Result<Output> {
    let out = cmd
        .output()
        .with_context(|| format!("could not start {what} (is it installed?)"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: String = err.lines().rev().take(4).collect::<Vec<_>>().join(" | ");
        bail!("{what} failed: {tail}");
    }
    Ok(out)
}

pub fn probe_duration(video: &Path) -> Result<f64> {
    let out = run(
        Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=duration",
                "-of",
                "csv=p=0",
            ])
            .arg(video),
        "ffprobe",
    )?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.trim()
        .parse()
        .with_context(|| format!("bad duration {text:?} for {}", video.display()))
}

/// Writes 16 kHz mono f32 WAV.
pub fn extract_wav(video: &Path, out: &Path) -> Result<()> {
    run(
        Command::new("ffmpeg")
            .args(["-nostdin", "-y", "-v", "error", "-i"])
            .arg(video)
            .args(["-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_f32le"])
            .arg(out),
        "ffmpeg (extract audio)",
    )?;
    Ok(())
}

pub fn detect_silences(wav: &Path, noise_db: i32, min_dur: f64) -> Result<Vec<Span>> {
    let filter = format!("silencedetect=noise={noise_db}dB:d={min_dur}");
    let out = run(
        Command::new("ffmpeg")
            .args(["-nostdin", "-hide_banner", "-i"])
            .arg(wav)
            .args(["-af", &filter, "-f", "null", "-"]),
        "ffmpeg (silencedetect)",
    )?;
    Ok(parse_silences(&String::from_utf8_lossy(&out.stderr)))
}

/// Saves one frame of `video` at `at` seconds.
pub fn frame(video: &Path, at: f64, out: &Path) -> Result<()> {
    run(
        Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-y",
                "-v",
                "error",
                "-ss",
                &format!("{at:.3}"),
                "-i",
            ])
            .arg(video)
            .args(["-frames:v", "1", "-q:v", "2"])
            .arg(out),
        "ffmpeg (frame)",
    )?;
    Ok(())
}

/// Times (seconds) where the picture changes noticeably: cuts, slide changes,
/// screen shares, overlays. Analyzed at 2 fps and low resolution for speed.
pub fn scene_changes(video: &Path, threshold: f64) -> Result<Vec<f64>> {
    let filter = format!("fps=2,scale=320:-2,select='gt(scene\\,{threshold})',showinfo");
    let out = run(
        Command::new("ffmpeg")
            .args(["-nostdin", "-hide_banner", "-i"])
            .arg(video)
            .args(["-an", "-vf", &filter, "-f", "null", "-"]),
        "ffmpeg (scene detection)",
    )?;
    Ok(parse_pts_times(&String::from_utf8_lossy(&out.stderr)))
}

fn parse_pts_times(stderr: &str) -> Vec<f64> {
    stderr
        .split("pts_time:")
        .skip(1)
        .filter_map(|rest| rest.split_whitespace().next()?.parse().ok())
        .collect()
}

/// Parses `ss`, `mm:ss`, or `hh:mm:ss` (fractions allowed).
pub fn parse_ts(s: &str) -> Result<f64> {
    let mut total = 0.0;
    for part in s.trim().split(':') {
        let v: f64 = part
            .parse()
            .with_context(|| format!("bad timestamp {s:?}"))?;
        total = total * 60.0 + v;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pts_times_parse_from_showinfo() {
        let s = "[Parsed_showinfo_3 @ 0x1] n:0 pts:64 pts_time:32 duration:1\n[x] n:1 pts:91 pts_time:45.5 d";
        assert_eq!(parse_pts_times(s), vec![32.0, 45.5]);
    }

    #[test]
    fn parse_ts_handles_formats() {
        assert_eq!(parse_ts("75").unwrap(), 75.0);
        assert_eq!(parse_ts("01:15").unwrap(), 75.0);
        assert_eq!(parse_ts("1:02:05").unwrap(), 3725.0);
    }

    #[test]
    fn extract_and_detect_on_generated_audio() {
        if Command::new("ffmpeg").arg("-version").output().is_err() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("tone with space.wav");
        // 3 s tone, 2 s silence, 3 s tone.
        run(
            Command::new("ffmpeg")
                .args([
                    "-nostdin",
                    "-y",
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=f=440:d=3",
                    "-f",
                    "lavfi",
                    "-i",
                    "anullsrc=r=44100:cl=mono:d=2",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=f=440:d=3",
                    "-filter_complex",
                    "[0][1][2]concat=n=3:v=0:a=1",
                ])
                .arg(&src),
            "ffmpeg gen",
        )
        .unwrap();
        let wav = tmp.path().join("out [1].wav");
        extract_wav(&src, &wav).unwrap();
        let d = probe_duration(&wav).unwrap();
        assert!((d - 8.0).abs() < 0.2, "duration {d}");
        let s = detect_silences(&wav, -35, 0.3).unwrap();
        assert!(!s.is_empty());
    }
}
