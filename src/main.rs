//! course2skill: turn a video course into timestamped transcripts and agent skills.

mod ai;
mod asr;
mod chunk;
mod docs;
mod export;
mod graph;
mod layout;
mod media;
mod pipeline;
mod runs;
mod skill;
mod transcript;
mod tui;
mod wizard;

use ai::{AiConfig, Backend};
use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use layout::LayoutMode;
use pipeline::{Event, Opts, Stages};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::channel;

#[derive(Parser)]
#[command(
    name = "course2skill",
    version,
    about = "Turn a video course into timestamped transcripts and one agent skill per unit.\nRun with no arguments for the interactive wizard."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Handouts, then every transcript, then skills (end to end)
    Run(RunArgs),
    /// Handouts and transcripts only
    Transcribe(RunArgs),
    /// Convert handouts only
    Docs(RunArgs),
    /// Write skills from existing transcripts
    Skills(RunArgs),
    /// Show how the course will be grouped into units, and save plan.json
    Plan(RunArgs),
    /// Copy finished skills (and the course content and graph) somewhere to share or install
    Export(ExportArgs),
    /// Save one frame of a video: course2skill frame <video> <mm:ss> <out.jpg>
    Frame {
        video: PathBuf,
        at: String,
        out: PathBuf,
    },
    /// Install the local speech-to-text environment (uv + mlx-qwen3-asr)
    Setup(AsrArgs),
    /// Check tools, AI backends and the speech-to-text environment
    Doctor(AsrArgs),
}

#[derive(Args, Clone)]
struct ExportArgs {
    /// Where to put the export
    #[arg(long, default_value = "export")]
    to: PathBuf,
    /// Base output folder the runs live in
    #[arg(long, default_value = "result")]
    out: PathBuf,
    /// Which run to export ("latest" or a run folder name)
    #[arg(long, default_value = "latest")]
    run: String,
    /// Put skill folders directly in --to (e.g. --to ~/.claude/skills --flat)
    #[arg(long)]
    flat: bool,
    /// Also make one zip per skill and one for the whole course
    #[arg(long)]
    zip: bool,
    /// Leave out the course transcripts and files
    #[arg(long)]
    skills_only: bool,
    /// Replace skills that already exist at the destination
    #[arg(long)]
    force: bool,
}

#[derive(Args, Clone)]
struct AsrArgs {
    /// Python venv with mlx-qwen3-asr [default: ~/.cache/course2skill/qwen-venv]
    #[arg(long)]
    venv: Option<PathBuf>,
    /// Speech model (Qwen/Qwen3-ASR-0.6B is ~3x faster, a bit less accurate)
    #[arg(long, default_value = asr::DEFAULT_MODEL)]
    asr_model: String,
    /// Forced aligner used for word timestamps
    #[arg(long, default_value = asr::DEFAULT_ALIGNER)]
    aligner: String,
    /// Spoken language, e.g. English, Spanish, Chinese ("auto" to detect)
    #[arg(long, default_value = "English")]
    language: String,
}

#[derive(Args, Clone)]
struct AiArgs {
    /// AI that groups lessons (with --layout ai) and writes the skills
    #[arg(long, value_enum, default_value_t = Backend::Claude)]
    ai: Backend,
    /// Model for that AI (default: the tool's own default)
    #[arg(long)]
    ai_model: Option<String>,
    /// Base URL for API and local backends (e.g. http://localhost:1234/v1 for LM Studio)
    #[arg(long)]
    ai_url: Option<String>,
    /// Minutes before a stuck AI call is stopped
    #[arg(long, default_value_t = 90)]
    skill_timeout: u64,
}

