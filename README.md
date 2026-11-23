# course2skill

**Turn a video course into skills your AI agent can use.**

You bought a course. Your agent can't watch it. course2skill transcribes every video on your Mac, reads every PDF and spreadsheet that came with it, captures the screens that matter, and writes one agent skill per section: a playbook another LLM can follow step by step, with the screenshots, numbers, templates and tips from the course, each traced back to the minute it was taught.

```sh
course2skill                      # opens the wizard: pick a course folder, pick an AI, go
```

## What you get

From one real run on a 14-section SEO course (37 videos, 9.8 hours, 23 PDFs and spreadsheets):

- 14 skills, one per section, ready to drop into `~/.claude/skills/`
- 37 transcripts, every sentence stamped with the second it is spoken
- 600 screenshots, picked from the moments the screen changes or the instructor says "as you can see", named like `v2-09m12s-google-search-console-pages-report.jpg`
- complete lesson notes for every video, so nothing said in passing is lost
- a knowledge graph of the whole course (1,103 concepts, 41 topics) that links each skill to the related ones
- a shareable package with an index README and one zip per skill

Each skill is a self-contained folder:

```
skills/04-getting-technical/
  SKILL.md               when to use it, core principles, step-by-step procedures with
                         if/then rules, numbers and thresholds, checklists, common mistakes,
                         tips and tricks, worked examples, glossary, related sections
  references/lessons/    full ordered notes for every video, with timestamps
  references/*.md        templates rebuilt as tables, walkthroughs, resources, coverage audit
  assets/screenshots/    the frames the skill refers to
  assets/files/          the section's original PDFs and spreadsheets
  scripts/frame.sh       grab any other moment from the source videos
```

Every claim in a skill cites its source, like `(Video_01-Getting_Technical.mp4 [09:12])` or `(file: Site Audit Template.xlsx)`, and every skill ends with a coverage audit that re-checks each transcript and file against what was written.

## Quick start

```sh
brew install ffmpeg poppler pandoc uv
cargo install --path .
course2skill setup        # installs local speech-to-text (Qwen3-ASR on MLX)
course2skill doctor       # shows which tools and AI backends are ready
course2skill              # the wizard
```

Then install the skills:

```sh
course2skill export --to ~/.claude/skills --flat
```

Requires macOS on Apple Silicon and one AI backend (Claude Code, Codex, opencode, Gemini CLI, Antigravity, an API key, or a local model).

## How it works

```
course folder ──▶ 1. copy  ──▶ 2. transcribe ──▶ 3. map ──────────▶ 4. write skills ──▶ 5. export
 (any layout)     course      every video        knowledge graph    one per section     share or
                  files       on your Mac        + screen changes   with screenshots    install
```

1. **Copy.** PDFs, spreadsheets, docs and images are copied into each section as-is.
2. **Transcribe.** Qwen3-ASR runs locally on the GPU with word-level timestamps. Terms from the section's own documents are passed in, so tool names and jargon come out right. About 0.15x real time, so a 10-hour course takes about 2 hours.
3. **Map.** graphify builds a knowledge graph across every section. ffmpeg finds every scene change, and each video gets a visual index of the moments worth looking at, paired with what is being said.
4. **Write.** The AI you chose reads every transcript and file in full, captures and checks the screenshots it needs from the visual index, writes the lesson notes, then the skill, then audits its own coverage. Up to 5 sections run at once, and large sections split their lesson notes across parallel subagents.
5. **Export.** A clean package, with your local paths removed, to share or install.

Everything is resumable. Quit at any time and the same command picks up where it stopped: transcribed audio, scene scans and the knowledge graph are cached, and finished skills are kept.

## Works with any course layout

Chapter folders, nested modules, or one flat folder of 80 videos all work. course2skill finds every file at any depth and groups lessons into sections by folder, by numbering like `2.3` or `S2E3`, or by asking your AI to group them by topic (`--layout ai`). The grouping is saved as `plan.json`, so you can edit it and rerun.

## Pick your AI

| Backend | How it runs | Setup |
|---|---|---|
| Claude Code | `claude -p`, writes files and views screenshots itself | `claude` on PATH |
| Codex | `codex exec` in a workspace sandbox | `codex` on PATH |
| opencode | `opencode run` | `opencode` on PATH |
| Gemini CLI | `gemini -p` | `gemini` on PATH |
| Antigravity | `agy -p` | `agy` on PATH |
| Anthropic API | Messages API, streaming | `ANTHROPIC_API_KEY` |
| OpenAI-compatible API | `/chat/completions` | `OPENAI_API_KEY` |
| Local model | Ollama, LM Studio or `mlx_lm.server` | `--ai-url` (default `http://localhost:11434/v1`) |

