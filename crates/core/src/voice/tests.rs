use super::*;

/// A tiny HTTP server answering every request with `body` (once per
/// connection), for download checks without the network.
async fn serve(body: Vec<u8>) -> String {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            let body = body.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf).await;
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(head.as_bytes()).await;
                let _ = s.write_all(&body).await;
            });
        }
    });
    format!("http://{addr}/file.bin")
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

#[tokio::test]
async fn downloads_are_used_only_when_size_and_fingerprint_match() {
    let dir = tempfile::tempdir().unwrap();
    let body = b"pretend this is a speech model".to_vec();
    let url = leak(serve(body.clone()).await);
    let http = reqwest::Client::new();
    let dest = dir.path().join("model.bin");
    let seen = std::sync::atomic::AtomicU64::new(0);

    let good = catalog::Download {
        url,
        sha256: leak(sha(&body)),
        size: body.len() as u64,
    };
    fetch(&http, &good, &dest, |n| {
        seen.store(n, std::sync::atomic::Ordering::Relaxed)
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), body);
    assert_eq!(
        seen.load(std::sync::atomic::Ordering::Relaxed),
        body.len() as u64
    );

    // Another file under the same name: refused, nothing left behind.
    std::fs::remove_file(&dest).unwrap();
    let other = catalog::Download {
        url,
        sha256: leak(sha(b"something else")),
        size: body.len() as u64,
    };
    let e = fetch(&http, &other, &dest, |_| {}).await.unwrap_err();
    assert!(e.contains("fingerprint"), "{e}");
    assert!(!dest.exists() && !suffixed(&dest, ".part").exists());

    // Larger than it should be: cut off.
    let small = catalog::Download {
        url,
        sha256: leak(sha(&body)),
        size: 4,
    };
    let e = fetch(&http, &small, &dest, |_| {}).await.unwrap_err();
    assert!(e.contains("larger"), "{e}");
    assert!(!dest.exists() && !suffixed(&dest, ".part").exists());
}

#[test]
fn no_speech_markers_are_dropped() {
    let out = " [BLANK_AUDIO]\n Hello there.\n (wind blowing)\n How are you?\n *music*\n";
    assert_eq!(clean_transcript(out), "Hello there. How are you?");
    assert_eq!(clean_transcript(" [Music]\n"), "");
    // A real sentence in brackets is kept when long.
    let long = " (and this part of the sentence was said in brackets on purpose)\n";
    assert_eq!(clean_transcript(long), long.trim());
}

#[test]
fn the_model_follows_the_choice_and_the_processor() {
    assert_eq!(Voice::model_for("tiny"), "tiny");
    assert_eq!(Voice::model_for("base"), "base");
    assert_eq!(
        Voice::model_for("auto"),
        if fast_cpu() { "base" } else { "tiny" }
    );
    assert_eq!(
        Voice::voice_for("de_DE-thorsten-medium", "en"),
        "de_DE-thorsten-medium"
    );
    assert_eq!(Voice::voice_for("", "nl"), "nl_NL-mls-medium");
    assert_eq!(Voice::voice_for("bogus", ""), "en_US-joe-medium");
}

#[test]
fn recordings_are_kept_privately_and_deleted_on_request() {
    let dir = tempfile::tempdir().unwrap();
    let v = Voice::new(dir.path());
    assert_eq!(v.kept(), (0, 0));
    let audio = wav::encode_pcm16(16_000, &[0; 160]);
    let p = v.keep(&audio, "audio/wav").unwrap();
    assert!(p.starts_with(dir.path().join("voice/recordings")));
    assert_eq!(p.extension().unwrap(), "wav");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    v.keep(b"webm", "audio/webm;codecs=opus").unwrap();
    assert_eq!(v.kept(), (2, audio.len() as u64 + 4));
    v.delete_kept().unwrap();
    assert_eq!(v.kept(), (0, 0));
    v.delete_kept().unwrap();
}

