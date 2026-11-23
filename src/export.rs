//! Packages a finished run for sharing: every skill as its own folder (and
//! optionally its own zip), the course transcripts and files, the knowledge
//! graph, and an index README. Machine-specific paths are scrubbed.

use crate::layout::{self, file_name};
use crate::skill::COMPLETE;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct ExportOpts {
    pub run: PathBuf,
    pub to: PathBuf,
    /// Put skill folders directly in `to` (for ~/.claude/skills and similar).
    pub flat: bool,
    pub zip: bool,
    /// Leave out the course transcripts and files.
    pub skills_only: bool,
    pub force: bool,
}

pub struct Summary {
    pub dest: PathBuf,
    pub skills: Vec<String>,
    pub incomplete: Vec<String>,
    pub zips: Vec<PathBuf>,
}

/// Files the pipeline uses internally; never shared.
const INTERNAL: &[&str] = &[COMPLETE, ".input", "_raw_answer.md", ".DS_Store"];

pub fn export(o: &ExportOpts) -> Result<Summary> {
    let plan: layout::Plan = serde_json::from_str(
        &std::fs::read_to_string(o.run.join("plan.json"))
            .with_context(|| format!("no plan.json in {}", o.run.display()))?,
    )?;
    let (course, _) = layout::course_from_plan(&plan)?;
    let slug = layout::slugify(&plan.course);
    let dest = if o.flat {
        o.to.clone()
    } else {
        o.to.join(&slug)
    };
    let skills_dest = if o.flat {
        dest.clone()
    } else {
        dest.join("skills")
    };
    let scrub = Scrubber::new(&plan.root, &o.run);

    let mut skills = Vec::new();
    let mut incomplete = Vec::new();
    let mut descriptions = Vec::new();
    for ch in &course.chapters {
        let src = o.run.join("skills").join(&ch.slug);
        if !src.join(COMPLETE).exists() {
            incomplete.push(ch.slug.clone());
            continue;
        }
        let target = skills_dest.join(&ch.slug);
        if target.exists() {
            if !o.force {
                bail!(
                    "{} already exists (use --force to replace it)",
                    target.display()
                );
            }
            std::fs::remove_dir_all(&target)?;
        }
        copy_tree(&src, &target, &scrub)?;
        descriptions.push((
            ch.slug.clone(),
            ch.name.clone(),
            skill_description(&target.join("SKILL.md")),
        ));
        skills.push(ch.slug.clone());
    }
    if skills.is_empty() {
        bail!("no finished skills in {} yet", o.run.display());
    }

    if !o.flat {
        if !o.skills_only {
            for ch in &course.chapters {
                let src = o.run.join(&ch.out_rel);
                if !src.is_dir() {
                    continue;
                }
                let target = dest.join("course").join(&ch.out_rel);
                let _ = std::fs::remove_dir_all(&target);
                copy_tree(&src, &target, &scrub)?;
            }
        }
        if let Some(graph) = crate::graph::find_graph(&o.run)
            && let Some(gdir) = graph.parent()
        {
            let target = dest.join("knowledge-graph");
            let _ = std::fs::remove_dir_all(&target);
            std::fs::create_dir_all(&target)?;
            for name in ["GRAPH_REPORT.md", "graph.html", "graph.json", "wiki"] {
                let p = gdir.join(name);
                if p.is_dir() {
                    copy_tree(&p, &target.join(name), &scrub)?;
                } else if p.is_file() {
                    copy_file(&p, &target.join(name), &scrub)?;
                }
            }
        }
        let sections: Vec<(String, String)> = course
            .chapters
            .iter()
            .map(|c| (c.slug.clone(), c.name.clone()))
            .collect();
        crate::kb::write(&dest, &plan.course, &sections)?;
    }

    let mut zips = Vec::new();
    if o.zip {
        let zip_dir = if o.flat {
            o.to.join("zips")
        } else {
            dest.join("zips")
        };
        std::fs::create_dir_all(&zip_dir)?;
        for s in &skills {
            let out = zip_dir.join(format!("{s}.zip"));
            zip(&skills_dest, s, &out, None)?;
            zips.push(out);
        }
        if !o.flat {
            let out = o.to.join(format!("{slug}.zip"));
            // The per-skill zips would duplicate everything inside the course zip.
            zip(&o.to, &slug, &out, Some(&format!("{slug}/zips/*")))?;
            zips.push(out);
        }
    }
    Ok(Summary {
        dest,
        skills,
        incomplete,
        zips,
    })
}

/// Replaces machine-specific paths with neutral placeholders.
struct Scrubber {
    pairs: Vec<(String, String)>,
}

