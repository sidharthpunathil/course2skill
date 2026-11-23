//! Finds every video and handout under a course folder (any depth) and groups
//! them into units. Each unit becomes one result folder and one skill.
//!
//! The grouping is a `Plan`, saved as `plan.json` in the output folder so it can
//! be reviewed or edited by hand and reused on the next run.

use crate::ai::{self, AiConfig};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

pub const VIDEO_EXTS: &[&str] = &[
    "mp4", "mkv", "mov", "webm", "m4v", "avi", "m4a", "mp3", "wav",
];
pub const DOC_EXTS: &[&str] = &["pdf", "docx", "xlsx", "pptx", "txt", "md"];
const SKIP_DIRS: &[&str] = &["node_modules", "__MACOSX", "target"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum LayoutMode {
    /// Reuse result/plan.json when present, otherwise use folder heuristics
    Auto,
    /// Folder heuristics, ignoring any saved plan
    Folders,
    /// Ask the chosen AI to group files into units
    Ai,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Plan {
    pub course: String,
    pub root: PathBuf,
    pub units: Vec<UnitPlan>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UnitPlan {
    pub name: String,
    /// Output folder, relative to the result folder.
    pub folder: String,
    /// Source files, relative to `Plan::root`.
    pub files: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Course {
    pub name: String,
    pub chapters: Vec<Chapter>,
}

#[derive(Debug, Clone)]
pub struct Chapter {
    pub name: String,
    pub slug: String,
    /// Output folder, relative to the result folder.
    pub out_rel: PathBuf,
    /// Folder the frame helper searches for bare video names.
    pub video_dir: PathBuf,
    pub videos: Vec<PathBuf>,
    pub docs: Vec<PathBuf>,
}

/// The real course root (single wrapper folders unwrapped) and every content
/// file under it, relative to that root, in natural order.
pub fn inventory(root: &Path, exclude: &[PathBuf]) -> Result<(PathBuf, Vec<String>)> {
    let mut root = root
        .canonicalize()
        .with_context(|| format!("course folder not found: {}", root.display()))?;
    if !root.is_dir() {
        bail!("{} is not a folder", root.display());
    }
    loop {
        let entries: Vec<PathBuf> = visible_entries(&root)?;
        let has_content = entries.iter().any(|p| p.is_file() && kind(p).is_some());
        let dirs: Vec<&PathBuf> = entries
            .iter()
            .filter(|p| p.is_dir() && !excluded(p, exclude))
            .collect();
        match (has_content, dirs.as_slice()) {
            (false, [only]) => root = (*only).clone(),
            _ => break,
        }
    }
    let mut files = Vec::new();
    walk(&root, &root, exclude, &mut files)?;
    if files.is_empty() {
        bail!("no videos or handouts found under {}", root.display());
    }
    files.sort_by(|a, b| path_cmp(a, b));
    Ok((root, files))
}

fn excluded(p: &Path, exclude: &[PathBuf]) -> bool {
    exclude.iter().any(|e| p.starts_with(e))
}

fn visible_entries(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let p = entry?.path();
        let name = file_name(&p);
        if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        out.push(p);
    }
    Ok(out)
}

fn walk(root: &Path, dir: &Path, exclude: &[PathBuf], out: &mut Vec<String>) -> Result<()> {
    for p in visible_entries(dir)? {
        if p.is_dir() {
            if !excluded(&p, exclude) {
                walk(root, &p, exclude, out)?;
            }
        } else if kind(&p).is_some()
            && let Ok(rel) = p.strip_prefix(root)
        {
            out.push(rel.to_string_lossy().into_owned());
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Video,
    Doc,
}

pub fn kind(p: &Path) -> Option<Kind> {
    let ext = p.extension()?.to_str()?.to_ascii_lowercase();
    if VIDEO_EXTS.contains(&ext.as_str()) {
        Some(Kind::Video)
    } else if DOC_EXTS.contains(&ext.as_str()) {
        Some(Kind::Doc)
    } else {
        None
    }
}

/// Groups files with folder heuristics:
/// - files spread over folders: one unit per folder that holds files, at any depth;
/// - one flat folder with two-level numbering (`2.3`, `02-03`, `S2E3`): one unit per first number;
/// - otherwise one flat folder is one unit.
pub fn heuristic_plan(course: &str, root: &Path, files: &[String]) -> Plan {
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for f in files {
        let parent = Path::new(f)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        groups.entry(parent).or_default().push(f.clone());
    }
    let mut keys: Vec<String> = groups.keys().cloned().collect();
    keys.sort_by(|a, b| dir_cmp(Path::new(a), Path::new(b)));

    let units = if keys.len() == 1 && keys[0].is_empty() {
        flat_units(course, files)
    } else {
        keys.into_iter()
            .map(|k| {
                let files = groups.remove(&k).unwrap_or_default();
                if k.is_empty() {
                    UnitPlan {
                        name: "Course materials".into(),
                        folder: "00-Course materials".into(),
                        files,
                    }
                } else {
                    let name = k.replace(std::path::MAIN_SEPARATOR, " › ");
                    UnitPlan {
                        name,
                        folder: k,
                        files,
                    }
                }
            })
            .collect()
    };
    Plan {
        course: course.to_string(),
        root: root.to_path_buf(),
        units,
    }
}

fn flat_units(course: &str, files: &[String]) -> Vec<UnitPlan> {
    let numbered: Vec<(u32, &String)> = files
        .iter()
        .filter_map(|f| two_level(&file_stem(Path::new(f))).map(|(a, _)| (a, f)))
        .collect();
    let firsts: BTreeSet<u32> = numbered.iter().map(|(a, _)| *a).collect();
    if numbered.len() * 5 >= files.len() * 4 && firsts.len() >= 2 {
        let mut by: BTreeMap<u32, Vec<String>> = BTreeMap::new();
        let mut rest = Vec::new();
        for f in files {
            match two_level(&file_stem(Path::new(f))) {
                Some((a, _)) => by.entry(a).or_default().push(f.clone()),
                None => rest.push(f.clone()),
            }
        }
        let mut units: Vec<UnitPlan> = by
            .into_iter()
            .map(|(n, files)| UnitPlan {
                name: format!("Part {n}"),
                folder: format!("{n:02}-Part {n}"),
                files,
            })
            .collect();
        if !rest.is_empty() {
            units.push(UnitPlan {
                name: "Other files".into(),
                folder: "99-Other files".into(),
                files: rest,
            });
        }
        return units;
    }
    vec![UnitPlan {
        name: course.to_string(),
        folder: course.to_string(),
        files: files.to_vec(),
    }]
}

/// Reads two leading numbers from names like `2.3 Intro`, `02-03_intro`, `S2E3`, `Section 2 Lesson 3`:
/// the first number starts within 12 characters and the second follows within 8.
fn two_level(stem: &str) -> Option<(u32, u32)> {
    let chars: Vec<char> = stem.chars().collect();
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < chars.len() && runs.len() < 2 {
        if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            runs.push((start, i));
        } else {
            i += 1;
        }
    }
    let [(s1, e1), (s2, e2)] = runs.as_slice() else {
        return None;
    };
    if *s1 > 12 || s2 - e1 > 8 {
        return None;
    }
    let num = |a: usize, b: usize| chars[a..b].iter().collect::<String>().parse::<u32>().ok();
    Some((num(*s1, *e1)?, num(*s2, *e2)?))
}

/// Asks the AI to group the files. The answer is checked: unknown paths are
/// dropped and any file left out goes into an "Other files" unit.
pub fn ai_plan(
    course: &str,
    root: &Path,
    files: &[String],
    durations: &BTreeMap<String, f64>,
    cfg: &AiConfig,
) -> Result<Plan> {
    let mut listing = String::new();
    for f in files {
        match durations.get(f) {
            Some(d) => {
                let _ = writeln!(listing, "{f}  [video {}]", crate::transcript::fmt_ts(*d));
            }
            None => {
                let _ = writeln!(listing, "{f}");
            }
        }
    }
    let prompt = format!(
        "You are organizing a video course into units. Each unit will become one folder of transcripts and one agent skill, so a unit should be one coherent topic: usually one chapter, module or section.\n\n\
         Course: {course}\n\nFiles (relative paths, in the order found):\n{listing}\n\
         Rules:\n- Keep the course's teaching order.\n- Every file appears in exactly one unit. Keep handouts (pdf, docx, xlsx...) with the lessons they belong to.\n- Use the folder structure and numbering when it is meaningful; regroup when folders are just storage (for example, all videos in one folder).\n- Prefer units of roughly 15 to 120 minutes of video. Do not make one unit per tiny clip unless the course is like that.\n- Names start with a two-digit order number, like \"01-Introduction\".\n\n\
         Reply with JSON only, no commentary:\n{{\"units\": [{{\"name\": \"01-Introduction\", \"files\": [\"path/one.mp4\", \"path/notes.pdf\"]}}]}}\n"
    );
    let answer = ai::complete(cfg, &prompt, root)?;
    let v = ai::extract_json(&answer)?;
    let known: BTreeSet<&str> = files.iter().map(String::as_str).collect();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut units = Vec::new();
    for u in v["units"]
        .as_array()
        .context("answer has no \"units\" list")?
    {
        let name = u["name"].as_str().unwrap_or("Unit").trim().to_string();
        let files: Vec<String> = u["files"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|f| f.as_str())
                    .filter(|f| known.contains(f))
                    .filter(|f| seen.insert(f.to_string()))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if !files.is_empty() {
            units.push(UnitPlan {
                folder: safe_folder(&name),
                name,
                files,
            });
        }
    }
    let missing: Vec<String> = files
        .iter()
        .filter(|f| !seen.contains(*f))
        .cloned()
        .collect();
    if !missing.is_empty() {
        units.push(UnitPlan {
            name: "Other files".into(),
            folder: "99-Other files".into(),
            files: missing,
        });
    }
    if units.is_empty() {
        bail!("the AI returned no usable units");
    }
    Ok(Plan {
        course: course.to_string(),
        root: root.to_path_buf(),
        units,
    })
}

/// A folder name that cannot escape the result folder.
pub fn safe_folder(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '\0') {
                '-'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    if cleaned.is_empty() {
        "unit".into()
    } else {
        cleaned
    }
}

/// Rejects absolute paths and `..` so a hand-edited plan cannot write outside `result/`.
fn safe_rel(p: &str) -> Result<PathBuf> {
    let path = PathBuf::from(p);
    if path.components().all(|c| matches!(c, Component::Normal(_))) {
        Ok(path)
    } else {
        bail!("unsafe folder in plan: {p:?}")
    }
}

pub fn save_plan(plan: &Plan, out: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(out)?;
    let path = out.join("plan.json");
    std::fs::write(&path, serde_json::to_string_pretty(plan)?)?;
    Ok(path)
}

pub fn load_plan(out: &Path, root: &Path) -> Option<Plan> {
    let plan: Plan =
        serde_json::from_str(&std::fs::read_to_string(out.join("plan.json")).ok()?).ok()?;
    (plan.root == root).then_some(plan)
}

/// Builds the course the pipeline runs on. Missing files are reported, not fatal.
pub fn course_from_plan(plan: &Plan) -> Result<(Course, Vec<String>)> {
    let mut warnings = Vec::new();
    let mut chapters = Vec::new();
    let mut slugs = BTreeSet::new();
    for u in &plan.units {
        let out_rel = safe_rel(&u.folder)?;
        let mut videos = Vec::new();
        let mut docs = Vec::new();
        for f in &u.files {
            let p = plan.root.join(f);
            if !p.is_file() {
                warnings.push(format!("missing file in plan: {f}"));
                continue;
            }
            match kind(&p) {
                Some(Kind::Video) => videos.push(p),
                Some(Kind::Doc) => docs.push(p),
                None => warnings.push(format!("unsupported file in plan: {f}")),
            }
        }
        videos.sort_by(|a, b| video_order(a, b));
        docs.sort_by(|a, b| natural_cmp(&file_name(a), &file_name(b)));
        let video_dir =
            common_parent(videos.iter().chain(&docs)).unwrap_or_else(|| plan.root.clone());
        let base = slugify(&u.name);
        let mut slug = if base.is_empty() {
            "unit".to_string()
        } else {
            base
        };
        let mut n = 2;
        while !slugs.insert(slug.clone()) {
            slug = format!("{}-{n}", slugify(&u.name));
            n += 1;
        }
        chapters.push(Chapter {
            name: u.name.clone(),
            slug,
            out_rel,
            video_dir,
            videos,
            docs,
        });
    }
    Ok((
        Course {
            name: plan.course.clone(),
            chapters,
        },
        warnings,
    ))
}

fn common_parent<'a>(mut paths: impl Iterator<Item = &'a PathBuf>) -> Option<PathBuf> {
    let mut common = paths.next()?.parent()?.to_path_buf();
    for p in paths {
        while !p.starts_with(&common) {
            common = common.parent()?.to_path_buf();
        }
    }
    Some(common)
}

pub fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn file_stem(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `Video_x.mp4` sorts before `Video_x_video2.mp4`, which sorts before `_video10`.
fn video_order(a: &Path, b: &Path) -> Ordering {
    let (base_a, n_a) = split_part(&file_stem(a));
    let (base_b, n_b) = split_part(&file_stem(b));
    natural_cmp(&base_a, &base_b).then(n_a.cmp(&n_b))
}

fn split_part(stem: &str) -> (String, u32) {
    if let Some(idx) = stem.rfind("_video")
        && let Ok(n) = stem[idx + 6..].parse::<u32>()
    {
        return (stem[..idx].to_string(), n);
    }
    (stem.to_string(), 1)
}

/// Natural order on folder paths, component by component.
fn dir_cmp(a: &Path, b: &Path) -> Ordering {
    let ca: Vec<String> = a
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let cb: Vec<String> = b
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    for (x, y) in ca.iter().zip(&cb) {
        match natural_cmp(x, y) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    ca.len().cmp(&cb.len())
}

/// Files: by folder, then lesson order within the folder.
fn path_cmp(a: &str, b: &str) -> Ordering {
    let (pa, pb) = (Path::new(a), Path::new(b));
    let (da, db) = (
        pa.parent().unwrap_or(Path::new("")),
        pb.parent().unwrap_or(Path::new("")),
    );
    dir_cmp(da, db).then_with(|| video_order(pa, pb))
}

/// Compares strings treating runs of digits as numbers.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let na = take_number(&mut ai);
                let nb = take_number(&mut bi);
                match na.cmp(&nb) {
                    Ordering::Equal => {}
                    other => return other,
                }
            }
            (Some(x), Some(y)) => match x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase()) {
                Ordering::Equal => {
                    ai.next();
                    bi.next();
                }
                other => return other,
            },
        }
    }
}

