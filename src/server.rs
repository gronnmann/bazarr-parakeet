use crate::{
    config::Config,
    inference::{self, Engine},
    store::Store,
    subtitles,
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Multipart, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    io::{BufWriter, Read, Write},
    path::Path,
    sync::{Arc, Mutex},
};
use tempfile::TempPath;
use tokio::{io::AsyncWriteExt, sync::Semaphore};

#[derive(Clone)]
struct AppState {
    config: Config,
    engine: Arc<Mutex<Engine>>,
    store: Arc<Mutex<Store>>,
    admission: Arc<Semaphore>,
    spool: Arc<tempfile::TempDir>,
    _data_lock: Arc<std::fs::File>,
}

#[derive(Debug)]
struct ApiError(StatusCode, String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({"error": self.1}))).into_response()
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        tracing::error!(error = %format!("{error:#}"), "request failed");
        Self(StatusCode::INTERNAL_SERVER_ERROR, format!("{error:#}"))
    }
}
fn bad(message: impl Into<String>) -> ApiError {
    ApiError(StatusCode::UNPROCESSABLE_ENTITY, message.into())
}

#[derive(Default, Deserialize)]
struct AsrQuery {
    task: Option<String>,
    language: Option<String>,
    output: Option<String>,
    encode: Option<String>,
}
impl AsrQuery {
    fn validate(&self, detection: bool) -> Result<bool, ApiError> {
        if self.task.as_deref().is_some_and(|t| t != "transcribe") {
            return Err(bad(
                "Only task=transcribe is supported. Parakeet does not translate audio.",
            ));
        }
        if !detection && self.output.as_deref().is_some_and(|o| o != "srt") {
            return Err(bad("Only output=srt is supported."));
        }
        if let Some(language) = &self.language
            && language != "auto"
            && inference::language_name(language).is_none()
        {
            return Err(bad(format!(
                "Unsupported language '{language}'. Use a supported two-letter code."
            )));
        }
        match self.encode.as_deref().unwrap_or("true") {
            "false" => Ok(false),
            "true" => Ok(true),
            _ => Err(bad("encode must be true or false")),
        }
    }
}

pub fn app(config: Config) -> Result<Router> {
    std::fs::create_dir_all(&config.data_dir)?;
    let data_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(config.data_dir.join("service.lock"))?;
    data_lock
        .try_lock()
        .context("DATA_DIR is already in use by another service process")?;
    let temp_root = config.data_dir.join("tmp");
    std::fs::create_dir_all(&temp_root)?;
    // The exclusive data-directory lock makes it safe to remove uploads left by a crash.
    for entry in std::fs::read_dir(&temp_root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.file_name().to_string_lossy().starts_with("upload-")
        {
            std::fs::remove_dir_all(entry.path())?;
        }
    }
    let spool = tempfile::Builder::new()
        .prefix("upload-")
        .tempdir_in(temp_root)?;
    let store = Store::open(&config.data_dir.join("jobs.sqlite"))?;
    let max_body = config.max_upload_bytes.saturating_add(1024 * 1024);
    let state = AppState {
        engine: Arc::new(Mutex::new(Engine::new(config.clone()))),
        store: Arc::new(Mutex::new(store)),
        config,
        admission: Arc::new(Semaphore::new(1)),
        spool: Arc::new(spool),
        _data_lock: Arc::new(data_lock),
    };
    Ok(Router::new()
        .route("/health", get(health))
        .route("/jobs", get(jobs))
        .route("/asr", post(asr))
        .route("/detect-language", post(detect))
        .layer(DefaultBodyLimit::max(max_body))
        .with_state(state))
}

async fn health(State(state): State<AppState>) -> Json<serde_json::Value> {
    let required = [
        "encoder-model.onnx",
        "decoder_joint-model.onnx",
        "vocab.txt",
    ];
    Json(
        serde_json::json!({"status": "ok", "model": state.config.model_id,
        "model_files_present": required.iter().all(|f| state.config.model_dir.join(f).is_file()),
        "busy": state.admission.available_permits() == 0}),
    )
}
async fn jobs(State(state): State<AppState>) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(
        state
            .store
            .lock()
            .map_err(|_| anyhow::anyhow!("job store lock poisoned"))?
            .jobs()?,
    ))
}
async fn asr(
    State(state): State<AppState>,
    Query(query): Query<AsrQuery>,
    multipart: Multipart,
) -> Result<Response, ApiError> {
    run(state, query, multipart, false).await
}
async fn detect(
    State(state): State<AppState>,
    Query(query): Query<AsrQuery>,
    multipart: Multipart,
) -> Result<Response, ApiError> {
    run(state, query, multipart, true).await
}

