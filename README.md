# bazarr-parakeet

Generate SRT subtitles from audio and video files using Parakeet Ultra. The Rust application provides a command-line interface and an HTTP service compatible with Bazarr's Whisper provider.

## Local use

Requirements: Rust 1.96+, Python 3, FFmpeg, and a C/C++ build toolchain.

Download the model and build the application:

```sh
python3 scripts/download-model.py
cargo build --release --locked
```

For native macOS builds, set `MACOSX_DEPLOYMENT_TARGET=13.4` or newer before running Cargo.

Transcribe a file:

```sh
./target/release/bazarr-parakeet transcribe movie.mkv -o movie.en.srt
```

Audio files such as WAV, MP3, and FLAC use the same command. Without `-o`, the output uses the input filename with an `.srt` extension:

```sh
./target/release/bazarr-parakeet transcribe episode.mkv
# Writes episode.srt
```

The CLI prefers an English audio track based on language tags. Among matching tracks, it prefers main audio over tracks marked as commentary or audio description, then a default track. If no matching language tag is found, it uses the first audio track and logs the fallback. Language selection uses metadata, not speech detection. An explicit `--audio-stream` overrides automatic selection.

Additional options:

```sh
# Prefer Polish audio instead of English.
./target/release/bazarr-parakeet transcribe movie.mkv --audio-language pl

# Select the second audio track explicitly (indices start at zero).
./target/release/bazarr-parakeet transcribe movie.mkv --audio-stream 1

# Use a different model directory and thread count.
./target/release/bazarr-parakeet transcribe movie.mkv --model-dir models/custom --threads 2

# Write SRT to stdout; logs go to stderr.
./target/release/bazarr-parakeet transcribe movie.mkv -o - > movie.srt

# Replace an existing subtitle file.
./target/release/bazarr-parakeet transcribe movie.mkv -o movie.srt --force

./target/release/bazarr-parakeet transcribe --help
```

Existing output files are preserved unless `--force` is supplied. The CLI runs inference directly and does not require Bazarr or a running HTTP service.

## Docker

The [Publish container](.github/workflows/publish.yaml) workflow publishes a Linux
AMD64 image to `ghcr.io/gronnmann/bazarr-parakeet` using the built-in `GITHUB_TOKEN`;
no registry secret is required. Image names are normalized to lowercase.

- Pushes to the default branch publish `latest`.
- Version tags such as `v0.1.0` publish `0.1.0` and `0.1` (prereleases keep their full version).
- Every published build also has a `sha-<full-commit-sha>` tag.
- The workflow can also be run manually from the Actions tab. Only runs on the default branch update `latest`.

