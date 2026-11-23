//! Builds one agent skill per unit from its transcripts and handouts.

use crate::ai::{self, AiConfig};
use crate::layout::{Chapter, file_name, natural_cmp};
use anyhow::{Context, Result, bail};
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

const PROMPT_TEMPLATE: &str = include_str!("../assets/skill_prompt.md");
const FRAME_SH: &str = include_str!("../assets/frame.sh");
const STAGE: &str = ".input";

/// Removes invisible Unicode used as text watermarks or hidden markup:
/// zero-width characters, bidi controls, tag characters, odd spaces.
pub fn strip_invisible(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c {
            '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
            | '\u{00AD}'
            | '\u{180E}'
            | '\u{E0000}'..='\u{E007F}' => None,
            '\u{00A0}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => {
                Some(' ')
            }
            _ => Some(c),
        })
        .collect()
}

/// Markdown files (transcripts and handouts) produced for this unit.
pub fn chapter_material(result_chapter_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(result_chapter_dir)
        .with_context(|| format!("no results for unit at {}", result_chapter_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    files.sort_by(|a, b| natural_cmp(&file_name(a), &file_name(b)));
    Ok(files)
}

fn fill(course: &str, chapter: &Chapter, inputs: &str, output: &str) -> String {
    PROMPT_TEMPLATE
        .replace("{{COURSE}}", course)
        .replace("{{CHAPTER}}", &chapter.name)
        .replace("{{SLUG}}", &chapter.slug)
        .replace("{{INPUTS}}", inputs.trim_end())
        .replace("{{OUTPUT}}", output)
}

/// Prompt for agent CLIs: inputs are staged under `.input/` in the working folder.
pub fn agent_prompt(course: &str, chapter: &Chapter, material: &[PathBuf]) -> String {
    let mut inputs = String::from(
        "Read every file below completely before writing. They are in the current working folder.\n\nConverted transcripts and handouts:\n",
    );
    for m in material {
        let _ = writeln!(inputs, "- `{STAGE}/material/{}`", file_name(m));
    }
    if !chapter.docs.is_empty() {
        inputs.push_str("\nOriginal handouts, for anything the conversion lost (slide layouts, images in PDFs, spreadsheet structure):\n");
        for d in &chapter.docs {
            let _ = writeln!(inputs, "- `{STAGE}/originals/{}`", file_name(d));
        }
    }
    let output = "Write the skill files into the current working folder (`SKILL.md`, `references/...`). Do not modify anything under `.input/`. When you are done, reply with one line listing the files you wrote.";
    fill(course, chapter, &inputs, output)
}

/// Prompt for text-only backends: material is inlined and files come back as blocks.
pub fn text_prompt(course: &str, chapter: &Chapter, material: &[(String, String)]) -> String {
    let mut inputs = String::from("The full material follows, one file per block.\n");
    for (name, body) in material {
        let _ = write!(
            inputs,
            "\n<file name=\"{name}\">\n{}\n</file>\n",
            body.trim_end()
        );
    }
    let output = "## Output format\n\nReply with the files only, each wrapped exactly like this, and nothing outside the blocks:\n\n=== FILE: SKILL.md ===\n(file content)\n=== END FILE ===\n=== FILE: references/topic-name.md ===\n(file content)\n=== END FILE ===\n";
    fill(course, chapter, &inputs, output)
}

fn sources_md(chapter: &Chapter, material: &[PathBuf]) -> String {
    let mut md = format!("# Sources for {}\n\n## Videos\n\n", chapter.name);
    for v in &chapter.videos {
        let _ = writeln!(md, "- `{}` (full path: `{}`)", file_name(v), v.display());
    }
    md.push_str("\n## Handouts\n\n");
    for d in &chapter.docs {
        let _ = writeln!(md, "- `{}` (full path: `{}`)", file_name(d), d.display());
    }
    md.push_str("\n## Converted material\n\n");
    for m in material {
        let _ = writeln!(md, "- `{}`", m.display());
    }
    md
}

/// Writes scaffolding, has the chosen AI author the skill, then cleans the text.
pub fn synthesize(
    course: &str,
    chapter: &Chapter,
    result_chapter_dir: &Path,
    skills_root: &Path,
    ai_cfg: &AiConfig,
) -> Result<PathBuf> {
    let material = chapter_material(result_chapter_dir)?;
    if material.is_empty() {
        bail!("unit {} has no transcripts or handouts yet", chapter.name);
    }
    let skill_dir = skills_root.join(&chapter.slug);
    std::fs::create_dir_all(skill_dir.join("scripts"))?;
    std::fs::create_dir_all(skill_dir.join("references"))?;
    std::fs::write(skill_dir.join("sources.md"), sources_md(chapter, &material))?;
    let frame = skill_dir.join("scripts/frame.sh");
    std::fs::write(
        &frame,
        FRAME_SH.replace("__VIDEO_DIR__", &chapter.video_dir.display().to_string()),
    )?;
    set_executable(&frame)?;

    if ai_cfg.backend.is_agent() {
        let stage = skill_dir.join(STAGE);
        stage_inputs(&stage, &material, &chapter.docs)?;
        let result = ai::run_agent(
            ai_cfg,
            &agent_prompt(course, chapter, &material),
            &skill_dir,
        );
        let _ = std::fs::remove_dir_all(&stage);
        let reply = result?;
        if !skill_dir.join("SKILL.md").exists() {
            bail!(
                "{} finished but wrote no SKILL.md: {}",
                ai_cfg.backend.label(),
                reply.trim()
            );
        }
    } else {
        let mut inline = Vec::new();
        for m in &material {
            inline.push((file_name(m), std::fs::read_to_string(m)?));
        }
        let answer = ai::complete(ai_cfg, &text_prompt(course, chapter, &inline), &skill_dir)?;
        let files = ai::parse_file_blocks(&answer);
        if !files.iter().any(|(p, _)| p == "SKILL.md") {
            std::fs::write(skill_dir.join("_raw_answer.md"), &answer)?;
            bail!(
                "the answer had no SKILL.md block (raw answer saved to {})",
                skill_dir.join("_raw_answer.md").display()
            );
        }
        for (rel, body) in files {
            let Some(target) = safe_output(&skill_dir, &rel) else {
                continue;
            };
            if let Some(dir) = target.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(target, body)?;
        }
    }
    clean_tree(&skill_dir)?;
    Ok(skill_dir)
}

fn stage_inputs(stage: &Path, material: &[PathBuf], docs: &[PathBuf]) -> Result<()> {
    let _ = std::fs::remove_dir_all(stage);
    std::fs::create_dir_all(stage.join("material"))?;
    for m in material {
        std::fs::copy(m, stage.join("material").join(file_name(m)))?;
    }
    if !docs.is_empty() {
        std::fs::create_dir_all(stage.join("originals"))?;
        for d in docs {
            std::fs::copy(d, stage.join("originals").join(file_name(d)))?;
        }
    }
    Ok(())
}

/// Only `.md` files inside the skill folder, never the scaffolding we wrote.
fn safe_output(skill_dir: &Path, rel: &str) -> Option<PathBuf> {
    let p = Path::new(rel.trim());
    let ok = p.components().all(|c| matches!(c, Component::Normal(_)))
        && p.extension().is_some_and(|e| e == "md")
        && p != Path::new("sources.md");
    ok.then(|| skill_dir.join(p))
}

fn clean_tree(dir: &Path) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let p = entry?.path();
        if p.is_dir() {
            clean_tree(&p)?;
        } else if p.extension().is_some_and(|e| e == "md") {
            let s = std::fs::read_to_string(&p)?;
            let cleaned = strip_invisible(&s);
            if cleaned != s {
                std::fs::write(&p, cleaned)?;
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_executable(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chapter() -> Chapter {
        Chapter {
            name: "05-On-Page SEO".into(),
            slug: "05-on-page-seo".into(),
            out_rel: "05-On-Page SEO".into(),
            video_dir: "/c/05-On-Page SEO".into(),
            videos: vec![],
            docs: vec!["/c/05/a.pdf".into()],
        }
    }

    #[test]
    fn strip_removes_invisible_and_keeps_text() {
        assert_eq!(strip_invisible("a\u{200B}b\u{202E}c\u{E0041}"), "abc");
        assert_eq!(strip_invisible("café 🚀\nx\u{00A0}y"), "café 🚀\nx y");
    }

    #[test]
    fn agent_prompt_points_at_staged_inputs() {
        let p = agent_prompt("C", &chapter(), &["/r/05/Video_01.md".into()]);
        assert!(p.contains("name: 05-on-page-seo"));
        assert!(p.contains(".input/material/Video_01.md"));
        assert!(p.contains(".input/originals/a.pdf"));
        assert!(p.contains("scripts/frame.sh"));
        assert!(!p.contains("{{"));
    }

    #[test]
    fn text_prompt_inlines_material_and_format() {
        let p = text_prompt("C", &chapter(), &[("v.md".into(), "hello".into())]);
        assert!(p.contains("<file name=\"v.md\">\nhello\n</file>"));
        assert!(p.contains("=== FILE: SKILL.md ==="));
    }

    #[test]
    fn safe_output_blocks_escapes_and_scaffolding() {
        let d = Path::new("/s");
        assert!(safe_output(d, "references/a.md").is_some());
        assert!(safe_output(d, "../x.md").is_none());
        assert!(safe_output(d, "/etc/x.md").is_none());
        assert!(safe_output(d, "scripts/frame.sh").is_none());
        assert!(safe_output(d, "sources.md").is_none());
    }
}
