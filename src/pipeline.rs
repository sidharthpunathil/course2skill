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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChState {
    Pending,
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
            state: ChState::Done,
        });
    }
    let mut failed: BTreeSet<usize> = BTreeSet::new();

    if do_docs {
        send(Event::Stage("Handouts".into()));
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
            for doc in &ch.docs {
                let target = dir.join(format!("{}.md", file_stem(doc)));
                if target.exists() && !opts.force {
                    continue;
                }
                match docs::convert(doc) {
                    Ok(md) => {
                        std::fs::write(&target, skill::strip_invisible(&md))?;
                        send(Event::Log(format!("handout → {}", file_name(&target))));
                    }
                    Err(e) => send(Event::Error(format!("{}: {e:#}", file_name(doc)))),
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
            let context = context_terms(&course.name, ch, &dir);
            for video in &ch.videos {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let target = dir.join(format!("{}.md", file_stem(video)));
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
                    asr = Some(Asr::spawn(&python, &opts.asr_model, &opts.aligner)?);
                    send(Event::Log("speech model ready".into()));
                }
                let Some(engine) = asr.as_mut() else { continue };
                if let Err(e) =
                    transcribe_video(engine, idx, ch, video, &target, &context, opts, tx, stop)
                {
                    failed.insert(idx);
                    send(Event::Error(format!("{}: {e:#}", file_name(video))));
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
    send(Event::Finished {
        ok,
        out: opts.out.clone(),
    });
    Ok(())
}

/// Domain words for the speech model: course and unit names plus capitalized
/// terms from the unit's handouts (tool names, brands, jargon).
fn context_terms(course: &str, ch: &Chapter, result_dir: &Path) -> String {
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
        let md = result_dir.join(format!("{}.md", file_stem(doc)));
        if let Ok(text) = std::fs::read_to_string(md) {
            for w in text.split_whitespace().take(4000) {
                add(w);
            }
        }
    }
    terms.truncate(80);
    terms.join(" ")
}

#[allow(clippy::too_many_arguments)]
fn transcribe_video(
    asr: &mut Asr,
    idx: usize,
    ch: &Chapter,
    video: &Path,
    target: &Path,
    context: &str,
    opts: &Opts,
    tx: &Sender<Event>,
    stop: &AtomicBool,
) -> Result<()> {
    let work = opts.cache.join(&ch.out_rel);
    std::fs::create_dir_all(&work)?;
    let stem = file_stem(video);
    let wav = work.join(format!("{stem}.wav"));
    let mut cache = Cache::load(&work.join(format!("{stem}.qwen.json")))?;

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
            Some(p) if (p.start - span.start).abs() < 0.01 => (p.clone(), true),
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
        if skills_root.join(&ch.slug).join("SKILL.md").exists() && !opts.force {
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
            Ok(p) => {
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
