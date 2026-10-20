//! What the server's speech engines are made of: official releases, each
//! pinned to its size and SHA-256 (checked before anything is unpacked or
//! run). To move to a newer release, download it, compare its fingerprint
//! with the one the project publishes, and change the entry here.

/// One file to download.
#[derive(Debug, Clone, Copy)]
pub struct Download {
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

/// A program in an archive: unpacked into `voice/<dir>`, then `bin` is
/// looked for anywhere inside it (releases differ in their layout).
#[derive(Debug, Clone, Copy)]
pub struct Program {
    pub dir: &'static str,
    pub bin: &'static str,
    pub archive: Download,
}

/// whisper.cpp (MIT), speech to text. Its CPU builds pick the best code for
/// the processor at start (SSE4.2 machines without AVX included).
pub fn whisper() -> Option<Program> {
    const DIR: &str = "whisper-b5454";
    let (bin, archive) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => (
            "whisper-cli",
            Download {
                url: "https://github.com/ggml-org/whisper.cpp/releases/download/b5454/whisper-bin-ubuntu-x64.tar.gz",
                sha256: "a72becf15d7917f990f6313867a52638b82b7f9ef237fb0c980dac56a135781c",
                size: 10_364_195,
            },
        ),
        ("linux", "aarch64") => (
            "whisper-cli",
            Download {
                url: "https://github.com/ggml-org/whisper.cpp/releases/download/b5454/whisper-bin-ubuntu-arm64.tar.gz",
                sha256: "6b95ebfc60447df48e70ef00a73bdc3f41679ed2d01ef465827206a2ff325149",
                size: 4_608_377,
            },
        ),
        ("windows", "x86_64") => (
            "whisper-cli.exe",
            Download {
                url: "https://github.com/ggml-org/whisper.cpp/releases/download/b5454/whisper-bin-x64.zip",
                sha256: "6ba69e3482d7826214f90a6a9c84ca07782aec1e1d0c6a7c30c994fd5d816ccb",
                size: 8_928_640,
            },
        ),
        ("windows", "aarch64") => (
            "whisper-cli.exe",
            Download {
                url: "https://github.com/ggml-org/whisper.cpp/releases/download/b5454/whisper-bin-win-cpu-arm64.zip",
                sha256: "28c37e7b598c3d9bbfef94f3bd67f2ee6f12b7c86f3da5edcf5e4308d615ff6d",
                size: 4_371_193,
            },
        ),
        // macOS: no ready-made program is published; a `whisper-cli` the user
        // installed (Homebrew's whisper-cpp) is used instead.
        _ => return None,
    };
    Some(Program {
        dir: DIR,
        bin,
        archive,
    })
}

/// Piper (MIT), text to speech; it brings its own ONNX runtime and
/// espeak-ng data.
pub fn piper() -> Option<Program> {
    const DIR: &str = "piper-2023.11.14-2";
    let (bin, archive) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => (
            "piper",
            Download {
                url: "https://github.com/rhasspy/piper/releases/download/2023.11.14-2/piper_linux_x86_64.tar.gz",
                sha256: "a50cb45f355b7af1f6d758c1b360717877ba0a398cc8cbe6d2a7a3a26e225992",
                size: 26_460_462,
            },
        ),
        ("linux", "aarch64") => (
            "piper",
            Download {
                url: "https://github.com/rhasspy/piper/releases/download/2023.11.14-2/piper_linux_aarch64.tar.gz",
                sha256: "fea0fd2d87c54dbc7078d0f878289f404bd4d6eea6e7444a77835d1537ab88eb",
                size: 26_004_717,
            },
        ),
        ("windows", "x86_64") => (
            "piper.exe",
            Download {
                url: "https://github.com/rhasspy/piper/releases/download/2023.11.14-2/piper_windows_amd64.zip",
                sha256: "f3c58906402b24f3a96d92145f58acba6d86c9b5db896d207f78dc80811efcea",
                size: 22_477_236,
            },
        ),
        ("macos", "x86_64") => (
            "piper",
            Download {
                url: "https://github.com/rhasspy/piper/releases/download/2023.11.14-2/piper_macos_x64.tar.gz",
                sha256: "ced85c0a3df13945b1e623b878a48fdc2854d5c485b4b67f62857cf551deaf8b",
                size: 19_146_927,
            },
        ),
        ("macos", "aarch64") => (
            "piper",
            Download {
                url: "https://github.com/rhasspy/piper/releases/download/2023.11.14-2/piper_macos_aarch64.tar.gz",
                sha256: "6b1eb03b3735946cb35216e063e7eebcc33a6bbf5dd96ec0217959bf1cdcb0cc",
                size: 19_146_957,
            },
        ),
        _ => return None,
    };
    Some(Program {
        dir: DIR,
        bin,
        archive,
    })
}

