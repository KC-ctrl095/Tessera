//! End-to-end encryption between Tessera apps and their server.
//!
//! This layer sits *above* TLS, so anything that terminates TLS on the way (a
//! Cloudflare tunnel, a reverse proxy, a corporate inspection box) relays
//! only ciphertext. It has two parts:
//!
//! * **Pairing** ([`pairing`]): SPAKE2 keyed by the pairing code. Both sides
//!   end up with the same key only if they used the same code. An attacker in
//!   the middle gets one online guess per attempt, and the server burns a code
//!   after a few failures. The server's long-term public key and the device
//!   token travel under that key, so a proxy can neither read nor swap them.
//! * **Sessions** ([`Initiator`], [`accept`]): the Noise `NK` handshake
//!   (X25519, ChaCha20-Poly1305, BLAKE2s) against the key learned while
//!   pairing, which gives forward secrecy. Clients send their device token as
//!   the first transport message, after the handshake has completed.
//!
//! The wire format is in `docs/PROTOCOL.md` → "End-to-end encryption".

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use sha2::{Digest, Sha256};
use snow::{Builder, HandshakeState, StatelessTransportState};
use zeroize::Zeroizing;

mod computer;
pub mod sign;
pub use computer::{computer_key, host_name};

const PARAMS: &str = "Noise_NK_25519_ChaChaPoly_BLAKE2s";
/// Bound into every handshake so a transcript can't be replayed against a
/// different protocol or version.
const PROLOGUE: &[u8] = b"tessera/e2e/1";
/// Largest Noise message (the spec's limit) and the payload that fits in it
/// next to the tag and our one-byte frame flag.
const NOISE_MAX: usize = 65_535;
const CHUNK: usize = NOISE_MAX - 16 - 1;
/// Handshake messages are tiny; refuse anything bigger before parsing it.
const HANDSHAKE_MAX: usize = 1024;

fn builder() -> Builder<'static> {
    Builder::new(PARAMS.parse().expect("valid Noise parameters"))
}

/// Stop other processes running as the same user from reading this one's
/// memory, environment and open files through `/proc`, and disable core
/// dumps (which would contain keys). Children re-enable it on `exec`.
pub fn harden_process() {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
    }
}

pub fn b64(bytes: &[u8]) -> String {
    B64.encode(bytes)
}

pub fn unb64(text: &str) -> Result<Vec<u8>> {
    B64.decode(text.trim()).context("invalid base64")
}

/// A short, human-comparable digest of a public key, e.g. `3f9a 1c2b 77e0 5d41 a0c9`.
pub fn fingerprint(public: &[u8]) -> String {
    let digest = Sha256::digest(public);
    digest[..10]
        .chunks(2)
        .map(|c| format!("{:02x}{:02x}", c[0], c[1]))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The server's long-term X25519 key pair.
pub struct Identity {
    secret: Zeroizing<Vec<u8>>,
    public: Vec<u8>,
}

impl Identity {
    pub fn generate() -> Result<Self> {
        let kp = builder()
            .generate_keypair()
            .map_err(|e| anyhow!("generating key pair: {e}"))?;
        Ok(Self {
            secret: Zeroizing::new(kp.private),
            public: kp.public,
        })
    }

    pub fn public(&self) -> &[u8] {
        &self.public
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(&self.public)
    }

    /// Load the key pair from `path`, creating it (owner-only) on first use.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).with_context(|| format!("reading {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let id = Self::generate()?;
                let text = Zeroizing::new(format!(
                    "tessera identity v1\n{}\n{}\n",
                    b64(&id.secret),
                    b64(&id.public)
                ));
                write_private(path, text.as_bytes())?;
                Ok(id)
            }
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Fingerprint of the identity stored at `path`, without creating one.
    pub fn fingerprint_at(path: &Path) -> Option<String> {
        let id = Self::parse(&std::fs::read_to_string(path).ok()?).ok()?;
        Some(id.fingerprint())
    }

    /// Public key of the identity stored at `path`, without creating one.
    pub fn public_at(path: &Path) -> Option<Vec<u8>> {
        Some(
            Self::parse(&std::fs::read_to_string(path).ok()?)
                .ok()?
                .public,
        )
    }

    fn parse(text: &str) -> Result<Self> {
        let mut lines = text.lines();
        if lines.next() != Some("tessera identity v1") {
            bail!("not a Tessera identity file");
        }
        let secret = Zeroizing::new(unb64(lines.next().unwrap_or_default())?);
        let public = unb64(lines.next().unwrap_or_default())?;
        if secret.len() != 32 || public.len() != 32 {
            bail!("corrupt identity file");
        }
        Ok(Self { secret, public })
    }
}

