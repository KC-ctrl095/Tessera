//! Voice requests: the user's choice of where speech is handled
//! (`[voice] engine`), the server's own engines (`crate::voice`), OpenAI with
//! the user's own key, and another transcription service.
//!
//! Audio only goes where the choice allows: `auto` and `server` never send it
//! off the server, OpenAI only gets it when the user picked OpenAI, and the
//! other service only when it is the choice. Keys never leave the server.

use super::*;
use crate::voice::{Failure, MAX_SPEAK_CHARS, Voice};
use std::net::{IpAddr, SocketAddr};

const TRANSCRIPT_BYTES: usize = 64 * 1024;
const ERROR_BYTES: usize = 16 * 1024;
const SPEECH_BYTES: usize = 16 * 1024 * 1024;
const LARGE_RESPONSE: &str = "Speech service returned a response larger than allowed.";
const HTTPS_REQUIRED: &str = "Use HTTPS for speech services outside your local network.";
const OPENROUTER_BASE: &str = "https://openrouter.ai/api/v1";
const OPENROUTER_TRANSCRIBE: &str = "openai/gpt-4o-mini-transcribe";
const OPENROUTER_SPEAK: &str = "elevenlabs/eleven-v4-turbo";
const OPENROUTER_VOICES: &[&str] = &[
    "sarah", "george", "adam", "alice", "bella", "bill", "brian", "callum", "charlie", "chris",
    "daniel", "eric", "harry", "jessica", "laura", "liam", "lily", "matilda", "river", "roger",
    "will",
];

/// A provider called OpenRouter is not enough. Never reuse a proxy's key or
/// send speech there. Production requests always use the fixed address above.
fn is_openrouter(base: &str) -> bool {
    url::Url::parse(base).is_ok_and(|u| {
        u.scheme() == "https"
            && u.host_str() == Some("openrouter.ai")
            && u.port_or_known_default() == Some(443)
            && u.path().trim_end_matches('/') == "/api/v1"
            && u.username().is_empty()
            && u.password().is_none()
            && u.query().is_none()
            && u.fragment().is_none()
    })
}

fn openrouter_voice(chosen: &str) -> Result<&str, ApiError> {
    let voice = if chosen.is_empty() { "sarah" } else { chosen };
    if !OPENROUTER_VOICES.contains(&voice) {
        return Err(bad(
            "Choose one of the OpenRouter voices in Settings → Web & voice.",
        ));
    }
    Ok(voice)
}

fn local_speech_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or_else(
            || ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
            |ip| local_speech_ip(IpAddr::V4(ip)),
        ),
    }
}

fn local_name(name: &str) -> bool {
    let name = name.trim_end_matches('.');
    !name.contains('.') || name.ends_with(".local")
}

fn local_addresses(addrs: &[SocketAddr]) -> Result<(), &'static str> {
    if addrs.is_empty() || addrs.iter().any(|addr| !local_speech_ip(addr.ip())) {
        return Err(HTTPS_REQUIRED);
    }
    Ok(())
}

async fn speech_http(base: &str) -> Result<reqwest::Client, ApiError> {
    let base = safe_voice_url(base).map_err(bad)?;
    let url = url::Url::parse(&base).map_err(|_| bad("Enter the speech service address."))?;
    let addrs = if url.scheme() == "http"
        && let Some(url::Host::Domain(host)) = url.host()
    {
        tokio::time::timeout(
            std::time::Duration::from_secs(15),
            tokio::net::lookup_host((host, url.port_or_known_default().unwrap_or(80))),
        )
        .await
        .map_err(|_| bad("Couldn't resolve the local speech service."))?
        .map_err(|_| bad("Couldn't resolve the local speech service."))?
        .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    speech_client(&url, &addrs)
}

fn speech_client(url: &url::Url, addrs: &[SocketAddr]) -> Result<reqwest::Client, ApiError> {
    let mut client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(15));
    if url.scheme() == "http" {
        // Do not route private audio through an environment proxy, or resolve a
        // local-looking name a second time after admission (DNS rebinding).
        client = client.no_proxy();
        if let Some(url::Host::Domain(host)) = url.host() {
            local_addresses(addrs).map_err(bad)?;
            client = client.resolve_to_addrs(host, addrs);
        }
    }
    client
        .build()
        .map_err(|_| internal("Couldn't prepare the speech connection."))
}

