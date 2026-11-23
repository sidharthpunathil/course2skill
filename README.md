# course2skill

course2skill turns a course folder into timestamped markdown transcripts of every video, plus a copy of every other course file, then into one agent skill per unit. Transcription runs locally on Apple Silicon with Qwen3-ASR. An AI of your choice writes the skills.

## What it produces

```
result/
  .cache/                      speech-to-text cache shared by every run
  2026-11-23_06-30-10/         one generation
    plan.json                  how files were grouped into units (editable)
    01-Introduction/
      Video_01-Welcome_to_Compact_Keywords.md   transcript, a [mm:ss] on every sentence
      01-Welcome to Compact Keywords.pdf        course files copied as-is
    skills/
      01-introduction/         one skill per section
        SKILL.md               the playbook: principles, procedures, thresholds, checklists,
                               mistakes, tips and tricks, examples, glossary, screenshots
        references/lessons/    complete ordered notes for every video, with timestamps
        references/*.md        templates, walkthroughs, resources.md, coverage.md
        assets/screenshots/    frames the AI picked from the videos and checked
        assets/files/          the section's course files, as-is
        sources.md             every video, transcript and file
        scripts/frame.sh       grab more frames from the source videos
  latest -> 2026-11-23_06-30-10
```

Each run gets a new timestamped folder. Transcribed audio is cached in `result/.cache`, keyed by the video's path in the course and the speech model, so a later run reuses it. If the latest run did not finish, running the same command continues it; `--new` starts a fresh one.

The stages run in order: course files are copied, then every video is transcribed and saved, then the skills are written. Only videos become markdown. PDFs, spreadsheets, documents, images and anything else are copied unchanged.

## Requirements

