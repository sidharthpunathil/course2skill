//! Runs the stages in order and reports progress as events:
//! 1. handouts to markdown, 2. every video transcribed and saved, 3. skills.

use crate::ai::AiConfig;
use crate::asr::{Asr, check_setup};
use crate::chunk::plan_chunks;
use crate::layout::{Chapter, Course, file_name, file_stem};
use crate::transcript::{Cache, Piece, render_markdown};
use crate::{docs, media, skill};
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread;

/// Audio sent to the worker per request. Qwen3-ASR chunks internally; this
/// size keeps progress moving and bounds what a crash can lose.
const MAX_CHUNK: f64 = 120.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stages {
    All,
    TranscribeOnly,
    SkillsOnly,
    DocsOnly,
}

#[derive(Debug, Clone)]
pub struct Opts {
    /// This run's folder.
    pub out: PathBuf,
    /// Speech cache shared by every run.
    pub cache: PathBuf,
    pub venv: PathBuf,
    pub asr_model: String,
    pub aligner: String,
    pub chapters: Option<Vec<usize>>,
    pub stages: Stages,
    pub force: bool,
    pub skill_jobs: usize,
    pub ai: AiConfig,
    pub silence_db: i32,
    pub silence_min: f64,
    /// Forced transcription language; empty lets the model detect it.
    pub language: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChState {
    Pending,
    /// Not selected with --units.
    Skipped,
    Docs,
    Transcribing,
    Transcribed,
    SkillQueued,
    Skill,
    Done,
    Failed,
}

#[derive(Debug, Clone)]
pub struct ChInfo {
    pub name: String,
    pub videos: usize,
    pub docs: usize,
    pub audio_secs: f64,
}

#[derive(Debug, Clone)]
pub enum Event {
    Plan {
        course: String,
        chapters: Vec<ChInfo>,
    },
    Stage(String),
    Chapter {
        idx: usize,
        state: ChState,
    },
    VideoStart {
        idx: usize,
        name: String,
        chunks: usize,
        duration: f64,
    },
    Chunk {
        done: usize,
        total: usize,
        audio_secs: f64,
        start: f64,
        text: String,
        cached: bool,
    },
    VideoDone,
    /// Audio that will not be transcribed this run (already done or filtered out).
    SkippedAudio {
        secs: f64,
    },
    Log(String),
    Error(String),
    Finished {
        ok: bool,
        out: PathBuf,
    },
}

pub fn result_chapter_dir(out: &Path, ch: &Chapter) -> PathBuf {
    out.join(&ch.out_rel)
}

/// Runs the pipeline on the current thread. `stop` aborts between chunks.
pub fn run(course: &Course, opts: &Opts, tx: &Sender<Event>, stop: &AtomicBool) -> Result<()> {
    let send = |e: Event| {
        let _ = tx.send(e);
    };
    let selected: Vec<usize> = (0..course.chapters.len())
        .filter(|i| opts.chapters.as_ref().is_none_or(|c| c.contains(&(i + 1))))
        .collect();
    let do_docs = matches!(
        opts.stages,
        Stages::All | Stages::TranscribeOnly | Stages::DocsOnly
    );
    let do_asr = matches!(opts.stages, Stages::All | Stages::TranscribeOnly);
    let do_skills = matches!(opts.stages, Stages::All | Stages::SkillsOnly);

    let infos: Vec<ChInfo> = course
        .chapters
        .iter()
        .map(|ch| ChInfo {
            name: ch.name.clone(),
            videos: ch.videos.len(),
            docs: ch.docs.len(),
            audio_secs: ch
                .videos
                .iter()
                .filter_map(|v| media::probe_duration(v).ok())
                .sum(),
        })
        .collect();
    let skipped: f64 = infos
        .iter()
        .enumerate()
        .filter(|(i, _)| !do_asr || !selected.contains(i))
        .map(|(_, c)| c.audio_secs)
        .sum();
    send(Event::Plan {
        course: course.name.clone(),
        chapters: infos,
    });
    send(Event::SkippedAudio { secs: skipped });
    for i in (0..course.chapters.len()).filter(|i| !selected.contains(i)) {
        send(Event::Chapter {
            idx: i,
            state: ChState::Skipped,
        });
    }
    let mut failed: BTreeSet<usize> = BTreeSet::new();

    if do_docs {
        send(Event::Stage("Copying course files".into()));
        for &idx in &selected {
            let ch = &course.chapters[idx];
            let dir = result_chapter_dir(&opts.out, ch);
            std::fs::create_dir_all(&dir)?;
            if ch.docs.is_empty() {
                continue;
            }
            send(Event::Chapter {
                idx,
                state: ChState::Docs,
            });
            for (doc, name) in ch.docs.iter().zip(&ch.doc_names) {
                let target = dir.join(name);
                let same_size = std::fs::metadata(&target).ok().map(|m| m.len())
                    == std::fs::metadata(doc).ok().map(|m| m.len());
                if target.exists() && same_size && !opts.force {
                    continue;
                }
                match std::fs::copy(doc, &target) {
                    Ok(_) => send(Event::Log(format!("copied {name}"))),
                    Err(e) => {
                        failed.insert(idx);
                        send(Event::Error(format!("copying {}: {e}", file_name(doc))));
                    }
                }
            }
            send(Event::Chapter {
                idx,
                state: ChState::Pending,
            });
        }
    }

    if do_asr {
        send(Event::Stage("Transcribing".into()));
        let mut asr: Option<Asr> = None;
        for &idx in &selected {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let ch = &course.chapters[idx];
            let dir = result_chapter_dir(&opts.out, ch);
            std::fs::create_dir_all(&dir)?;
            send(Event::Chapter {
                idx,
                state: ChState::Transcribing,
            });
            let context = context_terms(&course.name, ch);
            for (video, name) in ch.videos.iter().zip(&ch.transcript_names) {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let target = dir.join(format!("{name}.md"));
                if target.exists() && !opts.force {
                    send(Event::Log(format!(
                        "already transcribed: {}",
                        file_name(video)
                    )));
                    send(Event::SkippedAudio {
                        secs: media::probe_duration(video).unwrap_or(0.0),
                    });
                    continue;
                }
                if asr.is_none() {
                    send(Event::Log(format!(
                        "loading {} + aligner (first run downloads them)",
                        opts.asr_model
                    )));
                    let python = check_setup(&opts.venv)?;
                    asr = Some(Asr::spawn(
                        &python,
                        &opts.asr_model,
                        &opts.aligner,
                        &opts.language,
                    )?);
                    send(Event::Log("speech model ready".into()));
                }
                let Some(engine) = asr.as_mut() else { continue };
                let cache = cache_base(&opts.cache, &course.root, video, &opts.asr_model);
                let mut done_secs = 0.0;
                if let Err(e) = transcribe_video(
                    engine,
                    idx,
                    video,
                    &cache,
                    &target,
                    &context,
                    opts,
                    tx,
                    stop,
                    &mut done_secs,
                ) {
                    // Take the rest of this video out of the progress total.
                    let total = media::probe_duration(video).unwrap_or(done_secs);
                    send(Event::SkippedAudio {
                        secs: (total - done_secs).max(0.0),
                    });
                    failed.insert(idx);
                    send(Event::Error(format!("{}: {e:#}", file_name(video))));
                    // The worker may have died; start a fresh one for the next video.
                    asr = None;
                }
            }
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let state = if failed.contains(&idx) {
                ChState::Failed
            } else {
                ChState::Transcribed
            };
            send(Event::Chapter { idx, state });
        }
        // Free the speech model's memory before the skill stage.
        drop(asr);
        if !stop.load(Ordering::Relaxed) {
            send(Event::Log("all transcripts saved".into()));
        }
    }

    let mut skills_failed = false;
    if do_skills && !stop.load(Ordering::Relaxed) {
        send(Event::Stage(format!(
            "Writing skills with {}",
            opts.ai.backend.label()
        )));
        let queue: Vec<usize> = selected
            .iter()
            .copied()
            .filter(|i| !failed.contains(i))
            .collect();
        for &idx in &queue {
            send(Event::Chapter {
                idx,
                state: ChState::SkillQueued,
            });
        }
        let queue = Mutex::new(queue.into_iter());
        let any_failed = AtomicBool::new(false);
        thread::scope(|scope| {
            for _ in 0..opts.skill_jobs.max(1) {
                scope.spawn(|| skill_worker(&queue, tx, course, opts, stop, &any_failed));
            }
        });
        skills_failed = any_failed.load(Ordering::Relaxed);
    } else if !do_skills {
        for &idx in selected.iter().filter(|i| !failed.contains(i)) {
            send(Event::Chapter {
                idx,
                state: ChState::Done,
            });
        }
    }

    let ok = failed.is_empty() && !skills_failed && !stop.load(Ordering::Relaxed);
    if ok && opts.stages == Stages::All && opts.chapters.is_none() {
        crate::runs::mark_done(&opts.out)?;
    }
    send(Event::Finished {
        ok,
        out: opts.out.clone(),
    });
    Ok(())
}

/// Domain words for the speech model: course and unit names plus capitalized
/// terms from the unit's files (tool names, brands, jargon). Read in memory;
/// nothing is written.
fn context_terms(course: &str, ch: &Chapter) -> String {
    let mut terms: Vec<String> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut add = |w: &str| {
        let w = w.trim_matches(|c: char| !c.is_alphanumeric());
        if w.len() > 2
            && w.chars().next().is_some_and(char::is_uppercase)
            && seen.insert(w.to_lowercase())
        {
            terms.push(w.to_string());
        }
    };
    for w in course
        .split(['.', ' ', '-', '_'])
        .chain(ch.name.split([' ', '-', '_', '›']))
    {
        add(w);
    }
    for doc in &ch.docs {
        if let Ok(text) = docs::convert(doc) {
            for w in text.split_whitespace().take(4000) {
                add(w);
            }
        }
    }
    terms.truncate(80);
    terms.join(" ")
}

/// Cache location for one video: keyed by its path inside the course and the
/// speech model, so regrouping units or changing output names keeps the cache.
fn cache_base(cache: &Path, root: &Path, video: &Path, model: &str) -> PathBuf {
    let rel = video.strip_prefix(root).unwrap_or(video);
    let tag = if model == crate::asr::DEFAULT_MODEL {
        "qwen".to_string()
    } else {
        format!("qwen-{}", crate::layout::slugify(model))
    };
    let parent = rel.parent().map(Path::to_path_buf).unwrap_or_default();
    cache
        .join(parent)
        .join(format!("{}.{tag}", file_stem(video)))
}

#[allow(clippy::too_many_arguments)]
fn transcribe_video(
    asr: &mut Asr,
    idx: usize,
    video: &Path,
    cache_base: &Path,
    target: &Path,
    context: &str,
    opts: &Opts,
    tx: &Sender<Event>,
    stop: &AtomicBool,
    done_secs: &mut f64,
) -> Result<()> {
    if let Some(dir) = cache_base.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let stem = file_stem(video);
    // Append (not replace) extensions: the base ends in the model tag, e.g. `.qwen`.
    let with_ext = |ext: &str| PathBuf::from(format!("{}.{ext}", cache_base.display()));
    let wav = with_ext("wav");
    let mut cache = Cache::load(&with_ext("json"))?;

    media::extract_wav(video, &wav)?;
    let duration = media::probe_duration(&wav)?;
    let silences = media::detect_silences(&wav, opts.silence_db, opts.silence_min)?;
    let spans = plan_chunks(duration, &silences, MAX_CHUNK);
    let _ = tx.send(Event::VideoStart {
        idx,
        name: file_name(video),
        chunks: spans.len(),
        duration,
    });

    let mut pieces = Vec::with_capacity(spans.len());
    for (i, span) in spans.iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let (piece, cached) = match cache.get(i) {
            Some(p) if (p.start - span.start).abs() < 0.01 && (p.end - span.end).abs() < 0.01 => {
                (p.clone(), true)
            }
            _ => {
                let (text, words) = asr
                    .transcribe(&wav, *span, context)
                    .with_context(|| format!("chunk {i}"))?;
                let p = Piece {
                    start: span.start,
                    end: span.end,
                    text,
                    words,
                };
                cache.put(i, p.clone())?;
                (p, false)
            }
        };
        *done_secs += span.len();
        let _ = tx.send(Event::Chunk {
            done: i + 1,
            total: spans.len(),
            audio_secs: span.len(),
            start: span.start,
            text: piece.text.clone(),
            cached,
        });
        pieces.push(piece);
    }
    let title = stem.trim_start_matches("Video_").replace('_', " ");
    let md = render_markdown(&title, video, duration, &pieces);
    std::fs::write(target, skill::strip_invisible(&md))?;
    let _ = std::fs::remove_file(&wav);
    let _ = tx.send(Event::VideoDone);
    Ok(())
}

