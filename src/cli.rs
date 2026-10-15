use crate::{config::Config, inference::Engine, subtitles};
use anyhow::{Context, Result, ensure};
use clap::{Args, Parser, Subcommand};
use serde::Deserialize;
use std::collections::HashMap;
use std::{
    io::Write,
    num::NonZeroUsize,
    path::PathBuf,
    process::{Command, Stdio},
};

#[derive(Parser)]
#[command(
    version,
    about = "Generate SRT subtitles with Parakeet Ultra or serve Bazarr requests"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Action>,
}

#[derive(Subcommand)]
pub enum Action {
    /// Start the Bazarr-compatible HTTP service (default when no command is given).
    Serve,
    /// Transcribe a local audio or video file to SRT. Requires FFmpeg.
    Transcribe(TranscribeArgs),
}

#[derive(Args)]
pub struct TranscribeArgs {
    /// Input audio or video file.
    pub input: PathBuf,
    /// Output SRT path; defaults to the input filename with .srt extension. Use - for stdout.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Directory containing a compatible Parakeet TDT ONNX model (overrides MODEL_DIR).
    #[arg(long)]
    pub model_dir: Option<PathBuf>,
    /// Number of inference threads (overrides INFERENCE_THREADS).
    #[arg(long)]
    pub threads: Option<NonZeroUsize>,
    /// Explicit zero-based audio track index; overrides automatic language selection.
    #[arg(long)]
    pub audio_stream: Option<u32>,
    /// Preferred audio language tag; falls back to the first track if absent.
    #[arg(long, default_value = "en")]
    pub audio_language: String,
    /// FFprobe executable used for automatic track selection.
    #[arg(long, default_value = "ffprobe")]
    pub ffprobe: PathBuf,
    /// FFmpeg executable.
    #[arg(long, default_value = "ffmpeg")]
    pub ffmpeg: PathBuf,
    /// Replace an existing output file.
    #[arg(long)]
    pub force: bool,
}

pub fn transcribe(args: TranscribeArgs, mut config: Config) -> Result<()> {
    ensure!(
        args.input.is_file(),
        "Input file does not exist: {}",
        args.input.display()
    );
    let output = args
        .output
        .unwrap_or_else(|| args.input.with_extension("srt"));
    let stdout = output == std::path::Path::new("-");
    if !stdout {
        if output.exists() {
            ensure!(
                std::fs::canonicalize(&args.input)? != std::fs::canonicalize(&output)?,
                "Output must not replace the input media file"
            );
            ensure!(
                args.force,
                "Output already exists: {} (use --force to replace it)",
                output.display()
            );
        }
        let parent = output
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new("."));
        ensure!(
            parent.is_dir(),
            "Output directory does not exist: {}",
            parent.display()
        );
    }
    if let Some(path) = args.model_dir {
        config.model_dir = path;
    }
    if let Some(threads) = args.threads {
        config.threads = threads.get();
    }
    let input = std::fs::canonicalize(&args.input)?;
    let audio_stream = match args.audio_stream {
        Some(index) => index,
        None => probe_audio_stream(&args.ffprobe, &input, &args.audio_language)?,
    };
    tracing::info!(audio_stream, "selected audio track");
    let pcm = tempfile::NamedTempFile::new()?;
    // Keep input presentation timestamps so resampling can retain initial audio delay.
    let result = Command::new(&args.ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-threads",
            "1",
            "-copyts",
            "-start_at_zero",
            "-i",
        ])
        .arg(&input)
        .args([
            "-map",
            &format!("0:a:{audio_stream}"),
            "-vn",
            "-af",
            "aresample=async=1:first_pts=0",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-c:a",
            "pcm_s16le",
            "-f",
            "s16le",
            "-fs",
        ])
        .arg(config.max_upload_bytes.saturating_add(2).to_string())
        .arg(pcm.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .with_context(|| {
            format!(
                "Could not run {}; install FFmpeg or set --ffmpeg",
                args.ffmpeg.display()
            )
        })?;
    ensure!(
        result.success(),
        "FFmpeg failed to extract the selected audio track"
    );
    ensure!(
        pcm.as_file().metadata()?.len() <= config.max_upload_bytes as u64,
        "Decoded audio exceeds MAX_UPLOAD_BYTES; increase the limit to transcribe this file"
    );
    let mut engine = Engine::new(config.clone());
    let (words, duration) = engine.transcribe(pcm.path(), None)?;
    let cues = subtitles::cues(&words, &config.format, duration);
    ensure!(
        !cues.is_empty(),
        "No speech recognized; no subtitle was generated"
    );
    let srt = subtitles::srt(&cues);
    if stdout {
        std::io::stdout().lock().write_all(srt.as_bytes())?;
    } else {
        let parent = output
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new("."));
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(srt.as_bytes())?;
        file.as_file_mut().sync_all()?;
        if args.force {
            file.persist(&output)?;
        } else {
            file.persist_noclobber(&output)?;
        }
        tracing::info!(path = %output.display(), cues = cues.len(), "wrote subtitles");
    }
    Ok(())
}

#[derive(Default, Deserialize)]
struct AudioStream {
    #[serde(default)]
    tags: HashMap<String, String>,
    #[serde(default)]
    disposition: HashMap<String, u32>,
}

#[derive(Deserialize)]
struct Probe {
    streams: Vec<AudioStream>,
}

