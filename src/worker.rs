//! Background thread: record -> Whisper -> Ollama -> edge-tts -> robot effect -> speakers.
//! The Bevy window talks to it through two channels (Cmd in, Ev out).

use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::{answers, audio, tts};

// ---- Settings (same as server.py) ----
pub const OLLAMA_URL: &str = "http://localhost:11434/api/chat";
pub const OLLAMA_MODEL: &str = "ascend-en";
pub const MAX_HISTORY: usize = 12;
/// Leave "" to use only the SYSTEM prompt from your Modelfile.
pub const SYSTEM_PROMPT: &str = "";
/// Whisper model file (download instructions in README.md)
pub const WHISPER_MODEL: &str = "models/ggml-base.en.bin";

// ---- Auto-stop settings ----
/// How long you must be quiet (after speaking) before it stops listening
const SILENCE_END: Duration = Duration::from_millis(1200);
/// Give up if nothing is said at all for this long
const NO_SPEECH_TIMEOUT: Duration = Duration::from_secs(8);
/// Never record longer than this
const MAX_RECORDING: Duration = Duration::from_secs(30);
/// The speech threshold is measured from the room noise, but kept between these limits.
/// Raise MIN_THRESHOLD if it never stops (noisy room). Lower it if it misses quiet speech.
const MIN_THRESHOLD: f32 = 0.015;
const MAX_THRESHOLD: f32 = 0.05;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RobotState {
    Idle,
    Listening,
    Thinking,
    Talking,
}

pub enum Cmd {
    Toggle, // mic button or SPACE: start listening / stop listening early
    /// An info button was clicked: speak this fixed text (no Ollama involved)
    Speak { title: String, text: String },
}

pub enum Ev {
    State(RobotState),
    Status(String),
    Transcript(String),
    Reply(String),
    Hint(String),
}

pub fn run_worker(rx: Receiver<Cmd>, tx: Sender<Ev>) {
    // Make the voices for the info buttons in the background so clicking them is instant
    std::thread::spawn(prewarm_answers);
    if let Err(e) = run(&rx, &tx) {
        let _ = tx.send(Ev::Status("Status: Error".into()));
        let _ = tx.send(Ev::Hint(format!("{e:#}")));
    }
}

fn run(rx: &Receiver<Cmd>, tx: &Sender<Ev>) -> Result<()> {
    let _ = tx.send(Ev::Status("Status: Loading speech model...".into()));
    let ctx = WhisperContext::new_with_params(WHISPER_MODEL, WhisperContextParameters::default())
        .map_err(|e| anyhow!("Could not load {WHISPER_MODEL}: {e}. See README.md step 4."))?;
    let (_stream, handle) = rodio::OutputStream::try_default()
        .map_err(|e| anyhow!("No audio output device: {e}"))?;

    let mut history: Vec<(String, String)> = Vec::new();
    let _ = tx.send(Ev::Status("Status: Ready. Click the mic or press SPACE to talk".into()));

    loop {
        // 1) wait for the mic button / SPACE (start recording) or an info button (fixed answer)
        match rx.recv() {
            Ok(Cmd::Toggle) => {}
            Ok(Cmd::Speak { title, text }) => {
                if let Err(e) = speak_static(&handle, tx, &title, &text) {
                    let _ = tx.send(Ev::Status("Status: Error".into()));
                    let _ = tx.send(Ev::Hint(format!("{e:#}")));
                }
                let _ = tx.send(Ev::State(RobotState::Idle));
                while rx.try_recv().is_ok() {}
                continue;
            }
            Err(_) => return Ok(()), // window closed
        }
        let recorder = match audio::Recorder::start() {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.send(Ev::Status("Status: Error".into()));
                let _ = tx.send(Ev::Hint(format!("{e:#}")));
                continue;
            }
        };
        let _ = tx.send(Ev::Hint(String::new()));
        let _ = tx.send(Ev::Transcript("...".into()));
        let _ = tx.send(Ev::State(RobotState::Listening));
        let _ = tx.send(Ev::Status("Status: Listening... just speak".into()));

        // 2) record until you stop talking (or press SPACE)
        let heard = match wait_for_end_of_speech(rx, &recorder, tx) {
            Some(heard) => heard,
            None => return Ok(()), // window closed
        };
        let (samples, rate) = recorder.finish();

        // 3) process the turn
        if !heard {
            let _ = tx.send(Ev::Transcript("(I did not hear anything)".into()));
            let _ = tx.send(Ev::Status("Status: Ready. Click the mic or press SPACE to talk".into()));
        } else if let Err(e) = turn(&ctx, &handle, tx, &mut history, samples, rate) {
            let _ = tx.send(Ev::Status("Status: Error".into()));
            let _ = tx.send(Ev::Hint(format!("{e:#}")));
        }
        let _ = tx.send(Ev::State(RobotState::Idle));

        // ignore any SPACE presses made while the robot was busy
        while rx.try_recv().is_ok() {}
    }
}