#[derive(Args, Clone)]
struct RunArgs {
    /// Course folder, any layout
    course: PathBuf,
    /// Base output folder. Each run gets a timestamped folder inside it
    #[arg(long, default_value = "result")]
    out: PathBuf,
    /// Continue an existing run ("latest" or a folder name) instead of starting a new one.
    /// `skills` uses the latest run by default
    #[arg(long)]
    run: Option<String>,
    /// Always start a new run, even if the latest one did not finish
    #[arg(long, conflicts_with = "run")]
    new: bool,
    /// How lessons are grouped into units
    #[arg(long, value_enum, default_value_t = LayoutMode::Auto)]
    layout: LayoutMode,
    /// Only these units, 1-based: "1,3-5"
    #[arg(long)]
    units: Option<String>,
    /// Redo work that already has output
    #[arg(long)]
    force: bool,
    /// Plain log output instead of the TUI
    #[arg(long)]
    plain: bool,
    /// Skip the graphify knowledge graph built before the skills
    #[arg(long)]
    no_graph: bool,
    /// Skills written in parallel
    #[arg(long, default_value_t = 3)]
    skill_jobs: usize,
    /// Silence threshold in dB for chunk cuts
    #[arg(long, default_value_t = -35, allow_hyphen_values = true)]
    silence_db: i32,
    #[command(flatten)]
    ai: AiArgs,
    #[command(flatten)]
    asr: AsrArgs,
}

impl AiArgs {
    fn config(&self) -> AiConfig {
        AiConfig {
            backend: self.ai,
            model: self.ai_model.clone(),
            base_url: self.ai_url.clone(),
            timeout: std::time::Duration::from_secs(self.skill_timeout.max(1) * 60),
        }
    }
}

fn parse_units(s: &str) -> Result<Vec<usize>> {
    let mut out = Vec::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        if let Some((a, b)) = part.split_once('-') {
            let (a, b): (usize, usize) = (a.trim().parse()?, b.trim().parse()?);
            if a > b {
                bail!("bad range {part:?}: start is after end");
            }
            out.extend(a..=b);
        } else {
            out.push(
                part.parse()
                    .with_context(|| format!("bad unit number {part:?}"))?,
            );
        }
    }
    Ok(out)
}

fn out_dir(out: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(out)?;
    Ok(out.canonicalize()?)
}

/// Plans the course against the latest run's saved plan, without creating anything.
fn plan_for(args: &RunArgs) -> Result<layout::Plan> {
    let base = out_dir(&args.out)?;
    let prior = runs::resolve(&base, runs::LATEST).ok();
    let (root, files) = layout::inventory(&args.course, std::slice::from_ref(&base))?;
    wizard::make_plan(
        &root,
        &files,
        args.layout,
        &args.ai.config(),
        prior.as_deref(),
    )
}

/// Picks the run folder (new, latest or named), plans the course and saves the plan there.
fn start(args: &RunArgs, stages: Stages) -> Result<bool> {
    let base = out_dir(&args.out)?;
    // Check the AI before creating anything, so a failed start leaves no empty run.
    preflight(args, stages)?;
    let (root, files) = layout::inventory(&args.course, std::slice::from_ref(&base))?;
    let existing = match (&args.run, stages) {
        (Some(name), _) => Some(runs::resolve(&base, name)?),
        (None, Stages::SkillsOnly) => Some(
            runs::resolve(&base, runs::LATEST)
                .context("no earlier run to write skills from; use `run` or `transcribe` first")?,
        ),
        (None, _) if !args.new => runs::unfinished_latest(&base),
        _ => None,
    };
    let plan = match &existing {
        // Continuing a run: keep its own plan so units match what is on disk.
        Some(run) if run.join("plan.json").exists() => {
            let mut plan = layout::load_plan(run, &root)
                .context("the run's plan.json is for a different course folder")?;
            let added = layout::add_missing(&mut plan, &files);
            if added > 0 {
                eprintln!("note: {added} new course files added to unit \"New files\"");
            }
            plan
        }
        _ => {
            let prior = runs::list(&base)
                .into_iter()
                .rev()
                .find(|r| r.join("plan.json").exists());
            let mut plan = wizard::make_plan(
                &root,
                &files,
                args.layout,
                &args.ai.config(),
                prior.as_deref(),
            )?;
            let added = layout::add_missing(&mut plan, &files);
            if added > 0 {
                eprintln!(
                    "note: {added} course files not in the saved plan were added to unit \"New files\""
                );
            }
            plan
        }
    };
    let run = match existing {
        Some(run) => {
            eprintln!("continuing run {}", run.display());
            runs::touch_latest(&base, &run)?;
            run
        }
        None => runs::new_run(&base)?,
    };
    layout::save_plan(&plan, &run)?;
    run_pipeline(&plan, args, stages, &base, &run)
}

