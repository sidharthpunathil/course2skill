//! Runs the stages (handouts, transcription, skills) and reports progress as events.

use crate::asr::{Asr, check_setup};
use crate::chunk::plan_chunks;
use crate::scan::{Chapter, Course, file_name, file_stem};
use crate::transcript::{Cache, Piece, render_markdown};
use crate::{docs, media, skill};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;

const MAX_CHUNK: f64 = 28.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stages {
    All,
    TranscribeOnly,
    SkillsOnly,
    DocsOnly,
}

#[derive(Debug, Clone)]
pub struct Opts {
    pub out: PathBuf,
    pub venv: PathBuf,
    pub model: PathBuf,
    pub chapters: Option<Vec<usize>>,
    pub stages: Stages,
    pub force: bool,
    pub skill_jobs: usize,
    pub skill_model: Option<String>,
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
    out.join(&ch.name)
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

    let mut infos = Vec::new();
    for ch in &course.chapters {
        let secs = ch
            .videos
            .iter()
            .filter_map(|v| media::probe_duration(v).ok())
            .sum();
        infos.push(ChInfo {
            name: ch.name.clone(),
            videos: ch.videos.len(),
            docs: ch.docs.len(),
            audio_secs: secs,
        });
    }
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
    for (i, _) in course
        .chapters
        .iter()
        .enumerate()
        .filter(|(i, _)| !selected.contains(i))
    {
        send(Event::Chapter {
            idx: i,
            state: ChState::Done,
        });
    }
    let mut ok = true;

    // Skill writers run in the background while later chapters transcribe.
    let (skill_tx, skill_rx) = channel::<usize>();
    let skill_rx = Arc::new(Mutex::new(skill_rx));
    let failed = Arc::new(AtomicBool::new(false));
    let mut skill_threads = Vec::new();
    if do_skills {
        for _ in 0..opts.skill_jobs.max(1) {
            let rx = Arc::clone(&skill_rx);
            let tx = tx.clone();
            let course = course.clone();
            let opts = opts.clone();
            let failed = Arc::clone(&failed);
            skill_threads.push(thread::spawn(move || {
                skill_worker(&rx, &tx, &course, &opts, &failed)
            }));
        }
    }

    let mut asr: Option<Asr> = None;
    for &idx in &selected {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let ch = &course.chapters[idx];
        let dir = result_chapter_dir(&opts.out, ch);
        std::fs::create_dir_all(&dir)?;

        if do_docs {
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
        }

        if do_asr {
            send(Event::Chapter {
                idx,
                state: ChState::Transcribing,
            });
            let mut chapter_ok = true;
            for video in &ch.videos {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let target = dir.join(format!("{}.md", file_stem(video)));
                if target.exists() && !opts.force {
                    send(Event::Log(format!("skip (done) {}", file_name(video))));
                    send(Event::SkippedAudio {
                        secs: media::probe_duration(video).unwrap_or(0.0),
                    });
                    continue;
                }
                if asr.is_none() {
                    send(Event::Log(
                        "loading ARK-ASR model (first time takes a moment)".into(),
                    ));
                    let python = check_setup(&opts.venv, &opts.model)?;
                    asr = Some(Asr::spawn(&python, &opts.model)?);
                    send(Event::Log("model ready".into()));
                }
                let Some(engine) = asr.as_mut() else { continue };
                if let Err(e) = transcribe_video(engine, idx, ch, video, &target, opts, tx, stop) {
                    chapter_ok = false;
                    ok = false;
                    send(Event::Error(format!("{}: {e:#}", file_name(video))));
                }
            }
            if stop.load(Ordering::Relaxed) {
                break;
            }
            if !chapter_ok {
                send(Event::Chapter {
                    idx,
                    state: ChState::Failed,
                });
                continue;
            }
            send(Event::Chapter {
                idx,
                state: ChState::Transcribed,
            });
        }

        if do_skills {
            send(Event::Chapter {
                idx,
                state: ChState::SkillQueued,
            });
            let _ = skill_tx.send(idx);
        } else {
            send(Event::Chapter {
                idx,
                state: ChState::Done,
            });
        }
    }
    // Free the model's memory before waiting on skill writers.
    drop(asr);
    drop(skill_tx);
    for t in skill_threads {
        let _ = t.join();
    }
    let ok = ok && !failed.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed);
    send(Event::Finished {
        ok,
        out: opts.out.clone(),
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn transcribe_video(
    asr: &mut Asr,
    idx: usize,
    ch: &Chapter,
    video: &Path,
    target: &Path,
    opts: &Opts,
    tx: &Sender<Event>,
    stop: &AtomicBool,
) -> Result<()> {
    let work = opts.out.join(".cache").join(&ch.name);
    std::fs::create_dir_all(&work)?;
    let stem = file_stem(video);
    let wav = work.join(format!("{stem}.wav"));
    let mut cache = Cache::load(&work.join(format!("{stem}.json")))?;

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
                let text = asr
                    .transcribe(&wav, *span)
                    .with_context(|| format!("chunk {i}"))?;
                let p = Piece {
                    start: span.start,
                    end: span.end,
                    text,
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
    rx: &Mutex<Receiver<usize>>,
    tx: &Sender<Event>,
    course: &Course,
    opts: &Opts,
    failed: &AtomicBool,
) {
    loop {
        let next = match rx.lock() {
            Ok(guard) => guard.recv(),
            Err(_) => return,
        };
        let Ok(idx) = next else { return };
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
        match skill::synthesize(
            &course.name,
            ch,
            &dir,
            &skills_root,
            opts.skill_model.as_deref(),
        ) {
            Ok(p) => {
                let _ = tx.send(Event::Log(format!("skill ready → {}", p.display())));
                let _ = tx.send(Event::Chapter {
                    idx,
                    state: ChState::Done,
                });
            }
            Err(e) => {
                failed.store(true, Ordering::Relaxed);
                let _ = tx.send(Event::Error(format!("skill {}: {e:#}", ch.slug)));
                let _ = tx.send(Event::Chapter {
                    idx,
                    state: ChState::Failed,
                });
            }
        }
    }
}