/// Write a file that only the owner can read, without a window where it is
/// readable by others.
fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(&tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
    f.write_all(data)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Client side of a session handshake.
pub struct Initiator(HandshakeState);

impl Initiator {
    /// Begin a handshake with the server whose public key was learned while
    /// pairing. Returns the first handshake message.
    pub fn start(server_public: &[u8]) -> Result<(Self, Vec<u8>)> {
        if server_public.len() != 32 {
            bail!("invalid server key");
        }
        let mut hs = builder()
            .prologue(PROLOGUE)
            .and_then(|b| b.remote_public_key(server_public))
            .and_then(|b| b.build_initiator())
            .map_err(|e| anyhow!("handshake setup: {e}"))?;
        let mut buf = vec![0u8; HANDSHAKE_MAX];
        let n = hs
            .write_message(&[], &mut buf)
            .map_err(|e| anyhow!("handshake: {e}"))?;
        buf.truncate(n);
        Ok((Self(hs), buf))
    }

    /// Complete the handshake with the server's reply. Fails if the reply was
    /// not produced by the holder of the server's private key.
    pub fn finish(mut self, reply: &[u8]) -> Result<Channel> {
        if reply.len() > HANDSHAKE_MAX {
            bail!("handshake reply too large");
        }
        let mut buf = vec![0u8; HANDSHAKE_MAX];
        self.0.read_message(reply, &mut buf).map_err(|_| anyhow!("the server failed to prove its identity (wrong server, or its key changed; pair again)"))?;
        let hash = self.0.get_handshake_hash().to_vec();
        Channel::new(
            self.0
                .into_stateless_transport_mode()
                .map_err(|e| anyhow!("handshake: {e}"))?,
            hash,
        )
    }
}

/// Server side of a session handshake: answer the client's first message.
pub fn accept(identity: &Identity, first: &[u8]) -> Result<(Channel, Vec<u8>)> {
    if first.len() > HANDSHAKE_MAX {
        bail!("handshake message too large");
    }
    let mut hs = builder()
        .prologue(PROLOGUE)
        .and_then(|b| b.local_private_key(&identity.secret))
        .and_then(|b| b.build_responder())
        .map_err(|e| anyhow!("handshake setup: {e}"))?;
    let mut buf = vec![0u8; HANDSHAKE_MAX];
    hs.read_message(first, &mut buf)
        .map_err(|_| anyhow!("invalid handshake"))?;
    let n = hs
        .write_message(&[], &mut buf)
        .map_err(|e| anyhow!("handshake: {e}"))?;
    buf.truncate(n);
    let hash = hs.get_handshake_hash().to_vec();
    Ok((
        Channel::new(
            hs.into_stateless_transport_mode()
                .map_err(|e| anyhow!("handshake: {e}"))?,
            hash,
        )?,
        buf,
    ))
}

/// An established session, split into halves that can live on different
/// tasks. Messages of any size are carried as a sequence of frames.
pub struct Channel {
    pub sealer: Sealer,
    pub opener: Opener,
    /// The Noise handshake hash: the same on both ends of this session
    /// only, so a signature over it can't be replayed in another one.
    pub handshake_hash: Vec<u8>,
}

impl Channel {
    fn new(state: StatelessTransportState, handshake_hash: Vec<u8>) -> Result<Self> {
        let state = Arc::new(state);
        Ok(Self {
            handshake_hash,
            sealer: Sealer {
                state: state.clone(),
                nonce: 0,
            },
            opener: Opener {
                state,
                nonce: 0,
                partial: Vec::new(),
                limit: 64 << 20,
            },
        })
    }
}

pub struct Sealer {
    state: Arc<StatelessTransportState>,
    nonce: u64,
}

impl Sealer {
    /// Encrypt one message into one or more frames, to be sent in order.
    pub fn seal(&mut self, message: &[u8]) -> Result<Vec<Vec<u8>>> {
        self.seal_frames(message).collect()
    }

    /// Lazy, wire-identical sealing. Only the next frame is encrypted when
    /// requested; callers must send it before asking for another one.
    pub fn seal_frames<'a>(&'a mut self, message: &'a [u8]) -> SealedFrames<'a> {
        SealedFrames {
            sealer: self,
            chunks: message.chunks(CHUNK).peekable(),
            finished: false,
        }
    }

    fn seal_chunk(&mut self, chunk: &[u8], last: bool) -> Result<Vec<u8>> {
        let mut plain = Zeroizing::new(Vec::with_capacity(chunk.len() + 1));
        plain.push(u8::from(last));
        plain.extend_from_slice(chunk);
        if self.nonce == u64::MAX {
            bail!("session exhausted");
        }
        let mut out = vec![0u8; plain.len() + 16];
        let n = self
            .state
            .write_message(self.nonce, &plain, &mut out)
            .map_err(|e| anyhow!("encrypt: {e}"))?;
        out.truncate(n);
        self.nonce += 1;
        Ok(out)
    }
}

/// One frame per `next`, including the final empty-message frame.
pub struct SealedFrames<'a> {
    sealer: &'a mut Sealer,
    chunks: std::iter::Peekable<std::slice::Chunks<'a, u8>>,
    finished: bool,
}
impl Iterator for SealedFrames<'_> {
    type Item = Result<Vec<u8>>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        let chunk = self.chunks.next().unwrap_or_default();
        self.finished = self.chunks.peek().is_none();
        let frame = self.sealer.seal_chunk(chunk, self.finished);
        if frame.is_err() {
            self.finished = true;
        }
        Some(frame)
    }
}

