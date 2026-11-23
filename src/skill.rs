//! Builds one agent skill per unit from its transcripts and handouts.

use crate::ai::{self, AiConfig};
use crate::layout::{Chapter, file_name};
use anyhow::{Result, bail};
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

/// Marker written when a skill finished; a folder without it is redone.
pub const COMPLETE: &str = ".complete";

/// What a unit's skill is built from, all inside the unit's result folder.
pub struct Material {
    /// Transcript markdown, one per video.
    pub transcripts: Vec<PathBuf>,
    /// Course files copied as-is (PDFs, spreadsheets, images, ...).
    pub files: Vec<PathBuf>,
}

pub fn chapter_material(chapter: &Chapter, result_chapter_dir: &Path) -> Material {
    let transcripts = chapter
        .transcript_names
        .iter()
        .map(|n| result_chapter_dir.join(format!("{n}.md")))
        .filter(|p| p.is_file())
        .collect();
    let files = chapter
        .doc_names
        .iter()
        .map(|n| result_chapter_dir.join(n))
        .filter(|p| p.is_file())
        .collect();
    Material { transcripts, files }
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
/// `as_text` names the files that also have a text version in `.input/files-as-text/`.
pub fn agent_prompt(course: &str, chapter: &Chapter, m: &Material, as_text: &[String]) -> String {
    let mut inputs = String::from(
        "Read every file below completely before writing. They are in the current working folder.\n\nVideo transcripts:\n",
    );
    for t in &m.transcripts {
        let _ = writeln!(inputs, "- `{STAGE}/transcripts/{}`", file_name(t));
    }
    if !m.files.is_empty() {
        inputs.push_str("\nCourse files (handouts, templates, slides, images), as the instructor shared them:\n");
        for f in &m.files {
            let _ = writeln!(inputs, "- `{STAGE}/files/{}`", file_name(f));
        }
    }
    if !as_text.is_empty() {
        inputs.push_str("\nPlain-text extractions of some of those files, to help with reading (the originals above are the source of truth):\n");
        for n in as_text {
            let _ = writeln!(inputs, "- `{STAGE}/files-as-text/{n}.md`");
        }
    }
    let output = "Write the skill files into the current working folder (`SKILL.md`, `references/...`). Do not modify anything under `.input/`. When you are done, reply with one line listing the files you wrote.";
    fill(course, chapter, &inputs, output)
}

/// Prompt for text-only backends: material is inlined and files come back as blocks.
/// `unreadable` lists course files with no text form, by name only.
pub fn text_prompt(
    course: &str,
    chapter: &Chapter,
    material: &[(String, String)],
    unreadable: &[String],
) -> String {
    let mut inputs = String::from(
        "The full material follows, one file per block (video transcripts, then course files as text).\n",
    );
    for (name, body) in material {
        let _ = write!(
            inputs,
            "\n<file name=\"{name}\">\n{}\n</file>\n",
            body.trim_end()
        );
    }
    if !unreadable.is_empty() {
        let _ = write!(
            inputs,
            "\nThe unit also includes these files, which you cannot read here: {}.\n",
            unreadable.join(", ")
        );
    }
    let output = "## Output format\n\nReply with the files only, each wrapped exactly like this, and nothing outside the blocks:\n\n=== FILE: SKILL.md ===\n(file content)\n=== END FILE ===\n=== FILE: references/topic-name.md ===\n(file content)\n=== END FILE ===\n";
    fill(course, chapter, &inputs, output)
}

fn sources_md(chapter: &Chapter, m: &Material) -> String {
    let mut md = format!("# Sources for {}\n\n## Videos\n\n", chapter.name);
    for v in &chapter.videos {
        let _ = writeln!(md, "- `{}` (full path: `{}`)", file_name(v), v.display());
    }
    md.push_str("\n## Transcripts\n\n");
    for t in &m.transcripts {
        let _ = writeln!(md, "- `{}`", t.display());
    }
    md.push_str("\n## Course files (copied as-is)\n\n");
    for f in &m.files {
        let _ = writeln!(md, "- `{}`", f.display());
    }
    md
}

/// Quotes a value for a single-quoted bash string.
fn sh_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Text versions of the files that convert (PDF, DOCX, XLSX, ...), in memory.
fn files_as_text(files: &[PathBuf]) -> Vec<(String, String)> {
    files
        .iter()
        .filter_map(|f| {
            crate::docs::convert(f)
                .ok()
                .map(|t| (file_name(f), strip_invisible(&t)))
        })
        .collect()
}

/// Writes scaffolding, has the chosen AI author the skill, then cleans the text.
pub fn synthesize(
    course: &str,
    chapter: &Chapter,
    result_chapter_dir: &Path,
    skills_root: &Path,
    ai_cfg: &AiConfig,
) -> Result<PathBuf> {
    let m = chapter_material(chapter, result_chapter_dir);
    if m.transcripts.is_empty() && m.files.is_empty() {
        bail!("unit {} has no transcripts or files yet", chapter.name);
    }
    let skill_dir = skills_root.join(&chapter.slug);
    // An unfinished skill from an interrupted run is redone from scratch.
    if skill_dir.exists() && !skill_dir.join(COMPLETE).exists() {
        std::fs::remove_dir_all(&skill_dir)?;
    }
    std::fs::create_dir_all(skill_dir.join("scripts"))?;
    std::fs::create_dir_all(skill_dir.join("references"))?;
    std::fs::write(skill_dir.join("sources.md"), sources_md(chapter, &m))?;
    let frame = skill_dir.join("scripts/frame.sh");
    std::fs::write(
        &frame,
        FRAME_SH.replace(
            "'__VIDEO_DIR__'",
            &sh_single_quote(&chapter.video_dir.display().to_string()),
        ),
    )?;
    set_executable(&frame)?;
    let texts = files_as_text(&m.files);

    if ai_cfg.backend.is_agent() {
        let stage = skill_dir.join(STAGE);
        stage_inputs(&stage, &m, &texts)?;
        let names: Vec<String> = texts.iter().map(|(n, _)| n.clone()).collect();
        let result = ai::run_agent(
            ai_cfg,
            &agent_prompt(course, chapter, &m, &names),
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
        for t in &m.transcripts {
            inline.push((file_name(t), std::fs::read_to_string(t)?));
        }
        inline.extend(texts.iter().cloned());
        let unreadable: Vec<String> = m
            .files
            .iter()
            .map(|f| file_name(f))
            .filter(|n| !texts.iter().any(|(t, _)| t == n))
            .collect();
        let answer = ai::complete(ai_cfg, &text_prompt(course, chapter, &inline, &unreadable))?;
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
    std::fs::write(skill_dir.join(COMPLETE), "")?;
    Ok(skill_dir)
}

fn stage_inputs(stage: &Path, m: &Material, texts: &[(String, String)]) -> Result<()> {
    let _ = std::fs::remove_dir_all(stage);
    std::fs::create_dir_all(stage.join("transcripts"))?;
    for t in &m.transcripts {
        std::fs::copy(t, stage.join("transcripts").join(file_name(t)))?;
    }
    if !m.files.is_empty() {
        std::fs::create_dir_all(stage.join("files"))?;
        for f in &m.files {
            std::fs::copy(f, stage.join("files").join(file_name(f)))?;
        }
    }
    if !texts.is_empty() {
        std::fs::create_dir_all(stage.join("files-as-text"))?;
        for (n, body) in texts {
            std::fs::write(stage.join("files-as-text").join(format!("{n}.md")), body)?;
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
            transcript_names: vec![],
            docs: vec!["/c/05/a.pdf".into()],
            doc_names: vec!["a.pdf".into()],
        }
    }

    fn material() -> Material {
        Material {
            transcripts: vec!["/r/05/Video_01.md".into()],
            files: vec!["/r/05/a.pdf".into()],
        }
    }

    #[test]
    fn strip_removes_invisible_and_keeps_text() {
        assert_eq!(strip_invisible("a\u{200B}b\u{202E}c\u{E0041}"), "abc");
        assert_eq!(strip_invisible("café 🚀\nx\u{00A0}y"), "café 🚀\nx y");
    }

    #[test]
    fn agent_prompt_points_at_staged_inputs() {
        let p = agent_prompt("C", &chapter(), &material(), &["a.pdf".into()]);
        assert!(p.contains("name: 05-on-page-seo"));
        assert!(p.contains(".input/transcripts/Video_01.md"));
        assert!(p.contains(".input/files/a.pdf"));
        assert!(p.contains(".input/files-as-text/a.pdf.md"));
        assert!(p.contains("scripts/frame.sh"));
        assert!(!p.contains("{{"));
    }

    #[test]
    fn text_prompt_inlines_material_and_names_unreadable() {
        let p = text_prompt(
            "C",
            &chapter(),
            &[("v.md".into(), "hello".into())],
            &["cover.jpg".into()],
        );
        assert!(p.contains("<file name=\"v.md\">\nhello\n</file>"));
        assert!(p.contains("cover.jpg"));
        assert!(p.contains("=== FILE: SKILL.md ==="));
    }

    #[test]
    fn single_quote_escapes_quotes_and_dollars_stay_literal() {
        assert_eq!(
            sh_single_quote("/a/Make $10k/it's"),
            "'/a/Make $10k/it'\\''s'"
        );
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
