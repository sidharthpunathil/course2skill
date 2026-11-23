//! Client for the persistent Python Qwen3-ASR worker.

use crate::chunk::Span;
use crate::transcript::Word;
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const WORKER_SRC: &str = include_str!("../worker/asr_worker.py");
pub const DEFAULT_MODEL: &str = "Qwen/Qwen3-ASR-1.7B";
pub const DEFAULT_ALIGNER: &str = "Qwen/Qwen3-ForcedAligner-0.6B";
const PACKAGES: &[&str] = &["mlx-qwen3-asr[aligner]", "soundfile", "numpy"];

pub struct Asr {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    // Keeps the embedded worker script alive (deleted on drop).
    _script: Option<tempfile::TempPath>,
}

/// Default location for the worker venv.
pub fn default_venv() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".cache/course2skill/qwen-venv")
}

/// Returns the venv's python, failing with the command that fixes it.
pub fn check_setup(venv: &Path) -> Result<PathBuf> {
    let python = venv.join("bin/python");
    if !python.exists() {
        bail!(
            "speech-to-text env missing at {}. Run: course2skill setup",
            venv.display()
        );
    }
    let ok = Command::new(&python)
        .args(["-c", "import mlx_qwen3_asr, soundfile, numpy"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        bail!(
            "mlx-qwen3-asr is not installed in {}. Run: course2skill setup",
            venv.display()
        );
    }
    Ok(python)
}

/// Creates the venv with uv and installs mlx-qwen3-asr.
pub fn setup(venv: &Path) -> Result<PathBuf> {
    if !crate::ai::which("uv") {
        bail!("`uv` is required: curl -LsSf https://astral.sh/uv/install.sh | sh");
    }
    if !venv.join("bin/python").exists() {
        let st = Command::new("uv")
            .args(["venv", "--python", "3.12"])
            .arg(venv)
            .status()?;
        if !st.success() {
            bail!("uv venv failed");
        }
    }
    let python = venv.join("bin/python");
    let st = Command::new("uv")
        .args(["pip", "install", "--python"])
        .arg(&python)
        .args(PACKAGES)
        .status()?;
    if !st.success() {
        bail!("installing {} failed", PACKAGES.join(", "));
    }
    check_setup(venv)
}

impl Asr {
    /// Starts the worker and waits until both models are loaded (first run downloads them).
    pub fn spawn(python: &Path, model: &str, aligner: &str) -> Result<Asr> {
        let (script_path, script) = match std::env::var_os("COURSE2SKILL_WORKER") {
            Some(p) => (PathBuf::from(p), None),
            None => {
                let mut f = tempfile::Builder::new().suffix(".py").tempfile()?;
                f.write_all(WORKER_SRC.as_bytes())?;
                let tp = f.into_temp_path();
                (tp.to_path_buf(), Some(tp))
            }
        };
        let mut child = Command::new(python)
            .arg(&script_path)
            .arg(model)
            .arg(aligner)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("could not start ASR worker with {}", python.display()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("worker stdin unavailable"))?;
        let stdout = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| anyhow!("worker stdout unavailable"))?,
        );
        let mut asr = Asr {
            child,
            stdin,
            stdout,
            _script: script,
        };
        let hello = asr.read_line()?;
        if let Some(fatal) = hello.get("fatal").and_then(Value::as_str) {
            bail!("ASR worker could not load {model}: {fatal}");
        }
        if hello.get("ready").and_then(Value::as_bool) != Some(true) {
            bail!("ASR worker sent unexpected greeting: {hello}");
        }
        Ok(asr)
    }

    fn read_line(&mut self) -> Result<Value> {
        let mut line = String::new();
        loop {
            line.clear();
            if self.stdout.read_line(&mut line)? == 0 {
                let status = self.child.try_wait().ok().flatten();
                bail!("ASR worker exited unexpectedly ({status:?})");
            }
            // Only JSON objects are protocol lines; ignore anything else.
            if line.trim_start().starts_with('{') {
                return serde_json::from_str(line.trim()).context("bad JSON from ASR worker");
            }
        }
    }

    /// Transcribes `span` of `wav`. Word times come back absolute (offset by `span.start`).
    pub fn transcribe(
        &mut self,
        wav: &Path,
        span: Span,
        context: &str,
    ) -> Result<(String, Vec<Word>)> {
        let req = json!({ "wav": wav, "start": span.start, "end": span.end, "context": context });
        writeln!(self.stdin, "{req}")?;
        self.stdin.flush()?;
        let resp = self.read_line()?;
        if let Some(err) = resp.get("error").and_then(Value::as_str) {
            bail!("transcription failed at {:.1}s: {err}", span.start);
        }
        let text = resp
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let words = resp
            .get("words")
            .and_then(Value::as_array)
            .map(|ws| {
                ws.iter()
                    .filter_map(|w| {
                        let t = w.get(0)?.as_str()?.to_string();
                        let s = w.get(1)?.as_f64()? + span.start;
                        let e = w.get(2)?.as_f64()? + span.start;
                        Some(Word {
                            text: t,
                            start: s,
                            end: e,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok((text, words))
    }
}

impl Drop for Asr {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Tests touch a process-wide env var.
    static ENV: Mutex<()> = Mutex::new(());

    fn with_fake(src: &str) -> Result<Asr> {
        let dir = tempfile::tempdir().unwrap().keep();
        let script = dir.join("fake.py");
        std::fs::write(&script, src).unwrap();
        // SAFETY: guarded by the ENV mutex; these tests are the only writers.
        unsafe { std::env::set_var("COURSE2SKILL_WORKER", &script) };
        let r = Asr::spawn(Path::new("python3"), "m-model", "m-aligner");
        unsafe { std::env::remove_var("COURSE2SKILL_WORKER") };
        r
    }

    #[test]
    fn roundtrip_offsets_word_times() {
        let _g = ENV.lock().unwrap();
        let mut asr = with_fake(
            "import sys,json\nprint('noise')\nprint(json.dumps({'ready':True}),flush=True)\nfor l in sys.stdin:\n  print(json.dumps({'text':'hi there','words':[['hi',0.5,0.7],['there',0.8,1.0]]}),flush=True)\n",
        )
        .unwrap();
        let (text, words) = asr
            .transcribe(
                Path::new("a b.wav"),
                Span {
                    start: 100.0,
                    end: 200.0,
                },
                "",
            )
            .unwrap();
        assert_eq!(text, "hi there");
        assert_eq!(words.len(), 2);
        assert!((words[0].start - 100.5).abs() < 1e-9);
    }

    #[test]
    fn worker_exit_is_error() {
        let _g = ENV.lock().unwrap();
        let err = with_fake("import sys\nsys.exit(3)\n").err().unwrap();
        assert!(err.to_string().contains("worker"), "{err}");
    }

    #[test]
    fn fatal_load_names_model() {
        let _g = ENV.lock().unwrap();
        let err = with_fake("import json\nprint(json.dumps({'fatal':'boom'}))\n")
            .err()
            .unwrap();
        assert!(err.to_string().contains("m-model"), "{err}");
    }
}
