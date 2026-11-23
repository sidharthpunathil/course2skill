//! Optional stage: a graphify knowledge graph of the whole course, built after
//! every transcript exists and before skills are written. Skill writers read its
//! report and query it to link concepts across sections.

use crate::ai::{self, AiConfig, Backend};
use crate::layout::{Course, file_name};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const DIR: &str = "knowledge-graph";

/// How the graph gets built on this machine, if at all.
#[derive(Debug, Clone, PartialEq)]
pub enum Builder {
    /// `graphify extract` with an API backend (key found in the environment).
    Extract(&'static str),
    /// The `/graphify` skill run headless inside Claude Code.
    ClaudeSkill,
}

/// Picks a builder, or explains why none is available.
pub fn builder(ai_cfg: &AiConfig) -> Result<Builder, String> {
    if !ai::which("graphify") {
        return Err("graphify is not installed (pip install graphifyy)".into());
    }
    let has = |k: &str| std::env::var(k).is_ok_and(|v| !v.trim().is_empty());
    let api = match ai_cfg.backend {
        Backend::Anthropic if has("ANTHROPIC_API_KEY") => Some("claude"),
        Backend::Openai | Backend::Local
            if has("OPENAI_API_KEY") || ai_cfg.backend == Backend::Local =>
        {
            Some("openai")
        }
        _ if has("GEMINI_API_KEY") || has("GOOGLE_API_KEY") => Some("gemini"),
        _ if has("ANTHROPIC_API_KEY") => Some("claude"),
        _ => None,
    };
    if let Some(b) = api {
        return Ok(Builder::Extract(b));
    }
    let skill = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".claude/skills/graphify/SKILL.md");
    if ai_cfg.backend == Backend::Claude && skill.exists() {
        return Ok(Builder::ClaudeSkill);
    }
    Err("graphify needs an API key (GEMINI/ANTHROPIC/OPENAI) or Claude Code with the graphify skill".into())
}

/// `graph.json` under a run's knowledge-graph folder, wherever graphify put it.
pub fn find_graph(run: &Path) -> Option<PathBuf> {
    find_file(&run.join(DIR), "graph.json", 3)
}

fn find_file(dir: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let direct = dir.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    if depth == 0 {
        return None;
    }
    let mut subdirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    subdirs.sort();
    // graphify writes to graphify-out/; look there before the corpus.
    subdirs.sort_by_key(|p| file_name(p) != "graphify-out");
    subdirs
        .into_iter()
        .find_map(|d| find_file(&d, name, depth - 1))
}

/// Links (or copies) every transcript and course file into `knowledge-graph/corpus/<section>/`.
/// Returns true when the corpus changed since the last build.
fn stage_corpus(course: &Course, run: &Path) -> Result<bool> {
    let corpus = run.join(DIR).join("corpus");
    let mut listing = Vec::new();
    for ch in &course.chapters {
        let src_dir = run.join(&ch.out_rel);
        let names = ch
            .transcript_names
            .iter()
            .map(|n| format!("{n}.md"))
            .chain(ch.doc_names.iter().cloned());
        for name in names {
            let src = src_dir.join(&name);
            let Ok(meta) = std::fs::metadata(&src) else {
                continue;
            };
            let dst_dir = corpus.join(&ch.out_rel);
            std::fs::create_dir_all(&dst_dir)?;
            let dst = dst_dir.join(&name);
            if !dst.exists() && std::fs::hard_link(&src, &dst).is_err() {
                std::fs::copy(&src, &dst)?;
            }
            listing.push(format!("{}/{name}\t{}", ch.out_rel.display(), meta.len()));
        }
    }
    listing.sort();
    let manifest = run.join(DIR).join("corpus.txt");
    let now = listing.join("\n");
    let changed = std::fs::read_to_string(&manifest).ok().as_deref() != Some(now.as_str());
    std::fs::write(manifest, now)?;
    Ok(changed)
}