pub struct Opener {
    state: Arc<StatelessTransportState>,
    nonce: u64,
    partial: Vec<u8>,
    limit: usize,
}

impl Opener {
    /// Cap on a reassembled message (default 64 MiB).
    pub fn set_limit(&mut self, bytes: usize) {
        self.limit = bytes;
    }

    /// Decrypt the next frame. Returns a message once its last frame arrives.
    /// Any error (tampering, reordering, replay, oversize) is fatal for the
    /// session: the caller must close the connection.
    pub fn open(&mut self, frame: &[u8]) -> Result<Option<Vec<u8>>> {
        if frame.len() < 17 || frame.len() > NOISE_MAX {
            bail!("invalid frame");
        }
        let mut plain = vec![0u8; frame.len()];
        let n = self
            .state
            .read_message(self.nonce, frame, &mut plain)
            .map_err(|_| anyhow!("decryption failed (message tampered with or out of order)"))?;
        self.nonce += 1;
        let (flag, data) = plain[..n]
            .split_first()
            .ok_or_else(|| anyhow!("empty frame"))?;
        if *flag > 1 {
            bail!("invalid frame flag");
        }
        if self.partial.len() + data.len() > self.limit {
            bail!("message too large");
        }
        self.partial.extend_from_slice(data);
        Ok(if *flag == 1 {
            Some(std::mem::take(&mut self.partial))
        } else {
            None
        })
    }
}

/// A web browser's key, as it sends it: an uncompressed P-256 point.
pub fn browser_key_ok(key: &[u8]) -> bool {
    key.len() == 65 && key[0] == 4
}

/// Whether `proof` is the browser key `key`'s ECDSA P-256 / SHA-256
/// signature (`r || s`, as Web Crypto makes it) over `context` followed by
/// `handshake_hash` (the session's, see [`Channel::handshake_hash`]).
pub fn browser_proof_ok(key: &[u8], context: &[u8], handshake_hash: &[u8], proof: &[u8]) -> bool {
    use ring::signature::{ECDSA_P256_SHA256_FIXED, UnparsedPublicKey};
    if !browser_key_ok(key) || handshake_hash.is_empty() {
        return false;
    }
    let mut signed = Vec::with_capacity(context.len() + handshake_hash.len());
    signed.extend_from_slice(context);
    signed.extend_from_slice(handshake_hash);
    UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, key)
        .verify(&signed, proof)
        .is_ok()
}

