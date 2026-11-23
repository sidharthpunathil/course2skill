//! Builds one agent skill per chapter from its transcripts and handouts.

use crate::scan::{Chapter, file_name};
use anyhow::{Context, Result, bail};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const PROMPT_TEMPLATE: &str = include_str!("../assets/skill_prompt.md");
const FRAME_SH: &str = include_str!("../assets/frame.sh");

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

/// Markdown files (transcripts and handouts) produced for this chapter.
pub fn chapter_material(result_chapter_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(result_chapter_dir)
        .with_context(|| format!("no results for chapter at {}", result_chapter_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    files.sort_by(|a, b| crate::scan::natural_cmp(&file_name(a), &file_name(b)));
    Ok(files)
}

pub fn build_prompt(
    course: &str,
    chapter: &Chapter,
    material: &[PathBuf],
    skill_dir: &Path,
) -> String {
    let mut list = String::new();
    for m in material {
        let _ = writeln!(list, "- `{}`", m.display());
    }
    PROMPT_TEMPLATE
        .replace("{{COURSE}}", course)
        .replace("{{CHAPTER}}", &chapter.name)
        .replace("{{SLUG}}", &chapter.slug)
        .replace("{{MATERIAL_LIST}}", list.trim_end())
        .replace("{{CHAPTER_DIR}}", &chapter.dir.display().to_string())
        .replace("{{SKILL_DIR}}", &skill_dir.display().to_string())
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

/// Writes scaffolding, runs the `claude` CLI to author the skill, then cleans it.
pub fn synthesize(
    course: &str,
    chapter: &Chapter,
    result_chapter_dir: &Path,
    skills_root: &Path,
    model: Option<&str>,
) -> Result<PathBuf> {
    let material = chapter_material(result_chapter_dir)?;
    if material.is_empty() {
        bail!(
            "chapter {} has no transcripts or handouts yet",
            chapter.name
        );
    }
    let skill_dir = skills_root.join(&chapter.slug);
    std::fs::create_dir_all(skill_dir.join("scripts"))?;
    std::fs::create_dir_all(skill_dir.join("references"))?;
    std::fs::write(skill_dir.join("sources.md"), sources_md(chapter, &material))?;
    let frame = skill_dir.join("scripts/frame.sh");
    std::fs::write(
        &frame,
        FRAME_SH.replace("__VIDEO_DIR__", &chapter.dir.display().to_string()),
    )?;
    set_executable(&frame)?;

    let prompt = build_prompt(course, chapter, &material, &skill_dir);
    let mut cmd = Command::new("claude");
    cmd.current_dir(&skill_dir)
        .args([
            "-p",
            "--permission-mode",
            "acceptEdits",
            "--allowedTools",
            "Read,Write,Edit,Glob,Grep,Skill",
        ])
        .arg("--add-dir")
        .arg(result_chapter_dir)
        .arg("--add-dir")
        .arg(&chapter.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(m) = model {
        cmd.args(["--model", m]);
    }
    let mut child = cmd
        .spawn()
        .context("could not start the `claude` CLI (is Claude Code installed?)")?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(prompt.as_bytes())?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "claude failed for {}: {}",
            chapter.name,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let skill_md = skill_dir.join("SKILL.md");
    if !skill_md.exists() {
        bail!(
            "claude finished but wrote no SKILL.md for {}: {}",
            chapter.name,
            String::from_utf8_lossy(&out.stdout).trim()
        );
    }
    clean_tree(&skill_dir)?;
    Ok(skill_dir)
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

    #[test]
    fn strip_removes_invisible_and_keeps_text() {
        assert_eq!(strip_invisible("a\u{200B}b\u{202E}c\u{E0041}"), "abc");
        assert_eq!(strip_invisible("café 🚀\nx\u{00A0}y"), "café 🚀\nx y");
    }

    #[test]
    fn prompt_names_slug_files_and_frame_script() {
        let ch = Chapter {
            name: "05-On-Page SEO".into(),
            slug: "05-on-page-seo".into(),
            dir: "/c/05-On-Page SEO".into(),
            videos: vec![],
            docs: vec![],
        };
        let p = build_prompt(
            "C",
            &ch,
            &["/r/05/Video_01.md".into()],
            Path::new("/s/05-on-page-seo"),
        );
        assert!(p.contains("name: 05-on-page-seo"));
        assert!(p.contains("/r/05/Video_01.md"));
        assert!(p.contains("scripts/frame.sh"));
        assert!(!p.contains("{{"));
    }
}
