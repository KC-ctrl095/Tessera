// End-to-end encryption for Tessera in a web browser ("Use Tessera in a web
// browser"): the same pairing and sessions the apps use (crates/secure), so
// anything between the browser and the server (a Cloudflare tunnel, a
// proxy) relays only ciphertext.
//
// * Pairing: SPAKE2 keyed by the pairing code (the `spake2` crate's Ed25519
//   group, side A), then HKDF-SHA256 keys that carry the browser's hello and
//   the server's grant (its token and long-term key) sealed with
//   ChaCha20-Poly1305.
// * Sessions: Noise NK (X25519, ChaChaPoly, BLAKE2s) against the server key
//   learnt while pairing, then messages in frames of at most 64 KiB.
// * The browser's own key: ECDSA P-256 made by Web Crypto, never
//   extractable (kept in IndexedDB as a key object, not as bytes). Each
//   session it signs the session's handshake hash, so its login works only
//   with that key, which no script can copy out of the browser.
//
// Loaded only in a browser at `/web/` (lazily: the apps never load it). The
// primitives are the audited @noble libraries; nothing here is homemade
// crypto beyond putting them together the way the Rust side does.

import { ed25519, x25519 } from "@noble/curves/ed25519.js";
import { chacha20poly1305 } from "@noble/ciphers/chacha.js";
import { blake2s } from "@noble/hashes/blake2.js";
import { hkdf } from "@noble/hashes/hkdf.js";
import { hmac } from "@noble/hashes/hmac.js";
import { sha256 } from "@noble/hashes/sha2.js";
import { concatBytes, hexToBytes, randomBytes, utf8ToBytes } from "@noble/hashes/utils.js";

const EMPTY = new Uint8Array(0);

export function b64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
}

export function unb64(text: string): Uint8Array {
  const s = atob(text.trim());
  const out = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
  return out;
}

/** A short digest of the server's key to compare by eye, e.g. `3f9a 1c2b 77e0 5d41 a0c9` (as `tessera pair` shows it). */
export function fingerprint(serverKey: Uint8Array): string {
  const d = sha256(serverKey).subarray(0, 10);
  return Array.from(d, (b) => b.toString(16).padStart(2, "0"))
    .join("")
    .replace(/(.{4})(?=.)/g, "$1 ");
}

// ── Pairing ─────────────────────────────────────────────────────────────

const Point = ed25519.Point;
const ORDER = Point.Fn.ORDER;
// The `spake2` crate's constants for Ed25519 (python-spake2's M and N).
const M = Point.fromBytes(hexToBytes("15cfd18e385952982b6a8f8c7854963b58e34388c8e6dae891db756481a02312"));
const N = Point.fromBytes(hexToBytes("f04f2e7eb734b2a8f8b472eaf9c3c632576ac64aea650b496a8a20ff00e583c3"));
const ID_DEVICE = utf8ToBytes("tessera device");
const ID_SERVER = utf8ToBytes("tessera server");

function bigFromBytes(bytes: Uint8Array, littleEndian: boolean): bigint {
  let n = 0n;
  const ordered = littleEndian ? Array.from(bytes).reverse() : Array.from(bytes);
  for (const b of ordered) n = (n << 8n) | BigInt(b);
  return n;
}

/** The password as a scalar, as `spake2` does it: HKDF-SHA256 (no salt, info "SPAKE2 pw", 48 bytes) read big-endian, mod the group order. */
function passwordScalar(pw: Uint8Array): bigint {
  return bigFromBytes(hkdf(sha256, pw, EMPTY, utf8ToBytes("SPAKE2 pw"), 48), false) % ORDER;
}

/** A uniformly random non-zero scalar (64 random bytes mod the order, like curve25519-dalek's). */
function randomScalar(): bigint {
  for (;;) {
    const s = bigFromBytes(randomBytes(64), true) % ORDER;
    if (s !== 0n) return s;
  }
}

/** Keys both sides derive from the SPAKE2 secret; a wrong code gives keys that open nothing. */
export interface PairKeys {
  sealToServer(plain: Uint8Array): Uint8Array;
  openFromServer(sealed: Uint8Array): Uint8Array;
}

function seal(key: Uint8Array, plain: Uint8Array): Uint8Array {
  const nonce = randomBytes(12);
  return concatBytes(nonce, chacha20poly1305(key, nonce).encrypt(plain));
}

function open(key: Uint8Array, sealed: Uint8Array): Uint8Array {
  if (sealed.length < 12 + 16) throw new Error("invalid sealed message");
  return chacha20poly1305(key, sealed.subarray(0, 12)).decrypt(sealed.subarray(12));
}

