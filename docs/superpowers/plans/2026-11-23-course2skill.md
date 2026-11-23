# course2skill Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Rust ratatui CLI that turns a course folder of chapter videos and handouts into timestamped markdown and one AI-written agent skill per chapter.

**Architecture:** Rust drives ffmpeg, chunking, doc conversion, skill synthesis and the TUI. A persistent Python worker (embedded in the binary, run from a uv venv) holds ARK-ASR in memory and answers JSON-line requests. Pipeline stages emit `Event`s over a channel; the TUI or a plain logger consumes them.

**Tech Stack:** Rust 2024, ratatui 0.29 + crossterm, clap 4 (derive), anyhow, serde/serde_json, calamine; ffmpeg, pdftotext, pandoc, `claude` CLI; Python 3.12 + `ark_asr_mlx`.

**Spec:** `docs/superpowers/specs/2026-11-23-course2skill-design.md`

## Global Constraints

- Chunks sent to ASR are at most 28.0 s (ARK-ASR hard limit is 30 s).
- Audio is 16 kHz mono `f32` WAV.
- Silence detection defaults: `-35dB`, `d=0.3`.
- Output root `result/`; chapter folders keep the exact source chapter names; per-chunk cache in `result/.cache/<chapter>/<video-stem>.json`.
- Skills go to `result/skills/<chapter-slug>/` with `SKILL.md`, `references/`, `scripts/frame.sh`, `sources.md`.
- Worker venv default `~/.cache/course2skill/.venv`, model default `~/.cache/course2skill/ark-asr-3B-mlx`; both overridable with `--venv`/`--model`.
- No em dashes in generated prose written by this code; no provider/proxy names in code or copy.

## Review Focus

- File and chapter names with spaces, brackets and commas (`01-SEO Landing Page Template - [Keyword]_ [keyword one]...docx`) must round-trip through every subprocess call (always pass paths as args, never via shell strings). Test in Task 1.
- A video with no silence over 28 s must still be cut into chunks of 28 s or less. Test in Task 2.
- A silent or music-only chunk returns empty text and must not break paragraphs or the markdown. Test in Task 5.
- Rerun after a crash mid-video resumes from the cached chunks and does not re-transcribe. Test in Task 5.
- Worker crash or missing model gives one clear error naming the venv/model path, not a hang. Test in Task 4.

---

### Task 1: Scaffold and course scan

**Files:** Create `Cargo.toml`, `src/main.rs`, `src/scan.rs`; Test: inline `#[cfg(test)]` in `src/scan.rs`.

**Interfaces:**
- Produces: `pub struct Course { name: String, root: PathBuf, chapters: Vec<Chapter> }`, `pub struct Chapter { name: String, slug: String, dir: PathBuf, videos: Vec<PathBuf>, docs: Vec<PathBuf> }`, `pub fn scan(root: &Path) -> Result<Course>`, `pub fn slugify(s: &str) -> String`.
- `root` may be the course dir itself or a parent holding exactly one course dir.

- [ ] Test `scan_sorts_chapters_and_videos`: tempdir with `02-B/Video_x.mp4`, `01-A/Video_y_video2.mp4`, `01-A/Video_y.mp4`, `01-A/notes [a], b.pdf`, `.DS_Store` → chapters `["01-A","02-B"]`, chapter 1 videos `[Video_y.mp4, Video_y_video2.mp4]`, docs `[notes [a], b.pdf]`, no `.DS_Store`.
- [ ] Test `slugify("05-On-Page SEO") == "05-on-page-seo"`.
- [ ] Implement with natural sort (videos: stem without `_videoN` first, then N ascending). Video exts: mp4 mkv mov webm m4v. Doc exts: pdf docx xlsx pptx txt md.
- [ ] `cargo test` passes; commit `feat: scaffold and course scanning`.

### Task 2: Chunk planner

**Files:** Create `src/chunk.rs`.

**Interfaces:** Produces `pub struct Span { start: f64, end: f64 }`, `pub fn parse_silences(ffmpeg_stderr: &str) -> Vec<Span>`, `pub fn plan_chunks(duration: f64, silences: &[Span], max: f64) -> Vec<Span>`.

