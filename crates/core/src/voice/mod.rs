//! The server's own speech engines: whisper.cpp turns speech into text and
//! Piper turns text into speech. They are downloaded on first use from their
//! official releases, each file checked against a fixed size and SHA-256
//! before it is unpacked or run (`catalog.rs`), into `<data>/voice`. They run
//! one at a time per direction, below the server (lowest priority, like
//! agents' commands), with a cleared environment and a time limit.
//!
//! Audio is written to a private temporary file only because the engines
//! read files, and deleted as soon as they are done; nothing is kept unless
//! the user turned on keeping recordings (`keep`).

pub mod catalog;
pub mod wav;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use tessera_proto::{VoiceChoice, VoiceServer};
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;

mod process;
mod retention;
mod space;
use process::{AUDIO_BYTES, AUDIO_SECONDS, Limits};

/// Why the server's engines couldn't do it.
#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    /// Not downloaded yet, or not made for this computer.
    NotReady(String),
    /// Busy with another recording (only partial results are turned away).
    Busy,
    /// The request itself can't work (an audio format the engine can't read).
    Bad(String),
    /// The engine failed or took too long.
    Failed(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::NotReady(m) | Failure::Bad(m) | Failure::Failed(m) => f.write_str(m),
            Failure::Busy => f.write_str("the voice engine is busy"),
        }
    }
}

#[derive(Debug, Default, Clone)]
struct Install {
    running: bool,
    done: u64,
    total: u64,
    error: Option<String>,
}

/// The server's speech engines and their downloads.
pub struct Voice {
    dir: PathBuf,
    install: Mutex<Install>,
    listen: Semaphore,
    speak: Semaphore,
    /// Seconds of work per second of speech (smoothed over final results).
    pace: Mutex<Option<f64>>,
}

/// The longest text one `speak` takes.
pub const MAX_SPEAK_CHARS: usize = 4_000;

impl Voice {
    pub fn new(data_dir: &Path) -> Self {
        let dir = data_dir.join("voice");
        // Temporary audio of a run that never finished (the server stopped).
        let _ = std::fs::remove_dir_all(dir.join("tmp"));
        Self {
            dir,
            install: Mutex::new(Install::default()),
            listen: Semaphore::new(1),
            speak: Semaphore::new(1),
            pace: Mutex::new(None),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The speech model for `pref` (`auto`, `tiny`, `base`): `auto` takes
    /// `base` where it is quick (AVX2, Apple silicon), `tiny` elsewhere.
    pub fn model_for(pref: &str) -> &'static str {
        match pref {
            "tiny" => "tiny",
            "base" => "base",
            _ if fast_cpu() => "base",
            _ => "tiny",
        }
    }

    /// The voice id for the setting (`server_voice`, else by `language`).
    pub fn voice_for(chosen: &str, language: &str) -> &'static str {
        catalog::voice(chosen)
            .unwrap_or_else(|| catalog::voice_for(language))
            .id
    }

    fn program(&self, p: Option<catalog::Program>, path_name: &str) -> Option<PathBuf> {
        match p {
            Some(p) => find_bin(&self.dir.join(p.dir), p.bin, 3),
            // Nothing to download for this computer: one the user installed.
            None => which(path_name),
        }
    }

    fn whisper_bin(&self) -> Option<PathBuf> {
        self.program(catalog::whisper(), "whisper-cli")
    }

    fn piper_bin(&self) -> Option<PathBuf> {
        catalog::piper().and_then(|p| find_bin(&self.dir.join(p.dir), p.bin, 3))
    }

    /// The model file to use: the chosen one, else any that is there.
    fn model_file(&self, pref: &str) -> Option<PathBuf> {
        let want = Self::model_for(pref);
        [want, "base", "tiny"]
            .into_iter()
            .filter_map(catalog::model)
            .map(|(file, _)| self.dir.join("models").join(file))
            .find(|p| p.is_file())
    }

    fn voice_file(&self, id: &str) -> Option<PathBuf> {
        let v = catalog::voice(id)?;
        let onnx = self.dir.join("voices").join(format!("{}.onnx", v.id));
        let json = self.dir.join("voices").join(format!("{}.onnx.json", v.id));
        (onnx.is_file() && json.is_file()).then_some(onnx)
    }