fn language_code(code: &str) -> String {
    let lower = code.to_ascii_lowercase();
    let base = lower.split(['-', '_']).next().unwrap_or(&lower);
    match base {
        "en" => "eng",
        "bg" => "bul",
        "hr" => "hrv",
        "cs" | "cze" => "ces",
        "da" => "dan",
        "nl" | "dut" => "nld",
        "et" => "est",
        "fi" => "fin",
        "fr" | "fre" => "fra",
        "de" | "ger" => "deu",
        "el" | "gre" => "ell",
        "hu" => "hun",
        "it" => "ita",
        "lv" => "lav",
        "lt" => "lit",
        "mt" => "mlt",
        "pl" => "pol",
        "pt" => "por",
        "ro" | "rum" => "ron",
        "sk" | "slo" => "slk",
        "sl" => "slv",
        "es" => "spa",
        "sv" => "swe",
        "ru" => "rus",
        "uk" => "ukr",
        other => other,
    }
    .to_string()
}

fn select_audio_stream(streams: &[AudioStream], preferred: &str) -> Option<usize> {
    let preferred = language_code(preferred);
    streams
        .iter()
        .enumerate()
        .filter(|(_, stream)| {
            stream
                .tags
                .get("language")
                .is_some_and(|tag| language_code(tag) == preferred)
        })
        .min_by_key(|(index, stream)| {
            let title = stream
                .tags
                .get("title")
                .map(|t| t.to_ascii_lowercase())
                .unwrap_or_default();
            let auxiliary = stream.disposition.get("comment").copied().unwrap_or(0) != 0
                || stream
                    .disposition
                    .get("visual_impaired")
                    .copied()
                    .unwrap_or(0)
                    != 0
                || title.contains("commentary")
                || title.contains("audio description")
                || title.contains("descriptive audio");
            let default = stream.disposition.get("default").copied().unwrap_or(0) != 0;
            (auxiliary, !default, *index)
        })
        .map(|(index, _)| index)
        .or_else(|| (!streams.is_empty()).then_some(0))
}

fn probe_audio_stream(
    ffprobe: &std::path::Path,
    input: &std::path::Path,
    preferred: &str,
) -> Result<u32> {
    let result = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "a",
            "-show_entries",
            "stream_tags=language,title:stream_disposition=default,comment,visual_impaired",
            "-of",
            "json",
        ])
        .arg(input)
        .stdin(Stdio::null())
        .output()
        .with_context(|| {
            format!(
                "Could not run {}; install FFmpeg (including ffprobe) or set --ffprobe",
                ffprobe.display()
            )
        })?;
    ensure!(
        result.status.success(),
        "FFprobe failed to inspect audio tracks: {}",
        String::from_utf8_lossy(&result.stderr).trim()
    );
    let probe: Probe =
        serde_json::from_slice(&result.stdout).context("Invalid FFprobe response")?;
    let index =
        select_audio_stream(&probe.streams, preferred).context("Input contains no audio tracks")?;
    let selected_language = probe.streams[index]
        .tags
        .get("language")
        .map(String::as_str)
        .unwrap_or("und");
    if language_code(selected_language) != language_code(preferred) {
        tracing::warn!(
            preferred,
            selected_language,
            "preferred language tag not found; using first audio track"
        );
    }
    Ok(index as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn streams(json: &str) -> Vec<AudioStream> {
        serde_json::from_str::<Probe>(json).unwrap().streams
    }
    #[test]
    fn selects_english_main_track_over_foreign_and_commentary() {
        let tracks = streams(
            r#"{"streams":[
            {"tags":{"language":"deu"},"disposition":{"default":1}},
            {"tags":{"language":"eng","title":"Director commentary"},"disposition":{"default":1}},
            {"tags":{"language":"en-US","title":"Main audio"}}
        ]}"#,
        );
        assert_eq!(select_audio_stream(&tracks, "en"), Some(2));
        assert_eq!(select_audio_stream(&tracks, "de"), Some(0));
    }
    #[test]
    fn prefers_default_track_within_matching_language_and_falls_back() {
        let tracks = streams(
            r#"{"streams":[
            {"tags":{"language":"eng"}},
            {"tags":{"language":"eng"},"disposition":{"default":1}}
        ]}"#,
        );
        assert_eq!(select_audio_stream(&tracks, "en"), Some(1));
        assert_eq!(select_audio_stream(&tracks, "pl"), Some(0));
        assert_eq!(
            select_audio_stream(&streams(r#"{"streams":[{},{}]}"#), "en"),
            Some(0)
        );
        assert_eq!(select_audio_stream(&[], "en"), None);
    }
    #[test]
    fn parses_file_output_and_model_options() {
        let cli = Cli::try_parse_from([
            "bazarr-parakeet",
            "transcribe",
            "movie.mkv",
            "-o",
            "movie.en.srt",
            "--model-dir",
            "models/custom",
            "--threads",
            "2",
            "--audio-stream",
            "1",
        ])
        .unwrap();
        let Some(Action::Transcribe(args)) = cli.command else {
            panic!("expected transcribe")
        };
        assert_eq!(args.output.unwrap(), PathBuf::from("movie.en.srt"));
        assert_eq!(args.model_dir.unwrap(), PathBuf::from("models/custom"));
        assert_eq!(args.audio_stream, Some(1));
        assert_eq!(args.threads.unwrap().get(), 2);
    }
    #[test]
    fn defaults_to_server_and_rejects_zero_threads() {
        assert!(
            Cli::try_parse_from(["bazarr-parakeet"])
                .unwrap()
                .command
                .is_none()
        );
        assert!(
            Cli::try_parse_from([
                "bazarr-parakeet",
                "transcribe",
                "movie.mkv",
                "--threads",
                "0"
            ])
            .is_err()
        );
    }
}
