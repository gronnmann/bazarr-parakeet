use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{env, path::PathBuf};

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: String,
    pub model_dir: PathBuf,
    pub model_id: String,
    pub data_dir: PathBuf,
    pub threads: usize,
    pub chunk_seconds: usize,
    pub overlap_seconds: usize,
    pub max_upload_bytes: usize,
    pub cache_entries: usize,
    pub format: FormatConfig,
}

#[derive(Clone, Debug, Serialize)]
pub struct FormatConfig {
    pub line_chars: usize,
    pub max_duration: f64,
    pub min_duration: f64,
    pub pause_seconds: f64,
    pub trailing_seconds: f64,
    pub target_cps: f64,
}

fn value<T: std::str::FromStr>(name: &str, default: T) -> Result<T> {
    match env::var(name) {
        Ok(v) => v
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid {name}: {v}")),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(e) => Err(e).with_context(|| format!("invalid {name}")),
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let config = Self {
            bind: value("BIND", "0.0.0.0:9000".to_string())?,
            model_dir: value("MODEL_DIR", PathBuf::from("models/ultra"))?,
            model_id: value("MODEL_ID", "parakeet-ultra".to_string())?,
            data_dir: value("DATA_DIR", PathBuf::from("data"))?,
            threads: value("INFERENCE_THREADS", 1)?,
            chunk_seconds: value("CHUNK_SECONDS", 30)?,
            overlap_seconds: value("OVERLAP_SECONDS", 2)?,
            max_upload_bytes: value("MAX_UPLOAD_BYTES", 512 * 1024 * 1024)?,
            cache_entries: value("CACHE_ENTRIES", 100)?,
            format: FormatConfig {
                line_chars: value("SUBTITLE_LINE_CHARS", 42)?,
                max_duration: value("SUBTITLE_MAX_SECONDS", 6.0)?,
                min_duration: value("SUBTITLE_MIN_SECONDS", 1.0)?,
                pause_seconds: value("SUBTITLE_PAUSE_SECONDS", 0.7)?,
                trailing_seconds: value("SUBTITLE_TRAILING_SECONDS", 0.15)?,
                target_cps: value("SUBTITLE_TARGET_CPS", 20.0)?,
            },
        };
        ensure!(config.threads > 0, "INFERENCE_THREADS must be positive");
        ensure!(
            (5..=120).contains(&config.chunk_seconds),
            "CHUNK_SECONDS must be 5..120"
        );
        ensure!(
            config.overlap_seconds > 0 && config.overlap_seconds * 2 < config.chunk_seconds,
            "OVERLAP_SECONDS must be positive and less than half CHUNK_SECONDS"
        );
        ensure!(
            config.max_upload_bytes >= 32000,
            "MAX_UPLOAD_BYTES must allow at least one second"
        );
        let f = &config.format;
        ensure!(
            (10..=100).contains(&f.line_chars),
            "SUBTITLE_LINE_CHARS must be 10..100"
        );
        ensure!(
            f.max_duration.is_finite()
                && f.min_duration.is_finite()
                && f.min_duration > 0.0
                && f.max_duration >= f.min_duration,
            "subtitle durations must be finite, positive, and min <= max"
        );
        ensure!(
            f.pause_seconds.is_finite()
                && f.pause_seconds > 0.0
                && f.trailing_seconds.is_finite()
                && f.trailing_seconds >= 0.0
                && f.target_cps.is_finite()
                && f.target_cps > 0.0,
            "invalid subtitle formatting settings"
        );
        Ok(config)
    }
}