    /// What is still missing for this model and voice, with sizes.
    fn missing(&self, model: &str, voice: &str) -> Vec<Part> {
        let mut out = Vec::new();
        if let Some(p) = catalog::whisper()
            && self.whisper_bin().is_none()
        {
            out.push(Part::Program(p));
        }
        if let Some(p) = catalog::piper()
            && self.piper_bin().is_none()
        {
            out.push(Part::Program(p));
        }
        if let Some((file, d)) = catalog::model(model)
            && !self.dir.join("models").join(file).is_file()
            && (self.whisper_bin().is_some() || catalog::whisper().is_some())
        {
            out.push(Part::File(format!("models/{file}"), d));
        }
        if let Some(v) = catalog::voice(voice)
            && self.voice_file(v.id).is_none()
            && catalog::piper().is_some()
        {
            out.push(Part::File(format!("voices/{}.onnx", v.id), v.onnx));
            out.push(Part::File(format!("voices/{}.onnx.json", v.id), v.json));
        }
        out
    }

    /// Where the engines stand, for `VoiceConfig::server`.
    pub fn status(&self, model_pref: &str, voice_id: &str) -> VoiceServer {
        let model = Self::model_for(model_pref);
        let listen = self.whisper_bin().is_some() && self.model_file(model_pref).is_some();
        let speak = self.piper_bin().is_some() && self.voice_file(voice_id).is_some();
        let can_listen = catalog::whisper().is_some() || self.whisper_bin().is_some();
        let can_speak = catalog::piper().is_some();
        let inst = self.install.lock().clone();
        let missing: u64 = self.missing(model, voice_id).iter().map(Part::size).sum();
        let state = if inst.running {
            "installing"
        } else if !can_listen && !can_speak {
            "unsupported"
        } else if inst.error.is_some() {
            "failed"
        } else if missing == 0 && (listen || !can_listen) && (speak || !can_speak) {
            "ready"
        } else {
            "missing"
        };
        let live = (*self.pace.lock()).map_or(fast_cpu(), |p| p < 0.45);
        VoiceServer {
            state: state.into(),
            error: inst.error,
            download_bytes: missing,
            done_bytes: inst.done,
            total_bytes: inst.total,
            model: self
                .model_file(model_pref)
                .and_then(|p| {
                    ["tiny", "base"]
                        .into_iter()
                        .find(|m| catalog::model(m).is_some_and(|(f, _)| p.ends_with(f)))
                })
                .unwrap_or(model)
                .into(),
            listen,
            speak,
            voice: voice_id.into(),
            live: listen && live,
            voices: catalog::VOICES
                .iter()
                .map(|v| VoiceChoice {
                    id: v.id.into(),
                    label: v.label.into(),
                    language: v.language.into(),
                    installed: self.voice_file(v.id).is_some(),
                    size_bytes: v.onnx.size + v.json.size,
                })
                .collect(),
        }
    }

    /// Download what the model and voice still need, in the background
    /// (once at a time), then check that both engines really run here.
    pub fn install(self: &Arc<Self>, http: reqwest::Client, model_pref: &str, voice_id: &str) {
        let model = Self::model_for(model_pref).to_string();
        let voice = voice_id.to_string();
        {
            let mut i = self.install.lock();
            if i.running {
                return;
            }
            *i = Install {
                running: true,
                ..Default::default()
            };
        }
        let me = self.clone();
        tokio::spawn(async move {
            let result = me.install_now(&http, &model, &voice).await;
            let mut i = me.install.lock();
            i.running = false;
            i.error = result.err();
        });
    }

    async fn install_now(
        &self,
        http: &reqwest::Client,
        model: &str,
        voice: &str,
    ) -> Result<(), String> {
        self.install_now_with_space(http, model, voice, &space::available)
            .await
    }