/// Blocks while recording. Returns Some(true) if speech was heard, Some(false) if not,
/// and None if the window was closed.
fn wait_for_end_of_speech(
    rx: &Receiver<Cmd>,
    rec: &audio::Recorder,
    tx: &Sender<Ev>,
) -> Option<bool> {
    let start = Instant::now();
    let mut threshold: Option<f32> = None;
    let mut heard = false;
    let mut last_loud = Instant::now();

    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Cmd::Toggle) => return Some(heard), // mic button / SPACE = stop now
            Ok(Cmd::Speak { .. }) => {}             // info buttons are hidden while listening
            Err(RecvTimeoutError::Disconnected) => return None,
            Err(RecvTimeoutError::Timeout) => {}
        }
        let elapsed = start.elapsed();

        // First 0.4 s: measure the room noise to set the speech threshold
        let thr = match threshold {
            Some(t) => t,
            None => {
                if elapsed >= Duration::from_millis(400) {
                    let noise = rec.recent_rms(0.4);
                    threshold = Some((noise * 3.0).clamp(MIN_THRESHOLD, MAX_THRESHOLD));
                }
                continue;
            }
        };

        if rec.recent_rms(0.1) > thr {
            if !heard {
                let _ = tx.send(Ev::Status("Status: Listening... (stops when you pause)".into()));
            }
            heard = true;
            last_loud = Instant::now();
        }

        if heard && last_loud.elapsed() >= SILENCE_END {
            return Some(true);
        }
        if !heard && elapsed >= NO_SPEECH_TIMEOUT {
            return Some(false);
        }
        if elapsed >= MAX_RECORDING {
            return Some(heard);
        }
    }
}

fn turn(
    ctx: &WhisperContext,
    handle: &rodio::OutputStreamHandle,
    tx: &Sender<Ev>,
    history: &mut Vec<(String, String)>,
    samples: Vec<f32>,
    rate: u32,
) -> Result<()> {
    let _ = tx.send(Ev::State(RobotState::Thinking));
    let _ = tx.send(Ev::Status("Status: Transcribing...".into()));

    let audio16 = audio::resample(&samples, rate, 16_000);
    if audio16.len() < 8_000 {
        let _ = tx.send(Ev::Transcript("(too short, try again)".into()));
        let _ = tx.send(Ev::Status("Status: Ready. Click the mic or press SPACE to talk".into()));
        return Ok(());
    }

    let text = transcribe(ctx, &audio16)?;
    if text.is_empty() {
        let _ = tx.send(Ev::Transcript("(I did not catch that)".into()));
        let _ = tx.send(Ev::Status("Status: Ready. Click the mic or press SPACE to talk".into()));
        return Ok(());
    }
    let _ = tx.send(Ev::Transcript(format!("\"{text}\"")));
    let _ = tx.send(Ev::Reply("...".into()));
    let _ = tx.send(Ev::Status("Status: Robot is thinking...".into()));

    history.push(("user".into(), text));
    let reply = match ask_ollama(history) {
        Ok(r) if !r.is_empty() => r,
        other => {
            history.pop(); // drop the failed question so history stays consistent
            let _ = tx.send(Ev::Reply(String::new()));
            let why = match other {
                Err(e) => e.to_string(),
                _ => "empty reply".into(),
            };
            return Err(anyhow!(
                "Could not get a response. Make sure Ollama is running and the model {OLLAMA_MODEL} exists. ({why})"
            ));
        }
    };
    history.push(("assistant".into(), reply.clone()));
    let _ = tx.send(Ev::Reply(reply.clone()));

    speak(handle, tx, &reply, false)
}

/// Info button: show the fixed text and speak it (no Ollama).
fn speak_static(
    handle: &rodio::OutputStreamHandle,
    tx: &Sender<Ev>,
    title: &str,
    text: &str,
) -> Result<()> {
    let _ = tx.send(Ev::Hint(String::new()));
    let _ = tx.send(Ev::State(RobotState::Thinking));
    let _ = tx.send(Ev::Transcript(format!("[{title}]")));
    let _ = tx.send(Ev::Reply(text.to_string()));
    speak(handle, tx, text, true)
}

/// Text -> voice -> robot effect -> speakers. Used by both normal replies and info buttons.
/// `cached` = true for the fixed button answers: their voice is saved in audio_cache/ and reused.
fn speak(handle: &rodio::OutputStreamHandle, tx: &Sender<Ev>, text: &str, cached: bool) -> Result<()> {
    let _ = tx.send(Ev::Status("Status: Generating robot voice...".into()));
    let spoken = prepare_for_speech(text);
    let mp3 = if cached { cached_synth(&spoken)? } else { tts::synth(&spoken)? };
    let (mut pcm, channels, sample_rate) = audio::decode(mp3)?;
    audio::robot_effect(&mut pcm, channels, sample_rate, 0.3);

    let _ = tx.send(Ev::Status("Status: Speaking (British male voice)...".into()));
    let _ = tx.send(Ev::State(RobotState::Talking));
    // playbackRate 0.97 in the web version -> play slightly slower/lower
    audio::play(handle, pcm, channels, (sample_rate as f32 * 0.97) as u32)?;
    let _ = tx.send(Ev::Status("Status: Finished! Click the mic or press SPACE to talk again".into()));
    Ok(())
}

