//! Builds the knowledge-base README of an export: one entry point an AI agent
//! can be handed to find every answer in the course. It is generated from the
//! exported skills (descriptions, reference lists, lesson notes, glossaries) and
//! the knowledge graph report.

use crate::layout::{file_name, natural_cmp};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

struct SkillInfo {
    slug: String,
    title: String,
    description: String,
    references: Vec<(String, String)>,
    lessons: Vec<(String, String)>,
    screenshots: usize,
    files: Vec<String>,
}

/// Writes README.md and AGENTS.md into an export folder.
pub fn write(dest: &Path, course: &str, sections: &[(String, String)]) -> std::io::Result<()> {
    let pretty = pretty_name(course);
    let course = pretty.as_str();
    let skills: Vec<SkillInfo> = sections
        .iter()
        .filter_map(|(slug, name)| read_skill(dest, slug, name))
        .collect();
    let has_course = dest.join("course").is_dir();
    let has_graph = dest.join("knowledge-graph").is_dir();
    std::fs::write(
        dest.join("README.md"),
        readme(dest, course, &skills, has_course, has_graph),
    )?;
    std::fs::write(dest.join("AGENTS.md"), agents_md(course))?;
    Ok(())
}

fn read_skill(dest: &Path, slug: &str, section: &str) -> Option<SkillInfo> {
    let dir = dest.join("skills").join(slug);
    let md = std::fs::read_to_string(dir.join("SKILL.md")).ok()?;
    let title = md
        .lines()
        .find_map(|l| l.strip_prefix("# "))
        .unwrap_or(section)
        .trim()
        .to_string();
    let references = reference_lines(&section_body(&md, "Reference files"))
        .into_iter()
        .filter(|(p, _)| !p.starts_with("references/lessons/"))
        .collect();
    let mut lessons: Vec<(String, String)> = std::fs::read_dir(dir.join("references/lessons"))
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .map(|p| {
            let heading = std::fs::read_to_string(&p)
                .ok()
                .and_then(|t| {
                    t.lines()
                        .find_map(|l| l.strip_prefix("# ").map(str::to_string))
                })
                .unwrap_or_default();
            (
                format!("references/lessons/{}", file_name(&p)),
                heading
                    .trim_start_matches("Lesson notes:")
                    .trim()
                    .to_string(),
            )
        })
        .collect();
    lessons.sort_by(|a, b| natural_cmp(&a.0, &b.0));
    let count = |sub: &str| {
        std::fs::read_dir(dir.join(sub))
            .map(|r| r.count())
            .unwrap_or(0)
    };
    let mut files: Vec<String> = std::fs::read_dir(dir.join("assets/files"))
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| file_name(&e.path())))
        .collect();
    files.sort_by(|a, b| natural_cmp(a, b));
    Some(SkillInfo {
        slug: slug.to_string(),
        title,
        description: frontmatter_description(&md),
        references,
        lessons,
        screenshots: count("assets/screenshots"),
        files,
    })
}

/// Body of `## <heading>` up to the next `## `.
fn section_body(md: &str, heading: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for l in md.lines() {
        if let Some(h) = l.strip_prefix("## ") {
            inside = h.trim().eq_ignore_ascii_case(heading);
            continue;
        }
        if inside {
            out.push_str(l);
            out.push('\n');
        }
    }
    out
}

/// `- `path`: what` or `- [path](path): what` lines.
fn reference_lines(body: &str) -> Vec<(String, String)> {
    body.lines()
        .filter_map(|l| {
            let l = l.trim().strip_prefix("- ")?;
            let (path, rest) = if let Some(r) = l.strip_prefix('`') {
                let (p, rest) = r.split_once('`')?;
                (p.to_string(), rest)
            } else {
                let r = l.strip_prefix('[')?;
                let (_, rest) = r.split_once("](")?;
                let (p, rest) = rest.split_once(')')?;
                (p.to_string(), rest)
            };
            let what = rest.trim_start_matches([':', ' ', '-']).trim().to_string();
            Some((path, what))
        })
        .collect()
}

/// `**Term**: definition` pairs from a glossary section (several per line allowed).
fn glossary_terms(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in body.lines() {
        let parts: Vec<&str> = line.split("**").collect();
        let mut i = 1;
        while i + 1 < parts.len() {
            let term = parts[i].trim();
            if let Some(def) = parts[i + 1].trim_start().strip_prefix(':') {
                let def = def.trim().trim_end_matches('.').to_string();
                if !term.is_empty() && term.len() < 80 {
                    out.push((term.trim_end_matches(':').to_string(), def));
                }
            }
            i += 2;
        }
    }
    out
}

fn frontmatter_description(md: &str) -> String {
    let mut lines = md.lines().skip_while(|l| l.trim() != "---").skip(1);
    let mut out = String::new();
    let mut in_desc = false;
    for l in lines.by_ref() {
        if l.trim() == "---" {
            break;
        }
        if let Some(rest) = l.strip_prefix("description:") {
            in_desc = true;
            let rest = rest.trim();
            if !matches!(rest, ">" | "|" | ">-" | "|-") {
                out.push_str(rest.trim_matches('"'));
            }
        } else if in_desc && l.starts_with([' ', '\t']) {
            out.push(' ');
            out.push_str(l.trim());
        } else {
            in_desc = false;
        }
    }
    out.trim().to_string()
}

