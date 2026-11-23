//! AI backends: coding-agent CLIs, hosted APIs and local OpenAI-compatible servers.
//!
//! Agent CLIs can read and write files themselves, so for skill writing they get a
//! staged workspace. API and local backends only return text; callers parse files
//! out of that text.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Backend {
    /// Claude Code CLI (`claude`)
    Claude,
    /// OpenAI Codex CLI (`codex`)
    Codex,
    /// opencode CLI (`opencode`)
    Opencode,
    /// Google Gemini CLI (`gemini`)
    Gemini,
    /// Google Antigravity CLI (`agy`)
    Antigravity,
    /// Anthropic Messages API (ANTHROPIC_API_KEY, ANTHROPIC_BASE_URL)
    Anthropic,
    /// OpenAI-compatible API (OPENAI_API_KEY, OPENAI_BASE_URL)
    Openai,
    /// Local OpenAI-compatible server: Ollama, LM Studio, mlx_lm.server
    Local,
}

pub const ALL: &[Backend] = &[
    Backend::Claude,
    Backend::Codex,
    Backend::Opencode,
    Backend::Gemini,
    Backend::Antigravity,
    Backend::Anthropic,
    Backend::Openai,
    Backend::Local,
];

const ANTHROPIC_HOST: &str = "https://api.anthropic.com";
const OPENAI_HOST: &str = "https://api.openai.com/v1";
const LOCAL_DEFAULT: &str = "http://localhost:11434/v1";
const ANTHROPIC_DEFAULT_MODEL: &str = "claude-opus-5-5";
const OPENAI_DEFAULT_MODEL: &str = "gpt-5";

#[derive(Debug, Clone)]
pub struct AiConfig {
    pub backend: Backend,
    pub model: Option<String>,
    /// Overrides the base URL for API and local backends.
    pub base_url: Option<String>,
    /// An agent CLI still running after this long is stopped.
    pub timeout: Duration,
}

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(90 * 60);

impl Backend {
    pub fn label(self) -> &'static str {
        match self {
            Backend::Claude => "Claude Code",
            Backend::Codex => "Codex CLI",
            Backend::Opencode => "opencode",
            Backend::Gemini => "Gemini CLI",
            Backend::Antigravity => "Antigravity",
            Backend::Anthropic => "Anthropic API",
            Backend::Openai => "OpenAI-compatible API",
            Backend::Local => "Local model",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Backend::Claude => "coding agent, reads PDFs and frames itself",
            Backend::Codex => "coding agent, writes files in a sandbox",
            Backend::Opencode => "coding agent, any provider opencode is set up for",
            Backend::Gemini => "coding agent, long context",
            Backend::Antigravity => "coding agent (agy)",
            Backend::Anthropic => "API key, claude-opus-5-5 by default",
            Backend::Openai => "API key, any OpenAI-compatible endpoint",
            Backend::Local => "Ollama / LM Studio / mlx_lm.server, nothing leaves the machine",
        }
    }

    pub fn binary(self) -> Option<&'static str> {
        match self {
            Backend::Claude => Some("claude"),
            Backend::Codex => Some("codex"),
            Backend::Opencode => Some("opencode"),
            Backend::Gemini => Some("gemini"),
            Backend::Antigravity => Some("agy"),
            _ => None,
        }
    }

    /// Agent CLIs edit files themselves; the rest return text only.
    pub fn is_agent(self) -> bool {
        self.binary().is_some()
    }

    /// Whether this backend looks usable right now, with a short reason.
    pub fn availability(self) -> (bool, String) {
        if let Some(bin) = self.binary() {
            return match which(bin) {
                true => (true, format!("`{bin}` found")),
                false => (false, format!("`{bin}` not on PATH")),
            };
        }
        match self {
            Backend::Anthropic => env_status("ANTHROPIC_API_KEY"),
            Backend::Openai => env_status("OPENAI_API_KEY"),
            _ => {
                let url =
                    std::env::var("LOCAL_AI_BASE_URL").unwrap_or_else(|_| LOCAL_DEFAULT.into());
                let ok = ureq::get(&format!("{}/models", url.trim_end_matches('/')))
                    .timeout(Duration::from_millis(800))
                    .call()
                    .is_ok();
                (
                    ok,
                    if ok {
                        format!("server up at {url}")
                    } else {
                        format!("no server at {url}")
                    },
                )
            }
        }
    }
}

fn env_status(var: &str) -> (bool, String) {
    match std::env::var(var) {
        Ok(v) if !v.trim().is_empty() => (true, format!("{var} set")),
        _ => (false, format!("{var} not set")),
    }
}

