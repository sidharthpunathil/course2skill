"""course2skill ASR worker (Qwen3-ASR on MLX, word timestamps via the forced aligner).

argv: <asr model> <aligner model>
Prints {"ready": true} once both models are loaded, then answers JSON lines on stdin:
  {"wav": "/path.wav", "start": 0.0, "end": 118.2, "context": "terms"}
with one JSON line on stdout:
  {"text": "...", "words": [["word", start, end], ...]}   (times relative to "start")
or {"error": "..."}.
"""

import json
import os
import sys


def reply(obj):
    sys.__stdout__.write(json.dumps(obj, ensure_ascii=False) + "\n")
    sys.__stdout__.flush()


def main():
    asr_name, aligner_name = sys.argv[1], sys.argv[2]
    # Keep library chatter (download bars, warnings) off the protocol channel.
    sys.stdout = sys.stderr
    os.environ.setdefault("HF_HUB_DISABLE_PROGRESS_BARS", "1")
    try:
        import numpy as np
        import soundfile as sf
        import mlx_qwen3_asr as qasr

        model, _config = qasr.load_model(asr_name)
        aligner = qasr.ForcedAligner(aligner_name)
    except Exception as error:  # report load failures to the parent
        reply({"fatal": f"{type(error).__name__}: {error}"})
        return 1

    reply({"ready": True})
    cached_path, audio, rate = None, None, 16000
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
            if req["wav"] != cached_path:
                audio, rate = sf.read(req["wav"], dtype="float32", always_2d=False)
                if audio.ndim > 1:
                    audio = audio.mean(axis=1)
                cached_path = req["wav"]
            a = max(0, int(req["start"] * rate))
            b = min(len(audio), int(req["end"] * rate))
            clip = np.ascontiguousarray(audio[a:b])
            if len(clip) < rate // 5 or float(np.abs(clip).max(initial=0.0)) < 1e-4:
                reply({"text": "", "words": []})
                continue
            result = qasr.transcribe(
                (clip, rate),
                model=model,
                forced_aligner=aligner,
                return_timestamps=True,
                language=req.get("language") or "English",
                context=req.get("context", ""),
            )
            words = [
                [str(s.get("text", "")), float(s.get("start", 0.0)), float(s.get("end", 0.0))]
                for s in (result.segments or [])
            ]
            reply({"text": (result.text or "").strip(), "words": words})
        except Exception as error:
            reply({"error": f"{type(error).__name__}: {error}"})
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
