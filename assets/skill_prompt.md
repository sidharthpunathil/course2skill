You are turning one unit of a video course into a single, self-contained agent skill.

Course: {{COURSE}}
Unit: {{CHAPTER}}
Skill name (use exactly): {{SLUG}}

## Your inputs

{{INPUTS}}

Transcripts come from local speech recognition, so expect some misheard words, especially names of tools, brands and jargon. Fix them when the context or the handouts make the right word clear. Each transcript passage starts with a `[mm:ss]` marker. Handouts may include the instructor's own outline with timestamps: use it to structure the skill.

## What to write

1. `SKILL.md`
   - YAML frontmatter with exactly two keys: `name: {{SLUG}}` and `description:`. The description says what the skill does and when to use it, in one paragraph with the concrete tasks, questions and keywords that should trigger it. Keep it under 1024 characters.
   - Body: the working knowledge of this unit, written so an agent can do the work with no access to the videos. Keep everything that teaches: every procedure step by step, every rule of thumb, numbers and thresholds, named tools and how they are used, worked examples and case-study figures, templates and how to fill them, mistakes to avoid, and the reasoning behind each recommendation. Preserve the instructor's specific advice and opinions, attributed as theirs. Do not summarize detail away. If it is long, keep `SKILL.md` as the core playbook (aim for under 500 lines) and move deep material into `references/`.
   - Cite where things come from as `(Video_..._video2.mp4 [12:34])` or `(handout: name.pdf)` so a reader can check the source.
   - A section `## Looking at the video` explaining that when on-screen detail matters (a tool's UI, a spreadsheet, a slide, a search result page), run `scripts/frame.sh "<video file>" <mm:ss>` or `scripts/frame.sh "<video file>" <from> <to> [step]` and read the saved images in `./frames/`. `sources.md` lists every video. Name the specific moments in this unit where looking is most useful.
   - A short `## Reference files` list linking each file in `references/` with one line on when to open it.
2. `references/*.md`
   - Topic files for detail that does not fit in SKILL.md: full templates (rebuild spreadsheet columns and example rows as markdown tables), checklists, long examples, case-study numbers, a glossary of terms used in the unit.
   - Do not copy raw transcripts. Rewrite them into organized knowledge.

`scripts/frame.sh` and `sources.md` already exist. Do not write or change them.

## Writing rules

- Plain, direct English. Imperative voice for instructions.
- Never use the em dash character. Use commas, colons, parentheses or periods.
- No filler, no hype words, no "in conclusion". If a skill named `humanizer` is available to you, follow it for the prose.
- Do not invent facts that are not in the material. If something is unclear in the source, say so.

{{OUTPUT}}
