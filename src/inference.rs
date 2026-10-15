use crate::{config::Config, subtitles::Word};
use anyhow::{Context, Result, ensure};
use parakeet_rs::{ExecutionConfig, ParakeetTDT, TimestampMode, Transcriber};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

const SAMPLE_RATE: usize = 16000;

pub struct Engine {
    model: Option<ParakeetTDT>,
    config: Config,
}

impl Engine {
    pub fn new(config: Config) -> Self {
        Self {
            model: None,
            config,
        }
    }

    fn model(&mut self) -> Result<&mut ParakeetTDT> {
        if self.model.is_none() {
            tracing::info!(model = %self.config.model_id, "loading model");
            let environment = parakeet_rs::ort::init().build()?;
            let config = ExecutionConfig::new()
                .with_environment(environment)
                .with_intra_threads(self.config.threads)
                .with_inter_threads(1)
                .with_custom_configure(|builder| {
                    Ok(builder
                        .with_config_entry("session.intra_op.allow_spinning", "0")?
                        .with_config_entry("session.inter_op.allow_spinning", "0")?)
                });
            self.model = Some(
                ParakeetTDT::from_pretrained(&self.config.model_dir, Some(config))
                    .context("could not load TDT model; run scripts/download-model.py first")?,
            );
        }
        Ok(self.model.as_mut().unwrap())
    }

    pub fn transcribe(
        &mut self,
        path: &Path,
        sample_limit: Option<usize>,
    ) -> Result<(Vec<Word>, f64)> {
        let mut input = File::open(path)?;
        let bytes = input.metadata()?.len();
        ensure!(
            bytes > 0 && bytes % 2 == 0,
            "audio must contain complete PCM s16le samples"
        );
        let samples = (bytes as usize / 2).min(sample_limit.unwrap_or(usize::MAX));
        let duration = samples as f64 / SAMPLE_RATE as f64;
        let chunk = self.config.chunk_seconds * SAMPLE_RATE;
        let overlap = self.config.overlap_seconds * SAMPLE_RATE;
        let windows = windows(samples, chunk, overlap);
        let mut words: Vec<Word> = Vec::new();
        for (index, window) in windows.iter().enumerate() {
            input.seek(SeekFrom::Start((window.start * 2) as u64))?;
            let mut raw = vec![0u8; (window.end - window.start) * 2];
            input.read_exact(&mut raw)?;
            let audio: Vec<f32> = raw
                .chunks_exact(2)
                .map(|s| i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0)
                .collect();
            // Skip only exact digital silence; do not gate quiet dialogue on an energy threshold.
            if audio.iter().all(|v| *v == 0.0) {
                continue;
            }
            let result = self.model()?.transcribe_samples(
                audio,
                SAMPLE_RATE as u32,
                1,
                Some(TimestampMode::Words),
            )?;
            let offset = window.start as f64 / SAMPLE_RATE as f64;
            for token in result.tokens {
                let word = Word {
                    text: token.text.trim().to_string(),
                    start: token.start as f64 + offset,
                    end: token.end as f64 + offset,
                };
                let middle = (word.start + word.end) / 2.0 * SAMPLE_RATE as f64;
                if middle >= window.own_start as f64
                    && middle < window.own_end as f64
                    && word.start.is_finite()
                    && word.end.is_finite()
                    && word.end > word.start
                {
                    words.push(word);
                }
            }
            tracing::info!(
                chunk = index + 1,
                total = windows.len(),
                "transcribed audio chunk"
            );
        }
        Ok((reconcile(words, duration), duration))
    }
}

#[derive(Debug)]
struct Window {
    start: usize,
    end: usize,
    own_start: usize,
    own_end: usize,
}

fn windows(samples: usize, chunk: usize, overlap: usize) -> Vec<Window> {
    let mut result = Vec::new();
    let mut start = 0;
    while start < samples {
        let end = (start + chunk).min(samples);
        result.push(Window {
            start,
            end,
            own_start: if start == 0 { 0 } else { start + overlap / 2 },
            own_end: if end == samples {
                samples
            } else {
                end - overlap / 2
            },
        });
        if end == samples {
            break;
        }
        start = end - overlap;
    }
    result
}

