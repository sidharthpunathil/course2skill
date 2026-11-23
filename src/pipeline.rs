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
    /// Build a graphify knowledge graph before writing skills.
    pub graph: bool,
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

    // Video lengths, probed in parallel (one thread per unit).
    let infos: Vec<ChInfo> = thread::scope(|scope| {
        let handles: Vec<_> = course
            .chapters
            .iter()
            .map(|ch| {
                scope.spawn(move || ChInfo {
                    name: ch.name.clone(),
                    videos: ch.videos.len(),
                    docs: ch.docs.len(),
                    audio_secs: ch
                        .videos
                        .iter()
                        .filter_map(|v| media::probe_duration(v).ok())
                        .sum(),
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
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
        // Videos still to transcribe, in course order; finished ones are skipped here.
        let mut jobs: Vec<Job> = Vec::new();
        let mut remaining: Vec<usize> = vec![0; course.chapters.len()];
        for &idx in &selected {
            let ch = &course.chapters[idx];
            let dir = result_chapter_dir(&opts.out, ch);
            std::fs::create_dir_all(&dir)?;
            let mut context: Option<std::sync::Arc<String>> = None;
            for (video, name) in ch.videos.iter().zip(&ch.transcript_names) {
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
                let context = context
                    .get_or_insert_with(|| std::sync::Arc::new(context_terms(&course.name, ch)))
                    .clone();
                remaining[idx] += 1;
                jobs.push(Job {
                    idx,
                    video: video.clone(),
                    target,
                    cache: cache_base(&opts.cache, &course.root, video, &opts.asr_model),
                    context,
                });
            }
            if remaining[idx] == 0 {
                send(Event::Chapter {
                    idx,
                    state: ChState::Transcribed,
                });
            }
        }

        let scan_videos: Vec<&PathBuf> = if do_skills {
            selected
                .iter()
                .flat_map(|&i| &course.chapters[i].videos)
                .collect()
        } else {
            Vec::new()
        };
        let scan_queue = Mutex::new(scan_videos.into_iter());
        let failed_asr = Mutex::new(BTreeSet::new());
        thread::scope(|scope| -> Result<()> {
            // Scene detection is CPU work; run it now, while the GPU transcribes.
            for _ in 0..SCAN_THREADS_DURING_ASR {
                scope.spawn(|| scan_worker(&scan_queue, opts, stop, &send));
            }
            // Prepare the next video's audio (extract, silences, chunk plan) ahead of the GPU.
            let (prep_tx, prep_rx) = std::sync::mpsc::sync_channel::<(Job, Result<Prepared>)>(1);
            let jobs_ref = &jobs;
            scope.spawn(move || {
                for job in jobs_ref {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let prepared = prepare(job, opts);
                    if prep_tx.send((job.clone(), prepared)).is_err() {
                        return;
                    }
                }
            });

            let mut asr: Option<Asr> = None;
            let mut started: BTreeSet<usize> = BTreeSet::new();
            for (job, prepared) in prep_rx {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                if started.insert(job.idx) {
                    send(Event::Chapter {
                        idx: job.idx,
                        state: ChState::Transcribing,
                    });
                }
                let mut done_secs = 0.0;
                let result = prepared.and_then(|p| {
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
                    let engine = asr.as_mut().context("speech model not loaded")?;
                    transcribe_prepared(engine, &job, &p, tx, stop, &mut done_secs)
                });
                if let Err(e) = result {
                    let total = media::probe_duration(&job.video).unwrap_or(done_secs);
                    send(Event::SkippedAudio {
                        secs: (total - done_secs).max(0.0),
                    });
                    if let Ok(mut f) = failed_asr.lock() {
                        f.insert(job.idx);
                    }
                    send(Event::Error(format!("{}: {e:#}", file_name(&job.video))));
                    // The worker may have died; start a fresh one for the next video.
                    asr = None;
                }
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                remaining[job.idx] -= 1;
                if remaining[job.idx] == 0 {
                    let bad = failed_asr.lock().is_ok_and(|f| f.contains(&job.idx));
                    send(Event::Chapter {
                        idx: job.idx,
                        state: if bad {
                            ChState::Failed
                        } else {
                            ChState::Transcribed
                        },
                    });
                }
            }
            // Free the speech model's memory before the skill stage.
            drop(asr);
            Ok(())
        })?;
        failed.extend(failed_asr.into_inner().unwrap_or_default());
        if !stop.load(Ordering::Relaxed) {
            send(Event::Log("all transcripts saved".into()));
        }
    }

    // Knowledge graph of the whole course, used by every skill writer.
    let mut graph_json: Option<PathBuf> = None;
    if do_skills && opts.graph && !stop.load(Ordering::Relaxed) {
        match crate::graph::builder(&opts.ai) {
            Ok(b) => {
                send(Event::Stage("Building knowledge graph (graphify)".into()));
                match crate::graph::build(course, &opts.out, &b, &opts.ai) {
                    Ok(g) => {
                        send(Event::Log(format!("knowledge graph → {}", g.display())));
                        graph_json = Some(g);
                    }
                    Err(e) => send(Event::Error(format!("knowledge graph skipped: {e:#}"))),
                }
            }
            Err(why) => send(Event::Log(format!("knowledge graph skipped: {why}"))),
        }
    }

    // Scene changes for every video (most were found during transcription and
    // come from the cache), so skill writers never wait.
    if do_skills && !stop.load(Ordering::Relaxed) {
        let videos: Vec<&PathBuf> = selected
            .iter()
            .flat_map(|&i| &course.chapters[i].videos)
            .collect();
        send(Event::Stage(format!(
            "Finding screen changes in {} videos",
            videos.len()
        )));
        let queue = Mutex::new(videos.into_iter());
        thread::scope(|scope| {
            for _ in 0..scan_threads() {
                scope.spawn(|| scan_worker(&queue, opts, stop, &send));
            }
        });
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
                scope.spawn(|| {
                    skill_worker(
                        &queue,
                        tx,
                        course,
                        opts,
                        stop,
                        &any_failed,
                        graph_json.as_deref(),
                    )
                });
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

/// One video waiting to be transcribed.
#[derive(Clone)]
struct Job {
    idx: usize,
    video: PathBuf,
    target: PathBuf,
    cache: PathBuf,
    context: std::sync::Arc<String>,
}

/// A video's audio, ready for the speech model.
struct Prepared {
    wav: PathBuf,
    duration: f64,
    spans: Vec<crate::chunk::Span>,
}

/// Scene-detection threads that run alongside transcription (the GPU does the
/// speech work, so a few CPU cores are free).
const SCAN_THREADS_DURING_ASR: usize = 2;

/// Scene-detection threads when nothing else is running: about half the cores,
/// since each ffmpeg uses more than one.
fn scan_threads() -> usize {
    std::thread::available_parallelism().map_or(4, |n| (n.get() / 2).clamp(2, 8))
}

/// Appends an extension (the cache base ends in the model tag, e.g. `.qwen`).
fn with_ext(base: &Path, ext: &str) -> PathBuf {
    PathBuf::from(format!("{}.{ext}", base.display()))
}

fn prepare(job: &Job, opts: &Opts) -> Result<Prepared> {
    if let Some(dir) = job.cache.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let wav = with_ext(&job.cache, "wav");
    media::extract_wav(&job.video, &wav)?;
    let duration = media::probe_duration(&wav)?;
    let silences = media::detect_silences(&wav, opts.silence_db, opts.silence_min)?;
    Ok(Prepared {
        wav,
        duration,
        spans: plan_chunks(duration, &silences, MAX_CHUNK),
    })
}

fn transcribe_prepared(
    asr: &mut Asr,
    job: &Job,
    p: &Prepared,
    tx: &Sender<Event>,
    stop: &AtomicBool,
    done_secs: &mut f64,
) -> Result<()> {
    let mut cache = Cache::load(&with_ext(&job.cache, "json"))?;
    let _ = tx.send(Event::VideoStart {
        idx: job.idx,
        name: file_name(&job.video),
        chunks: p.spans.len(),
        duration: p.duration,
    });
    let mut pieces = Vec::with_capacity(p.spans.len());
    for (i, span) in p.spans.iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let (piece, cached) = match cache.get(i) {
            Some(c) if (c.start - span.start).abs() < 0.01 && (c.end - span.end).abs() < 0.01 => {
                (c.clone(), true)
            }
            _ => {
                let (text, words) = asr
                    .transcribe(&p.wav, *span, &job.context)
                    .with_context(|| format!("chunk {i}"))?;
                let piece = Piece {
                    start: span.start,
                    end: span.end,
                    text,
                    words,
                };
                cache.put(i, piece.clone())?;
                (piece, false)
            }
        };
        *done_secs += span.len();
        let _ = tx.send(Event::Chunk {
            done: i + 1,
            total: p.spans.len(),
            audio_secs: span.len(),
            start: span.start,
            text: piece.text.clone(),
            cached,
        });
        pieces.push(piece);
    }
    let stem = file_stem(&job.video);
    let title = stem.trim_start_matches("Video_").replace('_', " ");
    let md = render_markdown(&title, &job.video, p.duration, &pieces);
    std::fs::write(&job.target, skill::strip_invisible(&md))?;
    let _ = std::fs::remove_file(&p.wav);
    let _ = tx.send(Event::VideoDone);
    Ok(())
}

fn scan_worker(
    queue: &Mutex<std::vec::IntoIter<&PathBuf>>,
    opts: &Opts,
    stop: &AtomicBool,
    send: &(dyn Fn(Event) + Sync),
) {
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let Some(v) = queue.lock().ok().and_then(|mut q| q.next()) else {
            return;
        };
        match crate::visual::cached_scenes(&opts.cache, v) {
            Ok(sc) => send(Event::Log(format!(
                "{} screen changes in {}",
                sc.len(),
                file_name(v)
            ))),
            Err(e) => send(Event::Error(format!(
                "scene detection {}: {e:#}",
                file_name(v)
            ))),
        }
    }
}

fn skill_worker(
    queue: &Mutex<std::vec::IntoIter<usize>>,
    tx: &Sender<Event>,
    course: &Course,
    opts: &Opts,
    stop: &AtomicBool,
    any_failed: &AtomicBool,
    graph: Option<&Path>,
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
        match skill::synthesize(
            &course.name,
            ch,
            &dir,
            &skills_root,
            &opts.ai,
            graph,
            &opts.cache,
        ) {
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