fn skill_worker(
    queue: &Mutex<std::vec::IntoIter<usize>>,
    tx: &Sender<Event>,
    course: &Course,
    opts: &Opts,
    stop: &AtomicBool,
    any_failed: &AtomicBool,
) {
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let next = match queue.lock() {
            Ok(mut q) => q.next(),
            Err(_) => return,
        };
        let Some(idx) = next else { return };
        let ch = &course.chapters[idx];
        let skills_root = opts.out.join("skills");
        if skills_root.join(&ch.slug).join(skill::COMPLETE).exists() && !opts.force {
            let _ = tx.send(Event::Log(format!("skill exists, skipping {}", ch.slug)));
            let _ = tx.send(Event::Chapter {
                idx,
                state: ChState::Done,
            });
            continue;
        }
        let _ = tx.send(Event::Chapter {
            idx,
            state: ChState::Skill,
        });
        let _ = tx.send(Event::Log(format!("writing skill {}", ch.slug)));
        let dir = result_chapter_dir(&opts.out, ch);
        match skill::synthesize(&course.name, ch, &dir, &skills_root, &opts.ai) {
            Ok((p, notes)) => {
                for n in notes {
                    let _ = tx.send(Event::Log(n));
                }
                let _ = tx.send(Event::Log(format!("skill ready → {}", p.display())));
                let _ = tx.send(Event::Chapter {
                    idx,
                    state: ChState::Done,
                });
            }
            Err(e) => {
                any_failed.store(true, Ordering::Relaxed);
                let _ = tx.send(Event::Error(format!("skill {}: {e:#}", ch.slug)));
                let _ = tx.send(Event::Chapter {
                    idx,
                    state: ChState::Failed,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_base_keeps_old_layout_and_tags_model() {
        let b = cache_base(
            Path::new("/r/.cache"),
            Path::new("/c"),
            Path::new("/c/01-Intro/1.1 Hi.mp4"),
            crate::asr::DEFAULT_MODEL,
        );
        assert_eq!(b, Path::new("/r/.cache/01-Intro/1.1 Hi.qwen"));
        let small = cache_base(
            Path::new("/r/.cache"),
            Path::new("/c"),
            Path::new("/c/a.mp4"),
            "Qwen/Qwen3-ASR-0.6B",
        );
        assert_eq!(small, Path::new("/r/.cache/a.qwen-qwen-qwen3-asr-0-6b"));
    }
}