async fn response_bytes(
    mut response: reqwest::Response,
    cap: usize,
) -> Result<Vec<u8>, &'static str> {
    if response
        .content_length()
        .is_some_and(|size| size > cap as u64)
    {
        return Err(LARGE_RESPONSE);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Couldn't read the speech service response.")?
    {
        if chunk.len() > cap.saturating_sub(bytes.len()) {
            return Err(LARGE_RESPONSE);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Where one request's audio or text goes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Route {
    /// The server's own engines (whisper.cpp, Piper).
    Server,
    /// OpenAI, with the user's key.
    OpenAi,
    /// OpenRouter, only with explicit opt-in and the user's OpenRouter key.
    OpenRouter,
    /// The transcription service set up under More.
    Service,
}

impl Route {
    fn name(self) -> &'static str {
        match self {
            Route::Server => "server",
            Route::OpenAi => "openai",
            Route::OpenRouter => "openrouter",
            Route::Service => "service",
        }
    }
}

fn safe_voice_url(base: &str) -> Result<String, &'static str> {
    if base.is_empty() {
        return Ok(String::new());
    }
    let url = url::Url::parse(base).map_err(|_| "the voice base URL is not a valid URL")?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("Use an HTTP or HTTPS speech service address.");
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "Use a speech service address without credentials, query parameters or a fragment. Put its key in the API key field.",
        );
    }
    if url.scheme() == "http"
        && !match url.host() {
            Some(url::Host::Ipv4(ip)) => local_speech_ip(IpAddr::V4(ip)),
            Some(url::Host::Ipv6(ip)) => local_speech_ip(IpAddr::V6(ip)),
            Some(url::Host::Domain(host)) => local_name(host),
            None => false,
        }
    {
        return Err(HTTPS_REQUIRED);
    }
    Ok(base.trim_end_matches('/').to_string())
}

