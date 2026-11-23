You are turning one section of a video course into a single, self-contained agent skill. Another LLM will load this skill later and must be able to do the work the section teaches using only what you write, so completeness and precision matter more than brevity. The devil is in the detail: a number, a setting, a tool name, a caveat or an exception that you drop is knowledge the reader loses for good.

Course: {{COURSE}}
Section: {{CHAPTER}}
Skill name (use exactly): {{SLUG}}

## Your inputs

{{INPUTS}}

Transcripts come from local speech recognition, so expect some misheard words, especially names of tools, brands and jargon. Fix them when the context or the course files make the right word clear; when you cannot tell, keep the word and mark it `(unclear in audio)`. Every transcript sentence starts with `[mm:ss]`, the moment it is spoken in the video. Course files may include the instructor's own outline with timestamps: use it to structure the skill.

## How to work

Work in this order. Do not skip steps and do not sample: read everything in full.

1. Inspect everything. Read every transcript completely, start to end, and open every course file (spreadsheets sheet by sheet, documents page by page).
2. Find the visual moments. While reading, note each moment where what is on screen carries information the words do not: a tool's interface or settings, a report, a spreadsheet layout, a search results page, a page being built, a before/after, a chart, a slide. Phrases like "as you can see", "right here", "this page", "click on", "look at this" are strong signals.
3. Capture screenshots. {{SCREENSHOTS}}
4. Write the lesson notes (below) for every video, in order. This is where nothing gets lost.
5. Write `SKILL.md` and the topic references from the notes, the course files and the screenshots.
6. Audit coverage (below) and fix every gap you find.

## What to write

All paths are relative to the skill folder (the current working folder).

### 1. `references/lessons/<video file name without extension>.md` (one per video)

Complete, ordered notes of what the video teaches, so a reader who never watches it misses nothing.
- Follow the video's order. Start each topic with its timestamp, like `### [04:12] Setting up the sitemap`.
- Capture every concrete detail: every step and its order, every number, threshold, price, date, count and duration, every tool, website, setting, menu path, button and field named, every example (the keyword, the page, the result it got), every rule of thumb, every warning, exception and "don't do this", every opinion and recommendation (attributed: "Sturm recommends...", "Sturm's view is..."), and every result or claim the instructor reports (as reported).
- Capture the extras too, the things said in passing that are easy to skip: tips and tricks, shortcuts and time savers, hacks and workarounds, hidden or lesser-known features of a tool, "pro tip" and "bonus" remarks, personal stories and what they taught, side comments about the industry or competitors, predictions, recommended tools, books, people, sites and communities, and anything the instructor says he does differently from common advice. Mark each one in the notes with `Tip:`, `Trick:`, `Resource:` or `Aside:` so it is easy to find.
- Rewrite speech into clear written form: remove filler and repetition, but never merge away a distinct point. When the instructor repeats something for emphasis, note that it is emphasized.
- Embed the screenshots that belong to each topic, right where they apply.
- End with `## Key takeaways` (the points the instructor stresses most) and `## Open questions` (anything unclear, cut off or inaudible in the source).

### 2. `SKILL.md`

