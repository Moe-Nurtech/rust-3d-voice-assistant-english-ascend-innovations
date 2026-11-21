//! Text-to-speech. Uses a built-in Rust client for Microsoft's Edge voices (no Python needed).
//! If that ever fails, it falls back to the Python `edge-tts` tool when it is installed.

use std::process::Command;

use anyhow::{anyhow, Result};
use msedge_tts::tts::client::connect;
use msedge_tts::tts::SpeechConfig;

pub const VOICE: &str = "en-GB-RyanNeural";

/// Returns MP3 bytes for the given text.
pub fn synth(text: &str) -> Result<Vec<u8>> {
    let text: String = text.chars().take(1200).collect();

    let native_err = match synth_native(&text) {
        Ok(bytes) if !bytes.is_empty() => return Ok(bytes),
        Ok(_) => "no audio returned".to_string(),
        Err(e) => e.to_string(),
    };
    eprintln!("native TTS failed: {native_err}");
    synth_python(&text, &native_err)
}

fn synth_native(text: &str) -> Result<Vec<u8>> {
    let config = SpeechConfig {
        voice_name: VOICE.to_string(),
        audio_format: "audio-24khz-48kbitrate-mono-mp3".to_string(),
        pitch: 0,
        rate: 0,
        volume: 0,
    };
    let mut client = connect().map_err(|e| anyhow!("connect: {e}"))?;
    let audio = client
        .synthesize(&xml_escape(text), &config)
        .map_err(|e| anyhow!("synthesize: {e}"))?;
    Ok(audio.audio_bytes)
}

/// The text is placed inside SSML (XML), so these characters must be escaped.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn synth_python(text: &str, native_err: &str) -> Result<Vec<u8>> {
    let out = std::env::temp_dir().join("robot_assistant_tts.mp3");
    let _ = std::fs::remove_file(&out);

    let attempts: [(&str, &[&str]); 3] = [
        ("edge-tts", &[]),
        ("python", &["-m", "edge_tts"]),
        ("py", &["-m", "edge_tts"]),
    ];

    let mut last_err = String::new();
    for (program, prefix) in attempts {
        let result = Command::new(program)
            .args(prefix)
            .arg("--voice")
            .arg(VOICE)
            .arg(format!("--text={text}"))
            .arg("--write-media")
            .arg(&out)
            .output();

        match result {
            Ok(o) if o.status.success() => return Ok(std::fs::read(&out)?),
            Ok(o) => last_err = String::from_utf8_lossy(&o.stderr).trim().to_string(),
            Err(e) => last_err = format!("{program}: {e}"),
        }
    }
    Err(anyhow!(
        "Voice failed. Built-in client: {native_err}. Python fallback: {last_err}"
    ))
}