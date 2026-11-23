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

const MANIFEST: &str = "assets/screenshots.tsv";

const AGENT_SCREENSHOTS: &str = "Run `scripts/frame.sh \"<video file>\" <mm:ss> <descriptive-name>.jpg` for each visual moment. It saves the frame into `assets/screenshots/` and prints the path. Open every screenshot you take and check that it shows what you need. If it caught a transition, the wrong screen or the presenter's face, try a few seconds earlier or later, or grab a short range with `scripts/frame.sh \"<video file>\" <from> <to> 2` and keep the clearest frame. Screenshots you do not embed are removed automatically afterwards. Embed the ones that genuinely help someone do the work, usually 5 to 25 per section. If you cannot run commands, list each screenshot you want in `assets/screenshots.tsv` instead, one per line as `name.jpg<TAB>video file<TAB>mm:ss<TAB>what it shows`; they are captured after you finish.";

const TEXT_SCREENSHOTS: &str = "You cannot watch the video, so choose moments from the transcripts where the screen most likely shows something useful (the sentence is spoken while it is on screen, so pick a time one or two seconds after the sentence starts). List them in a file block for `assets/screenshots.tsv`, one per line as `name.jpg<TAB>video file<TAB>mm:ss<TAB>what it shows`. They are captured after you reply, so embed them in SKILL.md by the names you chose.";

fn fill(course: &str, chapter: &Chapter, inputs: &str, screenshots: &str, output: &str) -> String {
    PROMPT_TEMPLATE
        .replace("{{COURSE}}", course)
        .replace("{{CHAPTER}}", &chapter.name)
        .replace("{{SLUG}}", &chapter.slug)
        .replace("{{INPUTS}}", inputs.trim_end())
        .replace("{{SCREENSHOTS}}", screenshots)
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
        inputs.push_str("\nCourse files (handouts, templates, slides, images), as the instructor shared them, already copied into the skill:\n");
        for f in &m.files {
            let _ = writeln!(inputs, "- `assets/files/{}`", file_name(f));
        }
    }
    if !as_text.is_empty() {
        inputs.push_str("\nPlain-text extractions of some of those files, to help with reading (the originals above are the source of truth):\n");
        for n in as_text {
            let _ = writeln!(inputs, "- `{STAGE}/files-as-text/{n}.md`");
        }
    }
    let output = "Write the skill files into the current working folder (`SKILL.md`, `references/...`, screenshots in `assets/screenshots/`). Do not modify anything under `.input/`. When you are done, reply with one line: how many screenshots you kept and the files you wrote.";
    fill(course, chapter, &inputs, AGENT_SCREENSHOTS, output)
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
    let output = "## Output format\n\nReply with the files only, each wrapped exactly like this, and nothing outside the blocks:\n\n=== FILE: SKILL.md ===\n(file content)\n=== END FILE ===\n=== FILE: references/topic-name.md ===\n(file content)\n=== END FILE ===\n=== FILE: assets/screenshots.tsv ===\n(one screenshot per line)\n=== END FILE ===\n";
    fill(course, chapter, &inputs, TEXT_SCREENSHOTS, output)
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
    md.push_str("\n## Course files (copied as-is into assets/files/)\n\n");
    for f in &m.files {
        let _ = writeln!(md, "- `assets/files/{}`", file_name(f));
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

/// Writes scaffolding, has the chosen AI author the skill, captures requested
/// screenshots, then cleans up. Returns the skill folder and notes for the log.
pub fn synthesize(
    course: &str,
    chapter: &Chapter,
    result_chapter_dir: &Path,
    skills_root: &Path,
    ai_cfg: &AiConfig,
    graph: Option<&Path>,
) -> Result<(PathBuf, Vec<String>)> {
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
    std::fs::create_dir_all(skill_dir.join("assets/screenshots"))?;
    if !m.files.is_empty() {
        std::fs::create_dir_all(skill_dir.join("assets/files"))?;
        for f in &m.files {
            std::fs::copy(f, skill_dir.join("assets/files").join(file_name(f)))?;
        }
    }
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
            &with_graph(agent_prompt(course, chapter, &m, &names), graph, true),
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
        let answer = ai::complete(
            ai_cfg,
            &with_graph(
                text_prompt(course, chapter, &inline, &unreadable),
                graph,
                false,
            ),
        )?;
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
    let mut notes = capture_requested(chapter, &skill_dir);
    notes.extend(tidy_screenshots(&skill_dir)?);
    clean_tree(&skill_dir)?;
    std::fs::write(skill_dir.join(COMPLETE), "")?;
    Ok((skill_dir, notes))
}

/// Adds the knowledge-graph instructions after the inputs section.
fn with_graph(prompt: String, graph: Option<&Path>, agent: bool) -> String {
    match graph {
        Some(g) => prompt.replacen(
            "\n## How to work",
            &format!(
                "\n## Course knowledge graph\n\n{}\n\n## How to work",
                crate::graph::prompt_section(g, agent)
            ),
            1,
        ),
        None => prompt,
    }
}

/// Captures screenshots listed in `assets/screenshots.tsv` that do not exist yet.
fn capture_requested(chapter: &Chapter, skill_dir: &Path) -> Vec<String> {
    let mut notes = Vec::new();
    let Ok(manifest) = std::fs::read_to_string(skill_dir.join(MANIFEST)) else {
        return notes;
    };
    let mut captured = 0;
    for line in manifest
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
    {
        let cols: Vec<&str> = line.split('\t').map(str::trim).collect();
        let [name, video, at, ..] = cols.as_slice() else {
            notes.push(format!("{}: bad screenshot line {line:?}", chapter.slug));
            continue;
        };
        let name = file_name(Path::new(name));
        let out = skill_dir.join("assets/screenshots").join(&name);
        if out.exists() {
            continue;
        }
        let Some(src) = chapter
            .videos
            .iter()
            .find(|v| file_name(v) == *video || crate::layout::file_stem(v) == *video)
        else {
            notes.push(format!(
                "{}: screenshot {name}: unknown video {video:?}",
                chapter.slug
            ));
            continue;
        };
        match crate::media::parse_ts(at).and_then(|t| crate::media::frame(src, t, &out)) {
            Ok(()) => captured += 1,
            Err(e) => notes.push(format!("{}: screenshot {name}: {e:#}", chapter.slug)),
        }
    }
    if captured > 0 {
        notes.push(format!(
            "{}: captured {captured} requested screenshots",
            chapter.slug
        ));
    }
    notes
}

/// Removes screenshots no markdown file uses, and reports image links that point nowhere.
fn tidy_screenshots(skill_dir: &Path) -> Result<Vec<String>> {
    let mut text = String::new();
    collect_md(skill_dir, &mut text)?;
    let mut notes = Vec::new();
    let shots = skill_dir.join("assets/screenshots");
    let mut kept = 0;
    for entry in std::fs::read_dir(&shots)? {
        let p = entry?.path();
        let name = file_name(&p);
        if text.contains(&format!("screenshots/{name}")) {
            kept += 1;
        } else {
            std::fs::remove_file(&p)?;
        }
    }
    for link in text
        .split("](")
        .skip(1)
        .filter_map(|rest| rest.split(')').next())
    {
        if let Some(name) = link.split("assets/screenshots/").nth(1)
            && !shots.join(name).exists()
        {
            notes.push(format!("{}: missing image {name}", file_name(skill_dir)));
        }
    }
    notes.push(format!("{}: {kept} screenshots", file_name(skill_dir)));
    Ok(notes)
}

fn collect_md(dir: &Path, out: &mut String) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let p = entry?.path();
        if p.is_dir() && file_name(&p) != STAGE {
            collect_md(&p, out)?;
        } else if p.extension().is_some_and(|e| e == "md") {
            out.push_str(&std::fs::read_to_string(&p)?);
            out.push('\n');
        }
    }
    Ok(())
}