/// Whisper speech models (MIT), quantized for small computers.
pub fn model(id: &str) -> Option<(&'static str, Download)> {
    match id {
        "tiny" => Some((
            "ggml-tiny-q5_1.bin",
            Download {
                url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny-q5_1.bin",
                sha256: "818710568da3ca15689e31a743197b520007872ff9576237bda97bd1b469c3d7",
                size: 32_152_673,
            },
        )),
        "base" => Some((
            "ggml-base-q5_1.bin",
            Download {
                url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base-q5_1.bin",
                sha256: "422f1ae452ade6f30a004d7e5c6a43195e4433bc370bf23fac9cc591f01a8898",
                size: 59_707_625,
            },
        )),
        _ => None,
    }
}

/// A Piper voice: its model and its settings file. Only voices whose
/// recordings allow any use (CC0 or CC BY 4.0) are offered.
#[derive(Debug, Clone, Copy)]
pub struct VoiceDef {
    pub id: &'static str,
    pub label: &'static str,
    pub language: &'static str,
    pub onnx: Download,
    pub json: Download,
}

macro_rules! voice {
    ($id:literal, $label:literal, $lang:literal, $path:literal,
     $onnx_sha:literal, $onnx_size:literal, $json_sha:literal, $json_size:literal) => {
        VoiceDef {
            id: $id,
            label: $label,
            language: $lang,
            onnx: Download {
                url: concat!(
                    "https://huggingface.co/rhasspy/piper-voices/resolve/v1.0.0/",
                    $path,
                    "/",
                    $id,
                    ".onnx"
                ),
                sha256: $onnx_sha,
                size: $onnx_size,
            },
            json: Download {
                url: concat!(
                    "https://huggingface.co/rhasspy/piper-voices/resolve/v1.0.0/",
                    $path,
                    "/",
                    $id,
                    ".onnx.json"
                ),
                sha256: $json_sha,
                size: $json_size,
            },
        }
    };
}