/// Pairing: a password-authenticated key exchange keyed by the pairing code.
pub mod pairing {
    use super::*;
    use chacha20poly1305::aead::{Aead, KeyInit};
    use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
    use hkdf::Hkdf;
    use spake2::{Ed25519Group, Identity as PakeId, Password, Spake2};

    const DEVICE: &[u8] = b"tessera device";
    const SERVER: &[u8] = b"tessera server";

    /// Only the digits of a code matter (`1234-5678` == `12345678`).
    pub fn normalize_code(code: &str) -> String {
        code.chars().filter(char::is_ascii_digit).collect()
    }

    /// The device's half of the exchange.
    pub struct DevicePake(Spake2<Ed25519Group>);

    impl DevicePake {
        /// Returns the message to send to the server.
        pub fn start(code: &str) -> (Self, Vec<u8>) {
            let code = Zeroizing::new(normalize_code(code));
            let (s, msg) = Spake2::<Ed25519Group>::start_a(
                &Password::new(code.as_bytes()),
                &PakeId::new(DEVICE),
                &PakeId::new(SERVER),
            );
            (Self(s), msg)
        }

        pub fn finish(self, server_msg: &[u8]) -> Result<PairKeys> {
            let key = Zeroizing::new(
                self.0
                    .finish(server_msg)
                    .map_err(|e| anyhow!("pairing exchange failed: {e:?}"))?,
            );
            Ok(PairKeys::derive(&key))
        }
    }

    /// The server's half: answer a device's message for the active code.
    pub fn respond(code: &str, device_msg: &[u8]) -> Result<(PairKeys, Vec<u8>)> {
        let code = Zeroizing::new(normalize_code(code));
        let (s, msg) = Spake2::<Ed25519Group>::start_b(
            &Password::new(code.as_bytes()),
            &PakeId::new(DEVICE),
            &PakeId::new(SERVER),
        );
        let key = Zeroizing::new(
            s.finish(device_msg)
                .map_err(|e| anyhow!("invalid pairing message: {e:?}"))?,
        );
        Ok((PairKeys::derive(&key), msg))
    }

    /// Directional keys from the shared secret. Opening a message proves the
    /// sender used the same code, so it doubles as key confirmation.
    pub struct PairKeys {
        to_server: Zeroizing<[u8; 32]>,
        to_device: Zeroizing<[u8; 32]>,
    }

    impl PairKeys {
        fn derive(shared: &[u8]) -> Self {
            let hk = Hkdf::<Sha256>::new(Some(b"tessera pairing v1"), shared);
            let mut to_server = Zeroizing::new([0u8; 32]);
            let mut to_device = Zeroizing::new([0u8; 32]);
            hk.expand(b"device to server", to_server.as_mut())
                .expect("32 bytes is a valid HKDF length");
            hk.expand(b"server to device", to_device.as_mut())
                .expect("32 bytes is a valid HKDF length");
            Self {
                to_server,
                to_device,
            }
        }

        pub fn seal_to_server(&self, plain: &[u8]) -> Vec<u8> {
            seal(&self.to_server, plain)
        }
        pub fn open_from_device(&self, sealed: &[u8]) -> Result<Vec<u8>> {
            open(&self.to_server, sealed).ok_or_else(|| anyhow!("wrong pairing code"))
        }
        pub fn seal_to_device(&self, plain: &[u8]) -> Vec<u8> {
            seal(&self.to_device, plain)
        }
        pub fn open_from_server(&self, sealed: &[u8]) -> Result<Vec<u8>> {
            open(&self.to_device, sealed).ok_or_else(|| {
                anyhow!("wrong pairing code, or something tampered with the pairing")
            })
        }
    }

    fn seal(key: &[u8; 32], plain: &[u8]) -> Vec<u8> {
        let nonce: [u8; 12] = rand::random();
        let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
        let mut out = nonce.to_vec();
        out.extend(
            cipher
                .encrypt(Nonce::from_slice(&nonce), plain)
                .expect("in-memory encryption cannot fail"),
        );
        out
    }