/** The browser's side of pairing: its first message, then the keys from the server's answer. */
export function pairStart(code: string): { message: Uint8Array; finish(serverMessage: Uint8Array): PairKeys } {
  const pw = utf8ToBytes(code.replace(/\D/g, ""));
  const pwScalar = passwordScalar(pw);
  const x = randomScalar();
  // X = B·x + M·pw, sent as 'A' and the point.
  const ours = Point.BASE.multiply(x).add(M.multiply(pwScalar)).toBytes();
  return {
    message: concatBytes(new Uint8Array([0x41]), ours),
    finish(theirMessage) {
      if (theirMessage.length !== 33 || theirMessage[0] !== 0x42) throw new Error("the server's pairing answer was invalid");
      const theirs = theirMessage.subarray(1);
      // K = (Y − N·pw)·x, then the transcript hash.
      const k = Point.fromBytes(theirs).add(N.multiply(pwScalar).negate()).multiply(x).toBytes();
      const shared = sha256(concatBytes(sha256(pw), sha256(ID_DEVICE), sha256(ID_SERVER), ours, theirs, k));
      const salt = utf8ToBytes("tessera pairing v1");
      const toServer = hkdf(sha256, shared, salt, utf8ToBytes("device to server"), 32);
      const toDevice = hkdf(sha256, shared, salt, utf8ToBytes("server to device"), 32);
      shared.fill(0);
      return {
        sealToServer: (plain) => seal(toServer, plain),
        openFromServer: (sealed) => {
          try {
            return open(toDevice, sealed);
          } catch {
            throw new Error("Wrong pairing code, or something tampered with the pairing.");
          }
        },
      };
    },
  };
}

// ── Sessions: Noise NK ──────────────────────────────────────────────────

const PROTOCOL = utf8ToBytes("Noise_NK_25519_ChaChaPoly_BLAKE2s");
const PROLOGUE = utf8ToBytes("tessera/e2e/1");
/** The largest Noise message, and the payload that fits next to the tag and the frame flag. */
const NOISE_MAX = 65535;
const CHUNK = NOISE_MAX - 16 - 1;

function nonceBytes(n: number): Uint8Array {
  const out = new Uint8Array(12);
  new DataView(out.buffer).setBigUint64(4, BigInt(n), true);
  return out;
}

/** Noise's HKDF over HMAC-BLAKE2s: two (or three) 32-byte outputs. */
function noiseHkdf(ck: Uint8Array, ikm: Uint8Array): [Uint8Array, Uint8Array] {
  const temp = hmac(blake2s, ck, ikm);
  const one = hmac(blake2s, temp, new Uint8Array([1]));
  const two = hmac(blake2s, temp, concatBytes(one, new Uint8Array([2])));
  return [one, two];
}

/** One direction of an open session: frames in order, each with the next nonce. */
export class Sealer {
  private n = 0;
  private key: Uint8Array;
  constructor(key: Uint8Array) {
    this.key = key;
  }
  /** `message` in one or more frames; the last says so in its first byte. */
  seal(message: Uint8Array): Uint8Array[] {
    const frames: Uint8Array[] = [];
    let at = 0;
    for (;;) {
      const chunk = message.subarray(at, at + CHUNK);
      at += chunk.length;
      const last = at >= message.length;
      const plain = concatBytes(new Uint8Array([last ? 1 : 0]), chunk);
      frames.push(chacha20poly1305(this.key, nonceBytes(this.n++)).encrypt(plain));
      if (last) return frames;
    }
  }
}

export class Opener {
  private n = 0;
  private parts: Uint8Array[] = [];
  private size = 0;
  private key: Uint8Array;
  private limit: number;
  constructor(key: Uint8Array, limit = 64 << 20) {
    this.key = key;
    this.limit = limit;
  }
  /** A whole message once its last frame is in; null before. Throws on anything tampered with or out of order. */
  open(frame: Uint8Array): Uint8Array | null {
    if (frame.length < 17 || frame.length > NOISE_MAX) throw new Error("invalid frame");
    let plain: Uint8Array;
    try {
      plain = chacha20poly1305(this.key, nonceBytes(this.n)).decrypt(frame);
    } catch {
      throw new Error("decryption failed (message tampered with or out of order)");
    }
    this.n++;
    const flag = plain[0];
    if (flag > 1) throw new Error("invalid frame flag");
    this.size += plain.length - 1;
    if (this.size > this.limit) throw new Error("message too large");
    this.parts.push(plain.subarray(1));
    if (flag !== 1) return null;
    const whole = concatBytes(...this.parts);
    this.parts = [];
    this.size = 0;
    return whole;
  }
}