    async fn install_now_with_space(
        &self,
        http: &reqwest::Client,
        model: &str,
        voice: &str,
        free: &(dyn Fn(&Path) -> Result<u64, String> + Sync),
    ) -> Result<(), String> {
        let parts = self.missing(model, voice);
        let work = |parts: &[Part]| {
            parts
                .iter()
                .map(|p| {
                    p.size().saturating_add(if matches!(p, Part::Program(_)) {
                        process::UNPACK_BYTES
                    } else {
                        0
                    })
                })
                .sum::<u64>()
        };
        if !parts.is_empty() {
            space::admit(free(&self.dir)?, work(&parts))?;
        }
        self.install.lock().total = parts.iter().map(Part::size).sum();
        for dir in ["models", "voices", "tmp"] {
            tokio::fs::create_dir_all(self.dir.join(dir))
                .await
                .map_err(|e| format!("can't write to {}: {e}", self.dir.display()))?;
        }
        for (index, part) in parts.iter().enumerate() {
            let remaining = work(&parts[index..]);
            space::admit(free(&self.dir)?, remaining)?;
            let check = |got| space::admit(free(&self.dir)?, remaining.saturating_sub(got));
            let base = self.install.lock().done;
            let progress = |n: u64| self.install.lock().done = base + n;
            match part {
                Part::File(rel, d) => {
                    fetch_checked(http, d, &self.dir.join(rel), progress, check).await?;
                }
                Part::Program(p) => {
                    let archive = self.dir.join("tmp").join(format!("{}.download", p.dir));
                    fetch_checked(http, &p.archive, &archive, progress, check).await?;
                    let extracted = |used: u64| check(p.archive.size.saturating_add(used));
                    let unpacked = unpack(&archive, &self.dir.join(p.dir), p.dir, &extracted).await;
                    let _ = tokio::fs::remove_file(&archive).await;
                    unpacked?;
                    if find_bin(&self.dir.join(p.dir), p.bin, 3).is_none() {
                        return Err(format!("{} wasn't in the download", p.bin));
                    }
                }
            }
        }
        self.check(model, voice).await
    }

    /// Speak a few words and hear them back: proves both engines run on
    /// this computer (and measures how quick the speech model is here).
    async fn check(&self, model: &str, voice: &str) -> Result<(), String> {
        let spoken = if self.piper_bin().is_some() && self.voice_file(voice).is_some() {
            Some(
                self.speak("Testing, one two three.", voice)
                    .await
                    .map_err(|e| format!("the voice doesn't run on this computer: {e}"))?,
            )
        } else {
            None
        };
        if self.whisper_bin().is_some() && self.model_file(model).is_some() {
            let audio = match &spoken {
                Some(w) => w.clone(),
                // Half a second of silence still runs the whole model.
                None => wav::encode_pcm16(16_000, &[0; 8_000]),
            };
            self.transcribe(&audio, "audio/wav", model, "en", false)
                .await
                .map_err(|e| format!("speech recognition doesn't run on this computer: {e}"))?;
        }
        Ok(())
    }

