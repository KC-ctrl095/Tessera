# Third-party notices

Tessera is licensed under the Apache License 2.0 ([LICENSE](LICENSE),
[NOTICE](NOTICE)). It is built on the work of many other projects. This page
says what Tessera includes from others and under which licences, and what it
downloads from other companies onto your computer instead of including it.

The licence texts and copyright notices themselves are in
[THIRD_PARTY_LICENSES.txt](THIRD_PARTY_LICENSES.txt). The release archives
and the desktop app's installed folder carry both files (with LICENSE and
NOTICE) next to the program; the apps show this page under Settings → Help &
tour → Open-source licences, and carry the full texts too (**The full licence
texts in this app** at its end), the Android app included.

## What Tessera's programs include

### Rust crates and JavaScript packages

The server, the terminal app, `tessera node` and the desktop and Android apps
are built from the Rust crates in `Cargo.lock`; the apps' interface from the
JavaScript packages in `apps/tessera/ui/package-lock.json`. The lists at the
end group them by licence.

- Almost all of them are under permissive licences (MIT, Apache-2.0, BSD,
  ISC, Zlib, Unicode, CDLA-Permissive-2.0, CC0).
- Five crates are under the **Mozilla Public License 2.0**, a licence that
  applies file by file: `cssparser`, `cssparser-macros`, `dtoa-short`,
  `option-ext` and `selectors` (`option-ext` comes with `dirs`, in every
  program; the others only in the desktop and Android apps, through Tauri).
  Tessera uses them unchanged. Their
  source code is available from [crates.io](https://crates.io) and from the
  repositories named in THIRD_PARTY_LICENSES.txt. If Tessera ever changes one
  of their files, that file's changes must be published under the MPL too.
- `dompurify` (the apps' interface) is offered under MPL-2.0 or Apache-2.0;
  Tessera uses it under Apache-2.0.
- Where a package offers a choice of licences, Tessera uses it under the one
  named after "used under".
- The lists hold every crate in `Cargo.lock`: some are used only on one
  platform (Windows, macOS, Android), and some only to build or test Tessera.

Regenerate the lists and THIRD_PARTY_LICENSES.txt after changing a lockfile:
`node scripts/licenses.mjs` (CI runs it with `--check`).

### wry, changed for security (`vendor/wry`)

wry 0.57.0, by the Tauri Programme within The Commons Conservancy, under
MIT or Apache-2.0 (Tessera uses it under MIT; both licence files are in
`vendor/wry`). Tessera keeps five small changes to it that stop untrusted
whiteboard content from reaching the app's commands; `vendor/README.md`
describes each, and the code marks them `TESSERA`.

Three of wry's Android files (`RustWebChromeClient.kt`, `Logger.kt`,
`PermissionHelper.kt`) are based on Capacitor, Copyright 2015-present Drifty
Co., under the MIT License (`vendor/wry/LICENSE-CAPACITOR`).

### Whiteboard libraries (`assets/board-lib`)

The server offers these to whiteboards, unchanged:

- **three.js** r170 with four of its add-ons (OrbitControls,
  TrackballControls, CSS2DRenderer, CSS3DRenderer): MIT, Copyright © 2010–2024
  three.js authors.
- **d3** 7.9.0: ISC, Copyright 2010–2023 Mike Bostock.

### Fonts (`apps/tessera/ui/src/assets/fonts`)

Under the SIL Open Font License 1.1, with their licence files next to them:

- **Geologica**, Copyright 2020 The Geologica Project Authors.
- **Marcellus**, Copyright 2012 Brian J. Bonislawsky DBA Astigmatic (AOETI),
  with Reserved Font Name "Marcellus".
- **Martian Mono**, Copyright 2020 The Martian Mono Project Authors.

The files are the Latin and Latin Extended web subsets as published by
Fontsource from Google Fonts.

### The Android app's Android libraries

The Android app is also built from Android libraries that Gradle fetches when
it is built (they are not in `Cargo.lock`):

| Library | Licence |
|---|---|
| AndroidX (core, appcompat, activity, lifecycle, webkit, CameraX) | Apache-2.0 |
| Material Components for Android | Apache-2.0 |
| Jackson (databind, datatype-json-org) | Apache-2.0 |
| Kotlin standard library | Apache-2.0 |
| **Google ML Kit barcode scanning** (`com.google.android.gms:play-services-mlkit-barcode-scanning`, through Tauri's barcode scanner plugin) | **Not open source**: Google's [ML Kit terms](https://developers.google.com/ml-kit/terms) and the Google APIs Terms of Service |

ML Kit reads the QR code when you pair a phone by scanning. It runs through
Google Play services on the phone, and Google says it collects device and
usage diagnostics for that feature ([PRIVACY.md](PRIVACY.md) has the
details). Pasting a pairing link instead doesn't use it.

### Tessera's own artwork

The icons, the logo, the mosaic avatars and the app icons are Tessera's own:
drawn in code and SVG in this repository, under the same Apache-2.0 licence
as the rest. The name and the logo are also the project's marks; see
[TRADEMARKS.md](TRADEMARKS.md).

## What Tessera downloads instead of including

These programs are not part of Tessera and Tessera does not hand them out.
When you choose a feature that needs one, it is fetched on your computer
directly from its maker, under the maker's own licence and terms:

| Program | When | From | Licence and terms |
|---|---|---|---|
| **cloudflared** (Cloudflare's tunnel program) | When you set up remote access through Cloudflare | Cloudflare's releases on GitHub, checked against the checksum Cloudflare publishes | Apache-2.0; using Cloudflare's service is under [Cloudflare's terms](https://www.cloudflare.com/terms/) |
| **Google Chrome for Testing** (the browser for web views on Linux) | When you accept it in the Linux installer | Google's download servers | Google's proprietary licence ("Copyright Google LLC. All rights reserved"; its terms are at `chrome://terms` inside it); it includes open-source parts listed at `chrome://credits` |
| **Rust** (through rustup) | Only when Tessera is built from source and Rust is missing | rustup.rs | MIT or Apache-2.0 |
| **whisper.cpp** and OpenAI's Whisper model (speech to text) | When you choose voice **On my server** | ggml-org's releases on GitHub and the model files on Hugging Face, checked against pinned checksums | MIT |
| **Piper** (text to speech), which includes eSpeak NG and ONNX Runtime | When you choose voice **On my server** | rhasspy's releases on GitHub, checked against pinned checksums | Piper: MIT; the eSpeak NG it links: GPL-3.0-or-later; ONNX Runtime: MIT |
| **Piper voices** | With Piper, the voice you pick | Hugging Face (`rhasspy/piper-voices`), checked against pinned checksums | Per voice, as its model card says (CC0 or CC BY 4.0 for the voices Tessera offers) |
| The apps' JavaScript packages | Only when building from source (`npm ci`) | The npm registry | As listed below |

Other programs Tessera works with are yours to install, under their own
terms: **Claude Code** (Anthropic's commercial terms; Tessera runs your own,
unmodified copy), web browsers such as Chrome, Chromium or Microsoft Edge,
WebKitGTK and WebView2 (the operating system's web engines the desktop app
uses), Tailscale, language servers, MCP servers, and model servers such as
llama.cpp, Ollama or LM Studio.

## The lists

<!-- BEGIN GENERATED by scripts/licenses.mjs: don't edit by hand -->

**Rust crates** (619):

- **MIT OR Apache-2.0** (used under MIT), 248: aead, allocator-api2, android_system_properties, anstream, anstyle, anstyle-parse, anstyle-query, anstyle-wincon, anyhow, async-broadcast, async-recursion, async-trait, backtrace, base64, bitflags, blake2, block-buffer, bstr, bumpalo, camino, cargo-platform, cc, cfg-expr, cfg-if, chacha20, chrono, cipher, clap, clap_builder, clap_derive, clap_lex, colorchoice, cookie, core-foundation, core-foundation-sys, core-graphics, core-graphics-types, cpufeatures, crc32fast, crossbeam-channel, crossbeam-deque, crossbeam-epoch, crossbeam-utils, crypto-common, defmt, defmt-macros, defmt-parser, deranged, digest, dirs, dirs-sys, displaydoc, document-features, dtoa, dyn-clone, either, embed_plist, enumflags2, enumflags2_derive, erased-serde, errno, fdeflate, field-offset, find-msvc-tools, flate2, form_urlencoded, futures, futures-channel, futures-core, futures-executor, futures-io, futures-macro, futures-sink, futures-task, futures-util, getrandom, gimli, glob, hashbrown, hashlink, heck, hermit-abi, hex, hkdf, hmac, html5ever, http, httparse, httpdate, iana-time-zone, iana-time-zone-haiku, idna, indoc, inout, ipnet, is_terminal_polyfill, itertools, itoa, jni-sys, jni-sys-macros, jpeg-decoder, js-sys, jsonptr, keyboard-types, lazy_static, libc, litrs, lock_api, log, markup5ever, mime, ndk, ndk-context, ndk-sys, notify-rust, num-conv, num-traits, num_threads, once_cell, once_cell_polyfill, opaque-debug, ordered-stream, parking_lot, parking_lot_core, paste, percent-encoding, petgraph, piper, pkg-config, png, powerfmt, ppv-lite86, proc-macro-crate, proc-macro-error, proc-macro-error-attr, proc-macro2, qrcode, quinn, quinn-proto, quinn-udp, quote, rand, rand_chacha, rand_core, rand_pcg, rayon, rayon-core, ref-cast, ref-cast-impl, regex, regex-automata, regex-syntax, reqwest, rustc_version, rustls-pki-types, rustversion, saphyr-parser, scopeguard, semver, serde, serde-untagged, serde_core, serde_derive, serde_derive_internals, serde_json, serde_path_to_error, serde_repr, serde_spanned, serde_with, serde_with_macros, serialize-to-javascript, serialize-to-javascript-impl, servo_arc, sha1, sha2, shlex, signal-hook-mio, signal-hook-registry, siphasher, smallvec, socket2, softbuffer, spake2, stable_deref_trait, static_assertions, string_cache, string_cache_codegen, swift-rs, syn, system-deps, tao-macros, tauri-winrt-notification, tempfile, tendril, thiserror, thiserror-impl, thread_local, time, time-core, time-macros, tokio-rustls, toml, toml_datetime, toml_edit, toml_parser, toml_writer, tray-icon, tungstenite, typeid, typenum, unicase, unicode-segmentation, unicode-truncate, unicode-width, universal-hash, url, utf-8, wasm-bindgen, wasm-bindgen-futures, wasm-bindgen-macro, wasm-bindgen-macro-support, wasm-bindgen-shared, wasm-streams, web-sys, web-time, web_atoms, windows, windows-collections, windows-core, windows-future, windows-implement, windows-interface, windows-link, windows-numerics, windows-result, windows-strings, windows-sys, windows-targets, windows-threading, windows-version, windows_aarch64_gnullvm, windows_aarch64_msvc, windows_i686_gnu, windows_i686_gnullvm, windows_i686_msvc, windows_x86_64_gnu, windows_x86_64_gnullvm, windows_x86_64_msvc
- **MIT**, 135: atk, atk-sys, axum, axum-core, block2, bytes, cairo-rs, cairo-sys-rs, cargo_metadata, castaway, cfb, cfg_aliases, combine, compact_str, convert_case, crossterm, crossterm_winapi, darling, darling_core, darling_macro, data-encoding, derive_more, derive_more-impl, dlopen2, dlopen2_derive, dom_query, embed-resource, endi, gdk, gdk-pixbuf, gdk-pixbuf-sys, gdk-sys, gdkwayland-sys, gdkx11, gdkx11-sys, generic-array, gio, gio-sys, glib, glib-macros, glib-sys, gobject-sys, gtk, gtk-sys, gtk3-macros, h2, http-body, http-body-util, hyper, hyper-util, ico, infer, instability, is-docker, is-wsl, javascriptcore-rs, javascriptcore-rs-sys, libredox, libsqlite3-sys, lru, matchers, memoffset, mime_guess, mio, new_debug_unreachable, nu-ansi-term, objc2, objc2-encode, objc2-foundation, open, pango, pango-sys, phf, phf_codegen, phf_generator, phf_macros, phf_shared, plist, precomputed-hash, pulldown-cmark, quick-xml, ratatui, redox_syscall, redox_users, rusqlite, schemars, schemars_derive, sharded-slab, simd-adler32, slab, soup3, soup3-sys, strsim, strum, strum_macros, synstructure, tauri-winres, tokio, tokio-macros, tokio-tungstenite, tokio-util, tower, tower-http, tower-layer, tower-service, tracing, tracing-attributes, tracing-core, tracing-log, tracing-subscriber, try-lock, uds_windows, urlpattern, valuable, version-compare, vswhom, vswhom-sys, want, webkit2gtk, webkit2gtk-sys, webview2-com, webview2-com-macros, webview2-com-sys, winnow, winreg, x11, x11-dl, zbus, zbus_macros, zbus_names, zcheapstr, zmij, zvariant, zvariant_derive, zvariant_utils
- **Apache-2.0 OR MIT** (used under MIT), 53: addr2line, async-channel, async-executor, async-io, async-lock, async-process, async-signal, async-task, atomic-waker, autocfg, bit-set, bit-vec, blocking, cargo_toml, chacha20, chacha20poly1305, concurrent-queue, ctor, equivalent, event-listener, event-listener-strategy, fastrand, futures-lite, idna_adapter, indexmap, libappindicator, libappindicator-sys, muda, object, parking, pin-project-lite, polling, poly1305, portable-atomic, portable-atomic-util, rustc-hash, snow, tauri, tauri-build, tauri-codegen, tauri-macros, tauri-plugin, tauri-plugin-barcode-scanner, tauri-plugin-notification, tauri-plugin-opener, tauri-runtime, tauri-runtime-wry, tauri-utils, utf8_iter, utf8parse, uuid, window-vibrancy, zeroize
- **MIT/Apache-2.0** (used under MIT), 21: arraydeque, bitflags, bs58, curve25519-dalek-derive, fallible-iterator, fallible-streaming-iterator, fixedbitset, foreign-types, foreign-types-macros, foreign-types-shared, ident_case, jni, json-patch, mac-notification-sys, rustc-demangle, serde_urlencoded, vcpkg, version_check, winapi, winapi-i686-pc-windows-gnu, winapi-x86_64-pc-windows-gnu
- **Unicode-3.0**, 18: icu_collections, icu_locale_core, icu_normalizer, icu_normalizer_data, icu_properties, icu_properties_data, icu_provider, litemap, potential_utf, tinystr, writeable, yoke, yoke-derive, zerofrom, zerofrom-derive, zerotrie, zerovec, zerovec-derive
- **Zlib OR Apache-2.0 OR MIT** (used under MIT), 17: bytemuck, dispatch2, objc2-app-kit, objc2-cloud-kit, objc2-core-data, objc2-core-foundation, objc2-core-graphics, objc2-core-image, objc2-core-location, objc2-core-text, objc2-exception-helper, objc2-io-surface, objc2-quartz-core, objc2-ui-kit, objc2-user-notifications, objc2-web-kit, tinyvec
- **Unlicense OR MIT** (used under MIT), 11: aho-corasick, byteorder, globset, ignore, jiff, jiff-core, jiff-static, jiff-tzdb, jiff-tzdb-platform, memchr, winapi-util
- **Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT** (used under MIT), 5: linux-raw-sys, rustix, wasi, wasip2, wit-bindgen
- **MPL-2.0**, 5: cssparser, cssparser-macros, dtoa-short, option-ext, selectors
- **Apache-2.0/MIT** (used under MIT), 4: cesu8, dbus, libdbus-sys, signal-hook
- **BSD-3-Clause**, 4: alloc-no-stdlib, alloc-stdlib, curve25519-dalek, subtle
- **Apache-2.0**, 3: similar, sync_wrapper, tao
- **ISC**, 3: libloading, rustls-webpki, untrusted
- **Apache-2.0 OR ISC OR MIT** (used under MIT), 2: hyper-rustls, rustls
- **BSD-2-Clause OR Apache-2.0 OR MIT** (used under MIT), 2: zerocopy, zerocopy-derive
- **BSD-3-Clause OR MIT OR Apache-2.0** (used under MIT), 2: num_enum, num_enum_derive
- **MIT OR Apache-2.0 OR Zlib** (used under MIT), 2: lru-slab, raw-window-handle
- **Unlicense/MIT** (used under MIT), 2: same-file, walkdir
- **Zlib**, 2: foldhash, zlib-rs
- **(MIT OR Apache-2.0) AND Unicode-3.0** (used under MIT AND Unicode-3.0): unicode-ident
- **0BSD OR MIT OR Apache-2.0** (used under MIT): adler2
- **Apache-2.0 / MIT** (used under MIT): fnv
- **Apache-2.0 AND ISC**: ring
- **Apache-2.0 AND MIT**: dpi
- **Apache-2.0 OR BSL-1.0** (used under Apache-2.0): ryu
- **Apache-2.0 WITH LLVM-exception**: target-lexicon
- **BSD-3-Clause AND MIT**: brotli
- **BSD-3-Clause/MIT** (used under BSD-3-Clause): brotli-decompressor
- **CC0-1.0 OR MIT-0 OR Apache-2.0** (used under CC0-1.0): dunce
- **CDLA-Permissive-2.0**: webpki-roots
- **MIT / Apache-2.0** (used under MIT): cassowary
- **MIT AND BSD-3-Clause**: matchit
- **MIT OR Apache-2.0 OR BSD-1-Clause** (used under MIT): fiat-crypto
- **MIT OR Apache-2.0 OR LGPL-2.1-or-later** (used under MIT): r-efi
- **MIT OR Zlib OR Apache-2.0** (used under MIT): miniz_oxide

**Rust crates kept in this repository** (1):

- **Apache-2.0 OR MIT** (used under MIT): wry

**JavaScript packages in the apps' interface** (41):

- **MIT**, 37: @codemirror/autocomplete, @codemirror/commands, @codemirror/lang-css, @codemirror/lang-html, @codemirror/lang-javascript, @codemirror/lang-markdown, @codemirror/language, @codemirror/lint, @codemirror/search, @codemirror/state, @codemirror/view, @lezer/common, @lezer/css, @lezer/highlight, @lezer/html, @lezer/javascript, @lezer/lr, @lezer/markdown, @marijn/find-cluster-break, @noble/ciphers, @noble/curves, @noble/hashes, @types/trusted-types, @xterm/addon-fit, @xterm/addon-search, @xterm/addon-unicode11, @xterm/addon-web-links, @xterm/xterm, crelt, csstype, marked, seroval, seroval-plugins, solid-js, style-mod, uqr, w3c-keyname
- **Apache-2.0 OR MIT** (used under MIT): @tauri-apps/api
- **BSD-3-Clause**: highlight.js
- **MIT OR Apache-2.0** (used under MIT): @tauri-apps/plugin-barcode-scanner
- **MPL-2.0 OR Apache-2.0** (used under Apache-2.0): dompurify

**Libraries and fonts kept in this repository** (6):

- **OFL-1.1**, 3: Geologica (font), Marcellus (font), Martian Mono (font)
- **MIT**, 2: Capacitor (parts of wry's Android code), three.js
- **ISC**: d3

<!-- END GENERATED -->