After the first successful publish, use the image in your server's Compose file
as shown below. For anonymous pulls, set the package visibility to public in GitHub's package
settings. For a private package, first run `docker login ghcr.io` on the server
with your GitHub username and a personal access token (classic) with `read:packages`.
See [GitHub's container registry authentication documentation](https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry#authenticating-to-the-container-registry).

To build locally instead:

```sh
docker build -t bazarr-parakeet .
```

Download model files on the host with `python3 scripts/download-model.py`, then mount the model directory into the container.

To transcribe a file using Docker:

```sh
docker run --rm --no-healthcheck \
  --user "$(id -u):$(id -g)" \
  -v "$PWD/models/ultra:/models/ultra:ro" \
  -v "$PWD:/media" \
  bazarr-parakeet transcribe /media/movie.mkv -o /media/movie.en.srt
```

FFmpeg is included in the image. The media mount supplies the input file and output directory for CLI use.

### Add the service to an existing Compose stack

After publishing the image and downloading the model, add this service to the Compose file containing Bazarr. Replace the model path with its location on the host:

```yaml
services:
  bazarr-parakeet:
    image: ghcr.io/gronnmann/bazarr-parakeet:latest
    restart: unless-stopped
    environment:
      MODEL_DIR: /models/ultra
      MODEL_ID: parakeet-ultra
      INFERENCE_THREADS: "1"
    volumes:
      - /path/to/bazarr-parakeet/models/ultra:/models/ultra:ro
      - parakeet-data:/data
    cpus: 1.0
    mem_limit: 4g

volumes:
  parakeet-data:
```

Pull and start the service on the server:

```sh
docker compose pull bazarr-parakeet
docker compose up -d bazarr-parakeet
```

Use a version or commit tag instead of `latest` to pin a deployment. Model weights
are downloaded separately and must still be mounted as shown above.

Alternatively, replace `image` with `build: /path/to/bazarr-parakeet` to build from the source directory within your stack.

Place the service on a network shared with Bazarr. Its endpoint is `http://bazarr-parakeet:9000`; a published host port is not required for communication between containers on that network. Bazarr uploads audio, so the HTTP service does not need a media-library mount.

## Bazarr configuration

1. Enable **Whisper** in Settings → Providers.
2. Set its endpoint to `http://bazarr-parakeet:9000`.
3. Set the transcription timeout to accommodate the expected processing time.
4. Configure a subtitle language matching a supported audio language.
5. Select a Whisper result to generate subtitles.

Bazarr handles audio extraction, track selection, subtitle naming, and saving. Its provider remains named Whisper. Provider scores and language profiles determine when generation runs automatically. Existing subtitles require an explicit regeneration request to replace them.

Parakeet transcribes speech in its original language. Translation is not supported.

## HTTP service

Start the service locally:

```sh
./target/release/bazarr-parakeet serve
```

Running the binary without a subcommand also starts the service. The default address is `0.0.0.0:9000`.

Endpoints:

- `POST /asr?task=transcribe&language=en&output=srt&encode=false`: upload `audio_file` as multipart data; receive SRT.
- `POST /detect-language?encode=false`: upload audio; receive `language_code`, `detected_language`, and `confidence`.
- `GET /health`: service status, model file presence, and worker activity.
- `GET /jobs`: recent job statuses.

With `encode=false`, audio must be headerless 16 kHz mono signed 16-bit little-endian PCM. With `encode=true` (the default), input must be 16 kHz mono 16-bit PCM WAV.

```sh
ffmpeg -nostdin -threads 1 -i movie.mkv -map 0:a:0 \
  -vn -ac 1 -ar 16000 -c:a pcm_s16le /tmp/audio.wav
curl --fail-with-body -F audio_file=@/tmp/audio.wav \
  'http://127.0.0.1:9000/asr?task=transcribe&language=en&output=srt&encode=true' \
  -o movie.en.srt
```

The service processes one request at a time. Concurrent requests receive HTTP 503. Completed results are cached; `/jobs` records processing outcomes. Temporary uploads are removed after processing, and interrupted jobs are marked on restart. Use one service process per data directory.

The service has no authentication and is intended for trusted networks.

## Models

The default model is [Moondream Parakeet Ultra](https://huggingface.co/moondream/parakeet-ultra), using a pinned [ONNX export](https://huggingface.co/altunenes/parakeet-rs/tree/main/parakeet-ultra). The download script verifies file checksums and records the source revision in `manifest.json`.

To use another compatible Parakeet TDT ONNX model, set `MODEL_DIR` or pass `--model-dir` to the CLI. The directory must contain:

```text
encoder-model.onnx
decoder_joint-model.onnx
vocab.txt
encoder-model.onnx.data       # when referenced by the encoder
```

For example, download the original Parakeet v3 export into a separate directory:

```sh
python3 scripts/download-model.py \
  --repo istupakov/parakeet-tdt-0.6b-v3-onnx \
  --revision main --subdir '' --output models/parakeet-v3
```

The script resolves `main` to a commit. Use the revision recorded in the manifest to pin subsequent downloads. Restart the HTTP service after changing its model, and update `MODEL_ID` to identify the replacement.

Supported language codes: `bg hr cs da nl en et fi fr de el hu it lv lt mt pl pt ro sk sl es sv ru uk`.

The HTTP `language` parameter validates the requested language; the model identifies the spoken language internally. `/detect-language` uses transcription of the first two minutes with a text language classifier. Missing or ambiguous speech can prevent language identification; media language tags are preferred.

## Subtitle formatting

Word timestamps are grouped into phrases using punctuation, pauses, duration, and line length. Longer cues wrap onto two lines. Default targets are 42 characters per line, 1–6 seconds per cue, and 20 characters per second.

Audio is processed in overlapping chunks. Word timestamps are mapped to the original timeline, preserving silence between dialogue. Reading speed and minimum duration are targets; they do not remove or rewrite recognized words.

## Configuration

Environment variables apply to the HTTP service and CLI unless noted:

- `MODEL_DIR`: `models/ultra` locally; `/models/ultra` in the image.
- `MODEL_ID`: `parakeet-ultra`.
- `INFERENCE_THREADS`: `1`.
- `CHUNK_SECONDS`: `30` (range 5–120).
- `OVERLAP_SECONDS`: `2` (less than half the chunk length).
- `MAX_UPLOAD_BYTES`: `536870912`; also limits decoded CLI audio size.
- `SUBTITLE_LINE_CHARS`: `42`.
- `SUBTITLE_MIN_SECONDS`: `1`.
- `SUBTITLE_MAX_SECONDS`: `6`.
- `SUBTITLE_PAUSE_SECONDS`: `0.7`.
- `SUBTITLE_TRAILING_SECONDS`: `0.15`.
- `SUBTITLE_TARGET_CPS`: `20`.
- `RUST_LOG`: `bazarr_parakeet=info`.
- `BIND`: `0.0.0.0:9000` (HTTP service).
- `DATA_DIR`: `data` locally; `/data` in the image (HTTP service).
- `CACHE_ENTRIES`: `100`; `0` disables result caching (HTTP service).

## Development

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

Inference uses [parakeet-rs](https://github.com/altunenes/parakeet-rs) and ONNX Runtime. The first build downloads the runtime's native libraries. Model weights are downloaded separately under CC BY 4.0; see [NOTICE.md](NOTICE.md).