/// Fake engines in the places downloads go: whisper prints its arguments
/// (and what of the server's environment it sees); piper copies a sample.
#[cfg(unix)]
fn fake_engines(data: &Path) -> Arc<Voice> {
    use std::os::unix::fs::PermissionsExt;
    let v = Arc::new(Voice::new(data));
    let w = catalog::whisper().expect("a whisper build for this computer");
    let p = catalog::piper().expect("a piper build for this computer");
    let wdir = v.dir.join(w.dir).join("bin");
    let pdir = v.dir.join(p.dir).join("piper");
    std::fs::create_dir_all(&wdir).unwrap();
    std::fs::create_dir_all(&pdir).unwrap();
    let script = |path: PathBuf, body: &str| {
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    script(
        wdir.join(w.bin),
        "#!/bin/sh\necho ' [BLANK_AUDIO]'\necho \" heard: $* home=${HOME:-none}\"\n[ -f \"$4\" ] && echo ' file there'\nsleep \"${SLOW:-0}\"\n",
    );
    script(
        pdir.join(p.bin),
        "#!/bin/sh\nread line\nout=''\nwhile [ $# -gt 0 ]; do [ \"$1\" = --output_file ] && out=\"$2\"; shift; done\ncp \"$(dirname \"$0\")/sample.wav\" \"$out\"\necho \"$line\" > \"$(dirname \"$0\")/said.txt\"\n",
    );
    std::fs::write(
        pdir.join("sample.wav"),
        wav::encode_pcm16(22_050, &[100; 22_050]),
    )
    .unwrap();
    std::fs::create_dir_all(v.dir.join("models")).unwrap();
    std::fs::write(v.dir.join("models/ggml-tiny-q5_1.bin"), b"model").unwrap();
    std::fs::create_dir_all(v.dir.join("voices")).unwrap();
    std::fs::write(v.dir.join("voices/en_US-joe-medium.onnx"), b"voice").unwrap();
    std::fs::write(v.dir.join("voices/en_US-joe-medium.onnx.json"), b"{}").unwrap();
    v
}

#[cfg(all(
    unix,
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_os = "linux"
))]
#[tokio::test]
async fn the_server_engines_run_without_the_servers_environment_and_leave_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let v = Voice::new(dir.path());
    let s = v.status("tiny", "en_US-joe-medium");
    assert_eq!(s.state, "missing");
    assert!(!s.listen && !s.speak);
    let whisper = catalog::whisper().unwrap().archive.size;
    let piper = catalog::piper().unwrap().archive.size;
    let model = catalog::model("tiny").unwrap().1.size;
    let voice = catalog::voice("en_US-joe-medium").unwrap();
    assert_eq!(
        s.download_bytes,
        whisper + piper + model + voice.onnx.size + voice.json.size
    );
    let e = v
        .transcribe(b"x", "audio/wav", "tiny", "", false)
        .await
        .unwrap_err();
    assert!(matches!(e, Failure::NotReady(_)), "{e:?}");

    let v = fake_engines(dir.path());
    let s = v.status("tiny", "en_US-joe-medium");
    assert_eq!(
        (s.state.as_str(), s.listen, s.speak, s.download_bytes),
        ("ready", true, true, 0)
    );
    assert_eq!(s.model, "tiny");
    assert!(
        s.voices
            .iter()
            .any(|c| c.id == "en_US-joe-medium" && c.installed)
    );
    assert!(
        s.voices
            .iter()
            .any(|c| c.id == "nl_NL-mls-medium" && !c.installed)
    );
    // Another voice still needs its download.
    assert_eq!(v.status("tiny", "nl_NL-mls-medium").state, "missing");

    // 2 s of 16 kHz speech: a part of the model's window (2 × 50 + 128 → 384 at least).
    let audio = wav::encode_pcm16(16_000, &vec![300; 32_000]);
    let text = v
        .transcribe(&audio, "audio/wav", "auto", "nl-NL", false)
        .await
        .unwrap();
    assert!(!text.contains("BLANK_AUDIO"), "{text}");
    assert!(
        text.contains("-l nl ") && text.contains("-ac 384") && text.contains("-nf"),
        "{text}"
    );
    assert!(
        text.contains("home=none"),
        "the engine saw the server's environment: {text}"
    );
    assert!(text.contains("file there"), "{text}");
    let seen = text
        .split_whitespace()
        .skip_while(|w| *w != "-f")
        .nth(1)
        .unwrap();
    assert!(
        !Path::new(seen).exists(),
        "the recording was left behind: {seen}"
    );
    assert_eq!(std::fs::read_dir(v.dir.join("tmp")).unwrap().count(), 0);
    // A long one uses the whole window; no language hint detects it.
    let long = wav::encode_pcm16(16_000, &vec![300; 16_000 * 40]);
    let text = v
        .transcribe(&long, "audio/wav", "tiny", "", false)
        .await
        .unwrap();
    assert!(
        text.contains("-ac 1500") && text.contains("-l auto"),
        "{text}"
    );
    // Too short to hold a word.
    let blip = wav::encode_pcm16(16_000, &[0; 800]);
    assert_eq!(
        v.transcribe(&blip, "audio/wav", "tiny", "", false)
            .await
            .unwrap(),
        ""
    );

    let speech = v
        .speak("Hello\nthere.\u{7}  How are you?", "en_US-joe-medium")
        .await
        .unwrap();
    assert!(wav::info(&speech).is_some());
    let said = std::fs::read_to_string(
        v.dir
            .join(catalog::piper().unwrap().dir)
            .join("piper/said.txt"),
    )
    .unwrap();
    assert_eq!(
        said.trim(),
        "Hello there. How are you?",
        "one line for piper"
    );
    assert_eq!(std::fs::read_dir(v.dir.join("tmp")).unwrap().count(), 0);
    assert!(matches!(
        v.speak(" \n ", "en_US-joe-medium").await,
        Err(Failure::Bad(_))
    ));
    assert!(matches!(
        v.speak("Hoi", "nl_NL-mls-medium").await,
        Err(Failure::NotReady(_))
    ));

    // The check after a download speaks and listens once.
    v.check("tiny", "en_US-joe-medium").await.unwrap();
}