fn content_type(response: &reqwest::Response) -> String {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

const OFF: &str = "Voice is off. Turn it on in Settings → Web & voice.";
const DEVICE_ONLY: &str = "Voice is set to work on each device only, so your server doesn't handle speech. Change it in Settings → Web & voice.";

/// Speech to text for the setting `engine`, when a client `asked` for an
/// engine (none: the setting decides). The server's own engine is always
/// allowed where the server handles speech at all (it keeps audio at home);
/// OpenAI and the other service only when they are the choice.
pub(crate) fn listen_route(engine: &str, asked: Option<&str>) -> Result<Route, String> {
    let chosen = match engine {
        "off" => return Err(OFF.into()),
        "device" => return Err(DEVICE_ONLY.into()),
        "openai" => Route::OpenAi,
        "openrouter" => Route::OpenRouter,
        "service" => Route::Service,
        _ => Route::Server,
    };
    match asked {
        None => Ok(chosen),
        Some("server") => Ok(Route::Server),
        Some(a) if a == chosen.name() => Ok(chosen),
        Some(a) => Err(format!(
            "Your voice setting doesn't send speech to {}. Change it in Settings → Web & voice.",
            match a {
                "openai" => "OpenAI",
                "openrouter" => "OpenRouter",
                _ => "that service",
            }
        )),
    }
}

/// Text to speech: OpenAI when it is the choice, else the server's voice
/// (the other service only transcribes).
pub(crate) fn speak_route(engine: &str, asked: Option<&str>) -> Result<Route, String> {
    match listen_route(engine, asked)? {
        Route::Service => Ok(Route::Server),
        r => Ok(r),
    }
}

fn failure(f: Failure) -> ApiError {
    match f {
        Failure::NotReady(m) => ApiError::new(ErrorCode::NotFound, m),
        Failure::Busy => ApiError::new(ErrorCode::Busy, Failure::Busy.to_string()),
        Failure::Bad(m) => bad(m),
        Failure::Failed(m) => ApiError::new(ErrorCode::Provider, m),
    }
}

impl Inner {
    fn scrub_voice(&self, message: impl std::fmt::Display, key: Option<&str>) -> String {
        // Providers can echo Authorization, and transport errors can include
        // userinfo/query credentials. Scrub before truncation or wire output.
        let mut secrets = self.redaction_secrets();
        if let Some(key) = key.filter(|k| !k.is_empty()) {
            secrets.push(key.to_string());
        }
        let forms = crate::auth::value_forms(secrets.iter());
        secrets.extend(forms);
        let text = crate::tools::redact(&message.to_string(), &secrets);
        let text = regex::Regex::new(r"://[^/\s@]+@")
            .expect("userinfo pattern")
            .replace_all(&text, "://[hidden]@");
        regex::Regex::new(r#"([?&][^=&#\s]+)=([^&#\s\"']*)"#)
            .expect("query credential pattern")
            .replace_all(&text, "${1}=[hidden]")
            .into_owned()
    }

    fn voice_provider_error(&self, message: impl std::fmt::Display, key: Option<&str>) -> ApiError {
        ApiError::new(
            ErrorCode::Provider,
            crate::util::truncate(&self.scrub_voice(message, key), 1000),
        )
    }

    fn voice_http_error(&self, status: u16, body: &str, key: Option<&str>) -> ApiError {
        if matches!(status, 401 | 403) {
            return ApiError::new(
                ErrorCode::Provider,
                "Speech authentication failed. Check the saved API key.",
            );
        }
        // Even the shared HTTP parser can truncate a plain-text body. Redact
        // first so a credential crossing its limit cannot leak a prefix.
        let body = self.scrub_voice(body, key);
        let message = crate::llm::LlmError::from_http(status, &body, None).message;
        self.voice_provider_error(format!("speech service failed: {message}"), key)
    }
}
fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

impl Inner {
    /// The user's OpenAI key and where to send it: the OpenAI model
    /// service's (Models; its address, so a proxy set there is used too),
    /// else `OPENAI_API_KEY` from Passwords & keys (api.openai.com).
    fn openai(&self) -> Option<(String, String)> {
        for p in self.providers.configs() {
            let host = url::Url::parse(&p.base_url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string));
            if (p.id == "openai" || host.as_deref() == Some("api.openai.com"))
                && let Some(Credential::ApiKey { key }) = self.auth.get(&p.id)
            {
                let base = if p.base_url.is_empty() {
                    "https://api.openai.com/v1".to_string()
                } else {
                    p.base_url.trim_end_matches('/').to_string()
                };
                return Some((base, key));
            }
        }
        self.auth
            .server_key("OPENAI_API_KEY", "api.openai.com")
            .map(|k| ("https://api.openai.com/v1".into(), k))
    }

    fn openrouter(&self) -> Option<String> {
        for p in self.providers.configs() {
            if is_openrouter(&p.base_url)
                && let Some(Credential::ApiKey { key }) = self.auth.get(&p.id)
                && !key.trim().is_empty()
            {
                return Some(key);
            }
        }
        self.auth.server_key("OPENROUTER_API_KEY", "openrouter.ai")
    }

    pub(super) fn voice_config(&self) -> VoiceConfig {
        let v = self.config.read().voice.clone();
        let voice_id = Voice::voice_for(&v.server_voice, &v.language);
        let (kept, kept_bytes) = self.voice.kept();
        VoiceConfig {
            enabled: v.enabled,
            engine: v.engine().into(),
            server: Some(self.voice.status(&v.server_model, voice_id)),
            provider: v.provider,
            // Legacy config files may contain credentials; never return them.
            base_url: safe_voice_url(&v.base_url).unwrap_or_default(),
            model: v.model,
            language: v.language,
            has_key: self.auth.get("voice").is_some(),
            keep_audio: v.keep_audio,
            server_model: v.server_model,
            server_voice: v.server_voice,
            openai_key: self.openai().is_some(),
            openrouter_key: self.openrouter().is_some(),
            openrouter_voice: openrouter_voice(&v.openrouter_voice)
                .unwrap_or("sarah")
                .into(),
            kept,
            kept_bytes,
        }
    }

    /// The server can turn speech into text right now (what clients from
    /// before the engine choice look at to show their microphone).
    pub(super) fn voice_ready(&self) -> bool {
        let v = self.config.read().voice.clone();
        match v.engine() {
            "service" => v.enabled,
            "openai" => self.openai().is_some(),
            "openrouter" => self.openrouter().is_some(),
            "auto" | "server" => {
                let voice_id = Voice::voice_for(&v.server_voice, &v.language);
                self.voice.status(&v.server_model, voice_id).listen
            }
            _ => false,
        }
    }

    pub(super) fn voice_engine(&self) -> String {
        self.config.read().voice.engine().to_string()
    }

    pub(super) async fn save_voice(
        &self,
        config: VoiceConfig,
        api_key: Option<String>,
    ) -> Result<VoiceConfig, ApiError> {
        let current = self.config.read().voice.clone();
        // Apps from before the choice only turn the service on or off.
        let engine = match config.engine.as_str() {
            "" if config.enabled => "service".to_string(),
            "" if current.engine() == "service" => "auto".to_string(),
            "" => current.engine().to_string(),
            e => e.to_string(),
        };
        if !crate::config::VoiceSettings::ENGINES.contains(&engine.as_str()) {
            return Err(bad(format!("unknown voice engine `{engine}`")));
        }
        let server_model = match config.server_model.trim() {
            "" => "auto",
            m @ ("auto" | "tiny" | "base") => m,
            m => return Err(bad(format!("unknown speech model `{m}`"))),
        }
        .to_string();
        let server_voice = config.server_voice.trim().to_string();
        if !server_voice.is_empty() && Voice::voice_for(&server_voice, "") != server_voice {
            return Err(bad(format!("unknown voice `{server_voice}`")));
        }
        let openrouter_voice = openrouter_voice(config.openrouter_voice.trim())?.to_string();
        if engine == "openrouter" && self.openrouter().is_none() {
            return Err(bad(
                "Save your OpenRouter API key in Passwords & keys as OPENROUTER_API_KEY, or add the official OpenRouter service under Models.",
            ));
        }
        let check_service = engine == "service" || config.base_url.trim() != current.base_url;
        // An unavailable, unchanged inactive service must not prevent turning
        // voice off or choosing another engine. Dispatch always checks again.
        let base_url = if check_service {
            safe_voice_url(config.base_url.trim()).map_err(bad)?
        } else {
            current.base_url.clone()
        };
        if check_service && config.enabled && base_url.is_empty() {
            return Err(bad("Enter the speech service address."));
        }
        if engine == "service" && !config.enabled {
            return Err(bad(
                "set up the transcription service first (its address and model)",
            ));
        }
        if engine == "openai" && self.openai().is_none() {
            return Err(bad(
                "save your OpenAI API key first: Settings → Privacy & safety → Passwords & keys, as OPENAI_API_KEY (or add OpenAI under Models)",
            ));
        }
        if check_service && !base_url.is_empty() {
            speech_http(&base_url).await?;
        }
        if engine == "openai"
            && let Some((base, _)) = self.openai()
        {
            speech_http(&base).await?;
        }
        if let Some(k) = api_key {
            if k.trim().is_empty() {
                self.auth.remove("voice").map_err(internal)?;
            } else {
                self.auth
                    .set(
                        "voice",
                        Credential::ApiKey {
                            key: k.trim().to_string(),
                        },
                    )
                    .map_err(internal)?;
            }
        }
        {
            let mut c = self.config.write();
            c.voice = crate::config::VoiceSettings {
                enabled: config.enabled,
                provider: config.provider,
                base_url,
                model: config.model.trim().to_string(),
                language: config.language.trim().to_string(),
                engine,
                keep_audio: config.keep_audio,
                server_model,
                server_voice,
                openrouter_voice,
            };
            c.save(&self.paths).map_err(internal)?;
        }
        Ok(self.voice_config())
    }

    /// Download the server's engines, model and voice (in the background).
    pub(super) fn install_voice(&self) -> Result<VoiceConfig, ApiError> {
        let v = self.config.read().voice.clone();
        if v.engine() == "off" {
            return Err(bad(OFF));
        }
        let voice_id = Voice::voice_for(&v.server_voice, &v.language);
        self.voice
            .install(self.providers.http().clone(), &v.server_model, voice_id);
        Ok(self.voice_config())
    }

    pub(super) fn delete_voice_recordings(&self) -> Result<VoiceConfig, ApiError> {
        self.voice.delete_kept().map_err(internal)?;
        Ok(self.voice_config())
    }

    pub(super) async fn transcribe(
        &self,
        audio_b64: &str,
        mime: &str,
        engine: Option<&str>,
        partial: bool,
        language: Option<&str>,
    ) -> Result<Transcript, ApiError> {
        use base64::Engine as _;
        let v = self.config.read().voice.clone();
        let route = listen_route(v.engine(), engine).map_err(bad)?;
        // A saved language always wins. The device hint avoids Whisper's costly
        // language-detection pass without changing shared settings.
        let language = if v.language.is_empty() {
            language.unwrap_or("").trim()
        } else {
            &v.language
        };
        if language.len() > 16
            || !language
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(bad("Use a short speech language code such as en or en-US."));
        }
        let primary_language = language.split('-').next().unwrap_or("");
        // Paid services are asked once, for the final words.
        if partial && route != Route::Server {
            return Err(failure(Failure::Busy));
        }
        let audio = base64::engine::general_purpose::STANDARD
            .decode(audio_b64.trim())
            .map_err(|_| bad("audio is not valid base64"))?;
        if audio.is_empty() {
            return Err(bad("no audio"));
        }
        if audio.len() > 25 * 1024 * 1024 {
            return Err(bad("recording is too long (25 MB limit)"));
        }
        if v.keep_audio
            && !partial
            && let Err(e) = self.voice.keep(&audio, mime)
        {
            return Err(failure(Failure::Failed(format!(
                "Couldn't keep the voice recording: {e}"
            ))));
        }
        let text = match route {
            Route::Server => self
                .voice
                .transcribe(&audio, mime, &v.server_model, primary_language, partial)
                .await
                .map_err(failure)?,
            Route::OpenAi => {
                let (base, key) = self.openai().ok_or_else(|| {
                    bad("no OpenAI API key is saved (Settings → Privacy & safety → Passwords & keys)")
                })?;
                self.transcribe_http(
                    &base,
                    Some(&key),
                    "gpt-4o-mini-transcribe",
                    primary_language,
                    audio,
                    mime,
                )
                .await?
            }
            Route::OpenRouter => {
                let key = self.openrouter().ok_or_else(|| bad("No OpenRouter API key is saved for openrouter.ai. Check Passwords & keys or Models."))?;
                self.transcribe_http(
                    OPENROUTER_BASE,
                    Some(&key),
                    OPENROUTER_TRANSCRIBE,
                    primary_language,
                    audio,
                    mime,
                )
                .await?
            }
            Route::Service => {
                if !v.enabled {
                    return Err(bad(
                        "voice input is not set up; configure it in Settings → Web & voice",
                    ));
                }
                let key = match self.auth.get("voice") {
                    Some(Credential::ApiKey { key }) => Some(key),
                    _ => None,
                };
                self.transcribe_http(
                    &v.base_url,
                    key.as_deref(),
                    &v.model,
                    primary_language,
                    audio,
                    mime,
                )
                .await?
            }
        };
        Ok(Transcript {
            text,
            engine: route.name().into(),
        })
    }

    /// Speech to text through an OpenAI-compatible `/audio/transcriptions`.
    /// The key stays on the server; clients only send audio.
    async fn transcribe_http(
        &self,
        base_url: &str,
        key: Option<&str>,
        model: &str,
        language: &str,
        audio: Vec<u8>,
        mime: &str,
    ) -> Result<String, ApiError> {
        let base_url = safe_voice_url(base_url).map_err(bad)?;
        let kind = mime.split(';').next().unwrap_or("").trim();
        let ext = match kind {
            "audio/webm" => "webm",
            "audio/ogg" => "ogg",
            "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
            "audio/mpeg" | "audio/mp3" => "mp3",
            "audio/mp4" | "audio/m4a" | "audio/x-m4a" | "audio/aac" => "m4a",
            "audio/flac" => "flac",
            _ => "webm",
        };
        let part = reqwest::multipart::Part::bytes(audio)
            .file_name(format!("audio.{ext}"))
            .mime_str(if kind.is_empty() { "audio/webm" } else { kind })
            .map_err(internal)?;
        let mut form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", model.to_string())
            .text("response_format", "json");
        let lang = language.split(['-', '_']).next().unwrap_or("").trim();
        if !lang.is_empty() {
            form = form.text("language", lang.to_ascii_lowercase());
        }
        let client = speech_http(&base_url).await?;
        let mut rb = client
            .post(format!(
                "{}/audio/transcriptions",
                base_url.trim_end_matches('/')
            ))
            .multipart(form)
            .timeout(std::time::Duration::from_secs(120));
        if let Some(key) = key {
            rb = rb.bearer_auth(key);
        }
        let resp = rb.send().await.map_err(|e| {
            self.voice_provider_error(format!("transcription request failed: {e}"), key)
        })?;
        let status = resp.status();
        let json_type = content_type(&resp) == "application/json";
        let body = response_bytes(
            resp,
            if status.is_success() {
                TRANSCRIPT_BYTES
            } else {
                ERROR_BYTES
            },
        )
        .await
        .map_err(|error| self.voice_provider_error(error, key))?;
        let body = String::from_utf8_lossy(&body);
        if !status.is_success() {
            return Err(self.voice_http_error(status.as_u16(), &body, key));
        }
        let value = serde_json::from_str::<Value>(&body).ok();
        let text = value
            .as_ref()
            .filter(|v| v.get("error").is_none())
            .and_then(|v| v.get("text").and_then(Value::as_str));
        if !json_type || text.is_none() {
            return Err(self.voice_provider_error(
                "Speech service returned an invalid transcript (expected JSON text).",
                key,
            ));
        }
        Ok(self.scrub_voice(text.unwrap().trim(), key))
    }

    pub(super) async fn speak(
        &self,
        text: &str,
        engine: Option<&str>,
    ) -> Result<SpeechAudio, ApiError> {
        if text.chars().count() > MAX_SPEAK_CHARS {
            return Err(bad(format!(
                "speak at most {MAX_SPEAK_CHARS} characters at a time"
            )));
        }
        if text.trim().is_empty() {
            return Err(bad("nothing to say"));
        }
        let v = self.config.read().voice.clone();
        match speak_route(v.engine(), engine).map_err(bad)? {
            Route::OpenAi => {
                let (base, key) = self.openai().ok_or_else(|| {
                    bad("no OpenAI API key is saved (Settings → Privacy & safety → Passwords & keys)")
                })?;
                self.speak_http(&base, &key, "gpt-4o-mini-tts", "alloy", text)
                    .await
            }
            Route::OpenRouter => {
                let key = self.openrouter().ok_or_else(|| bad("No OpenRouter API key is saved for openrouter.ai. Check Passwords & keys or Models."))?;
                self.speak_http(
                    OPENROUTER_BASE,
                    &key,
                    OPENROUTER_SPEAK,
                    openrouter_voice(&v.openrouter_voice)?,
                    text,
                )
                .await
            }
            _ => {
                let voice_id = Voice::voice_for(&v.server_voice, &v.language);
                let wav = self.voice.speak(text, voice_id).await.map_err(failure)?;
                let ms = crate::voice::wav::info(&wav)
                    .map_or(0, |i| (i.seconds() * 1000.0).round() as u64);
                Ok(SpeechAudio {
                    audio: b64(&wav),
                    mime: "audio/wav".into(),
                    ms,
                })
            }
        }
    }

    async fn speak_http(
        &self,
        base: &str,
        key: &str,
        model: &str,
        voice: &str,
        text: &str,
    ) -> Result<SpeechAudio, ApiError> {
        let base = safe_voice_url(base).map_err(bad)?;
        let client = speech_http(&base).await?;
        let resp = client
            .post(format!("{base}/audio/speech"))
            .bearer_auth(key)
            .json(&serde_json::json!({
                "model": model,
                "voice": voice,
                "input": text,
                // MP3 works in Android WebView, unlike headerless provider PCM.
                "response_format": "mp3",
            }))
            .timeout(std::time::Duration::from_secs(90))
            .send()
            .await
            .map_err(|e| {
                self.voice_provider_error(format!("speech request failed: {e}"), Some(key))
            })?;
        let status = resp.status();
        let mime = content_type(&resp);
        let body = response_bytes(
            resp,
            if status.is_success() {
                SPEECH_BYTES
            } else {
                ERROR_BYTES
            },
        )
        .await
        .map_err(|error| self.voice_provider_error(error, Some(key)))?;
        if !status.is_success() {
            return Err(self.voice_http_error(
                status.as_u16(),
                &String::from_utf8_lossy(&body),
                Some(key),
            ));
        }
        let audio_shape = match mime.as_str() {
            "audio/mpeg" | "audio/mp3" => {
                body.starts_with(b"ID3")
                    || (body.len() > 1 && body[0] == 0xff && body[1] & 0xe0 == 0xe0)
            }
            "audio/wav" | "audio/x-wav" => crate::voice::wav::info(&body).is_some(),
            "audio/ogg" => body.starts_with(b"OggS"),
            "audio/flac" => body.starts_with(b"fLaC"),
            _ => false,
        };
        if !audio_shape || body.is_empty() || serde_json::from_slice::<Value>(&body).is_ok() {
            return Err(self.voice_provider_error(
                "Speech service returned invalid audio (expected an audio content type).",
                Some(key),
            ));
        }
        Ok(SpeechAudio {
            audio: b64(&body),
            mime,
            ms: 0,
        })
    }
}