    fn open(key: &[u8; 32], sealed: &[u8]) -> Option<Vec<u8>> {
        if sealed.len() < 12 + 16 {
            return None;
        }
        let (nonce, body) = sealed.split_at(12);
        ChaCha20Poly1305::new(Key::from_slice(key))
            .decrypt(Nonce::from_slice(nonce), body)
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::pairing::*;
    use super::*;

    fn session() -> (Channel, Channel) {
        let id = Identity::generate().unwrap();
        let (init, m1) = Initiator::start(id.public()).unwrap();
        let (server, m2) = accept(&id, &m1).unwrap();
        (init.finish(&m2).unwrap(), server)
    }

    #[test]
    fn both_ends_share_the_handshake_hash_and_browser_proofs_bind_to_it() {
        use ring::rand::SystemRandom;
        use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
        let (c, s) = session();
        assert_eq!(c.handshake_hash.len(), 32);
        assert_eq!(c.handshake_hash, s.handshake_hash);
        let (other, _) = session();
        assert_ne!(other.handshake_hash, c.handshake_hash, "one per session");

        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        let key = pair.public_key().as_ref().to_vec();
        assert!(browser_key_ok(&key));
        let sign = |hash: &[u8]| {
            let mut m = b"ctx".to_vec();
            m.extend_from_slice(hash);
            pair.sign(&rng, &m).unwrap().as_ref().to_vec()
        };
        let proof = sign(&s.handshake_hash);
        assert!(browser_proof_ok(&key, b"ctx", &s.handshake_hash, &proof));
        assert!(
            !browser_proof_ok(&key, b"ctx", &other.handshake_hash, &proof),
            "not in another session"
        );
        assert!(!browser_proof_ok(&key, b"other", &s.handshake_hash, &proof));
        let mut bad = proof.clone();
        bad[5] ^= 1;
        assert!(!browser_proof_ok(&key, b"ctx", &s.handshake_hash, &bad));
        assert!(!browser_key_ok(&key[1..]));
    }

    #[test]
    fn progressive_sealing_is_byte_identical_to_the_original_wire_format() {
        let (mut client, _) = session();
        let mut old_nonce = 0u64;
        for message in [
            vec![],
            b"hello".to_vec(),
            vec![7; CHUNK],
            vec![9; CHUNK * 3 + 5],
        ] {
            let mut expected = Vec::new();
            let mut chunks = message.chunks(CHUNK).peekable();
            loop {
                let chunk = chunks.next().unwrap_or_default();
                let last = chunks.peek().is_none();
                let mut plain = vec![u8::from(last)];
                plain.extend_from_slice(chunk);
                let mut cipher = vec![0; plain.len() + 16];
                let n = client
                    .sealer
                    .state
                    .write_message(old_nonce, &plain, &mut cipher)
                    .unwrap();
                old_nonce += 1;
                cipher.truncate(n);
                expected.push(cipher);
                if last {
                    break;
                }
            }
            assert_eq!(
                client
                    .sealer
                    .seal_frames(&message)
                    .collect::<Result<Vec<_>>>()
                    .unwrap(),
                expected
            );
            assert_eq!(client.sealer.nonce, old_nonce);
        }
    }

    #[test]
    fn maximum_request_is_sealed_progressively_one_nonce_per_pulled_frame() {
        let (mut client, mut server) = session();
        let message = vec![7; tessera_proto::MAX_SERIALIZED_REQUEST_BYTES];
        server.opener.set_limit(message.len());
        let mut frames = client.sealer.seal_frames(&message);
        assert_eq!(
            frames.sealer.nonce, 0,
            "iterator construction must not preseal"
        );
        let frame_count = message.len().div_ceil(CHUNK);
        let mut received = None;
        for count in 1..=frame_count {
            let frame = frames.next().unwrap().unwrap();
            assert_eq!(
                frames.sealer.nonce, count as u64,
                "only the requested chunk was sealed"
            );
            assert!(received.is_none());
            received = server.opener.open(&frame).unwrap();
        }
        assert!(frames.next().is_none());
        assert_eq!(received.unwrap(), message);
    }
    #[test]
    fn round_trip_small_and_large_messages() {
        let (mut c, mut s) = session();
        for msg in [
            b"".to_vec(),
            b"hello".to_vec(),
            vec![7u8; CHUNK],
            vec![9u8; CHUNK * 3 + 5],
        ] {
            let frames = c.sealer.seal(&msg).unwrap();
            assert_eq!(
                frames.len(),
                msg.len() / CHUNK + usize::from(msg.len() % CHUNK != 0 || msg.is_empty())
            );
            let mut got = None;
            for f in &frames {
                assert!(f.len() <= NOISE_MAX);
                assert!(got.is_none());
                got = s.opener.open(f).unwrap();
            }
            assert_eq!(got.unwrap(), msg);
            // And back the other way.
            let back = s.sealer.seal(&msg).unwrap();
            let mut got = None;
            for f in &back {
                got = c.opener.open(f).unwrap();
            }
            assert_eq!(got.unwrap(), msg);
        }
    }

    #[test]
    fn tampering_replay_and_reordering_are_fatal() {
        let (mut c, mut s) = session();
        let a = c.sealer.seal(b"first").unwrap().remove(0);
        let b = c.sealer.seal(b"second").unwrap().remove(0);
        let mut bad = a.clone();
        bad[20] ^= 1;
        assert!(s.opener.open(&bad).is_err(), "tampered frame");
        let (mut c, mut s) = session();
        let a2 = c.sealer.seal(b"first").unwrap().remove(0);
        let b2 = c.sealer.seal(b"second").unwrap().remove(0);
        assert!(s.opener.open(&b2).is_err(), "out of order");
        let (_, mut s) = session();
        assert!(s.opener.open(&a2).is_err(), "frame from another session");
        let _ = (a, b);
        let (mut c, mut s) = session();
        let m = c.sealer.seal(b"once").unwrap().remove(0);
        assert!(s.opener.open(&m).unwrap().is_some());
        assert!(s.opener.open(&m).is_err(), "replay");
    }

    #[test]
    fn wrong_server_key_is_detected() {
        let real = Identity::generate().unwrap();
        let imposter = Identity::generate().unwrap();
        let (init, m1) = Initiator::start(real.public()).unwrap();
        // An imposter can't even read the first message…
        assert!(accept(&imposter, &m1).is_err());
        // …and a reply made for another handshake doesn't verify.
        let (init2, m1b) = Initiator::start(imposter.public()).unwrap();
        let (_, reply) = accept(&imposter, &m1b).unwrap();
        assert!(init.finish(&reply).is_err());
        drop(init2);
    }

    #[test]
    fn oversized_messages_are_refused() {
        let (mut c, mut s) = session();
        s.opener.set_limit(1000);
        let frames = c.sealer.seal(&vec![1u8; 5000]).unwrap();
        assert!(s.opener.open(&frames[0]).is_err());
    }

    #[test]
    fn pairing_agrees_only_on_the_same_code() {
        let (device, m1) = DevicePake::start("1234-5678");
        let (server_keys, m2) = respond("12345678", &m1).unwrap();
        let device_keys = device.finish(&m2).unwrap();
        let sealed = device_keys.seal_to_server(b"hi server");
        assert_eq!(server_keys.open_from_device(&sealed).unwrap(), b"hi server");
        let back = server_keys.seal_to_device(b"key+token");
        assert_eq!(device_keys.open_from_server(&back).unwrap(), b"key+token");
        // Directions are distinct keys.
        assert!(server_keys.open_from_device(&back).is_err());

        let (device, m1) = DevicePake::start("1234-5679");
        let (server_keys, m2) = respond("12345678", &m1).unwrap();
        let device_keys = device.finish(&m2).unwrap();
        assert!(
            server_keys
                .open_from_device(&device_keys.seal_to_server(b"x"))
                .is_err()
        );
        assert!(
            device_keys
                .open_from_server(&server_keys.seal_to_device(b"x"))
                .is_err()
        );
    }

    #[test]
    fn pairing_rejects_reflected_messages() {
        let (_device, m1) = DevicePake::start("12345678");
        let (device2, _) = DevicePake::start("12345678");
        // A device message is not a valid server message.
        assert!(device2.finish(&m1).is_err());
        assert!(respond("12345678", &[0u8; 5]).is_err());
    }

    #[test]
    fn identity_persists_privately() {
        let dir = std::env::temp_dir().join(format!("tessera-id-{}", std::process::id()));
        let path = dir.join("identity.key");
        let a = Identity::load_or_create(&path).unwrap();
        let b = Identity::load_or_create(&path).unwrap();
        assert_eq!(a.public(), b.public());
        assert_eq!(a.fingerprint().len(), 24);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