/// "Edward.Sturm.Compact_Keywords" -> "Edward Sturm Compact Keywords".
fn pretty_name(s: &str) -> String {
    s.split(['.', '_'])
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("{n} {word}")
    } else {
        format!("{n} {word}s")
    }
}

fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

fn readme(
    dest: &Path,
    course: &str,
    skills: &[SkillInfo],
    has_course: bool,
    has_graph: bool,
) -> String {
    let mut md = String::new();
    let shots: usize = skills.iter().map(|s| s.screenshots).sum();
    let lessons: usize = skills.iter().map(|s| s.lessons.len()).sum();
    let _ = writeln!(md, "# {course}: knowledge base\n");
    let _ = writeln!(
        md,
        "> **AI agents: start here.** This repository is the complete, source-cited knowledge of the course \"{course}\": {} sections as agent skills, notes for all {lessons} videos, {shots} screenshots, the original course files{}{}. This file tells you where every answer lives. Read \"How to use this knowledge base\", then go straight to the section or term you need.\n",
        skills.len(),
        if has_course { ", every transcript" } else { "" },
        if has_graph {
            " and a knowledge graph of the whole course"
        } else {
            ""
        },
    );

    md.push_str("## How to use this knowledge base\n\n");
    md.push_str("1. **Find the section.** Use the course map below, or look the term up in the term index at the end. Each section is one skill folder in `skills/`.\n");
    md.push_str("2. **Read that skill's `SKILL.md` first.** It is the playbook: when to use it, core principles, step-by-step procedures with explicit conditions, numbers and thresholds, checklists, common mistakes, tips and tricks, worked examples, glossary and related sections.\n");
    md.push_str("3. **Go deeper only as needed.** Topic references (templates rebuilt as tables, walkthroughs, case studies) are listed under each section below. `references/lessons/` has complete, ordered, timestamped notes for every video: use them for anything the playbook summarizes, and for exact steps shown in a video. `references/resources.md` lists every tool, site and person mentioned. `references/coverage.md` says what was unclear in the source.\n");
    if has_course {
        md.push_str("4. **Quote exact wording from the transcripts** in `course/<section>/<video>.md`. Every sentence starts with `[mm:ss]`, the moment it is spoken.\n");
    } else {
        md.push_str("4. **Exact wording** is quoted in the lesson notes with timestamps.\n");
    }
    md.push_str("5. **Look at the screen** through `assets/screenshots/` in each skill. File names encode the video, the time and the content (`v2-09m12s-google-search-console-pages-report.jpg`), and the text visible in each screenshot is written into the lesson notes, so you can use it without viewing images.\n");
    md.push_str("6. **Use the original course files** (handouts, templates, spreadsheets) in each skill's `assets/files/`.\n");
    if has_graph {
        md.push_str("7. **Answer questions that span sections** with `knowledge-graph/GRAPH_REPORT.md` (main concepts and how sections connect), `knowledge-graph/wiki/` (one article per topic) and the \"Related sections\" part of each `SKILL.md`.\n");
    }
    md.push_str("\nWhen answering: cite sources the way the skills do, as `(Video_name.mp4 [mm:ss])` or `(file: name.pdf)`; keep the instructor's claims and opinions attributed to the instructor; keep numbers exactly as stated; and say so when the source is unclear instead of guessing. For end-to-end work, follow the sections in course order.\n\n");

    md.push_str("## Course map\n\nSections in teaching order. Each heading links to the skill's playbook.\n\n");
    for (i, s) in skills.iter().enumerate() {
        let _ = writeln!(
            md,
            "### {}. [{}](skills/{}/SKILL.md)\n",
            i + 1,
            s.title,
            s.slug
        );
        let _ = writeln!(
            md,
            "Skill `{}` · {} · {}{}\n",
            s.slug,
            plural(s.lessons.len(), "video"),
            plural(s.screenshots, "screenshot"),
            if s.files.is_empty() {
                String::new()
            } else {
                format!(" · files: {}", s.files.join(", "))
            }
        );
        if !s.description.is_empty() {
            let _ = writeln!(md, "{}\n", s.description);
        }
        if !s.references.is_empty() {
            md.push_str("Open for depth:\n\n");
            for (p, what) in &s.references {
                let _ = writeln!(md, "- [`{p}`](skills/{}/{p}): {what}", s.slug);
            }
            md.push('\n');
        }
        if !s.lessons.is_empty() {
            md.push_str("Lesson notes:\n\n");
            for (p, title) in &s.lessons {
                let label = if title.is_empty() {
                    file_name(Path::new(p))
                } else {
                    title.clone()
                };
                let _ = writeln!(md, "- [{label}](skills/{}/{p})", s.slug);
            }
            md.push('\n');
        }
    }

    if has_graph
        && let Ok(report) = std::fs::read_to_string(dest.join("knowledge-graph/GRAPH_REPORT.md"))
    {
        let hubs: Vec<String> = section_body(&report, "Community Hubs (Navigation)")
            .lines()
            .filter_map(|l| l.trim().strip_prefix("- ").map(str::to_string))
            .collect();
        if !hubs.is_empty() {
            md.push_str("## Topics across the course\n\nTopic clusters found by the knowledge graph. Each has an article in `knowledge-graph/wiki/`; `knowledge-graph/GRAPH_REPORT.md` explains how they connect, and `knowledge-graph/graph.html` shows the graph in a browser.\n\n");
            let _ = writeln!(md, "{}\n", hubs.join(" · "));
        }
    }

    // Term index from every skill's glossary.
    let mut terms: BTreeMap<String, (String, String, Vec<String>)> = BTreeMap::new();
    for s in skills {
        let md_text = std::fs::read_to_string(dest.join("skills").join(&s.slug).join("SKILL.md"))
            .unwrap_or_default();
        for (term, def) in glossary_terms(&section_body(&md_text, "Glossary")) {
            let entry = terms
                .entry(term.to_lowercase())
                .or_insert_with(|| (term.clone(), def.clone(), Vec::new()));
            if !entry.2.contains(&s.slug) {
                entry.2.push(s.slug.clone());
            }
        }
    }
    if !terms.is_empty() {
        md.push_str("## Term index\n\nEvery term defined in the skills' glossaries, with the sections that use it.\n\n| Term | Meaning | Sections |\n|---|---|---|\n");
        for (term, def, slugs) in terms.values() {
            let links: Vec<String> = slugs
                .iter()
                .map(|sl| format!("[{sl}](skills/{sl}/SKILL.md)"))
                .collect();
            let short: String = def.chars().take(160).collect();
            let more = if def.chars().count() > 160 { "..." } else { "" };
            let _ = writeln!(
                md,
                "| {} | {}{more} | {} |",
                cell(term),
                cell(&short),
                links.join(", ")
            );
        }
        md.push('\n');
    }

    md.push_str("## Folder layout\n\n```\nREADME.md               this index (start here)\nAGENTS.md               short instructions for coding agents\nskills/<section>/       one agent skill per section\n  SKILL.md              the playbook\n  references/           topic references, resources.md, coverage.md\n  references/lessons/   complete timestamped notes for every video\n  assets/screenshots/   frames from the videos (video, time and content in the name)\n  assets/files/         the section's original course files\n  scripts/frame.sh      grab more frames (needs the videos, set COURSE_VIDEO_DIR)\n");
    if has_course {
        md.push_str(
            "course/<section>/       transcripts ([mm:ss] per sentence) and course files\n",
        );
    }
    if has_graph {
        md.push_str("knowledge-graph/        GRAPH_REPORT.md, wiki/, graph.json, graph.html\n");
    }
    md.push_str("```\n\n");

    md.push_str("## Install as agent skills\n\n```sh\n# Claude Code (all projects)\ncp -R skills/* ~/.claude/skills/\n\n# Claude Code (one project)\nmkdir -p .claude/skills && cp -R skills/* .claude/skills/\n\n# Codex\nmkdir -p ~/.codex/skills && cp -R skills/* ~/.codex/skills/\n```\n\nOr give an agent this README (or the repository) and let it read what it needs.\n\nMade with [course2skill](https://github.com/sidharthpunathil/course2skill).\n");
    md
}