The entry point an LLM reads first. It must let the reader act correctly without opening the videos; it points to the lesson notes and topic references for depth.
- YAML frontmatter with exactly two keys: `name: {{SLUG}}` and `description:`. The description says what the skill does and when to use it, in one paragraph with the concrete tasks, questions and keywords that should trigger it. Keep it under 1024 characters.
- Then these sections, in this order (leave out a section only if the source truly has nothing for it):
  - `# <Section title>` and a short paragraph: what this section teaches and the outcome it produces.
  - `## When to use this skill`: concrete situations and requests, and when not to use it (point to the related section instead if known).
  - `## Core principles`: the instructor's main ideas and the reasoning behind each, stated so the reader can apply them to new cases.
  - `## Procedures`: each task as numbered steps, in order, with exact inputs, tools, settings and expected results. Write decisions as explicit conditions ("If the page has fewer than X ..., then ..."). Embed the screenshots that show a step next to that step.
  - `## Numbers and thresholds`: a table of every figure the section gives (value, what it applies to, source timestamp).
  - `## Checklists`: pre-flight and review checklists the reader can run through.
  - `## Common mistakes`: what goes wrong, why, and how to avoid or fix it.
  - `## Tips, tricks and extras`: every tip, trick, shortcut, hack, hidden feature, "pro tip", contrarian view and useful aside from the section, each with its source timestamp. Nothing here is too small to include.
  - `## Worked examples`: the real examples and case figures from the section, with their numbers.
  - `## Glossary`: every term or abbreviation the section uses, defined in one line.
  - `## Looking at the video`: for any moment not captured, run `scripts/frame.sh "<video file>" <mm:ss>` (saves into `assets/screenshots/`) or `scripts/frame.sh "<video file>" <from> <to> [step]`, then view the image. `sources.md` lists every video. Name the moments in this section where looking helps most.
  - `## Course files`: what is in `assets/files/` and when to open each one; for templates, how to fill them in, column by column.
  - `## Reference files`: a list linking every file in `references/` (including each lesson-notes file) with one line on when to open it.
- Keep `SKILL.md` focused and actionable (aim for under 500 lines); when something needs more room, give the short version in `SKILL.md` and the full version in a topic reference, and link to it. Never drop it.
- Embed screenshots as `![What it shows](assets/screenshots/v2-09m12s-tool-what-it-shows.jpg)` followed by a one-line italic caption with the source, e.g. `*Google Search Console, Pages report (Video_01-Getting_Technical.mp4 [09:12])*`.
- Cite where facts come from as `(Video_..._video2.mp4 [12:34])` or `(file: name.pdf)`, so every claim can be checked.

### 3. `references/<topic>.md` (as many as the section needs)

Deep material by topic: full templates (rebuild every spreadsheet's columns, formulas and example rows as markdown tables and explain each column), step-by-step walkthroughs with their screenshots, long checklists, case-study numbers, comparison tables. Reference images from there as `../assets/screenshots/name.jpg`. Do not paste raw transcripts anywhere; rewrite them into organized knowledge.

### 4. `references/resources.md`

Every tool, website, app, plugin, template, book, person, community and link the section mentions, as a table: name, what it is, what the instructor uses it for or says about it (including pricing or plan if mentioned), and source timestamp.

### 5. Coverage audit, then `references/coverage.md`

After writing, go back through every transcript from start to end, and through every course file, and check each concrete detail, tip, trick, aside and resource against what you wrote (lesson notes, `SKILL.md`, topic references and `references/resources.md`). Add anything missing to the right place. Then write `references/coverage.md`: for each video and each course file, one line saying it was fully covered, how many details the audit pass added, and any parts that could not be used (unclear audio, cut-off sentences) with their timestamps.

`scripts/frame.sh`, `sources.md` and `assets/files/` already exist. Do not change them.

## Writing rules for an LLM reader

- Make every section understandable on its own: name things explicitly instead of "this", "it" or "the above", and repeat the key term rather than relying on a previous paragraph.
- Define each term once in the glossary and use it the same way everywhere.
- Keep exact values exactly as stated (numbers, units, names, settings, quotes). Never round, generalize or "improve" them.
- Separate what the instructor claims or reports ("Sturm reports 415 words on average") from general facts, and attribute opinions.
- Prefer lists, numbered steps and tables over long paragraphs; use explicit if/then for decisions.
- Plain, direct English. Imperative voice for instructions.
- Never use the em dash character. Use commas, colons, parentheses or periods.
- No filler, no hype words, no "in conclusion". If a skill named `humanizer` is available to you, follow it for the prose.
- Do not invent facts that are not in the material. If something is unclear in the source, say so.

{{OUTPUT}}