fn take_number(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> u64 {
    let mut n: u64 = 0;
    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
        n = n
            .saturating_mul(10)
            .saturating_add(u64::from(c as u8 - b'0'));
        it.next();
    }
    n
}

pub fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(p: &Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"x").unwrap();
    }

    fn names(plan: &Plan) -> Vec<&str> {
        plan.units.iter().map(|u| u.name.as_str()).collect()
    }

    #[test]
    fn inventory_unwraps_wrappers_and_skips_junk() {
        let tmp = tempfile::tempdir().unwrap();
        let c = tmp.path().join("wrap/My Course");
        touch(&c.join("02-B/Video_x.mp4"));
        touch(&c.join("01-A/Video_y_video2.mp4"));
        touch(&c.join("01-A/Video_y_video10.mp4"));
        touch(&c.join("01-A/Video_y.mp4"));
        touch(&c.join("01-A/notes [a], b.pdf"));
        touch(&c.join("01-A/.DS_Store"));
        touch(&c.join("01-A/cover.jpg"));
        let (root, files) = inventory(tmp.path(), &[]).unwrap();
        assert_eq!(file_name(&root), "My Course");
        assert_eq!(
            files,
            [
                "01-A/notes [a], b.pdf",
                "01-A/Video_y.mp4",
                "01-A/Video_y_video2.mp4",
                "01-A/Video_y_video10.mp4",
                "02-B/Video_x.mp4"
            ]
        );
    }

    #[test]
    fn inventory_skips_excluded_output() {
        let tmp = tempfile::tempdir().unwrap();
        touch(&tmp.path().join("a/v.mp4"));
        touch(&tmp.path().join("b/w.mp4"));
        touch(&tmp.path().join("result/a/v.md"));
        let out = tmp.path().canonicalize().unwrap().join("result");
        let (_, files) = inventory(tmp.path(), &[out]).unwrap();
        assert_eq!(files, ["a/v.mp4", "b/w.mp4"]);
    }

    #[test]
    fn nested_folders_become_units_at_any_depth() {
        let files: Vec<String> = [
            "Module 1/Lesson 1/a.mp4",
            "Module 1/Lesson 2/b.mp4",
            "Module 2/c.mp4",
            "intro.pdf",
        ]
        .map(String::from)
        .to_vec();
        let plan = heuristic_plan("C", Path::new("/c"), &files);
        assert_eq!(
            names(&plan),
            [
                "Course materials",
                "Module 1 › Lesson 1",
                "Module 1 › Lesson 2",
                "Module 2"
            ]
        );
        assert_eq!(plan.units[1].folder, "Module 1/Lesson 1");
    }

    #[test]
    fn flat_two_level_numbering_groups_by_section() {
        let files: Vec<String> = [
            "1.1 Intro.mp4",
            "1.2 Why.mp4",
            "2.1 Setup.mp4",
            "2.2 Tools.mp4",
            "2.3 More.mp4",
        ]
        .map(String::from)
        .to_vec();
        let plan = heuristic_plan("C", Path::new("/c"), &files);
        assert_eq!(names(&plan), ["Part 1", "Part 2"]);
        assert_eq!(plan.units[1].files.len(), 3);
    }

    #[test]
    fn two_level_reads_common_patterns() {
        assert_eq!(two_level("S2E3 Setup"), Some((2, 3)));
        assert_eq!(two_level("Section 2 Lesson 3"), Some((2, 3)));
        assert_eq!(two_level("02-03_intro"), Some((2, 3)));
        assert_eq!(two_level("Intro"), None);
    }

    #[test]
    fn flat_unnumbered_folder_is_one_unit() {
        let files: Vec<String> = ["intro.mp4", "deep dive.mp4"].map(String::from).to_vec();
        let plan = heuristic_plan("C", Path::new("/c"), &files);
        assert_eq!(names(&plan), ["C"]);
    }

    #[test]
    fn plan_rejects_escaping_folders() {
        let plan = Plan {
            course: "C".into(),
            root: "/c".into(),
            units: vec![UnitPlan {
                name: "x".into(),
                folder: "../evil".into(),
                files: vec![],
            }],
        };
        assert!(course_from_plan(&plan).is_err());
    }

    #[test]
    fn slugs_are_unique() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = Plan {
            course: "C".into(),
            root: tmp.path().to_path_buf(),
            units: vec![
                UnitPlan {
                    name: "Intro".into(),
                    folder: "a".into(),
                    files: vec![],
                },
                UnitPlan {
                    name: "Intro".into(),
                    folder: "b".into(),
                    files: vec![],
                },
            ],
        };
        let (c, _) = course_from_plan(&plan).unwrap();
        assert_eq!(c.chapters[1].slug, "intro-2");
    }

    #[test]
    fn natural_cmp_orders_numbers() {
        assert_eq!(natural_cmp("2-a", "10-a"), Ordering::Less);
        assert_eq!(slugify("05-On-Page SEO"), "05-on-page-seo");
    }
}