#[cfg(all(
    unix,
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[tokio::test]
async fn words_so_far_are_turned_away_while_the_engine_is_busy() {
    let dir = tempfile::tempdir().unwrap();
    let v = fake_engines(dir.path());
    let audio = wav::encode_pcm16(16_000, &vec![300; 16_000]);
    let held = v.listen.acquire().await.unwrap();
    assert_eq!(
        v.transcribe(&audio, "audio/wav", "tiny", "", true).await,
        Err(Failure::Busy)
    );
    // A final result waits its turn instead.
    let v2 = v.clone();
    let audio2 = audio.clone();
    let waiting =
        tokio::spawn(async move { v2.transcribe(&audio2, "audio/wav", "tiny", "", false).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!waiting.is_finished());
    drop(held);
    assert!(waiting.await.unwrap().unwrap().contains("heard:"));
}

#[cfg(all(
    unix,
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[tokio::test]
async fn an_engine_that_fails_says_why() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let v = fake_engines(dir.path());
    let w = catalog::whisper().unwrap();
    let bin = v.dir.join(w.dir).join("bin").join(w.bin);
    std::fs::write(
        &bin,
        "#!/bin/sh\necho 'illegal instruction' >&2\nexit 132\n",
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let audio = wav::encode_pcm16(16_000, &vec![300; 16_000]);
    match v.transcribe(&audio, "audio/wav", "tiny", "", false).await {
        Err(Failure::Failed(m)) => assert!(
            m.contains("whisper-cli") && m.contains("illegal instruction"),
            "{m}"
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read_dir(v.dir.join("tmp")).unwrap().count(), 0);
    let e = v.check("tiny", "en_US-joe-medium").await.unwrap_err();
    assert!(
        e.contains("speech recognition doesn't run on this computer"),
        "{e}"
    );
}

#[test]
fn programs_are_found_inside_their_release_folders() {
    let dir = tempfile::tempdir().unwrap();
    let deep = dir.path().join("Release/bin");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join("whisper-cli"), b"").unwrap();
    assert_eq!(
        find_bin(dir.path(), "whisper-cli", 3),
        Some(deep.join("whisper-cli"))
    );
    assert_eq!(find_bin(dir.path(), "whisper-cli", 1), None);
    assert_eq!(find_bin(dir.path(), "piper", 3), None);
    assert_eq!(
        suffixed(Path::new("/a/piper-2023.11.14-2"), ".unpacking"),
        Path::new("/a/piper-2023.11.14-2.unpacking")
    );
}

#[tokio::test]
async fn low_disk_install_refuses_before_network_or_partial_files() {
    let home = tempfile::tempdir().unwrap();
    let voice = Voice::new(home.path());
    let http = reqwest::Client::builder().build().unwrap();
    let checks = std::sync::atomic::AtomicUsize::new(0);
    let free = |_: &Path| {
        checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(space::RESERVE)
    };
    let err = voice
        .install_now_with_space(&http, "tiny", "en_US-lessac-medium", &free)
        .await
        .unwrap_err();
    assert!(err.contains("20 GB"), "{err}");
    assert_eq!(checks.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(!voice.dir().exists());
}

#[test]
fn download_admission_preserves_reserve_including_unpacking() {
    assert!(space::admit(space::RESERVE + 99, 100).is_err());
    assert!(space::admit(space::RESERVE + 100, 100).is_ok());
    assert!(space::admit(10 * 1024 * 1024 * 1024, 1).is_err());
    assert!(space::admit(u64::MAX, u64::MAX).is_err());
    assert!(space::available(tempfile::tempdir().unwrap().path()).is_ok());
}

#[tokio::test]
async fn reserve_lost_mid_download_removes_partial_file() {
    let url = leak(serve(vec![1, 2, 3]).await);
    let d = catalog::Download {
        url,
        sha256: "irrelevant",
        size: 3,
    };
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("model");
    let err = fetch_checked(
        &reqwest::Client::new(),
        &d,
        &dest,
        |_| {},
        |got| {
            space::admit(
                if got == 0 {
                    space::RESERVE + 3
                } else {
                    space::RESERVE - 1
                },
                3 - got,
            )
        },
    )
    .await
    .unwrap_err();
    assert!(err.contains("20 GB"));
    assert!(!dest.exists());
    assert!(!suffixed(&dest, ".part").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn blocked_stdin_obeys_one_deadline_and_kills_descendants() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("descendant");
    let args = vec![
        "-c".into(),
        format!("/bin/sleep 30 & echo $! > {}; wait", pidfile.display()),
    ];
    let start = Instant::now();
    let result = process::run(
        Path::new("/bin/sh"),
        &args,
        Some("x".repeat(1024 * 1024)),
        dir.path(),
        Limits::engine(Duration::from_millis(300)),
    )
    .await;
    assert!(result.unwrap_err().to_string().contains("longer than"));
    assert!(start.elapsed() < Duration::from_secs(3));
    let pid: u32 = std::fs::read_to_string(pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    for _ in 0..50 {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        if stat.is_empty() || stat.split(") ").nth(1).is_some_and(|s| s.starts_with('Z')) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("owned descendant {pid} still running");
}

#[cfg(unix)]
#[tokio::test]
async fn pipes_drain_while_stdin_is_written_and_floods_are_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let good = vec!["-c".into(), "printf hello; /bin/cat >/dev/null".into()];
    let out = process::run(
        Path::new("/bin/sh"),
        &good,
        Some("x".repeat(1024 * 1024)),
        dir.path(),
        Limits::engine(Duration::from_secs(5)),
    )
    .await
    .unwrap();
    assert_eq!(out, b"hello");
    for redirect in ["", " >&2"] {
        let args = vec!["-c".into(), format!("/bin/cat /dev/zero{redirect}")];
        let err = process::run(
            Path::new("/bin/sh"),
            &args,
            None,
            dir.path(),
            Limits::engine(Duration::from_secs(5)),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("larger than allowed"), "{err}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn fake_engine_memory_and_file_limits_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    if let Some(python) = which("python3") {
        let args = vec!["-c".into(), "data = bytearray(128 * 1024 * 1024)".into()];
        let mut limits = Limits::engine(Duration::from_secs(5));
        limits.memory = 64 * 1024 * 1024;
        let err = process::run(&python, &args, None, dir.path(), limits)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("MemoryError"), "{err}");
    }
    let args = vec!["-c".into(), "/bin/cat /dev/zero > oversized.wav".into()];
    let mut limits = Limits::engine(Duration::from_secs(5));
    limits.files = 64 * 1024;
    assert!(
        process::run(Path::new("/bin/sh"), &args, None, dir.path(), limits)
            .await
            .is_err()
    );
    assert!(
        std::fs::metadata(dir.path().join("oversized.wav"))
            .unwrap()
            .len()
            <= limits.files
    );
    std::fs::remove_file(dir.path().join("oversized.wav")).unwrap();
    let args = vec![
        "-c".into(),
        "for file in a b c; do /bin/dd if=/dev/zero of=$file bs=32768 count=1 2>/dev/null; done"
            .into(),
    ];
    let err = process::run(Path::new("/bin/sh"), &args, None, dir.path(), limits)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("files are larger"), "{err}");
}

#[cfg(unix)]
#[tokio::test]
async fn canceled_run_kills_its_owned_process_group() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let tmp = dir.path().to_owned();
    let args = vec![
        "-c".into(),
        format!("echo $$ > {}; /bin/sleep 30", pidfile.display()),
    ];
    let task = tokio::spawn(async move {
        process::run(
            Path::new("/bin/sh"),
            &args,
            None,
            &tmp,
            Limits::engine(Duration::from_secs(30)),
        )
        .await
    });
    for _ in 0..100 {
        if pidfile.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let pid: u32 = std::fs::read_to_string(pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    task.abort();
    let _ = task.await;
    for _ in 0..50 {
        if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("owned child {pid} still exists");
}

#[cfg(unix)]
#[tokio::test]
async fn overlong_input_is_rejected_without_running_whisper() {
    let home = tempfile::tempdir().unwrap();
    let voice = fake_engines(home.path());
    let bin = voice.whisper_bin().unwrap();
    let marker = home.path().join("invoked");
    std::fs::write(
        &bin,
        format!("#!/bin/sh\necho ran > {}\n", marker.display()),
    )
    .unwrap();
    let audio = wav::encode_pcm16(16_000, &vec![0; 16_000 * 121]);
    let err = voice
        .transcribe(&audio, "audio/wav", "tiny", "en", false)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("two minutes"));
    assert!(!marker.exists());
}

#[tokio::test]
async fn output_audio_size_and_duration_are_bounded_before_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.wav");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(AUDIO_BYTES + 1).unwrap();
    assert!(
        read_audio(&path)
            .await
            .unwrap_err()
            .to_string()
            .contains("16 MB")
    );
    std::fs::write(&path, wav::encode_pcm16(1, &[0; 121])).unwrap();
    assert!(
        read_audio(&path)
            .await
            .unwrap_err()
            .to_string()
            .contains("two minutes")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn engine_audio_cannot_block_reader_with_a_fifo() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.wav");
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: a fresh path in our throwaway directory, NUL-terminated.
    assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
    let result = tokio::time::timeout(Duration::from_millis(300), read_audio(&path))
        .await
        .unwrap();
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("regular audio file")
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn piper_output_obeys_the_audio_caps() {
    let home = tempfile::tempdir().unwrap();
    let voice = fake_engines(home.path());
    let sample = voice
        .piper_bin()
        .unwrap()
        .parent()
        .unwrap()
        .join("sample.wav");
    std::fs::write(&sample, wav::encode_pcm16(1, &[0; 121])).unwrap();
    let err = voice.speak("test", "en_US-joe-medium").await.unwrap_err();
    assert!(err.to_string().contains("two minutes"), "{err}");
    assert_eq!(std::fs::read_dir(voice.dir.join("tmp")).unwrap().count(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn reserve_lost_during_extraction_cleans_staging() {
    let home = tempfile::tempdir().unwrap();
    let source = home.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("fake-engine"), [0; 8192]).unwrap();
    let archive = home.path().join("engine.tar");
    assert!(
        std::process::Command::new(which("tar").unwrap())
            .args(["-cf"])
            .arg(&archive)
            .arg("-C")
            .arg(&source)
            .arg("fake-engine")
            .status()
            .unwrap()
            .success()
    );
    let into = home.path().join("installed");
    let err = unpack(&archive, &into, "fixture", &|used| {
        space::admit(
            if used == 0 {
                space::RESERVE + process::UNPACK_BYTES
            } else {
                space::RESERVE - 1
            },
            process::UNPACK_BYTES.saturating_sub(used),
        )
    })
    .await
    .unwrap_err();
    assert!(err.contains("20 GB"), "{err}");
    assert!(!into.exists());
    assert!(!suffixed(&into, ".unpacking").exists());
}

#[tokio::test]
async fn low_runtime_disk_rejects_scratch_audio_and_engine_before_side_effects() {
    let parent = tempfile::tempdir().unwrap();
    let low = |work| space::admit(space::RESERVE - 1, work);
    assert!(
        Scratch::new_checked(parent.path(), &low)
            .err()
            .unwrap()
            .contains("20 GB")
    );
    assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
    let scratch = Scratch::new_checked(parent.path(), &|_| Ok(())).unwrap();
    assert!(
        scratch
            .write_checked("in.wav", &[0; 10], &low)
            .unwrap_err()
            .contains("20 GB")
    );
    assert!(!scratch.dir.join("in.wav").exists());
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let bytes = vec![0; 128 * 1024];
    let error = scratch
        .write_checked("in.wav", &bytes, &|work| {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            space::admit(
                if n < 2 {
                    space::RESERVE + bytes.len() as u64
                } else {
                    space::RESERVE - 1
                },
                work,
            )
        })
        .unwrap_err();
    assert!(error.contains("20 GB"));
    assert!(!scratch.dir.join("in.wav").exists());
    // The admission callback precedes spawning, even for a nonexistent executable.
    let error = process::run_checked(
        Path::new("nonexistent-fixture"),
        &[],
        None,
        &scratch.dir,
        Limits::engine(Duration::from_secs(1)),
        &low,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("20 GB"), "{error}");
    let path = scratch.dir.clone();
    drop(scratch);
    assert!(!path.exists());
}