pub const VOICES: &[VoiceDef] = &[
    // CC0 (NabuCasa voice datasets).
    voice!(
        "en_US-joe-medium",
        "English (US), Joe",
        "en",
        "en/en_US/joe/medium",
        "58afce0321b8d9c46d7cdf9c16500cc55a793b4220212dba6b70fb788b3baf06",
        63_201_294,
        "3d6d5410b3795cb1950595247ef8f06190719e6fdbfa3a2356d8ec368e1aad33",
        4_794
    ),
    // CC BY 4.0 (University of Edinburgh).
    voice!(
        "en_GB-alba-medium",
        "English (UK), Alba",
        "en",
        "en/en_GB/alba/medium",
        "401369c4a81d09fdd86c32c5c864440811dbdcc66466cde2d64f7133a66ad03b",
        63_201_294,
        "aa965a2f02ecced632c2694e1fc72bbff6d65f265fab567ca945918c73dd89f4",
        4_888
    ),
    // CC BY 4.0 (Multilingual LibriSpeech).
    voice!(
        "nl_NL-mls-medium",
        "Nederlands",
        "nl",
        "nl/nl_NL/mls/medium",
        "88312e0fbf505b87caf2373d94c1384892e86b1bf2ee482cf65dc8ba179cc7d3",
        76_584_246,
        "6ddb215d38f1392ab935ad45441b82ada1eeae0452a2d6849ed71ea4f2e0aa63",
        5_856
    ),
    // CC0 (Thorsten-Voice).
    voice!(
        "de_DE-thorsten-medium",
        "Deutsch, Thorsten",
        "de",
        "de/de_DE/thorsten/medium",
        "7e64762d8e5118bb578f2eea6207e1a35a8e0c30595010b666f983fc87bb7819",
        63_201_294,
        "974adee790533adb273a1ac88f49027d2a1b8f0f2cf4905954a4791e79264e85",
        4_819
    ),
    // CC BY 4.0 (SIWIS).
    voice!(
        "fr_FR-siwis-medium",
        "Français, Siwis",
        "fr",
        "fr/fr_FR/siwis/medium",
        "641d1ab097da2b81128c076810edb052b385decc8be3381814802a64a73baf99",
        63_201_294,
        "39479916c2db192b5ac9764daddd0c744d83e023ad890c6976c0633ae4df8959",
        4_875
    ),
    // CC0 (NabuCasa voice datasets).
    voice!(
        "es_ES-davefx-medium",
        "Español, Davefx",
        "es",
        "es/es_ES/davefx/medium",
        "6658b03b1a6c316ee4c265a9896abc1393353c2d9e1bca7d66c2c442e222a917",
        63_201_294,
        "0e0dda87c732f6f38771ff274a6380d9252f327dca77aa2963d5fbdf9ec54842",
        4_817
    ),
];

pub fn voice(id: &str) -> Option<&'static VoiceDef> {
    VOICES.iter().find(|v| v.id == id)
}

/// The voice for a language hint ("nl", "en-GB"…); English otherwise.
pub fn voice_for(language: &str) -> &'static VoiceDef {
    let lang = language.trim().to_ascii_lowercase();
    let short = lang.split(['-', '_']).next().unwrap_or("");
    if lang == "en-gb" || lang == "en_gb" {
        return &VOICES[1];
    }
    VOICES
        .iter()
        .find(|v| v.language == short)
        .unwrap_or(&VOICES[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    const VOICES_URL: &str = "https://huggingface.co/rhasspy/piper-voices/resolve/v1.0.0/";

    #[test]
    fn every_pin_is_a_full_sha256_over_https() {
        let mut all: Vec<Download> = VOICES.iter().flat_map(|v| [v.onnx, v.json]).collect();
        all.extend(
            ["tiny", "base"]
                .iter()
                .filter_map(|m| model(m))
                .map(|m| m.1),
        );
        all.extend(whisper().map(|p| p.archive));
        all.extend(piper().map(|p| p.archive));
        for d in all {
            assert!(
                d.url.starts_with("https://github.com/")
                    || d.url.starts_with(VOICES_URL)
                    || d.url.starts_with("https://huggingface.co/ggerganov/"),
                "{}",
                d.url
            );
            assert_eq!(d.sha256.len(), 64, "{}", d.url);
            assert!(
                d.sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
                "{}",
                d.url
            );
            assert!(d.size > 1_000, "{}", d.url);
        }
    }

    #[test]
    fn a_language_picks_its_voice_and_english_is_the_fallback() {
        assert_eq!(voice_for("nl").id, "nl_NL-mls-medium");
        assert_eq!(voice_for("de-AT").id, "de_DE-thorsten-medium");
        assert_eq!(voice_for("en-GB").id, "en_GB-alba-medium");
        assert_eq!(voice_for("").id, "en_US-joe-medium");
        assert_eq!(voice_for("ja").id, "en_US-joe-medium");
        assert!(voice("fr_FR-siwis-medium").is_some());
        assert!(voice("../../etc/passwd").is_none());
    }
}