impl Scrubber {
    fn new(course_root: &Path, run: &Path) -> Scrubber {
        let mut pairs = vec![
            (run.display().to_string(), ".".to_string()),
            (
                course_root.display().to_string(),
                "<course folder>".to_string(),
            ),
        ];
        if let Some(home) = std::env::var_os("HOME") {
            pairs.push((PathBuf::from(home).display().to_string(), "~".to_string()));
        }
        // Longest first so a run path inside the home folder is replaced whole.
        pairs.sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));
        Scrubber { pairs }
    }

    fn text(&self, s: &str) -> String {
        let mut out = s.to_string();
        for (from, to) in &self.pairs {
            if !from.is_empty() && from != "/" {
                out = out.replace(from.as_str(), to);
            }
        }
        out
    }
}

/// frame.sh points at the original videos on this machine; shared copies read
/// the location from COURSE_VIDEO_DIR instead.
fn portable_frame_sh(s: &str) -> String {
    s.lines()
        .map(|l| {
            if l.starts_with("VIDEO_DIR='") {
                "# Set COURSE_VIDEO_DIR to the folder holding this course's videos.\nVIDEO_DIR=\"${COURSE_VIDEO_DIR:-}\"\n[ -n \"$VIDEO_DIR\" ] || { echo \"set COURSE_VIDEO_DIR to the course video folder\" >&2; exit 1; }".to_string()
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn copy_tree(src: &Path, dst: &Path, scrub: &Scrubber) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let p = entry?.path();
        let name = file_name(&p);
        if INTERNAL.contains(&name.as_str()) {
            continue;
        }
        if p.is_dir() {
            copy_tree(&p, &dst.join(&name), scrub)?;
        } else {
            copy_file(&p, &dst.join(&name), scrub)?;
        }
    }
    Ok(())
}

fn copy_file(src: &Path, dst: &Path, scrub: &Scrubber) -> Result<()> {
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(ext.as_str(), "md" | "json" | "tsv" | "txt" | "html" | "sh") {
        let text = std::fs::read_to_string(src)?;
        let mut out = scrub.text(&text);
        if file_name(src) == "frame.sh" {
            out = portable_frame_sh(&text);
            out = scrub.text(&out);
        }
        std::fs::write(dst, out)?;
        if ext == "sh" {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(dst, std::fs::Permissions::from_mode(0o755))?;
            }
        }
    } else {
        std::fs::copy(src, dst)?;
    }
    Ok(())
}

/// The `description:` value from a SKILL.md frontmatter (single line or folded).
fn skill_description(skill_md: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(skill_md) else {
        return String::new();
    };
    let mut lines = text.lines().skip_while(|l| l.trim() != "---").skip(1);
    let mut out = String::new();
    let mut in_desc = false;
    for l in lines.by_ref() {
        if l.trim() == "---" {
            break;
        }
        if let Some(rest) = l.strip_prefix("description:") {
            let rest = rest.trim();
            in_desc = true;
            if !matches!(rest, ">" | "|" | ">-" | "|-") {
                out.push_str(rest.trim_matches('"'));
            }
        } else if in_desc && l.starts_with([' ', '\t']) {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(l.trim());
        } else {
            in_desc = false;
        }
    }
    out
}

fn zip(cwd: &Path, name: &str, out: &Path, exclude: Option<&str>) -> Result<()> {
    let _ = std::fs::remove_file(out);
    let mut cmd = Command::new("zip");
    cmd.current_dir(cwd)
        .args(["-r", "-q", "-X"])
        .arg(out)
        .arg(name)
        .args(["-x", "*.DS_Store"]);
    if let Some(x) = exclude {
        cmd.arg(x);
    }
    let status = cmd.status().context("could not run `zip`")?;
    if !status.success() {
        bail!("zip failed for {name}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubber_replaces_run_course_and_home() {
        let s = Scrubber::new(
            Path::new("/Users/me/c/Course"),
            Path::new("/Users/me/p/result/2026"),
        );
        let t = s.text("a /Users/me/p/result/2026/x.md b /Users/me/c/Course/v.mp4");
        assert_eq!(t, "a ./x.md b <course folder>/v.mp4");
    }

    #[test]
    fn frame_sh_reads_env_instead_of_local_path() {
        let out = portable_frame_sh("#!/bin/bash\nVIDEO_DIR='/Users/me/c'\necho hi\n");
        assert!(out.contains("VIDEO_DIR=\"${COURSE_VIDEO_DIR:-}\""));
        assert!(!out.contains("/Users/me/c"));
    }

    #[test]
    fn description_reads_single_and_folded() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("SKILL.md");
        std::fs::write(&p, "---\nname: x\ndescription: Does a thing.\n---\nbody").unwrap();
        assert_eq!(skill_description(&p), "Does a thing.");
        std::fs::write(
            &p,
            "---\nname: x\ndescription: >\n  Line one\n  line two\n---\n",
        )
        .unwrap();
        assert_eq!(skill_description(&p), "Line one line two");
    }
}