The coding CLIs work best: they open the PDFs and look at every screenshot before keeping it. API and local models get the material in the prompt and request screenshots by timestamp, which course2skill then captures.

With Claude Code, a good setup is `--ai-model opus --notes-model sonnet`: Opus writes and audits the skill, while Sonnet subagents write the per-video lesson notes.

## Privacy and cost

- Transcription, scene detection and screenshots run on your Mac. Videos never leave it.
- Transcripts and course files are sent only to the AI backend you pick. With a local model, nothing leaves the machine.
- Writing skills is the expensive part. On a Claude subscription it counts against your plan's usage limits; on an API it is billed per token. The knowledge graph for a 10-hour course used about 1.3 million input tokens.
- Use it on courses you have the right to use. Exports contain the course's transcripts and files, so check the course license before sharing them.

## Share or install

```sh
course2skill export                                  # into ./export
course2skill export --zip                            # plus one zip per skill and one for the course
course2skill export --to ~/.claude/skills --flat     # install straight into Claude Code
course2skill export --skills-only                    # leave out transcripts and course files
```

An export has `skills/`, `course/` (transcripts and course files), `knowledge-graph/` (open `graph.html` in a browser) and a `README.md` index with install steps for Claude Code and Codex. `scripts/frame.sh` reads the video location from `COURSE_VIDEO_DIR`, so anyone who also has the videos can keep grabbing frames.

<details>
<summary><b>Command reference</b></summary>

```sh
course2skill                                # wizard
course2skill run <course>                   # everything: copy, transcribe, graph, skills
course2skill transcribe <course>            # copy files and transcribe only
course2skill skills <course>                # write skills from the latest run's transcripts
course2skill plan <course> --layout ai      # preview how lessons are grouped
course2skill export [--to DIR] [--zip]      # package the latest run
course2skill frame <video> 03:10 shot.jpg   # one frame
course2skill setup | doctor
```

| Flag | Meaning |
|---|---|
| `--ai claude\|codex\|opencode\|gemini\|antigravity\|anthropic\|openai\|local` | AI that writes the skills (default `claude`) |
| `--ai-model NAME` | Model for that AI, e.g. `opus` |
| `--notes-model NAME` | Model for the lesson-note subagents, e.g. `sonnet` |
| `--ai-url URL` | Base URL for API and local backends |
| `--skill-jobs N` | Sections written at once (default 5) |
| `--skill-timeout MIN` | Stop an AI call after this long (default 90) |
| `--no-graph` | Skip the knowledge graph |
| `--layout auto\|folders\|ai` | How lessons are grouped |
| `--units 1,3-5` | Only some sections |
| `--run latest\|NAME` / `--new` | Continue a specific run / force a fresh one |
| `--language NAME` | Spoken language (default `English`, `auto` to detect) |
| `--asr-model Qwen/Qwen3-ASR-0.6B` | About 3x faster speech model, a little less accurate |
| `--plain` | Log lines instead of the TUI |

Each run writes to `result/<timestamp>/` with a `latest` link; the speech and scene caches in `result/.cache/` are shared by every run.

</details>

<details>
<summary><b>Under the hood</b></summary>

**Transcription.** ffmpeg extracts 16 kHz mono audio, `silencedetect` finds pauses, and the audio is cut at those pauses into pieces of up to two minutes. A Python worker keeps Qwen3-ASR-1.7B and the Qwen3 forced aligner loaded and returns word timestamps; transcript tokens are matched to aligned words with a longest-common-subsequence alignment, so each sentence gets the time of its first word. Every piece is cached as it finishes. While the GPU works on one video, another thread prepares the next one and two more run scene detection.

**Screenshots.** For each video, a visual index lists every scene change (ffmpeg `select=gt(scene,0.25)`) and every sentence where the speaker points at the screen, with the time and what is said. The skill writer captures frames there, checks a few seconds around a moment when it needs more, reads the text in each frame into the notes, and names each file by video number, timestamp and content. Frames no skill embeds are deleted.

**Skills.** The prompt fixes the order: read everything, capture screenshots, write complete lesson notes per video (steps, numbers, tools, settings, examples, warnings, opinions, tips, tricks, asides, resources), write `SKILL.md` for an LLM reader, write references and `resources.md`, then audit coverage. A skill gets a `.complete` marker only when it finishes; an unfinished one is redone on the next run.

**Knowledge graph.** graphify runs headless with an API key, or through Claude Code's `/graphify` skill. Skill writers read its report and can run `graphify query`, `explain` and `path` while writing.

</details>

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

Rust, ratatui and clap; the speech worker is a small Python script embedded in the binary. Design notes are in `docs/superpowers/`.