- [ ] Test `parse_silences` on lines `silence_start: 10.5` / `silence_end: 11.2 | silence_duration: 0.7` → `[Span{10.5,11.2}]`.
- [ ] Test `plan_chunks(70.0, &[], 28.0)` → 3 spans, each `end-start <= 28.0`, contiguous, last ends at 70.0.
- [ ] Test `plan_chunks(50.0, &[Span{20.0,21.0}], 28.0)` → first cut at 20.5 (silence midpoint).
- [ ] Algorithm: from `cursor`, pick the latest silence midpoint in `(cursor+5, cursor+max]`; else hard cut at `cursor+max`. Drop a trailing span shorter than 0.3 s by merging into the previous if that stays ≤ max.
- [ ] `cargo test`; commit.

### Task 3: Media (ffmpeg)

**Files:** Create `src/media.rs`.

**Interfaces:** Produces `pub fn probe_duration(video: &Path) -> Result<f64>`, `pub fn extract_wav(video: &Path, out: &Path) -> Result<()>`, `pub fn detect_silences(wav: &Path) -> Result<Vec<Span>>`, `pub fn frame(video: &Path, at: f64, out: &Path) -> Result<()>`.

- [ ] Test `extract_and_detect` (ignored if ffmpeg missing): generate 10 s file via `ffmpeg -f lavfi -i "sine=d=4,apad=pad_dur=2" ...` style input with a gap, assert duration ≈ value and at least one silence found.
- [ ] Implement with `std::process::Command` args, never shell strings. `extract_wav`: `-vn -ac 1 -ar 16000 -c:a pcm_f32le`. `frame`: `-ss <at> -i <video> -frames:v 1 -q:v 2`.
- [ ] `cargo test`; commit.

### Task 4: ASR worker

**Files:** Create `worker/asr_worker.py`, `src/asr.rs`.

