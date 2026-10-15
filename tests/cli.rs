use std::process::Command;

#[test]
fn preserves_existing_output_and_input_media() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("movie.mkv");
    let output = dir.path().join("movie.srt");
    std::fs::write(&input, b"original media").unwrap();
    std::fs::write(&output, b"existing subtitle").unwrap();
    let response = Command::new(env!("CARGO_BIN_EXE_bazarr-parakeet"))
        .arg("transcribe")
        .arg(&input)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(!response.status.success());
    assert!(String::from_utf8_lossy(&response.stderr).contains("Output already exists"));
    assert_eq!(std::fs::read(&output).unwrap(), b"existing subtitle");
    let response = Command::new(env!("CARGO_BIN_EXE_bazarr-parakeet"))
        .arg("transcribe")
        .arg(&input)
        .arg("-o")
        .arg(&input)
        .arg("--force")
        .output()
        .unwrap();
    assert!(!response.status.success());
    assert_eq!(std::fs::read(&input).unwrap(), b"original media");
}

#[test]
fn missing_decoder_leaves_no_partial_subtitle() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("movie.mkv");
    let output = dir.path().join("movie.srt");
    std::fs::write(&input, b"media").unwrap();
    let response = Command::new(env!("CARGO_BIN_EXE_bazarr-parakeet"))
        .arg("transcribe")
        .arg(&input)
        .arg("-o")
        .arg(&output)
        .args(["--audio-stream", "0"])
        .arg("--ffmpeg")
        .arg(dir.path().join("missing-ffmpeg"))
        .output()
        .unwrap();
    assert!(!response.status.success());
    assert!(String::from_utf8_lossy(&response.stderr).contains("install FFmpeg"));
    assert!(!output.exists());
    assert!(response.stdout.is_empty());
}
