"""course2skill ASR worker.

Loads ARK-ASR once, then answers JSON lines on stdin:
  {"wav": "/path.wav", "start": 0.0, "end": 27.5}
with one JSON line on stdout:
  {"text": "..."} or {"error": "..."}
"""

import json
import sys


def reply(obj):
    sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def main():
    model_path = sys.argv[1]
    try:
        import numpy as np
        import soundfile as sf
        from ark_asr_mlx import ArkASR

        asr = ArkASR.from_pretrained(model_path)
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
            if len(clip) < rate // 10 or float(np.abs(clip).max(initial=0.0)) < 1e-4:
                reply({"text": ""})
                continue
            result = asr.transcribe(clip, sample_rate=rate, max_new_tokens=int(req.get("max_tokens", 384)))
            reply({"text": result.text.strip()})
        except Exception as error:
            reply({"error": f"{type(error).__name__}: {error}"})
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