fn reconcile(mut words: Vec<Word>, duration: f64) -> Vec<Word> {
    words.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out: Vec<Word> = Vec::new();
    for mut word in words {
        if word.text.is_empty() || word.start >= duration || word.end <= 0.0 {
            continue;
        }
        if let Some(previous) = out.last() {
            // Same word at nearly the same instant can occur on either side of a chunk boundary.
            if previous.text.to_lowercase() == word.text.to_lowercase()
                && word.start < previous.end
                && (previous.start - word.start).abs() < 0.15
            {
                continue;
            }
            word.start = word.start.max(previous.end);
        }
        word.start = word.start.max(0.0);
        word.end = word.end.min(duration);
        if word.end > word.start {
            out.push(word);
        }
    }
    out
}

pub fn language_name(code: &str) -> Option<&'static str> {
    Some(match code {
        "bg" => "Bulgarian",
        "hr" => "Croatian",
        "cs" => "Czech",
        "da" => "Danish",
        "nl" => "Dutch",
        "en" => "English",
        "et" => "Estonian",
        "fi" => "Finnish",
        "fr" => "French",
        "de" => "German",
        "el" => "Greek",
        "hu" => "Hungarian",
        "it" => "Italian",
        "lv" => "Latvian",
        "lt" => "Lithuanian",
        "mt" => "Maltese",
        "pl" => "Polish",
        "pt" => "Portuguese",
        "ro" => "Romanian",
        "sk" => "Slovak",
        "sl" => "Slovenian",
        "es" => "Spanish",
        "sv" => "Swedish",
        "ru" => "Russian",
        "uk" => "Ukrainian",
        _ => return None,
    })
}

// Parakeet's TDT API returns text, not an explicit language label. This is a
// best-effort text classifier for Bazarr's missing-metadata path, not acoustic LID.
pub fn detect_language(words: &[Word]) -> Result<(&'static str, &'static str, f64)> {
    let text = words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let info = whatlang::detect(&text)
        .context("not enough speech to identify language; set media language metadata")?;
    ensure!(
        text.chars().count() >= 20 && info.confidence() >= 0.5,
        "language detection uncertain; set media language metadata"
    );
    let code = match info.lang().code() {
        "bul" => "bg",
        "hrv" => "hr",
        "ces" => "cs",
        "dan" => "da",
        "nld" => "nl",
        "eng" => "en",
        "est" => "et",
        "fin" => "fi",
        "fra" => "fr",
        "deu" => "de",
        "ell" => "el",
        "hun" => "hu",
        "ita" => "it",
        "lav" => "lv",
        "lit" => "lt",
        "pol" => "pl",
        "por" => "pt",
        "ron" => "ro",
        "slk" => "sk",
        "slv" => "sl",
        "spa" => "es",
        "swe" => "sv",
        "rus" => "ru",
        "ukr" => "uk",
        _ => anyhow::bail!("detected language is unsupported; set media language metadata"),
    };
    Ok((code, language_name(code).unwrap(), info.confidence()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chunk_ownership_covers_entire_movie_without_gaps() {
        let w = windows(16000 * 7200 + 5, 16000 * 30, 16000 * 2);
        assert_eq!(w[0].own_start, 0);
        assert_eq!(w.last().unwrap().own_end, 16000 * 7200 + 5);
        assert!(w.windows(2).all(|p| p[0].own_end == p[1].own_start));
        assert!(w.iter().all(|w| w.end - w.start <= 480000));
    }
    #[test]
    fn preserves_fast_repeated_words_when_their_timings_do_not_overlap() {
        let words = vec![
            Word {
                text: "no".into(),
                start: 0.0,
                end: 0.05,
            },
            Word {
                text: "no".into(),
                start: 0.06,
                end: 0.11,
            },
        ];
        assert_eq!(reconcile(words, 1.0).len(), 2);
    }
    #[test]
    fn keeps_repeated_dialogue_but_removes_boundary_duplicates() {
        let w = |start| Word {
            text: "No".into(),
            start,
            end: start + 0.2,
        };
        let result = reconcile(vec![w(29.0), w(29.02), w(29.5)], 60.0);
        assert_eq!(result.len(), 2);
        assert_eq!(result[1].start, 29.5);
    }
}