- macOS on Apple Silicon (MLX)
- ffmpeg, pdftotext (poppler), pandoc: `brew install ffmpeg poppler pandoc`
- [uv](https://docs.astral.sh/uv/) for the Python speech environment
- At least one AI backend (see below)

## Install

```sh
cargo install --path .
course2skill setup     # creates ~/.cache/course2skill/qwen-venv with mlx-qwen3-asr
course2skill doctor    # checks tools, the speech env and which AI backends are ready
```

The first transcription downloads Qwen3-ASR-1.7B and the Qwen3 forced aligner from Hugging Face (about 5 GB).

## Use

Run it with no arguments to open the wizard. It asks for the course path, the AI, and how to group lessons, then shows the plan before starting:

```sh
course2skill
```

Or use subcommands:

```sh
course2skill run course/                    # copy files, transcripts, then skills
course2skill transcribe course/             # copy files and transcripts only
course2skill skills course/ --ai codex      # skills from the latest run's transcripts
course2skill plan course/ --layout ai       # preview the unit grouping
course2skill frame video.mp4 03:10 shot.jpg
```

Useful flags:

| Flag | Meaning |
|---|---|
| `--ai claude\|codex\|opencode\|gemini\|antigravity\|anthropic\|openai\|local` | Which AI writes the skills (default `claude`) |
| `--ai-model NAME` | Model for that AI |
| `--ai-url URL` | Base URL for API and local backends |
| `--layout auto\|folders\|ai` | How lessons are grouped into units |
| `--units 1,3-5` | Only some units |
| `--run latest\|NAME` | Continue a specific run |
| `--new` | Start a new run even if the latest one did not finish |
| `--skill-timeout MIN` | Stop an AI call that runs longer than this (default 90) |
| `--language NAME` | Spoken language (default `English`, `auto` to detect) |
| `--asr-model Qwen/Qwen3-ASR-0.6B` | Faster, slightly less accurate speech model |
| `--plain` | Log lines instead of the TUI |

Press `q` to stop the TUI. Running agent CLIs are stopped too. Finished chunks are cached and the run folder is kept, so running the same command again picks up where it stopped.

## Share or install the skills

```sh
course2skill export                                   # into ./export (the default)
course2skill export --to ~/Desktop/share              # the whole package for a friend or another agent
course2skill export --to ~/Desktop/share --zip        # plus one zip per skill and one for the course
course2skill export --to ~/.claude/skills --flat      # install the skills straight into Claude Code
```

An export holds `skills/` (each skill a self-contained folder), `course/` (transcripts and course files, leave out with `--skills-only`), `knowledge-graph/` and a `README.md` index with install steps. It exports the latest run unless you pass `--run`. Local paths are replaced with neutral placeholders, internal marker files are left out, and `scripts/frame.sh` reads the video location from `COURSE_VIDEO_DIR`, so the receiver can still grab frames if they have the course videos.

## Course layouts

course2skill finds videos and handouts at any depth. With `--layout auto` (the default):

- Chapter folders become one unit each, nested folders included (`Module 1 › Lesson 2`).
- A flat folder with two-level numbering (`2.3 Setup.mp4`, `S2E3`, `Section 2 Lesson 3`) is grouped by the first number.
- Any other flat folder becomes a single unit.

`--layout ai` sends the file list and video lengths to the chosen AI and asks it to group lessons by topic. The result is checked, so every file lands in exactly one unit. Either way the plan is saved as `plan.json` in the run folder. Edit `result/latest/plan.json` and the next run uses your version.

## AI backends

| Backend | How it runs | Setup |
|---|---|---|
| Claude Code | `claude -p`, writes the files itself | `claude` on PATH |
| Codex | `codex exec -s workspace-write` | `codex` on PATH |
| opencode | `opencode run` | `opencode` on PATH |
| Gemini CLI | `gemini -p --approval-mode auto_edit` | `gemini` on PATH |
| Antigravity | `agy -p --mode accept-edits` | `agy` on PATH |
| Anthropic API | Messages API, streaming | `ANTHROPIC_API_KEY`, optional `ANTHROPIC_BASE_URL` |
| OpenAI-compatible API | `/chat/completions` | `OPENAI_API_KEY`, optional `OPENAI_BASE_URL` |
| Local model | Ollama, LM Studio or `mlx_lm.server` | `--ai-url` or `LOCAL_AI_BASE_URL` (default `http://localhost:11434/v1`) |

The coding CLIs work in the skill folder with copies of the transcripts and the original course files, so they can open PDFs and images directly, plus temporary text extractions of PDFs, DOCX and XLSX. The API and local backends get the transcripts and those text extractions in the prompt and return the files as text blocks, which course2skill writes out. The extractions are deleted afterwards; nothing converted is kept in the results. A skill folder gets a `.complete` marker when it is finished, and an unfinished one is redone on the next run. When an API base URL points somewhere other than the provider's own host, the key is also sent as `Authorization: Bearer`.

## How transcription works

1. ffmpeg extracts 16 kHz mono audio.
2. `silencedetect` finds pauses, and the audio is cut into pieces of up to two minutes at those pauses.
3. A Python worker keeps Qwen3-ASR and the forced aligner loaded. It transcribes each piece with the course name, the unit name and terms read from the unit's documents as context, which helps with tool names and jargon. If the worker crashes, the next video starts a fresh one.
4. Each piece is cached as soon as it finishes. The transcript is written once the whole video is done, with the time of the first word at the start of every sentence.

On an M-series Mac the 1.7B model runs at roughly 0.15 to 0.2 times real time, so a 10-hour course takes about 2 hours.

## What goes into a skill

The skill writer works in a fixed order: read every transcript and course file in full, capture screenshots, write complete lesson notes for every video (every step, number, tool, setting, example, warning, opinion, tip, trick, shortcut, aside and resource, in order with timestamps), then write `SKILL.md` as a playbook another LLM can act on, plus topic references and `references/resources.md`. Last, it re-reads every transcript and file to check each detail made it in, adds what is missing, and records the result in `references/coverage.md`.

## Screenshots

While writing a skill, the AI reads every transcript in full and every course file, notes the moments where the screen shows something the words do not (a tool's settings, a report, a spreadsheet, a results page), captures as many frames as it needs, named like `v2-09m12s-google-search-console-pages-report.jpg` (video number, timestamp, what it shows) (every screen state, every walkthrough step), looks at each one, reads the text in it into the notes, retakes it a few seconds either way if it missed, and embeds the good ones in `SKILL.md` and `references/` with a caption and source timestamp. Backends that cannot run commands list the frames they want in `assets/screenshots.tsv`, and course2skill captures them afterwards. Screenshots that no file embeds are removed.

Every skill also has `scripts/frame.sh`:

```sh
scripts/frame.sh "Video_01-Getting_Technical.mp4" 09:11          # one frame
scripts/frame.sh "Video_01-Getting_Technical.mp4" 09:00 10:00 10 # one every 10 s
```

Frames are saved to the skill's `assets/screenshots/`; pass a third argument like `settings.jpg` to name one. A bare video name is looked up anywhere under the section's source folder. Because each transcript sentence has its own timestamp, an agent reading a skill can jump to the exact moment a tool or slide is on screen.

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

The design and plan are in `docs/superpowers/`.