// ---- voice cache for the info buttons ----

/// Folder (next to the app) where the button voices are stored as .mp3 files.
const CACHE_DIR: &str = "audio_cache";

/// Small stable hash so the file name changes whenever you edit the text or the voice.
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn cache_path(spoken: &str) -> PathBuf {
    let key = format!("{}|{}", tts::VOICE, spoken);
    PathBuf::from(CACHE_DIR).join(format!("{:016x}.mp3", fnv1a(&key)))
}

/// Returns the saved voice if it exists, otherwise makes it once and saves it.
fn cached_synth(spoken: &str) -> Result<Vec<u8>> {
    let path = cache_path(spoken);
    if let Ok(bytes) = fs::read(&path) {
        if !bytes.is_empty() {
            return Ok(bytes);
        }
    }
    let mp3 = tts::synth(spoken)?;
    let _ = fs::create_dir_all(CACHE_DIR);
    let tmp = path.with_extension("tmp");
    if fs::write(&tmp, &mp3).is_ok() {
        let _ = fs::rename(&tmp, &path);
    }
    Ok(mp3)
}

/// Runs once at startup: creates any missing button voices (takes a few seconds each, only the first time).
fn prewarm_answers() {
    for a in answers::all() {
        let spoken = prepare_for_speech(a.text);
        if let Err(e) = cached_synth(&spoken) {
            eprintln!("could not prepare the voice for '{}': {e}", a.label);
        }
    }
}

fn transcribe(ctx: &WhisperContext, audio16k: &[f32]) -> Result<String> {
    let mut state = ctx.create_state().map_err(|e| anyhow!("Whisper: {e}"))?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some("en"));
    params.set_translate(false);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
	
    params.set_single_segment(true);
    params.set_no_context(true);
    params.set_suppress_blank(true);
    params.set_temperature_inc(0.0);
    params.set_n_threads(6);

    state.full(params, audio16k).map_err(|e| anyhow!("Whisper: {e}"))?;
    let n = state.full_n_segments().map_err(|e| anyhow!("Whisper: {e}"))?;
    let mut out = String::new();
    for i in 0..n {
        out.push_str(&state.full_get_segment_text(i).map_err(|e| anyhow!("Whisper: {e}"))?);
    }
    let t = out.trim().to_string();
    // Whisper writes things like [BLANK_AUDIO] or (silence) when it hears nothing
    let is_noise_tag = (t.starts_with('[') && t.ends_with(']')) || (t.starts_with('(') && t.ends_with(')'));
    Ok(if is_noise_tag { String::new() } else { t })
}

fn ask_ollama(history: &[(String, String)]) -> Result<String> {
    let start = history.len().saturating_sub(MAX_HISTORY);
    let mut messages: Vec<Value> = Vec::new();
    if !SYSTEM_PROMPT.is_empty() {
        messages.push(json!({ "role": "system", "content": SYSTEM_PROMPT }));
    }
    for (role, content) in &history[start..] {
        let content: String = content.chars().take(2000).collect();
        messages.push(json!({ "role": role, "content": content }));
    }

    let body = json!({
        "model": OLLAMA_MODEL,
        "messages": messages,
        "stream": false,
        "keep_alive": "30m",
        "options": { "num_predict": 200 },
    });

    let resp: Value = ureq::post(OLLAMA_URL)
        .timeout(Duration::from_secs(120))
        .send_json(body)?
        .into_json()?;
    let content = resp["message"]["content"]
        .as_str()
        .ok_or_else(|| anyhow!("no content in Ollama reply"))?;
    Ok(strip_think(content))
}

/// Removes <think>...</think> blocks if the model emits them.
fn strip_think(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(a) = rest.find("<think>") {
        out.push_str(&rest[..a]);
        match rest[a..].find("</think>") {
            Some(b) => rest = &rest[a + b + "</think>".len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// Strip markdown symbols, and turn sentence ends into short pauses (same as the web page).
fn prepare_for_speech(s: &str) -> String {
    // Make symbols, phone numbers and e-mail addresses sound right
    let s = s
        .replace("+971 52 560 8250", "plus nine seven one, five two, five six zero, eight two five zero")
        .replace("info@adatech.ae", "info at adatech dot ae")
        .replace("C++", "C plus plus")
        .replace("C#", "C sharp")
        .replace(".NET", "dot net")
        .replace("micro:bit", "micro bit");
    let mut out = String::new();
    let mut in_pause_run = false;
    for ch in s.chars().filter(|c| !"*_#`>~|".contains(*c)) {
        if matches!(ch, '?' | '.' | '\u{2026}') {
            if !in_pause_run {
                out.push_str(" , ");
                in_pause_run = true;
            }
        } else {
            in_pause_run = false;
            out.push(ch);
        }
    }
    out.trim().to_string()
}