async fn upload(mut multipart: Multipart, state: &AppState) -> Result<TempPath, ApiError> {
    let path = tempfile::NamedTempFile::new_in(state.spool.path())
        .map_err(anyhow::Error::from)?
        .into_temp_path();
    let mut file = tokio::fs::File::create(&path)
        .await
        .map_err(anyhow::Error::from)?;
    let mut found = false;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError(e.status(), e.to_string()))?
    {
        if field.name() != Some("audio_file") {
            return Err(bad("Expected multipart field audio_file"));
        }
        if found {
            return Err(bad("Provide exactly one audio_file"));
        }
        found = true;
        let mut size = 0;
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|e| ApiError(e.status(), e.to_string()))?
        {
            size += chunk.len();
            if size > state.config.max_upload_bytes {
                return Err(ApiError(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Audio exceeds MAX_UPLOAD_BYTES".into(),
                ));
            }
            file.write_all(&chunk).await.map_err(anyhow::Error::from)?;
        }
        if size == 0 {
            return Err(bad("audio_file is empty"));
        }
    }
    file.flush().await.map_err(anyhow::Error::from)?;
    if !found {
        return Err(bad("Missing multipart field audio_file"));
    }
    Ok(path)
}

async fn run(
    state: AppState,
    query: AsrQuery,
    multipart: Multipart,
    detection: bool,
) -> Result<Response, ApiError> {
    let encode = query.validate(detection)?;
    let permit = state.admission.clone().try_acquire_owned().map_err(|_| {
        ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "Already processing audio; retry after completion (see /jobs).".into(),
        )
    })?;
    let upload = upload(multipart, &state).await?;
    let output = tokio::task::spawn_blocking(move || -> Result<(String, bool), ApiError> {
        // Keep ownership until inference finishes, even if the HTTP client disconnects.
        let _permit = permit;
        let normalized;
        let path: &Path = if encode {
            normalized = normalize_wav(&upload, state.spool.path(), state.config.max_upload_bytes).map_err(|e| bad(format!("{e:#}")))?;
            &normalized
        } else { &upload };
        let size = std::fs::metadata(path).map_err(anyhow::Error::from)?.len();
        if size == 0 || size % 2 != 0 {
            return Err(bad("Expected complete headerless mono 16kHz s16le PCM samples (encode=false), or PCM WAV (encode=true)"));
        }
        let key = cache_key(path, &state.config, query.language.as_deref(), detection)?;
        if state.config.cache_entries > 0 && let Some(cached) = state.store.lock().map_err(|_| anyhow::anyhow!("job store lock poisoned"))?.cached(&key)? {
            return Ok((cached, true));
        }
        let job_id = state.store.lock().map_err(|_| anyhow::anyhow!("job store lock poisoned"))?.start(&key)?;
        let result = (|| -> Result<String> {
            let (words, duration) = state.engine.lock().map_err(|_| anyhow::anyhow!("inference worker lock poisoned"))?
                .transcribe(path, if detection { Some(16000 * 120) } else { None })?;
            if detection {
                let (code, name, confidence) = inference::detect_language(&words)?;
                Ok(serde_json::json!({"language_code": code, "detected_language": name, "confidence": confidence}).to_string())
            } else {
                let cues = subtitles::cues(&words, &state.config.format, duration);
                ensure!(!cues.is_empty(), "No speech recognized; no subtitle was generated");
                Ok(subtitles::srt(&cues))
            }
        })();
        state.store.lock().map_err(|_| anyhow::anyhow!("job store lock poisoned"))?
            .finish(job_id, &key, &result, state.config.cache_entries)?;
        Ok((result?, false))
    }).await.context("inference worker stopped")??;
    let content_type = if detection {
        "application/json"
    } else {
        "application/x-subrip; charset=utf-8"
    };
    Ok((
        [
            (header::CONTENT_TYPE.as_str(), content_type),
            ("x-cache", if output.1 { "HIT" } else { "MISS" }),
        ],
        output.0,
    )
        .into_response())
}

fn normalize_wav(path: &Path, spool: &Path, limit: usize) -> Result<TempPath> {
    let mut reader = hound::WavReader::open(path)
        .context("encode=true accepts PCM WAV only; use encode=false for Bazarr PCM")?;
    let spec = reader.spec();
    ensure!(
        spec.channels == 1
            && spec.sample_rate == 16000
            && spec.bits_per_sample == 16
            && spec.sample_format == hound::SampleFormat::Int,
        "WAV must be mono, 16kHz, 16-bit PCM"
    );
    let mut file = tempfile::NamedTempFile::new_in(spool)?;
    let mut writer = BufWriter::new(file.as_file_mut());
    for (i, sample) in reader.samples::<i16>().enumerate() {
        ensure!((i + 1) * 2 <= limit, "Decoded WAV exceeds MAX_UPLOAD_BYTES");
        writer.write_all(&sample?.to_le_bytes())?;
    }
    writer.flush()?;
    drop(writer);
    Ok(file.into_temp_path())
}