    /// Speech to text with the server's model. `partial`: the words so far
    /// while the user still speaks; turned away when the engine is busy.
    pub async fn transcribe(
        &self,
        audio: &[u8],
        mime: &str,
        model_pref: &str,
        language: &str,
        partial: bool,
    ) -> Result<String, Failure> {
        let not_ready =
            || Failure::NotReady("the server's speech recognition isn't set up yet".into());
        let bin = self.whisper_bin().ok_or_else(not_ready)?;
        let model = self.model_file(model_pref).ok_or_else(not_ready)?;
        let _turn = if partial {
            self.listen.try_acquire().map_err(|_| Failure::Busy)?
        } else {
            tokio::time::timeout(Duration::from_secs(90), self.listen.acquire())
                .await
                .map_err(|_| Failure::Busy)?
                .map_err(|_| Failure::Busy)?
        };
        if audio.len() as u64 > AUDIO_BYTES {
            return Err(Failure::Bad("the recording is larger than 16 MB".into()));
        }
        let tmp = Scratch::new(&self.dir.join("tmp")).map_err(Failure::Failed)?;
        let _ = mime; // legacy/compressed audio is always normalized and bounded.
        let (input, secs) = match wav::info(audio) {
            Some(i) => (
                tmp.write("in.wav", audio).map_err(Failure::Failed)?,
                Some(i.seconds()),
            ),
            None => {
                let path = convert(&tmp, audio).await?;
                let bytes = read_audio(&path).await?;
                let secs = wav::info(&bytes).map(|i| i.seconds());
                (path, secs)
            }
        };
        if secs.is_some_and(|s| s > AUDIO_SECONDS) {
            return Err(Failure::Bad("record at most two minutes at a time".into()));
        }
        if secs.is_some_and(|s| s < 0.1) {
            return Ok(String::new());
        }
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get().clamp(1, 8));
        let lang = language.split(['-', '_']).next().unwrap_or("").trim();
        let mut args: Vec<String> = vec![
            "-m".into(),
            model.to_string_lossy().into_owned(),
            "-f".into(),
            input.to_string_lossy().into_owned(),
            "-nt".into(),
            "-np".into(),
            "-t".into(),
            threads.to_string(),
            "-l".into(),
            if lang.len() == 2 {
                lang.to_ascii_lowercase()
            } else {
                "auto".into()
            },
            // Greedy and without retries at higher temperatures: quick.
            "-bs".into(),
            "1".into(),
            "-bo".into(),
            "1".into(),
            "-nf".into(),
        ];
        if let Some(s) = secs {
            // The model looks at 30 s windows; a short clip needs a part of
            // one (50 positions a second), which is what makes it quick.
            let ctx = ((s * 50.0).ceil() as u32 + 128).clamp(384, 1500);
            args.extend(["-ac".into(), ctx.to_string()]);
        }
        let limit = secs.map_or(300.0, |s| (30.0 + 8.0 * s).min(600.0));
        let started = Instant::now();
        let out = run(&bin, &args, None, &tmp.dir, Duration::from_secs_f64(limit)).await?;
        if let Some(s) = secs.filter(|s| !partial && *s >= 1.0) {
            let pace = started.elapsed().as_secs_f64() / s;
            let mut p = self.pace.lock();
            *p = Some(p.map_or(pace, |old| old * 0.6 + pace * 0.4));
        }
        Ok(clean_transcript(&String::from_utf8_lossy(&out)))
    }

    /// Text to speech with the server's voice: a WAV file.
    pub async fn speak(&self, text: &str, voice_id: &str) -> Result<Vec<u8>, Failure> {
        let not_ready = || Failure::NotReady("the server's voice isn't set up yet".into());
        let bin = self.piper_bin().ok_or_else(not_ready)?;
        let model = self.voice_file(voice_id).ok_or_else(not_ready)?;
        // Piper reads a line at a time and each line replaces the file: one line.
        let line: String = text
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .take(MAX_SPEAK_CHARS)
            .collect();
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            return Err(Failure::Bad("nothing to say".into()));
        }
        let _turn = tokio::time::timeout(Duration::from_secs(90), self.speak.acquire())
            .await
            .map_err(|_| Failure::Busy)?
            .map_err(|_| Failure::Busy)?;
        let tmp = Scratch::new(&self.dir.join("tmp")).map_err(Failure::Failed)?;
        let out = tmp.dir.join("out.wav");
        let args = vec![
            "--model".to_string(),
            model.to_string_lossy().into_owned(),
            "--output_file".into(),
            out.to_string_lossy().into_owned(),
        ];
        let limit = Duration::from_secs((20 + line.len() as u64 / 8).min(300));
        run(&bin, &args, Some(format!("{line}\n")), &tmp.dir, limit).await?;
        read_audio(&out).await
    }

    /// Keep a recording (the user turned keeping on): `voice/recordings`.
    pub fn keep(&self, audio: &[u8], mime: &str) -> std::io::Result<PathBuf> {
        space::admit(
            space::available(&self.dir).map_err(std::io::Error::other)?,
            audio.len() as u64,
        )
        .map_err(std::io::Error::other)?;
        let dir = self.dir.join("recordings");
        std::fs::create_dir_all(&dir)?;
        let ext = match mime.split(';').next().unwrap_or("").trim() {
            "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
            "audio/ogg" => "ogg",
            "audio/mpeg" | "audio/mp3" => "mp3",
            "audio/mp4" | "audio/m4a" | "audio/x-m4a" | "audio/aac" => "m4a",
            "audio/flac" => "flac",
            _ => "webm",
        };
        let stamp = time::OffsetDateTime::now_utc()
            .format(time::macros::format_description!(
                "[year][month][day]-[hour][minute][second]"
            ))
            .unwrap_or_default();
        let path = dir.join(format!("{stamp}-{:016x}.{ext}", rand::random::<u64>()));
        retention::write_checked(&path, audio, &|work| {
            space::admit(space::available(&dir).map_err(std::io::Error::other)?, work)
                .map_err(std::io::Error::other)
        })?;
        Ok(path)
    }

    /// Recordings kept, and their total size.
    pub fn kept(&self) -> (u32, u64) {
        let Ok(rd) = std::fs::read_dir(self.dir.join("recordings")) else {
            return (0, 0);
        };
        rd.flatten()
            .filter_map(|e| e.metadata().ok().filter(|m| m.is_file()))
            .fold((0, 0), |(n, b), m| (n + 1, b + m.len()))
    }

    pub fn delete_kept(&self) -> std::io::Result<()> {
        match std::fs::remove_dir_all(self.dir.join("recordings")) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

enum Part {
    Program(catalog::Program),
    File(String, catalog::Download),
}

impl Part {
    fn size(&self) -> u64 {
        match self {
            Part::Program(p) => p.archive.size,
            Part::File(_, d) => d.size,
        }
    }
}

/// A private folder for one run's files, removed when dropped.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(parent: &Path) -> Result<Self, String> {
        Self::new_checked(parent, &|work| {
            space::admit(space::available(parent)?, work)
        })
    }

    fn new_checked(
        parent: &Path,
        admit: &dyn Fn(u64) -> Result<(), String>,
    ) -> Result<Self, String> {
        admit(AUDIO_BYTES * 2)?;
        let dir = parent.join(format!("run-{:016x}", rand::random::<u64>()));
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("can't make a temporary folder: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&dir)
                .map_err(|e| format!("can't make a temporary folder: {e}"))?;
        }
        #[cfg(not(unix))]
        std::fs::create_dir(&dir).map_err(|e| format!("can't make a temporary folder: {e}"))?;
        Ok(Self { dir })
    }

    fn write(&self, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
        self.write_checked(name, bytes, &|work| {
            space::admit(space::available(&self.dir)?, work)
        })
    }

    fn write_checked(
        &self,
        name: &str,
        bytes: &[u8],
        admit: &dyn Fn(u64) -> Result<(), String>,
    ) -> Result<PathBuf, String> {
        admit(bytes.len() as u64)?;
        let path = self.dir.join(name);
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        use std::io::Write;
        let result = (|| {
            let mut file = o
                .open(&path)
                .map_err(|e| format!("can't write the recording: {e}"))?;
            let mut remaining = bytes.len() as u64;
            for chunk in bytes.chunks(64 * 1024) {
                admit(remaining)?;
                file.write_all(chunk)
                    .map_err(|e| format!("can't write the recording: {e}"))?;
                remaining -= chunk.len() as u64;
            }
            admit(0)
        })();
        if let Err(error) = result {
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
        Ok(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Audio the engine can't read (Opus, AAC) made into WAV with ffmpeg, if
/// this computer has it.
async fn convert(tmp: &Scratch, audio: &[u8]) -> Result<PathBuf, Failure> {
    let Some(ffmpeg) = which("ffmpeg") else {
        return Err(Failure::Bad(
            "the server's speech recognition reads WAV recordings; update the Tessera app on this device".into(),
        ));
    };
    let input = tmp.write("in.audio", audio).map_err(Failure::Failed)?;
    let out = tmp.dir.join("in.wav");
    let args: Vec<String> = ["-nostdin", "-hide_banner", "-loglevel", "error", "-i"]
        .iter()
        .map(|s| s.to_string())
        .chain([
            input.to_string_lossy().into_owned(),
            "-t".into(),
            "120.1".into(),
            "-ar".into(),
            "16000".into(),
            "-ac".into(),
            "1".into(),
            out.to_string_lossy().into_owned(),
        ])
        .collect();
    run(&ffmpeg, &args, None, &tmp.dir, Duration::from_secs(60)).await?;
    Ok(out)
}

async fn read_audio(path: &Path) -> Result<Vec<u8>, Failure> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|e| Failure::Failed(format!("can't read speech: {e}")))?;
    if !metadata.is_file() {
        return Err(Failure::Failed(
            "speech must be a regular audio file".into(),
        ));
    }
    let size = metadata.len();
    if size > AUDIO_BYTES {
        return Err(Failure::Failed("speech is larger than 16 MB".into()));
    }
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| Failure::Failed(format!("can't read speech: {e}")))?;
    let mut bytes = Vec::new();
    file.take(AUDIO_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| Failure::Failed(format!("can't read speech: {e}")))?;
    if bytes.len() as u64 > AUDIO_BYTES {
        return Err(Failure::Failed("speech is larger than 16 MB".into()));
    }
    if wav::info(&bytes).is_none_or(|i| i.seconds() > AUDIO_SECONDS) {
        return Err(Failure::Failed(
            "speech must be valid WAV, at most two minutes".into(),
        ));
    }
    Ok(bytes)
}