pub fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join(bin).is_file()))
}

/// Asks for a plain-text answer. Agent CLIs run without write access.
/// Agent CLIs run in an empty temporary folder so they cannot touch real files.
pub fn complete(cfg: &AiConfig, prompt: &str) -> Result<String> {
    match cfg.backend {
        Backend::Anthropic => anthropic(cfg, prompt),
        Backend::Openai | Backend::Local => openai_compatible(cfg, prompt),
        _ => {
            let scratch = tempfile::tempdir()?;
            run_cli(cfg, prompt, scratch.path(), false, None)
        }
    }
}

/// Process ids of running agent CLIs, so quitting can stop them.
static CHILDREN: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

/// Stops every agent CLI still running (and anything it started).
pub fn kill_all() {
    let pids: Vec<u32> = CHILDREN
        .lock()
        .map(|mut c| std::mem::take(&mut *c))
        .unwrap_or_default();
    for pid in pids {
        // Each child leads its own process group; signal the whole group.
        let _ = Command::new("kill")
            .args(["-TERM", &format!("-{pid}")])
            .status();
    }
}

fn track(pid: u32, add: bool) {
    if let Ok(mut c) = CHILDREN.lock() {
        if add {
            c.push(pid);
        } else {
            c.retain(|p| *p != pid);
        }
    }
}

/// Tools Claude Code may use while writing a skill: file tools, the frame
/// grabber and graphify queries.
const SKILL_TOOLS: &str = "Read,Write,Edit,Glob,Grep,Skill,Bash(scripts/frame.sh:*),Bash(./scripts/frame.sh:*),Bash(bash scripts/frame.sh:*),Bash(ls:*),Bash(graphify query:*),Bash(graphify explain:*),Bash(graphify path:*)";

/// Runs an agent CLI with write access in `cwd`. Returns its final message.
pub fn run_agent(cfg: &AiConfig, prompt: &str, cwd: &Path) -> Result<String> {
    run_agent_with_tools(cfg, prompt, cwd, SKILL_TOOLS)
}

/// Like `run_agent`, with an explicit Claude Code tool allowlist.
pub fn run_agent_with_tools(
    cfg: &AiConfig,
    prompt: &str,
    cwd: &Path,
    claude_tools: &str,
) -> Result<String> {
    if !cfg.backend.is_agent() {
        bail!("{} cannot edit files; use complete()", cfg.backend.label());
    }
    run_cli(cfg, prompt, cwd, true, Some(claude_tools))
}