fn preflight(args: &RunArgs, stages: Stages) -> Result<()> {
    if matches!(stages, Stages::All | Stages::SkillsOnly) {
        let (ok, why) = args.ai.ai.availability();
        if !ok {
            bail!(
                "{} is not ready ({why}). Pick another with --ai",
                args.ai.ai.label()
            );
        }
    }
    Ok(())
}

fn print_plan(plan: &layout::Plan) {
    println!(
        "{} → {} units (edit result/latest/plan.json to regroup; the next run picks it up)",
        plan.course,
        plan.units.len()
    );
    for (i, u) in plan.units.iter().enumerate() {
        let videos = u
            .files
            .iter()
            .filter(|f| layout::kind(Path::new(f)) == Some(layout::Kind::Video))
            .count();
        println!(
            "{:>3}. {}  ({videos} videos, {} handouts) → {}",
            i + 1,
            u.name,
            u.files.len() - videos,
            u.folder
        );
    }
}

fn run_pipeline(
    plan: &layout::Plan,
    args: &RunArgs,
    stages: Stages,
    base: &Path,
    run: &Path,
) -> Result<bool> {
    let (course, warnings) = layout::course_from_plan(plan)?;
    let opts = Opts {
        out: run.to_path_buf(),
        cache: runs::cache_dir(base),
        venv: args.asr.venv.clone().unwrap_or_else(asr::default_venv),
        asr_model: args.asr.asr_model.clone(),
        aligner: args.asr.aligner.clone(),
        chapters: args.units.as_deref().map(parse_units).transpose()?,
        stages,
        force: args.force,
        skill_jobs: args.skill_jobs,
        ai: args.ai.config(),
        silence_db: args.silence_db,
        silence_min: 0.3,
        graph: !args.no_graph,
        language: if args.asr.language.eq_ignore_ascii_case("auto") {
            String::new()
        } else {
            args.asr.language.clone()
        },
    };
    if let Some(units) = &opts.chapters {
        let n = course.chapters.len();
        if let Some(bad) = units.iter().find(|u| **u == 0 || **u > n) {
            bail!("unit {bad} does not exist (the plan has {n} units; see `course2skill plan`)");
        }
    }
    let (tx, rx) = channel::<Event>();
    for w in warnings {
        let _ = tx.send(Event::Error(w));
    }
    let stop = Arc::new(AtomicBool::new(false));
    let worker = {
        let stop = Arc::clone(&stop);
        let tx2 = tx.clone();
        let opts = opts.clone();
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
                    println!("course: {course} ({} units)", chapters.len())
                }
                Event::Stage(s) => println!("── {s} ──"),
                Event::Chapter { idx, state } => println!("unit {}: {state:?}", idx + 1),
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

    let finished = tui::run(&rx, &stop)?;
    if !worker.is_finished() {
        // Stop agent CLIs that are still writing skills, then let the pipeline unwind.
        ai::kill_all();
        eprintln!("stopped. Progress is saved; run the same command again to continue this run.");
        return Ok(false);
    }
    let _ = worker.join();
    Ok(finished.unwrap_or(false))
}

fn run_export(a: &ExportArgs) -> Result<bool> {
    let base = out_dir(&a.out)?;
    let run = runs::resolve(&base, &a.run)?;
    std::fs::create_dir_all(&a.to)?;
    let s = export::export(&export::ExportOpts {
        run,
        to: a.to.canonicalize()?,
        flat: a.flat,
        zip: a.zip,
        skills_only: a.skills_only,
        force: a.force,
    })?;
    println!("exported {} skills to {}", s.skills.len(), s.dest.display());
    for sk in &s.skills {
        println!("  ✔ {sk}");
    }
    for sk in &s.incomplete {
        println!("  · {sk} (not finished in this run, skipped)");
    }
    for z in &s.zips {
        println!("  zip {}", z.display());
    }
    Ok(s.incomplete.is_empty())
}