async fn run(
    bin: &Path,
    args: &[String],
    stdin: Option<String>,
    tmp: &Path,
    limit: Duration,
) -> Result<Vec<u8>, Failure> {
    process::run(bin, args, stdin, tmp, Limits::engine(limit)).await
}

/// The text whisper printed, without the markers it uses for no speech.
fn clean_transcript(out: &str) -> String {
    let mut words = Vec::new();
    for line in out.lines() {
        let l = line.trim();
        let marker =
            |open: char, close: char| l.starts_with(open) && l.ends_with(close) && l.len() < 40;
        if l.is_empty() || marker('[', ']') || marker('(', ')') || marker('*', '*') {
            continue;
        }
        words.push(l);
    }
    words.join(" ")
}

/// `path` with `suffix` added to its name ("a.tar.gz" → "a.tar.gz.part").
fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// A program in `root`, at most `depth` folders down.
fn find_bin(root: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let direct = root.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    if depth == 0 {
        return None;
    }
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    dirs.into_iter().find_map(|d| find_bin(&d, name, depth - 1))
}

fn which(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(&exe))
        .find(|p| p.is_file())
}

/// Quick enough for the larger speech model and for live words.
fn fast_cpu() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("avx2")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        cfg!(all(target_os = "macos", target_arch = "aarch64"))
    }
}

