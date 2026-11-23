//! course2skill: turn a video course into markdown transcripts and agent skills.

mod asr;
mod chunk;
mod docs;
mod media;
mod pipeline;
mod scan;
mod skill;
mod transcript;
mod tui;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use pipeline::{Event, Opts, Stages};
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::channel;

#[derive(Parser)]
#[command(
    name = "course2skill",
    version,
    about = "Turn a video course into markdown transcripts and one agent skill per chapter"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Handouts, transcription and skills, end to end
    Run(RunArgs),
    /// Convert handouts and transcribe videos, no skills
    Transcribe(RunArgs),
    /// Convert handouts only
    Docs(RunArgs),
    /// Write skills from existing results
    Skills(RunArgs),
    /// Save one frame of a video: course2skill frame <video> <mm:ss> <out.jpg>
    Frame {
        video: PathBuf,
        at: String,
        out: PathBuf,
    },
    /// Check that every tool and the speech model are in place
    Doctor(SetupArgs),
}

#[derive(Args, Clone)]
struct SetupArgs {
    /// Python venv with ark_asr_mlx installed [default: ~/.cache/course2skill/.venv]
    #[arg(long)]
    venv: Option<PathBuf>,
    /// ARK-ASR model folder [default: ~/.cache/course2skill/ark-asr-3B-mlx]
    #[arg(long)]
    model: Option<PathBuf>,
}

#[derive(Args, Clone)]
struct RunArgs {
    /// Course folder (or a folder holding one course)
    course: PathBuf,
    /// Output folder
    #[arg(long, default_value = "result")]
    out: PathBuf,
    /// Only these chapters, 1-based: "1,3-5"
    #[arg(long)]
    chapters: Option<String>,
    /// Redo work that already has output
    #[arg(long)]
    force: bool,
    /// Plain log output instead of the TUI
    #[arg(long)]
    plain: bool,
    /// Skills written in parallel
    #[arg(long, default_value_t = 3)]
    skill_jobs: usize,
    /// Model passed to `claude --model` for skill writing
    #[arg(long)]
    skill_model: Option<String>,
    /// Silence threshold in dB for chunk cuts
    #[arg(long, default_value_t = -35, allow_hyphen_values = true)]
    silence_db: i32,
    #[command(flatten)]
    setup: SetupArgs,
}

fn parse_chapters(s: &str) -> Result<Vec<usize>> {
    let mut out = Vec::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        if let Some((a, b)) = part.split_once('-') {
            let (a, b): (usize, usize) = (a.trim().parse()?, b.trim().parse()?);
            out.extend(a..=b);
        } else {
            out.push(
                part.parse()
                    .with_context(|| format!("bad chapter {part:?}"))?,
            );
        }
    }
    Ok(out)
}

fn paths(setup: &SetupArgs) -> (PathBuf, PathBuf) {
    let home = asr::default_home();
    (
        setup.venv.clone().unwrap_or_else(|| home.join(".venv")),
        setup
            .model
            .clone()
            .unwrap_or_else(|| home.join("ark-asr-3B-mlx")),
    )
}

fn run_pipeline(args: RunArgs, stages: Stages) -> Result<bool> {
    let course = scan::scan(&args.course)?;
    let (venv, model) = paths(&args.setup);
    std::fs::create_dir_all(&args.out)?;
    let opts = Opts {
        out: args.out.canonicalize()?,
        venv,
        model,
        chapters: args.chapters.as_deref().map(parse_chapters).transpose()?,
        stages,
        force: args.force,
        skill_jobs: args.skill_jobs,
        skill_model: args.skill_model,
        silence_db: args.silence_db,
        silence_min: 0.3,
    };
    let (tx, rx) = channel::<Event>();
    let stop = Arc::new(AtomicBool::new(false));
    let worker = {
        let stop = Arc::clone(&stop);
        let course = course.clone();
        let tx2 = tx.clone();
        std::thread::spawn(move || {
            if let Err(e) = pipeline::run(&course, &opts, &tx2, &stop) {
                let _ = tx2.send(Event::Error(format!("{e:#}")));
                let _ = tx2.send(Event::Finished {
                    ok: false,
                    out: opts.out.clone(),
                });
            }
        })
    };
    drop(tx);

    if args.plain {
        let mut ok = true;
        for ev in rx {
            match ev {
                Event::Plan { course, chapters } => {
                    println!("course: {course} ({} chapters)", chapters.len())
                }
                Event::Chapter { idx, state } => println!("chapter {}: {state:?}", idx + 1),
                Event::VideoStart { name, chunks, .. } => println!("▶ {name} ({chunks} chunks)"),
                Event::Chunk {
                    done,
                    total,
                    start,
                    text,
                    ..
                } => {
                    println!(
                        "  [{done}/{total}] {} {}",
                        transcript::fmt_ts(start),
                        text.chars().take(100).collect::<String>()
                    )
                }
                Event::Log(m) => println!("· {m}"),
                Event::Error(m) => eprintln!("✘ {m}"),
                Event::Finished { ok: o, out } => {
                    ok = o;
                    println!(
                        "{} results in {}",
                        if o { "done," } else { "finished with errors," },
                        out.display()
                    );
                }
                Event::VideoDone | Event::SkippedAudio { .. } => {}
            }
        }
        let _ = worker.join();
        return Ok(ok);
    }

    tui::run(&rx, &stop)?;
    if !worker.is_finished() {
        eprintln!("stopping… progress is saved; rerun the same command to resume");
    }
    Ok(true)
}

fn doctor(setup: &SetupArgs) -> Result<bool> {
    let mut ok = true;
    for (tool, arg) in [
        ("ffmpeg", "-version"),
        ("ffprobe", "-version"),
        ("pdftotext", "-v"),
        ("pandoc", "--version"),
        ("claude", "--version"),
    ] {
        let found = Command::new(tool).arg(arg).output().is_ok();
        println!("{} {tool}", if found { "✔" } else { "✘" });
        ok &= found;
    }
    let (venv, model) = paths(setup);
    match asr::check_setup(&venv, &model) {
        Ok(py) => println!(
            "✔ ASR worker python {}\n✔ model {}",
            py.display(),
            model.display()
        ),
        Err(e) => {
            println!("✘ {e}");
            ok = false;
        }
    }
    Ok(ok)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Run(a) => run_pipeline(a, Stages::All),
        Cmd::Transcribe(a) => run_pipeline(a, Stages::TranscribeOnly),
        Cmd::Docs(a) => run_pipeline(a, Stages::DocsOnly),
        Cmd::Skills(a) => run_pipeline(a, Stages::SkillsOnly),
        Cmd::Frame { video, at, out } => media::parse_ts(&at)
            .and_then(|t| media::frame(&video, t, &out))
            .map(|()| true),
        Cmd::Doctor(s) => doctor(&s),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_chapters_handles_ranges() {
        assert_eq!(parse_chapters("1,3-5").unwrap(), vec![1, 3, 4, 5]);
    }

    #[test]
    fn parse_chapters_rejects_junk() {
        assert!(parse_chapters("x").is_err());
    }
}
