//! Discovers chapters, videos and handouts in a course folder.

use anyhow::{Context, Result, bail};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};

const VIDEO_EXTS: &[&str] = &["mp4", "mkv", "mov", "webm", "m4v"];
const DOC_EXTS: &[&str] = &["pdf", "docx", "xlsx", "pptx", "txt", "md"];

#[derive(Debug, Clone)]
pub struct Course {
    pub name: String,
    pub chapters: Vec<Chapter>,
}

#[derive(Debug, Clone)]
pub struct Chapter {
    pub name: String,
    pub slug: String,
    pub dir: PathBuf,
    pub videos: Vec<PathBuf>,
    pub docs: Vec<PathBuf>,
}

/// Scans `root`. It may be the course itself or a folder holding exactly one course.
pub fn scan(root: &Path) -> Result<Course> {
    let root = root
        .canonicalize()
        .with_context(|| format!("course folder not found: {}", root.display()))?;
    let mut chapters = read_chapters(&root)?;
    let mut course_root = root.clone();
    if chapters.is_empty() {
        let subdirs = list_dirs(&root)?;
        if let [only] = subdirs.as_slice() {
            chapters = read_chapters(only)?;
            course_root = only.clone();
        }
    }
    if chapters.is_empty() {
        bail!(
            "no chapter folders with videos or handouts in {}",
            root.display()
        );
    }
    let name = file_name(&course_root);
    Ok(Course { name, chapters })
}

fn read_chapters(dir: &Path) -> Result<Vec<Chapter>> {
    let mut chapters = Vec::new();
    for chapter_dir in list_dirs(dir)? {
        let mut videos = Vec::new();
        let mut docs = Vec::new();
        for entry in std::fs::read_dir(&chapter_dir)? {
            let path = entry?.path();
            if !path.is_file() || file_name(&path).starts_with('.') {
                continue;
            }
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase)
                .unwrap_or_default();
            if VIDEO_EXTS.contains(&ext.as_str()) {
                videos.push(path);
            } else if DOC_EXTS.contains(&ext.as_str()) {
                docs.push(path);
            }
        }
        if videos.is_empty() && docs.is_empty() {
            continue;
        }
        videos.sort_by(|a, b| video_order(a, b));
        docs.sort_by(|a, b| natural_cmp(&file_name(a), &file_name(b)));
        let name = file_name(&chapter_dir);
        chapters.push(Chapter {
            slug: slugify(&name),
            name,
            dir: chapter_dir,
            videos,
            docs,
        });
    }
    chapters.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    Ok(chapters)
}

fn list_dirs(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() && !file_name(&path).starts_with('.') {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
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

    #[test]
    fn scan_sorts_chapters_and_videos() {
        let tmp = tempfile::tempdir().unwrap();
        let course = tmp.path().join("My Course");
        touch(&course.join("02-B/Video_x.mp4"));
        touch(&course.join("01-A/Video_y_video2.mp4"));
        touch(&course.join("01-A/Video_y_video10.mp4"));
        touch(&course.join("01-A/Video_y.mp4"));
        touch(&course.join("01-A/notes [a], b.pdf"));
        touch(&course.join("01-A/.DS_Store"));

        let c = scan(tmp.path()).unwrap();
        assert_eq!(c.name, "My Course");
        let names: Vec<_> = c.chapters.iter().map(|ch| ch.name.as_str()).collect();
        assert_eq!(names, ["01-A", "02-B"]);
        let vids: Vec<_> = c.chapters[0].videos.iter().map(|p| file_name(p)).collect();
        assert_eq!(
            vids,
            ["Video_y.mp4", "Video_y_video2.mp4", "Video_y_video10.mp4"]
        );
        let docs: Vec<_> = c.chapters[0].docs.iter().map(|p| file_name(p)).collect();
        assert_eq!(docs, ["notes [a], b.pdf"]);
    }

    #[test]
    fn slugify_lowercases_and_dashes() {
        assert_eq!(slugify("05-On-Page SEO"), "05-on-page-seo");
        assert_eq!(
            slugify("11-Ranking Without Using a Website"),
            "11-ranking-without-using-a-website"
        );
    }

    #[test]
    fn natural_cmp_orders_numbers() {
        assert_eq!(natural_cmp("2-a", "10-a"), Ordering::Less);
    }
}