fn cache_key(
    path: &Path,
    config: &Config,
    language: Option<&str>,
    detection: bool,
) -> Result<String> {
    let mut hasher = Sha256::new();
    // Bump this namespace when inference or formatting behavior changes.
    hasher.update(b"bazarr-parakeet-v1\0");
    hasher.update(serde_json::to_vec(&(
        &config.model_id,
        &config.model_dir,
        config.chunk_seconds,
        config.overlap_seconds,
        &config.format,
        language,
        detection,
    ))?);
    // Fingerprint model files without hashing gigabytes on every request.
    for name in [
        "encoder-model.onnx",
        "encoder-model.onnx.data",
        "decoder_joint-model.onnx",
        "vocab.txt",
    ] {
        if let Ok(meta) = std::fs::metadata(config.model_dir.join(name)) {
            hasher.update(name.as_bytes());
            hasher.update(meta.len().to_le_bytes());
            hasher.update(
                meta.modified()?
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_nanos()
                    .to_le_bytes(),
            );
        }
    }
    let mut file = std::fs::File::open(path)?;
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn config(root: &Path) -> Config {
        Config {
            bind: "127.0.0.1:9000".into(),
            model_dir: root.join("model"),
            model_id: "test".into(),
            data_dir: root.join("data"),
            threads: 1,
            chunk_seconds: 30,
            overlap_seconds: 2,
            max_upload_bytes: 32000,
            cache_entries: 2,
            format: crate::config::FormatConfig {
                line_chars: 42,
                max_duration: 6.0,
                min_duration: 1.0,
                pause_seconds: 0.7,
                trailing_seconds: 0.15,
                target_cps: 20.0,
            },
        }
    }
    fn request(uri: &str, audio: &[u8]) -> Request<Body> {
        let mut body = b"--boundary\r\nContent-Disposition: form-data; name=\"audio_file\"; filename=\"audio\"\r\nContent-Type: application/octet-stream\r\n\r\n".to_vec();
        body.extend_from_slice(audio);
        body.extend_from_slice(b"\r\n--boundary--\r\n");
        Request::post(uri)
            .header(
                header::CONTENT_TYPE,
                "multipart/form-data; boundary=boundary",
            )
            .body(Body::from(body))
            .unwrap()
    }
    #[tokio::test]
    async fn bazarr_pcm_upload_returns_cached_srt_verbatim() {
        let root = tempfile::tempdir().unwrap();
        let c = config(root.path());
        std::fs::create_dir_all(&c.data_dir).unwrap();
        let pcm = vec![0u8; 32000];
        let audio = root.path().join("audio.pcm");
        std::fs::write(&audio, &pcm).unwrap();
        let key = cache_key(&audio, &c, Some("en"), false).unwrap();
        let expected = "1\n00:00:00,000 --> 00:00:01,000\nHello world.\n\n";
        let mut store = Store::open(&c.data_dir.join("jobs.sqlite")).unwrap();
        let id = store.start(&key).unwrap();
        store.finish(id, &key, &Ok(expected.into()), 2).unwrap();
        drop(store);
        let response = app(c).unwrap().oneshot(request(
            "/asr?task=transcribe&language=en&output=srt&encode=false&video_file=/media/movie.mkv", &pcm)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["x-cache"], "HIT");
        assert!(
            response.headers()[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .contains("subrip")
        );
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            expected
        );
    }
    #[tokio::test]
    async fn rejects_translation_unsupported_language_and_oversized_uploads() {
        let root = tempfile::tempdir().unwrap();
        let router = app(config(root.path())).unwrap();
        for uri in [
            "/asr?task=translate&encode=false",
            "/asr?language=ja&encode=false",
            "/asr?output=json&encode=false",
        ] {
            let response = router.clone().oneshot(request(uri, &[1, 0])).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        }
        let response = router
            .oneshot(request("/asr?encode=false", &vec![0; 32002]))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
    #[tokio::test]
    async fn malformed_audio_is_an_error_and_tempfiles_are_cleaned() {
        let root = tempfile::tempdir().unwrap();
        let c = config(root.path());
        let router = app(c.clone()).unwrap();
        for (uri, data) in [
            ("/asr?encode=false", vec![1]),
            ("/asr?encode=true", vec![1, 2, 3, 4]),
        ] {
            let response = router.clone().oneshot(request(uri, &data)).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        }
        for folder in std::fs::read_dir(c.data_dir.join("tmp")).unwrap() {
            assert_eq!(
                std::fs::read_dir(folder.unwrap().path()).unwrap().count(),
                0
            );
        }
    }
    #[test]
    fn wav_normalization_preserves_samples_and_rejects_wrong_rate() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("audio.wav");
        let mut spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        {
            let mut writer = hound::WavWriter::create(&path, spec).unwrap();
            writer.write_sample(1234i16).unwrap();
            writer.write_sample(-1234i16).unwrap();
            writer.finalize().unwrap();
        }
        let pcm = normalize_wav(&path, root.path(), 32000).unwrap();
        assert_eq!(
            std::fs::read(pcm).unwrap(),
            [1234i16.to_le_bytes(), (-1234i16).to_le_bytes()].concat()
        );
        spec.sample_rate = 44100;
        hound::WavWriter::create(&path, spec)
            .unwrap()
            .finalize()
            .unwrap();
        assert!(normalize_wav(&path, root.path(), 32000).is_err());
    }
}