fn stage_inputs(stage: &Path, m: &Material, texts: &[(String, String)]) -> Result<()> {
    let _ = std::fs::remove_dir_all(stage);
    std::fs::create_dir_all(stage.join("transcripts"))?;
    for t in &m.transcripts {
        std::fs::copy(t, stage.join("transcripts").join(file_name(t)))?;
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
        && (p.extension().is_some_and(|e| e == "md") || p == Path::new(MANIFEST))
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
        assert!(p.contains("assets/files/a.pdf"));
        assert!(p.contains("scripts/frame.sh \"<video file>\" <mm:ss>"));
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
    fn tidy_drops_unused_and_reports_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("skill");
        std::fs::create_dir_all(d.join("assets/screenshots")).unwrap();
        std::fs::write(d.join("assets/screenshots/used.jpg"), b"x").unwrap();
        std::fs::write(d.join("assets/screenshots/unused.jpg"), b"x").unwrap();
        std::fs::write(
            d.join("SKILL.md"),
            "![a](assets/screenshots/used.jpg) ![b](assets/screenshots/gone.jpg)",
        )
        .unwrap();
        let notes = tidy_screenshots(&d).unwrap();
        assert!(d.join("assets/screenshots/used.jpg").exists());
        assert!(!d.join("assets/screenshots/unused.jpg").exists());
        assert!(
            notes.iter().any(|n| n.contains("missing image gone.jpg")),
            "{notes:?}"
        );
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
        assert!(safe_output(d, "assets/screenshots.tsv").is_some());
    }
}
