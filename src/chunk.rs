//! Splits audio into ASR-sized spans, cutting at pauses where possible.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub start: f64,
    pub end: f64,
}

impl Span {
    pub fn len(&self) -> f64 {
        self.end - self.start
    }
}

/// Trailing chunks shorter than this get merged into the previous one.
const MIN_TAIL: f64 = 0.3;

/// Parses ffmpeg `silencedetect` stderr into silence spans.
pub fn parse_silences(stderr: &str) -> Vec<Span> {
    let mut out = Vec::new();
    let mut open: Option<f64> = None;
    for line in stderr.lines() {
        if let Some(v) = value_after(line, "silence_start:") {
            open = Some(v);
        } else if let Some(v) = value_after(line, "silence_end:")
            && let Some(start) = open.take()
        {
            out.push(Span {
                start: start.max(0.0),
                end: v,
            });
        }
    }
    out
}

fn value_after(line: &str, key: &str) -> Option<f64> {
    let rest = &line[line.find(key)? + key.len()..];
    rest.split_whitespace().next()?.parse().ok()
}

/// Plans contiguous spans covering `[0, duration]`, each no longer than `max`.
pub fn plan_chunks(duration: f64, silences: &[Span], max: f64) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    let mut cursor = 0.0;
    while duration - cursor > 1e-6 {
        let limit = cursor + max;
        if limit >= duration {
            spans.push(Span {
                start: cursor,
                end: duration,
            });
            break;
        }
        let cut = silences
            .iter()
            .map(|s| (s.start + s.end) / 2.0)
            .filter(|&mid| mid > cursor + max / 4.0 && mid <= limit)
            .fold(None, |best: Option<f64>, mid| {
                Some(best.map_or(mid, |b| b.max(mid)))
            })
            .unwrap_or(limit);
        spans.push(Span {
            start: cursor,
            end: cut,
        });
        cursor = cut;
    }
    if spans.len() >= 2 {
        let tail = spans[spans.len() - 1];
        let prev = spans[spans.len() - 2];
        if tail.len() < MIN_TAIL && tail.end - prev.start <= max {
            spans.pop();
            if let Some(last) = spans.last_mut() {
                last.end = tail.end;
            }
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_silences_reads_start_end_pairs() {
        let s = "[silencedetect @ 0x1] silence_start: 10.5\n[silencedetect @ 0x1] silence_end: 11.2 | silence_duration: 0.7\n";
        assert_eq!(
            parse_silences(s),
            vec![Span {
                start: 10.5,
                end: 11.2
            }]
        );
    }

    #[test]
    fn plan_without_silence_hard_cuts_under_max() {
        let spans = plan_chunks(70.0, &[], 28.0);
        assert_eq!(spans.len(), 3);
        assert!(spans.iter().all(|s| s.len() <= 28.0 + 1e-9));
        assert!(
            spans
                .windows(2)
                .all(|w| (w[0].end - w[1].start).abs() < 1e-9)
        );
        assert!((spans[2].end - 70.0).abs() < 1e-9);
    }

    #[test]
    fn plan_cuts_at_silence_midpoint() {
        let spans = plan_chunks(
            50.0,
            &[Span {
                start: 20.0,
                end: 21.0,
            }],
            28.0,
        );
        assert!((spans[0].end - 20.5).abs() < 1e-9);
    }

    #[test]
    fn plan_merges_tiny_tail() {
        let spans = plan_chunks(
            28.1,
            &[Span {
                start: 9.9,
                end: 10.1,
            }],
            28.0,
        );
        assert!(spans.iter().all(|s| s.len() <= 28.0 + 1e-9));
        assert!((spans.last().unwrap().end - 28.1).abs() < 1e-9);
    }
}