/// Download `d` to `dest`: refused if it grows past its size, and only
/// moved into place when its size and SHA-256 match.
#[cfg(test)]
async fn fetch(
    http: &reqwest::Client,
    d: &catalog::Download,
    dest: &Path,
    progress: impl Fn(u64),
) -> Result<(), String> {
    fetch_checked(http, d, dest, progress, |_| Ok(())).await
}

async fn fetch_checked(
    http: &reqwest::Client,
    d: &catalog::Download,
    dest: &Path,
    progress: impl Fn(u64),
    admit: impl Fn(u64) -> Result<(), String>,
) -> Result<(), String> {
    use futures::StreamExt;
    admit(0)?;
    let name = d.url.rsplit('/').next().unwrap_or(d.url);
    let part = suffixed(dest, ".part");
    let resp = http
        .get(d.url)
        .timeout(Duration::from_secs(30 * 60))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("couldn't download {name}: {e}"))?;
    admit(0)?;
    let mut file = tokio::fs::File::create(&part)
        .await
        .map_err(|e| format!("can't save {name}: {e}"))?;
    let mut hash = Sha256::new();
    let mut got = 0u64;
    let mut body = resp.bytes_stream();
    let result: Result<(), String> = async {
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| format!("the download of {name} broke off: {e}"))?;
            // `got` counts bytes already on disk, not the pending chunk.
            admit(got)?;
            got += chunk.len() as u64;
            if got > d.size {
                return Err(format!("{name} is larger than expected; not used"));
            }
            hash.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|e| format!("can't save {name}: {e}"))?;
            admit(got)?;
            progress(got);
        }
        file.flush()
            .await
            .map_err(|e| format!("can't save {name}: {e}"))?;
        let sum: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if got != d.size || sum != d.sha256 {
            return Err(format!(
                "{name} isn't the expected file (its fingerprint differs); not used"
            ));
        }
        Ok(())
    }
    .await;
    drop(file);
    if let Err(e) = result {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(e);
    }
    tokio::fs::rename(&part, dest)
        .await
        .map_err(|e| format!("can't save {name}: {e}"))
}

/// Unpacks a checked archive into `into` (made fresh). `tar` reads both
/// .tar.gz and, on Windows (bsdtar), .zip.
async fn unpack(
    archive: &Path,
    into: &Path,
    label: &str,
    admit: &(dyn Fn(u64) -> Result<(), String> + Sync),
) -> Result<(), String> {
    let staging = suffixed(into, ".unpacking");
    let _ = tokio::fs::remove_dir_all(&staging).await;
    tokio::fs::create_dir_all(&staging)
        .await
        .map_err(|e| format!("can't unpack {label}: {e}"))?;
    let tar = if cfg!(windows) {
        std::env::var_os("SystemRoot")
            .map(|r| PathBuf::from(r).join("System32").join("tar.exe"))
            .filter(|p| p.is_file())
            .unwrap_or_else(|| PathBuf::from("tar"))
    } else {
        which("tar").unwrap_or_else(|| PathBuf::from("tar"))
    };
    let args = vec![
        "-xf".into(),
        archive.to_string_lossy().into_owned(),
        "-C".into(),
        staging.to_string_lossy().into_owned(),
    ];
    let mut limits = Limits::engine(Duration::from_secs(120));
    limits.files = process::UNPACK_BYTES;
    let out = process::run_checked(&tar, &args, None, &staging, limits, admit).await;
    if let Err(e) = out {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(format!("can't unpack {label}: {e}"));
    }
    let _ = tokio::fs::remove_dir_all(into).await;
    tokio::fs::rename(&staging, into)
        .await
        .map_err(|e| format!("can't unpack {label}: {e}"))
}

#[cfg(test)]
mod tests;