/// Builds or updates the graph. Returns the path of graph.json, or `None` when
/// the corpus is unchanged and a graph already exists.
pub fn build(course: &Course, run: &Path, builder: &Builder, ai_cfg: &AiConfig) -> Result<PathBuf> {
    let changed = stage_corpus(course, run)?;
    let existing = find_graph(run);
    if let (Some(g), false) = (&existing, changed) {
        return Ok(g.clone());
    }
    let kg = run.join(DIR);
    match builder {
        Builder::Extract(backend) => {
            let mut cmd = Command::new("graphify");
            cmd.current_dir(&kg)
                .args(["extract", "corpus", "--backend", backend]);
            if let Some(m) = &ai_cfg.model {
                cmd.args(["--model", m]);
            }
            if ai_cfg.backend == Backend::Local {
                let url = ai_cfg
                    .base_url
                    .clone()
                    .or_else(|| std::env::var("LOCAL_AI_BASE_URL").ok())
                    .unwrap_or_else(|| "http://localhost:11434/v1".into());
                cmd.env("OPENAI_BASE_URL", url)
                    .args(["--max-concurrency", "1"]);
            }
            run_logged(&mut cmd, &kg, "graphify extract")?;
        }
        Builder::ClaudeSkill => {
            let flags = if existing.is_some() { " --update" } else { "" };
            let prompt = format!(
                "/graphify corpus --wiki{flags}\n\nThis is a video course: each folder under corpus/ is one section with its video transcripts (markdown with [mm:ss] timestamps) and the instructor's course files. Build the knowledge graph without asking questions. Do not edit anything under corpus/."
            );
            let cfg = AiConfig {
                model: ai_cfg.model.clone(),
                ..ai_cfg.clone()
            };
            ai::run_agent_with_tools(
                &cfg,
                &prompt,
                &kg,
                "Bash,Read,Write,Edit,Glob,Grep,Agent,Skill",
            )
            .context("graphify via Claude Code")?;
        }
    }
    find_graph(run)
        .with_context(|| format!("graphify finished but no graph.json under {}", kg.display()))
}

fn run_logged(cmd: &mut Command, dir: &Path, what: &str) -> Result<()> {
    let log = std::fs::File::create(dir.join("graphify.log"))?;
    let status = cmd
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .status()
        .with_context(|| format!("could not start {what}"))?;
    if !status.success() {
        bail!(
            "{what} failed ({status}); see {}",
            dir.join("graphify.log").display()
        );
    }
    Ok(())
}

/// Prompt text telling a skill writer how to use the graph.
pub fn prompt_section(graph_json: &Path, agent: bool) -> String {
    let dir = graph_json.parent().unwrap_or(graph_json);
    let report = dir.join("GRAPH_REPORT.md");
    if agent {
        format!(
            "A knowledge graph of the whole course (every section) was built with graphify.\n\
             - Read `{}` before writing: it shows the main concepts, which sections they appear in, and how sections connect.\n\
             - Query it while writing, for example `graphify query \"<question>\" --graph \"{}\"`, `graphify explain \"<concept>\" --graph \"{}\"`, or `graphify path \"<A>\" \"<B>\" --graph \"{}\"`.\n\
             - Use it to keep terms consistent with other sections and to write a `## Related sections` part in SKILL.md naming the other section skills (by their skill names, like `05-on-page-seo`) that cover connected concepts, with one line each on the connection.",
            report.display(),
            graph_json.display(),
            graph_json.display(),
            graph_json.display()
        )
    } else {
        let text = std::fs::read_to_string(&report).unwrap_or_default();
        let clipped: String = text.chars().take(20_000).collect();
        format!(
            "Report from a knowledge graph of the whole course (every section), built with graphify. Use it to keep terms consistent and to write a `## Related sections` part in SKILL.md naming other section skills that cover connected concepts:\n\n<graph_report>\n{clipped}\n</graph_report>"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_graph_prefers_graphify_out() {
        let tmp = tempfile::tempdir().unwrap();
        let kg = tmp.path().join(DIR);
        std::fs::create_dir_all(kg.join("corpus/x")).unwrap();
        std::fs::create_dir_all(kg.join("graphify-out")).unwrap();
        std::fs::write(kg.join("corpus/x/graph.json"), "{}").unwrap();
        std::fs::write(kg.join("graphify-out/graph.json"), "{}").unwrap();
        assert_eq!(
            find_graph(tmp.path()).unwrap(),
            kg.join("graphify-out/graph.json")
        );
    }

    #[test]
    fn agent_prompt_mentions_report_and_queries() {
        let p = prompt_section(
            Path::new("/r/knowledge-graph/graphify-out/graph.json"),
            true,
        );
        assert!(p.contains("/r/knowledge-graph/graphify-out/GRAPH_REPORT.md"));
        assert!(p.contains("graphify query"));
        assert!(p.contains("## Related sections"));
    }
}
