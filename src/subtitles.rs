use crate::config::FormatConfig;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Serialize)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

fn sentence_end(text: &str) -> bool {
    text.trim_end_matches(['"', '\'', '”', '’', ')'])
        .ends_with(['.', '?', '!'])
}

// Favor punctuation and balanced lines without splitting a word.
fn wrap(words: &[Word], limit: usize) -> String {
    let parts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
    let text = parts.join(" ");
    if text.chars().count() <= limit || words.len() < 2 {
        return text;
    }
    let mut best = None;
    for i in 1..parts.len() {
        let left = parts[..i].join(" ");
        let right = parts[i..].join(" ");
        let a = left.chars().count();
        let b = right.chars().count();
        if a > limit || b > limit {
            continue;
        }
        let punctuation_bonus = if left.ends_with([',', ';', ':']) {
            8
        } else {
            0
        };
        let score = a.abs_diff(b) as i64 - punctuation_bonus;
        if best.as_ref().is_none_or(|(s, _, _)| score < *s) {
            best = Some((score, left, right));
        }
    }
    best.map_or(text, |(_, a, b)| format!("{a}\n{b}"))
}

pub fn cues(words: &[Word], config: &FormatConfig, duration: f64) -> Vec<Cue> {
    let mut groups: Vec<Vec<Word>> = Vec::new();
    let mut current: Vec<Word> = Vec::new();
    for word in words {
        // Some TDT exports return punctuation as separate timed tokens. Attach it
        // before cue splitting so commas/periods never become their own subtitles.
        let punctuation = word.text.trim();
        if !punctuation.is_empty()
            && punctuation.chars().all(|c| {
                matches!(
                    c,
                    '.' | ','
                        | ';'
                        | ':'
                        | '!'
                        | '?'
                        | '…'
                        | '%'
                        | '，'
                        | '。'
                        | '！'
                        | '？'
                        | '、'
                        | '；'
                        | '：'
                )
            })
        {
            if word.start.is_finite()
                && word.end.is_finite()
                && let Some(previous) = current.last_mut()
            {
                previous.text.push_str(punctuation);
                previous.end = previous.end.max(word.end.min(duration));
            }
            continue;
        }
        if !word.start.is_finite()
            || !word.end.is_finite()
            || word.end <= word.start
            || word.start < 0.0
            || word.start >= duration
            || word.text.trim().is_empty()
        {
            continue;
        }
        let mut clean = word.clone();
        clean.text = clean.text.split_whitespace().collect::<Vec<_>>().join(" ");
        clean.end = clean.end.min(duration);
        if let (Some(first), Some(last)) = (current.first(), current.last()) {
            let mut candidate = current.clone();
            candidate.push(clean.clone());
            let formatted = wrap(&candidate, config.line_chars);
            let too_long = formatted
                .lines()
                .any(|l| l.chars().count() > config.line_chars);
            if clean.start - last.end >= config.pause_seconds
                || sentence_end(&last.text)
                || (last.text.ends_with([',', ';', ':']) && last.end - first.start >= 2.0)
                || clean.end - first.start > config.max_duration
                || too_long
            {
                groups.push(std::mem::take(&mut current));
            }
        }
        current.push(clean);
    }
    if !current.is_empty() {
        groups.push(current);
    }
    let mut result = Vec::new();
    for (i, group) in groups.iter().enumerate() {
        let start = group[0].start;
        let spoken_end = group.last().unwrap().end;
        let next_start = groups.get(i + 1).map_or(duration, |g| g[0].start);
        let text = wrap(group, config.line_chars);
        let reading_time = text.chars().filter(|c| *c != '\n').count() as f64 / config.target_cps;
        let desired_end = (spoken_end + config.trailing_seconds)
            .max(start + config.min_duration)
            .max(start + reading_time);
        // Reading speed is a target. Never move the next dialogue or drop words to enforce it.
        let end = desired_end
            .min((start + config.max_duration).max(spoken_end))
            .min(next_start)
            .min(duration);
        if end > start {
            result.push(Cue { start, end, text });
        }
    }
    result
}

fn timestamp(seconds: f64) -> String {
    let ms = (seconds.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

pub fn srt(cues: &[Cue]) -> String {
    let mut out = String::new();
    for (i, cue) in cues.iter().enumerate() {
        out.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            i + 1,
            timestamp(cue.start),
            timestamp(cue.end),
            cue.text
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> FormatConfig {
        FormatConfig {
            line_chars: 42,
            max_duration: 6.0,
            min_duration: 1.0,
            pause_seconds: 0.7,
            trailing_seconds: 0.15,
            target_cps: 20.0,
        }
    }
    fn words(text: &str, offset: f64) -> Vec<Word> {
        text.split_whitespace()
            .enumerate()
            .map(|(i, text)| Word {
                text: text.into(),
                start: offset + i as f64 * 0.3,
                end: offset + i as f64 * 0.3 + 0.25,
            })
            .collect()
    }
    #[test]
    fn groups_words_into_a_sentence() {
        let c = cues(
            &words("I thought you weren't coming.", 12.1),
            &config(),
            20.0,
        );
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].text, "I thought you weren't coming.");
        assert!(srt(&c).starts_with("1\n00:00:12,100 --> "));
    }
    #[test]
    fn wraps_and_preserves_every_word() {
        let text = "We should leave before the others realize what happened. Then we can decide where to go.";
        let c = cues(&words(text, 0.0), &config(), 20.0);
        assert!(c.iter().all(
            |c| c.text.lines().count() <= 2 && c.text.lines().all(|l| l.chars().count() <= 42)
        ));
        assert_eq!(
            c.iter()
                .map(|c| c.text.replace('\n', " "))
                .collect::<Vec<_>>()
                .join(" "),
            text
        );
        assert!(c.iter().any(|c| c.text.contains('\n')));
    }
    #[test]
    fn attaches_separate_model_punctuation_to_words() {
        let c = cues(
            &words("Hello , world . Next sentence !", 0.0),
            &config(),
            10.0,
        );
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].text, "Hello, world.");
        assert_eq!(c[1].text, "Next sentence!");
    }
    #[test]
    fn preserves_silence_and_never_overlaps() {
        let mut w = words("Hello there.", 0.0);
        w.extend(words("Goodbye now.", 60.0));
        let c = cues(&w, &config(), 65.0);
        assert_eq!(c.len(), 2);
        assert_eq!(c[1].start, 60.0);
        assert!(c[0].end < 7.0);
        assert!(c.windows(2).all(|p| p[0].end <= p[1].start));
    }
    #[test]
    fn rejects_bad_timestamps_and_handles_unicode() {
        let mut w = words("Cześć świecie!", 3601.0);
        w.push(Word {
            text: "bad".into(),
            start: f64::NAN,
            end: 5.0,
        });
        assert!(srt(&cues(&w, &config(), 3605.0)).contains("01:00:01,000"));
        assert_eq!(cues(&w, &config(), 3605.0).len(), 1);
    }
}