export interface Channel {
  sealer: Sealer;
  opener: Opener;
  /** The same on both ends of this session only (what the browser's key signs). */
  handshakeHash: Uint8Array;
}

/** The initiator's half of Noise NK against `serverKey`: the first message, then the channel from the server's answer. */
export function handshake(serverKey: Uint8Array): { message: Uint8Array; finish(reply: Uint8Array): Channel } {
  if (serverKey.length !== 32) throw new Error("invalid server key");
  // The name is longer than the hash: h starts as its hash.
  let h: Uint8Array = blake2s(PROTOCOL);
  let ck: Uint8Array = h;
  let k: Uint8Array | null = null;
  let n = 0;
  const mixHash = (data: Uint8Array) => {
    h = blake2s(concatBytes(h, data));
  };
  const mixKey = (ikm: Uint8Array) => {
    [ck, k] = noiseHkdf(ck, ikm);
    n = 0;
  };
  const encryptAndHash = (plain: Uint8Array) => {
    const out = k ? chacha20poly1305(k, nonceBytes(n++), h).encrypt(plain) : plain;
    mixHash(out);
    return out;
  };
  const decryptAndHash = (data: Uint8Array) => {
    const out = k ? chacha20poly1305(k, nonceBytes(n++), h).decrypt(data) : data;
    mixHash(data);
    return out;
  };
  mixHash(PROLOGUE);
  // <- s (known in advance)
  mixHash(serverKey);
  // -> e, es
  const e = x25519.utils.randomSecretKey();
  const ePub = x25519.getPublicKey(e);
  mixHash(ePub);
  mixKey(x25519.getSharedSecret(e, serverKey));
  const message = concatBytes(ePub, encryptAndHash(EMPTY));
  return {
    message,
    finish(reply) {
      if (reply.length !== 32 + 16) throw new Error("the server's handshake answer was invalid");
      // <- e, ee
      const re = reply.subarray(0, 32);
      mixHash(re);
      mixKey(x25519.getSharedSecret(e, re));
      try {
        decryptAndHash(reply.subarray(32));
      } catch {
        throw new Error("The server failed to prove its identity (a different server, or its key changed). Pair this browser again.");
      }
      e.fill(0);
      const [send, receive] = noiseHkdf(ck, EMPTY);
      return { sealer: new Sealer(send), opener: new Opener(receive), handshakeHash: h };
    },
  };
}

// ── The browser's own key ───────────────────────────────────────────────

/** What a browser's proof signs before the handshake hash (`tessera_proto::e2e::BROWSER_PROOF`). */
const BROWSER_PROOF = utf8ToBytes("tessera browser login v1");
const DB = "tessera-web";
const STORE = "keys";

function db(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open(DB, 1);
    req.onupgradeneeded = () => req.result.createObjectStore(STORE);
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error ?? new Error("this browser can't store a key (private browsing?)"));
  });
}

async function stored<T>(mode: IDBTransactionMode, act: (s: IDBObjectStore) => IDBRequest): Promise<T> {
  const d = await db();
  try {
    return await new Promise<T>((resolve, reject) => {
      const tx = d.transaction(STORE, mode);
      const req = act(tx.objectStore(STORE));
      tx.oncomplete = () => resolve(req.result as T);
      tx.onerror = () => reject(tx.error ?? new Error("storing the key failed"));
    });
  } finally {
    d.close();
  }
}

/**
 * A new key for this browser: its public half as the server keeps it (an
 * uncompressed point), and `keep()` to make it this browser's key once
 * pairing worked (replacing any old one).
 */
export async function newDeviceKey(): Promise<{ publicKey: Uint8Array; keep(): Promise<void> }> {
  if (!crypto.subtle) throw new Error("This page isn't on a secure (https) address, so the browser can't keep a key for it.");
  const pair = (await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, false, ["sign", "verify"])) as CryptoKeyPair;
  return {
    publicKey: new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey)),
    keep: () => stored("readwrite", (s) => s.put(pair, "device")),
  };
}

/** Forget this browser's key (signed out). */
export async function dropDeviceKey(): Promise<void> {
  await stored("readwrite", (s) => s.delete("device")).catch(() => undefined);
}

/** This session's proof that the browser holds its key: a signature over the handshake hash. */
export async function prove(handshakeHash: Uint8Array): Promise<string> {
  const pair = await stored<CryptoKeyPair | undefined>("readonly", (s) => s.get("device"));
  if (!pair?.privateKey) throw new Error("This browser's key is gone (its site data was cleared). Pair it again.");
  const sig = await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, pair.privateKey, concatBytes(BROWSER_PROOF, handshakeHash));
  return b64(new Uint8Array(sig));
}