fn agents_md(course: &str) -> String {
    format!(
        "# Instructions for AI agents\n\nThis repository is a knowledge base of the course \"{course}\". Read `README.md` first: it maps every section, reference file, lesson and term to its location.\n\n- For a question, find the section in the README's course map or term index, read that skill's `SKILL.md`, then open the reference or lesson notes it points to.\n- Quote exact wording from `course/<section>/<video>.md` (every sentence has its `[mm:ss]`).\n- Cite sources as `(Video_name.mp4 [mm:ss])` or `(file: name.pdf)`. Keep the instructor's claims attributed and numbers exact. Say when the source is unclear.\n- Do not modify files in this repository unless asked.\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_lines_parse_both_styles() {
        let r = reference_lines(
            "- `references/a.md`: the A file.\n- [references/b.md](references/b.md): the B file.\nprose\n",
        );
        assert_eq!(
            r,
            vec![
                ("references/a.md".into(), "the A file.".into()),
                ("references/b.md".into(), "the B file.".into())
            ]
        );
    }

    #[test]
    fn glossary_terms_parse_multiple_per_line() {
        let g = glossary_terms(
            "- **BOFU / bottom of funnel**: ready to buy. **TOFU**: informational.\n- **DA**: Moz score.\n",
        );
        let names: Vec<&str> = g.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(names, ["BOFU / bottom of funnel", "TOFU", "DA"]);
        assert_eq!(g[1].1, "informational");
    }

    #[test]
    fn pretty_name_spaces_dots_and_underscores() {
        assert_eq!(
            pretty_name("Edward.Sturm.Compact_Keywords"),
            "Edward Sturm Compact Keywords"
        );
    }

    #[test]
    fn section_body_stops_at_next_heading() {
        assert_eq!(section_body("## A\nx\n## B\ny\n", "a"), "x\n");
    }
}