#[cfg(test)]
#[path = "voice_openrouter_tests.rs"]
mod openrouter_tests;
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speech_http_only_allows_exact_local_ranges() {
        for base in [
            "http://127.0.0.2/v1",
            "http://10.1.2.3",
            "http://172.16.2.3",
            "http://192.168.1.2",
            "http://169.254.1.2",
            "http://[::1]",
            "http://[fc00::1]",
            "http://[fd00::1]",
            "http://[fe80::1]",
            "http://[::ffff:127.0.0.1]",
        ] {
            assert!(safe_voice_url(base).is_ok(), "{base}");
        }
        for base in [
            "http://speech.example/v1",
            "http://8.8.8.8",
            "http://172.32.0.1",
            "http://100.64.0.1",
            "http://192.0.2.1",
            "http://0.0.0.0",
            "http://[::]",
            "http://[2001:db8::1]",
            "http://[ff02::1]",
        ] {
            assert_eq!(safe_voice_url(base), Err(HTTPS_REQUIRED), "{base}");
        }
        assert!(safe_voice_url("https://speech.example/v1").is_ok());
        assert!(local_name("speech.local"));
        assert!(local_name("speech"));
        assert!(!local_name("speech.local.example"));
        assert!(
            local_addresses(&["127.0.0.1:80".parse().unwrap(), "[::1]:80".parse().unwrap()])
                .is_ok()
        );
        for addrs in [
            vec![],
            vec!["8.8.8.8:80".parse().unwrap()],
            vec![
                "127.0.0.1:80".parse().unwrap(),
                "8.8.8.8:80".parse().unwrap(),
            ],
        ] {
            assert_eq!(local_addresses(&addrs), Err(HTTPS_REQUIRED));
        }
    }

    #[tokio::test]
    async fn speech_local_names_resolve_at_admission() {
        assert!(speech_http("http://localhost/v1").await.is_ok());
    }

    #[tokio::test]
    async fn pinned_hostname_reaches_loopback_without_system_dns() {
        use axum::{Router, body::Bytes, http::HeaderMap, routing::post};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let host = format!("voice-pin-fixture.local:{}", addr.port());
        let expected = host.clone();
        let app = Router::new().route(
            "/audio",
            post(move |headers: HeaderMap, body: Bytes| {
                let expected = expected.clone();
                async move {
                    assert_eq!(headers["host"].to_str().unwrap(), expected);
                    assert_eq!(headers["authorization"], "Bearer fixture-pin-key");
                    assert_eq!(body.as_ref(), b"fixture private audio");
                    axum::Json(serde_json::json!({"text":"pinned connection works"}))
                }
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base = safe_voice_url(&format!("http://{host}")).unwrap();
        let url = url::Url::parse(&base).unwrap();
        // This name has no hosts entry or DNS setup. Exercise the exact client
        // builder used after production resolution, with checked loopback IPs.
        let client = speech_client(&url, &[addr]).unwrap();
        let response = client
            .post(format!("{base}/audio"))
            .bearer_auth("fixture-pin-key")
            .body("fixture private audio")
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        let body = response_bytes(response, TRANSCRIPT_BYTES).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["text"],
            "pinned connection works"
        );
        server.abort();
    }

    #[test]
    fn audio_goes_only_where_the_choice_allows() {
        // Private choices never reach a cloud service.
        for e in ["auto", "server"] {
            assert_eq!(listen_route(e, None), Ok(Route::Server));
            assert_eq!(listen_route(e, Some("server")), Ok(Route::Server));
            assert!(
                listen_route(e, Some("openai"))
                    .unwrap_err()
                    .contains("OpenAI")
            );
            assert!(listen_route(e, Some("service")).is_err());
            assert_eq!(speak_route(e, None), Ok(Route::Server));
            assert!(speak_route(e, Some("openai")).is_err());
        }
        // OpenAI when chosen; the server's own engine stays allowed.
        assert_eq!(listen_route("openai", None), Ok(Route::OpenAi));
        assert_eq!(listen_route("openai", Some("openai")), Ok(Route::OpenAi));
        assert_eq!(listen_route("openai", Some("server")), Ok(Route::Server));
        assert!(listen_route("openai", Some("service")).is_err());
        assert_eq!(speak_route("openai", None), Ok(Route::OpenAi));
        // The other service transcribes; speech comes from the server.
        assert_eq!(listen_route("service", None), Ok(Route::Service));
        assert!(listen_route("service", Some("openai")).is_err());
        assert_eq!(speak_route("service", None), Ok(Route::Server));
        // Off, or each device on its own: the server handles nothing.
        for e in ["off", "device"] {
            for asked in [None, Some("server"), Some("openai"), Some("service")] {
                assert!(listen_route(e, asked).is_err(), "{e} {asked:?}");
                assert!(speak_route(e, asked).is_err(), "{e} {asked:?}");
            }
        }
        assert!(
            listen_route("device", None)
                .unwrap_err()
                .contains("each device")
        );
    }
}