**Interfaces:** Produces `pub struct Asr` with `pub fn spawn(python: &Path, model: &Path) -> Result<Asr>`, `pub fn transcribe(&mut self, wav: &Path, span: Span) -> Result<String>`, `pub fn setup(venv: &Path, model: &Path) -> Result<PathBuf>` (returns python path; errors naming paths when missing).
- Protocol (stdin/stdout JSON lines): worker prints `{"ready":true}` after load; request `{"wav":"...","start":s,"end":e}`; reply `{"text":"..."}` or `{"error":"..."}`. Worker caches the last loaded wav (soundfile/numpy via `ark_asr_mlx` processor's `load_audio` path or `librosa.load(sr=16000)`), slices samples, calls `asr.transcribe(arr, sample_rate=16000, max_new_tokens=256)`.
- Worker script embedded with `include_str!` and written to a temp file at spawn.

- [ ] Test `asr_roundtrip_with_fake_worker`: spawn with a fake python script (env override `COURSE2SKILL_WORKER`) that echoes `{"text":"hi"}` → `transcribe` returns `"hi"`.
- [ ] Test `worker_exit_is_error`: fake worker that exits immediately → `spawn` returns Err containing "worker".
- [ ] `cargo test`; commit.

### Task 5: Transcript cache and markdown

**Files:** Create `src/transcript.rs`.

**Interfaces:** Produces `pub struct Piece { start: f64, end: f64, text: String }`, `pub struct Cache` (`load(path)`, `get(i) -> Option<&Piece>`, `put(i, Piece)` saves atomically), `pub fn render_markdown(title: &str, source: &Path, duration: f64, pieces: &[Piece]) -> String`, `pub fn fmt_ts(secs: f64) -> String` (`mm:ss`, `h:mm:ss` at 1 h+).

- [ ] Test `fmt_ts(75.0) == "01:15"`, `fmt_ts(3725.0) == "1:02:05"`.
- [ ] Test `render_skips_empty`: pieces `[("a"), (""), ("b")]` → contains `**[00:00]**` before a, no empty marker, front matter has `source:` and `duration:`.
- [ ] Test `cache_resume`: put 2 pieces, reload from disk, `get(1)` is Some.
- [ ] Paragraph rule: start a new paragraph with a timestamp marker every chunk (chunks are ~28 s, readable as paragraphs).
- [ ] `cargo test`; commit.

### Task 6: Handout conversion

**Files:** Create `src/docs.rs`.

**Interfaces:** Produces `pub fn convert(doc: &Path) -> Result<String>` (markdown), `fn xlsx_to_md(path) -> Result<String>`, `fn rows_to_table(rows: &[Vec<String>]) -> String`.

- [ ] Test `rows_to_table` escapes `|` and drops fully empty trailing columns/rows; header row is first row.
- [ ] Implement: pdf → `pdftotext -layout <f> -`; docx/pptx → `pandoc -t gfm <f>`; xlsx → calamine, one `## Sheet: <name>` section per sheet; txt/md → read.
- [ ] `cargo test`; commit.

### Task 7: Skill synthesis

**Files:** Create `src/skill.rs`, `assets/frame.sh`, `assets/skill_prompt.md`.

**Interfaces:** Produces `pub fn strip_invisible(s: &str) -> String`, `pub fn build_prompt(chapter: &Chapter, result_dir: &Path, skill_dir: &Path) -> String`, `pub fn synthesize(chapter: &Chapter, result_dir: &Path, skills_root: &Path) -> Result<PathBuf>`.
- `synthesize` writes `sources.md` and `scripts/frame.sh` first, then runs `claude -p <prompt> --permission-mode acceptEdits --allowedTools Read,Write,Edit,Glob --add-dir <chapter result dir>` with cwd = skill dir, then strips invisible chars from every `.md` it produced, and fails if `SKILL.md` is missing.
- Prompt demands: frontmatter `name: <slug>` and a trigger-rich `description`; full working knowledge (procedures, numbers, tools, examples, templates, mistakes to avoid), not a summary; `references/` for long material; cite `[mm:ss]` + video filename for claims; a "Looking at the video" section explaining `scripts/frame.sh "<video>" mm:ss out.png`; no em dashes.

- [ ] Test `strip_invisible("a\u{200B}b\u{202E}c\u{E0041}") == "abc"` and keeps `\n`, emoji, accented letters.
- [ ] Test `build_prompt` contains the slug, each transcript filename, and `frame.sh`.
- [ ] `cargo test`; commit.

### Task 8: Pipeline, TUI and CLI

**Files:** Create `src/pipeline.rs`, `src/tui.rs`; modify `src/main.rs`.

**Interfaces:** Produces `pub enum Event { Stage(String), ChapterStatus{idx, status}, VideoStart{chapter, video, chunks}, Chunk{done, total, text}, VideoDone, Log(String), Error(String), Finished }`, `pub fn run(opts: Opts, tx: Sender<Event>) -> Result<()>`.
- CLI (clap): `run <course> [--out result] [--plain] [--skip-skills] [--chapters 1,3-5]`, `transcribe`, `docs`, `skills` (same args), `frame <video> <mm:ss> <out.png>`, `setup` (checks ffmpeg, pdftotext, pandoc, claude, venv, model).
- TUI: header (course name, overall gauge, elapsed, ETA from chunk rate), left chapter list with status glyphs and per-chapter colors, right pane (current video, chunk gauge, live transcript tail wrapped), bottom log. `q` quits cleanly (worker killed, terminal restored, also on panic).
- Skills for chapters run in parallel (up to 3) after that chapter's transcripts exist.

- [ ] `cargo build --release` clean, `cargo clippy -- -D warnings` clean.
- [ ] Manual: `course2skill run course --chapters 1` produces `result/01-Introduction/*.md` and `result/skills/01-introduction/SKILL.md`.
- [ ] Commit `feat: pipeline, TUI and CLI`.

### Task 9: Full run and README

- [ ] Run on the whole course; verify every video has a `.md`, every chapter a skill.
- [ ] README (humanized) with install, setup, usage, output layout. Commit.