fn doctor(a: &AsrArgs) -> Result<bool> {
    let mut ok = true;
    println!("tools");
    for (tool, arg) in [
        ("ffmpeg", "-version"),
        ("ffprobe", "-version"),
        ("pdftotext", "-v"),
        ("pandoc", "--version"),
        ("uv", "--version"),
    ] {
        let found = Command::new(tool).arg(arg).output().is_ok();
        println!("  {} {tool}", if found { "✔" } else { "✘" });
        ok &= found;
    }
    println!("speech to text");
    match asr::check_setup(&a.venv.clone().unwrap_or_else(asr::default_venv)) {
        Ok(py) => println!(
            "  ✔ mlx-qwen3-asr in {}\n  · model {} + {}",
            py.display(),
            a.asr_model,
            a.aligner
        ),
        Err(e) => {
            println!("  ✘ {e}");
            ok = false;
        }
    }
    println!("AI backends (need at least one)");
    for b in ai::ALL {
        let (ready, why) = b.availability();
        println!(
            "  {} {:<22} {why}",
            if ready { "✔" } else { "·" },
            b.label()
        );
    }
    Ok(ok)
}

fn wizard_flow() -> Result<bool> {
    let out = PathBuf::from("result");
    let Some(choice) = wizard::run(None, &out, None)? else {
        return Ok(true);
    };
    let args = RunArgs {
        course: choice.plan.root.clone(),
        out: out.clone(),
        run: None,
        new: true,
        no_graph: false,
        layout: LayoutMode::Auto,
        units: None,
        force: false,
        plain: false,
        skill_jobs: 3,
        silence_db: -35,
        ai: AiArgs {
            ai: choice.ai.backend,
            ai_model: choice.ai.model.clone(),
            ai_url: choice.ai.base_url.clone(),
            skill_timeout: choice.ai.timeout.as_secs() / 60,
        },
        asr: AsrArgs {
            venv: None,
            asr_model: asr::DEFAULT_MODEL.into(),
            aligner: asr::DEFAULT_ALIGNER.into(),
            language: "English".into(),
        },
    };
    let base = out_dir(&out)?;
    let run = runs::new_run(&base)?;
    layout::save_plan(&choice.plan, &run)?;
    run_pipeline(&choice.plan, &args, Stages::All, &base, &run)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.cmd {
        None => wizard_flow(),
        Some(Cmd::Run(a)) => start(&a, Stages::All),
        Some(Cmd::Transcribe(a)) => start(&a, Stages::TranscribeOnly),
        Some(Cmd::Docs(a)) => start(&a, Stages::DocsOnly),
        Some(Cmd::Skills(a)) => start(&a, Stages::SkillsOnly),
        Some(Cmd::Plan(a)) => plan_for(&a).map(|p| {
            print_plan(&p);
            true
        }),
        Some(Cmd::Export(a)) => run_export(&a),
        Some(Cmd::Frame { video, at, out }) => media::parse_ts(&at)
            .and_then(|t| media::frame(&video, t, &out))
            .map(|()| true),
        Some(Cmd::Setup(a)) => asr::setup(&a.venv.unwrap_or_else(asr::default_venv)).map(|py| {
            println!("ready: {}", py.display());
            true
        }),
        Some(Cmd::Doctor(a)) => doctor(&a),
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
    fn parse_units_handles_ranges() {
        assert_eq!(parse_units("1,3-5").unwrap(), vec![1, 3, 4, 5]);
    }

    #[test]
    fn parse_units_rejects_junk() {
        assert!(parse_units("x").is_err());
        assert!(parse_units("5-3").is_err());
    }
}
