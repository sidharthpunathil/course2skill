You are turning one section of a video course into a single, self-contained agent skill, with screenshots from the videos where the screen matters.

Course: {{COURSE}}
Section: {{CHAPTER}}
Skill name (use exactly): {{SLUG}}

## Your inputs

{{INPUTS}}

Transcripts come from local speech recognition, so expect some misheard words, especially names of tools, brands and jargon. Fix them when the context or the course files make the right word clear. Every transcript sentence starts with `[mm:ss]`, the moment it is spoken in the video. Course files may include the instructor's own outline with timestamps: use it to structure the skill.

## How to work

1. Inspect everything first. Read every transcript completely, start to end, and open every course file. Do not skim or sample.
2. Find the visual moments. While reading, note each moment where what is on screen carries information the words do not: a tool's interface or settings, a report, a spreadsheet layout, a search results page, a page being built, a before/after, a chart, a slide. Phrases like "as you can see", "right here", "this page", "click on" are strong signals.
3. Capture screenshots. {{SCREENSHOTS}}
4. Write the skill (below), placing each kept screenshot next to the text it supports.

## What to write

All paths are relative to the skill folder (the current working folder).

1. `SKILL.md`
   - YAML frontmatter with exactly two keys: `name: {{SLUG}}` and `description:`. The description says what the skill does and when to use it, in one paragraph with the concrete tasks, questions and keywords that should trigger it. Keep it under 1024 characters.
   - Body: the working knowledge of this section, written so an agent can do the work with no access to the videos. Keep everything that teaches: every procedure step by step, every rule of thumb, numbers and thresholds, named tools and how they are used, worked examples and case-study figures, templates and how to fill them, mistakes to avoid, and the reasoning behind each recommendation. Preserve the instructor's specific advice and opinions, attributed as theirs. Do not summarize detail away. Keep `SKILL.md` as the core playbook (aim for under 500 lines) and move deep material into `references/`.
   - Embed screenshots as `![What it shows](assets/screenshots/name.jpg)` followed by a one-line caption with the source, e.g. `*Google Search Console, Pages report (Video_01-Getting_Technical.mp4 [09:12])*`.
   - Cite where facts come from as `(Video_..._video2.mp4 [12:34])` or `(file: name.pdf)`.
   - A section `## Looking at the video` explaining that for any moment not captured, run `scripts/frame.sh "<video file>" <mm:ss>` (saves into `assets/screenshots/`) or `scripts/frame.sh "<video file>" <from> <to> [step]`, then view the image. `sources.md` lists every video.
   - A section `## Course files` listing what is in `assets/files/` and when to open each one.
   - A short `## Reference files` list linking each file in `references/` with one line on when to open it.
2. `references/*.md`
   - Topic files for detail that does not fit in SKILL.md: full templates (rebuild spreadsheet columns and example rows as markdown tables), step-by-step walkthroughs with their screenshots, checklists, long examples, case-study numbers, and a glossary of terms used in the section. Reference images from there as `../assets/screenshots/name.jpg`.
   - Do not copy raw transcripts. Rewrite them into organized knowledge.

`scripts/frame.sh`, `sources.md` and `assets/files/` already exist. Do not change them.

## Writing rules

- Plain, direct English. Imperative voice for instructions.
- Never use the em dash character. Use commas, colons, parentheses or periods.
- No filler, no hype words, no "in conclusion". If a skill named `humanizer` is available to you, follow it for the prose.
- Do not invent facts that are not in the material. If something is unclear in the source, say so.

{{OUTPUT}}
