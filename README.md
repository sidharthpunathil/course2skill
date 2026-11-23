# course2skill

course2skill turns a folder of course videos into timestamped markdown transcripts, then into one agent skill per unit. Transcription runs locally on Apple Silicon with Qwen3-ASR. An AI of your choice writes the skills.

## What it produces

```
result/
  .cache/                      speech-to-text cache shared by every run
  2026-11-23_06-30-10/         one generation
    plan.json                  how files were grouped into units (editable)
    01-Introduction/
      Video_01-Welcome_to_Compact_Keywords.md   transcript, a [mm:ss] on every sentence
      01-Welcome to Compact Keywords.md         handout converted to markdown
    skills/
      01-introduction/
        SKILL.md
        references/*.md
        sources.md             every video and handout, with full paths
        scripts/frame.sh       screenshots from the source video
  latest -> 2026-11-23_06-30-10
```

Each run gets a new timestamped folder. Transcribed audio is cached in `result/.cache`, so a later run reuses it and only redoes the skills.

The stages run in order: handouts are converted, then every video is transcribed and saved, then the skills are written.

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
course2skill run course/                    # handouts, transcripts, then skills
course2skill transcribe course/             # handouts and transcripts only
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
| `--run latest\|NAME` | Continue an existing run instead of starting a new one |
| `--asr-model Qwen/Qwen3-ASR-0.6B` | Faster, slightly less accurate speech model |
| `--plain` | Log lines instead of the TUI |

Press `q` to stop the TUI. Finished chunks are cached, so running the same command again picks up where it stopped.

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

The coding CLIs work in the skill folder with copies of the transcripts and the original handouts, so they can open PDFs directly. The API and local backends get the material in the prompt and return the files as text blocks, which course2skill writes out. When an API base URL points somewhere other than the provider's own host, the key is also sent as `Authorization: Bearer`.

## How transcription works

1. ffmpeg extracts 16 kHz mono audio.
2. `silencedetect` finds pauses, and the audio is cut into pieces of up to two minutes at those pauses.
3. A Python worker keeps Qwen3-ASR and the forced aligner loaded. It transcribes each piece with the course name, the unit name and terms from the unit's handouts as context, which helps with tool names and jargon.
4. Each piece is cached as soon as it finishes. The transcript is written once the whole video is done, with the time of the first word at the start of every sentence.

On an M-series Mac the 1.7B model runs at roughly 0.15 to 0.2 times real time, so a 10-hour course takes about 2 hours.

## Screenshots

Every skill has `scripts/frame.sh`:

```sh
scripts/frame.sh "Video_01-Getting_Technical.mp4" 09:11          # one frame
scripts/frame.sh "Video_01-Getting_Technical.mp4" 09:00 10:00 10 # one every 10 s
```

Frames are saved to `./frames/`. Because each transcript sentence has its own timestamp, an agent reading a skill can jump to the exact moment a tool or slide is on screen.

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

The design and plan are in `docs/superpowers/`.
