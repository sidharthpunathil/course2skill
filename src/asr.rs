//! Client for the persistent Python ARK-ASR worker.

use crate::chunk::Span;
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const WORKER_SRC: &str = include_str!("../worker/asr_worker.py");

pub struct Asr {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    // Keeps the embedded worker script alive (deleted on drop).
    _script: Option<tempfile::TempPath>,
}

/// Default location for the worker venv and model.
pub fn default_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".cache/course2skill")
}

/// Returns the venv's python, failing with the paths a user needs to fix.
pub fn check_setup(venv: &Path, model: &Path) -> Result<PathBuf> {
    let python = venv.join("bin/python");
    if !python.exists() {
        bail!(
            "worker venv missing at {}. Create it with:\n  uv venv --python 3.12 {}\n  uv pip install --python {} -e {}",
            venv.display(),
            venv.display(),
            python.display(),
            model.display()
        );
    }
    if !model.join("model.safetensors").exists() {
        bail!(
            "ARK-ASR model missing at {}. Download it with:\n  hf download leope/ark-asr-3B-mlx --local-dir {}",
            model.display(),
            model.display()
        );
    }
    Ok(python)
}

impl Asr {
    /// Starts the worker and waits until the model is loaded.
    pub fn spawn(python: &Path, model: &Path) -> Result<Asr> {
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
            bail!(
                "ASR worker could not load model at {}: {fatal}",
                model.display()
            );
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
            let n = self.stdout.read_line(&mut line)?;
            if n == 0 {
                let status = self.child.try_wait().ok().flatten();
                bail!("ASR worker exited unexpectedly ({status:?})");
            }
            // Libraries sometimes print to stdout; only JSON objects are protocol lines.
            if line.trim_start().starts_with('{') {
                return serde_json::from_str(line.trim()).context("bad JSON from ASR worker");
            }
        }
    }

    pub fn transcribe(&mut self, wav: &Path, span: Span) -> Result<String> {
        let req = json!({ "wav": wav, "start": span.start, "end": span.end });
        writeln!(self.stdin, "{req}")?;
        self.stdin.flush()?;
        let resp = self.read_line()?;
        if let Some(err) = resp.get("error").and_then(Value::as_str) {
            bail!("transcription failed at {:.1}s: {err}", span.start);
        }
        Ok(resp
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
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
        // SAFETY: guarded by ENV mutex; tests in this module are the only writers.
        unsafe { std::env::set_var("COURSE2SKILL_WORKER", &script) };
        let r = Asr::spawn(Path::new("python3"), Path::new("/nonexistent"));
        unsafe { std::env::remove_var("COURSE2SKILL_WORKER") };
        r
    }

    #[test]
    fn roundtrip_with_fake_worker() {
        let _g = ENV.lock().unwrap();
        let mut asr = with_fake(
            "import sys,json\nprint('noise')\nprint(json.dumps({'ready':True}),flush=True)\nfor l in sys.stdin:\n  print(json.dumps({'text':'hi'}),flush=True)\n",
        )
        .unwrap();
        let t = asr
            .transcribe(
                Path::new("a b.wav"),
                Span {
                    start: 0.0,
                    end: 1.0,
                },
            )
            .unwrap();
        assert_eq!(t, "hi");
    }

    #[test]
    fn worker_exit_is_error() {
        let _g = ENV.lock().unwrap();
        let err = with_fake("import sys\nsys.exit(3)\n").err().unwrap();
        assert!(err.to_string().contains("worker"), "{err}");
    }

    #[test]
    fn fatal_load_names_model_path() {
        let _g = ENV.lock().unwrap();
        let err = with_fake("import json\nprint(json.dumps({'fatal':'boom'}))\n")
            .err()
            .unwrap();
        assert!(err.to_string().contains("/nonexistent"), "{err}");
    }
}
