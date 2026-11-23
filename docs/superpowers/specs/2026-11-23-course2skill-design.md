# course2skill design

## Intent

Turn a video course on disk into agent skills. Every lesson video gets transcribed locally. Each chapter becomes a markdown folder, and then one consolidated agent skill that holds everything the chapter teaches. The first course is Edward Sturm's Compact Keywords (14 chapters, 37 videos, 9.8 hours, plus PDF, DOCX and XLSX handouts).

## Goals

- One Rust binary, `course2skill`, with a ratatui TUI that shows each stage and file as it runs.
- Local speech to text with `leope/ark-asr-3B-mlx` on Apple Silicon.
- `result/<chapter>/` mirrors the course chapters. Each video becomes a timestamped `.md`, and each handout becomes a `.md` too.
- `result/skills/<chapter-slug>/` holds one skill per chapter. The skill is written by an AI from every transcript and handout in that chapter, with the detailed knowledge kept, not summarized away.
- Each skill can pull a screenshot from the source video at a timestamp when it needs to see what was on screen.
- Resumable. Rerunning skips finished work.

## Non-goals

- Visible watermark or logo removal from video.
- Cloud transcription.
- A GUI.

## Anti-goals

- Skills that are thin summaries. The chapter skill has to carry the actual procedures, numbers, examples and templates.
- Losing hours of transcription to a crash halfway through.

## Constraints

- ARK-ASR rejects clips over 30 seconds, has no timestamps, and only runs through its own Python package (`ark_asr_mlx`, Python 3.12, MLX). It needs about 8 GB of unified memory. The machine has 24 GB.
- ffmpeg 8, pdftotext and pandoc are installed. The `claude` CLI is installed.

## Approaches considered

1. **Rust TUI plus a persistent Python ASR worker (picked).** Rust handles scanning, ffmpeg, chunking, the UI, docs and skill synthesis. A small Python worker loads the model once and reads JSON requests on stdin. This loads the model once, keeps Python to about 40 lines, and keeps the app native.
2. Rust calls the `ark-asr-mlx` CLI per chunk. This is simpler, but it reloads 7.5 GB for each of roughly 1,200 chunks.
3. Port the model to Rust (candle or mlx-rs). This is far too much work and risk for the payoff.

## Design

### Pipeline (per video)
1. `ffmpeg` extracts 16 kHz mono WAV.
2. `ffmpeg silencedetect` finds pauses. Chunks are cut at the last pause before 28s, falling back to a hard 28s cut. Each chunk knows its start time.
3. The worker transcribes each chunk. Results are cached per chunk in `result/.cache/` as JSON so a crash resumes mid-video.
4. Writes `result/<chapter>/<video-stem>.md`: front matter (source path, duration), then paragraphs with `[mm:ss]` markers.

### Handouts
- PDF: `pdftotext -layout`.
- DOCX: `pandoc -t gfm`.
- XLSX: read with the `calamine` crate and emitted as markdown tables, one per sheet.

### Skill synthesis (per chapter)
- Gather every transcript and handout `.md` in the chapter.
- Run `claude -p` headless with a prompt that asks for a skill in Anthropic's format: `SKILL.md` (frontmatter with name and a trigger-rich description, then the full working knowledge, procedures, checklists and examples) and `references/` for long detail. The CLI is pointed at the chapter folder and allowed to write files there.
- Copy in `scripts/frame.sh <video> <mm:ss> [out.png]`, which calls ffmpeg for a screenshot. Also write `sources.md`, which maps each transcript to the absolute video path so the skill can find the frame it needs.
- Strip invisible Unicode (zero-width, bidi, tag characters) from the generated text, the same as the watermarks-remover text layer A.

### TUI
- Header with the course name and overall progress gauge.
- Left: chapter list with per-chapter status. Right: the current file, its chunk gauge, and a live tail of the transcript.
- Bottom: log lines and ETA. Keys: `q` quit, `p` pause.
- Subcommands: `course2skill run <course-dir>` (TUI), `transcribe`, `docs`, `skills`, `frame`, and a `--plain` flag for no TUI.

### Calls left to the builder
- Exact silence threshold (default -35 dB, 0.3s).
- Color palette and layout details for the TUI.
- Where the Python worker venv lives (default `~/.cache/course2skill`).