fn run_cli(
    cfg: &AiConfig,
    prompt: &str,
    cwd: &Path,
    write: bool,
    claude_tools: Option<&str>,
) -> Result<String> {
    let bin = cfg.backend.binary().unwrap_or_default();
    let mut cmd = Command::new(bin);
    cmd.current_dir(cwd);
    let mut via_stdin = true;
    let last_message = tempfile::NamedTempFile::new()?;
    match cfg.backend {
        Backend::Claude => {
            cmd.arg("-p");
            if write {
                cmd.args([
                    "--permission-mode",
                    "acceptEdits",
                    "--allowedTools",
                    claude_tools.unwrap_or(SKILL_TOOLS),
                ]);
            } else {
                cmd.args(["--allowedTools", "Read,Glob,Grep"]);
            }
        }
        Backend::Codex => {
            cmd.args([
                "exec",
                "--skip-git-repo-check",
                "-s",
                if write {
                    "workspace-write"
                } else {
                    "read-only"
                },
            ])
            .arg("-C")
            .arg(cwd)
            .arg("-o")
            .arg(last_message.path());
            if let Some(m) = &cfg.model {
                cmd.args(["-m", m]);
            }
            cmd.arg("-");
        }
        Backend::Gemini => {
            cmd.args([
                "-p",
                "Follow the instructions given above.",
                "--approval-mode",
                if write { "auto_edit" } else { "plan" },
            ]);
        }
        Backend::Antigravity => {
            via_stdin = false;
            cmd.args(["--mode", if write { "accept-edits" } else { "plan" }, "-p"])
                .arg(prompt);
        }
        Backend::Opencode => {
            via_stdin = false;
            cmd.arg("run");
            if let Some(m) = &cfg.model {
                cmd.args(["--model", m]);
            }
            cmd.arg(prompt);
        }
        _ => unreachable!("not a CLI backend"),
    }
    if let (Some(m), Backend::Claude | Backend::Gemini | Backend::Antigravity) =
        (&cfg.model, cfg.backend)
    {
        cmd.args(["--model", m]);
    }
    cmd.stdin(if via_stdin {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd
        .spawn()
        .with_context(|| format!("could not start `{bin}`"))?;
    let pid = child.id();
    track(pid, true);
    let timeout = cfg.timeout;
    std::thread::spawn(move || {
        std::thread::sleep(timeout);
        // Still registered means still running: stop its whole process group.
        if CHILDREN.lock().is_ok_and(|c| c.contains(&pid)) {
            let _ = Command::new("kill")
                .args(["-TERM", &format!("-{pid}")])
                .status();
        }
    });
    if via_stdin && let Some(mut stdin) = child.stdin.take() {
        // Write from a thread so a large prompt cannot deadlock against a full stdout pipe.
        let prompt = prompt.to_string();
        std::thread::spawn(move || {
            let _ = stdin.write_all(prompt.as_bytes());
        });
    }
    let started = std::time::Instant::now();
    let out = child.wait_with_output();
    track(pid, false);
    let out = out?;
    if !out.status.success() && started.elapsed() >= timeout {
        bail!(
            "{bin} was stopped after {} minutes (raise --skill-timeout)",
            timeout.as_secs() / 60
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().rev().take(6).collect();
        bail!(
            "{bin} exited with {}: {}",
            out.status,
            tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
        );
    }
    if cfg.backend == Backend::Codex {
        let msg = std::fs::read_to_string(last_message.path()).unwrap_or_default();
        if !msg.trim().is_empty() {
            return Ok(msg);
        }
    }
    Ok(stdout)
}

/// Adds auth headers. Off the provider's own host, the key also goes out as a
/// bearer token so a gateway in front of the API can authenticate it.
fn with_auth(
    req: ureq::Request,
    base: &str,
    own_host: &str,
    key: &str,
    x_api_key: bool,
) -> ureq::Request {
    let mut req = req;
    if x_api_key {
        req = req.set("x-api-key", key);
    }
    if !x_api_key || !base.starts_with(own_host) {
        req = req.set("Authorization", &format!("Bearer {key}"));
    }
    req
}

fn anthropic(cfg: &AiConfig, prompt: &str) -> Result<String> {
    let key = std::env::var("ANTHROPIC_API_KEY").context("ANTHROPIC_API_KEY is not set")?;
    let base = cfg
        .base_url
        .clone()
        .or_else(|| std::env::var("ANTHROPIC_BASE_URL").ok())
        .unwrap_or_else(|| ANTHROPIC_HOST.into());
    let base = base.trim_end_matches('/').to_string();
    let model = cfg
        .model
        .clone()
        .unwrap_or_else(|| ANTHROPIC_DEFAULT_MODEL.into());
    let first_party = base.starts_with(ANTHROPIC_HOST);
    let mut body = json!({
        "model": model,
        "max_tokens": 64000,
        "stream": true,
        "thinking": {"type": "adaptive"},
        "output_config": {"effort": "high"},
        "messages": [{"role": "user", "content": prompt}],
    });
    let mut req = ureq::post(&format!("{base}/v1/messages"))
        .timeout(Duration::from_secs(3600))
        .set("content-type", "application/json")
        .set("anthropic-version", "2026-10-21");
    if first_party {
        // Re-run on a fallback model if a safety classifier declines.
        body["fallbacks"] = json!("default");
        req = req.set("anthropic-beta", "server-side-fallback-2026-07-01");
    }
    let req = with_auth(req, &base, ANTHROPIC_HOST, &key, true);
    let resp = req.send_json(body).map_err(http_error)?;
    let mut text = String::new();
    let mut stop = String::new();
    for line in BufReader::new(resp.into_reader()).lines() {
        let line = line?;
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        let Ok(ev) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        match ev["type"].as_str() {
            Some("content_block_delta") if ev["delta"]["type"] == "text_delta" => {
                text.push_str(ev["delta"]["text"].as_str().unwrap_or_default());
            }
            Some("message_delta") => {
                if let Some(s) = ev["delta"]["stop_reason"].as_str() {
                    stop = s.to_string();
                }
            }
            Some("error") => bail!("Anthropic API error: {}", ev["error"]),
            _ => {}
        }
    }
    match stop.as_str() {
        "end_turn" | "stop_sequence" => Ok(text),
        "refusal" => bail!("the model declined this request"),
        "max_tokens" => bail!("the answer hit max_tokens and was cut off"),
        "" => bail!("the response stream ended early (no stop reason)"),
        other => bail!("unexpected stop reason {other:?}"),
    }
}

fn openai_compatible(cfg: &AiConfig, prompt: &str) -> Result<String> {
    let local = cfg.backend == Backend::Local;
    let (key, base) = if local {
        (
            std::env::var("LOCAL_AI_API_KEY").unwrap_or_default(),
            cfg.base_url
                .clone()
                .or_else(|| std::env::var("LOCAL_AI_BASE_URL").ok())
                .unwrap_or_else(|| LOCAL_DEFAULT.into()),
        )
    } else {
        (
            std::env::var("OPENAI_API_KEY").context("OPENAI_API_KEY is not set")?,
            cfg.base_url
                .clone()
                .or_else(|| std::env::var("OPENAI_BASE_URL").ok())
                .unwrap_or_else(|| OPENAI_HOST.into()),
        )
    };
    let base = base.trim_end_matches('/').to_string();
    let model = match (&cfg.model, local) {
        (Some(m), _) => m.clone(),
        (None, false) => OPENAI_DEFAULT_MODEL.into(),
        (None, true) => first_local_model(&base)?,
    };
    let mut req = ureq::post(&format!("{base}/chat/completions"))
        .timeout(Duration::from_secs(3600))
        .set("content-type", "application/json");
    if !key.is_empty() {
        req = with_auth(req, &base, OPENAI_HOST, &key, false);
    }
    let body = json!({ "model": model, "messages": [{"role": "user", "content": prompt}] });
    let resp: Value = req.send_json(body).map_err(http_error)?.into_json()?;
    if resp["choices"][0]["finish_reason"] == "length" {
        bail!("the answer hit the model's output limit and was cut off");
    }
    resp["choices"][0]["message"]["content"]
        .as_str()
        .map(str::to_string)
        .with_context(|| format!("unexpected response: {resp}"))
}

fn first_local_model(base: &str) -> Result<String> {
    let resp: Value = ureq::get(&format!("{base}/models"))
        .timeout(Duration::from_secs(5))
        .call()
        .map_err(http_error)
        .with_context(|| {
            format!("no local model server at {base}; start Ollama/LM Studio or pass --ai-url")
        })?
        .into_json()?;
    resp["data"][0]["id"]
        .as_str()
        .map(str::to_string)
        .context("the local server lists no models; pass --ai-model")
}

fn http_error(e: ureq::Error) -> anyhow::Error {
    match e {
        ureq::Error::Status(code, resp) => {
            let mut body = String::new();
            let _ = resp.into_reader().take(2000).read_to_string(&mut body);
            anyhow::anyhow!("HTTP {code}: {body}")
        }
        other => anyhow::anyhow!("{other}"),
    }
}

/// Parses `=== FILE: path ===` ... `=== END FILE ===` blocks from model text.
pub fn parse_file_blocks(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut current: Option<(String, Vec<&str>)> = None;
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t
            .strip_prefix("=== FILE:")
            .and_then(|r| r.strip_suffix("==="))
        {
            current = Some((rest.trim().to_string(), Vec::new()));
        } else if t == "=== END FILE ===" {
            if let Some((path, lines)) = current.take() {
                out.push((path, lines.join("\n") + "\n"));
            }
        } else if let Some((_, lines)) = current.as_mut() {
            lines.push(line);
        }
    }
    out
}

/// Pulls the first JSON object out of model text (handles ```json fences and chatter).
pub fn extract_json(text: &str) -> Result<Value> {
    let start = text.find('{').context("no JSON object in the answer")?;
    let end = text.rfind('}').context("no JSON object in the answer")?;
    serde_json::from_str(&text[start..=end]).context("the answer's JSON did not parse")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_blocks_parse() {
        let t = "intro\n=== FILE: SKILL.md ===\n---\nname: x\n---\nbody\n=== END FILE ===\n=== FILE: references/a.md ===\nA\n=== END FILE ===\n";
        let f = parse_file_blocks(t);
        assert_eq!(f.len(), 2);
        assert_eq!(
            f[0],
            (
                "SKILL.md".to_string(),
                "---\nname: x\n---\nbody\n".to_string()
            )
        );
        assert_eq!(f[1].0, "references/a.md");
    }

    #[test]
    fn json_is_found_inside_fences() {
        let v = extract_json("Sure:\n```json\n{\"units\": [1]}\n```").unwrap();
        assert_eq!(v["units"][0], 1);
    }
}
