//! Tessera Guard (Windows): the user's Stop for this computer, kept where
//! the signed-in user's own programs (an agent's code among them) can't
//! undo it, and enforced by ending everything Tessera's machine started.
//!
//! A small Windows service (`TesseraGuard`, from a copy of `tessera.exe` in
//! a folder only administrators can change) holds the Stop in a registry
//! key only it writes, and answers one request per connection on a local
//! pipe. Anyone signed in may stop; resuming takes a program the user
//! approved in Windows' administrator prompt (UAC, on the secure desktop).
//! The machine asks the guard to take it in (a Windows job that nothing can
//! leave): on a Stop, or when the machine goes, the guard ends it and
//! everything it started, whatever those programs do. Installing it takes
//! an administrator's approval; without it, Windows machines refuse
//! desktop control and commands.
//!
//! This file is the part every system builds and tests: the requests, who
//! may make them, and how the Stop changes. `windows.rs` is the service,
//! the pipe and the client.

use serde::{Deserialize, Serialize};

#[cfg(windows)]
pub mod windows;

/// The service's name (and how Windows lists it: "Tessera Guard").
pub const SERVICE_NAME: &str = "TesseraGuard";
pub const SERVICE_DISPLAY: &str = "Tessera Guard";
/// Where it answers: this computer only (remote clients are refused).
pub const PIPE_NAME: &str = r"\\.\pipe\TesseraGuard";
/// The Stop it keeps (HKEY_LOCAL_MACHINE): one value, `State`.
pub const STATE_KEY: &str = r"SOFTWARE\Tessera\Guard";
/// Its signing key (HKEY_LOCAL_MACHINE, `Key`): only SYSTEM may read it.
pub const SECRET_KEY: &str = r"SOFTWARE\Tessera\GuardSecret";
/// SYSTEM alone reads or writes it; Administrators may only delete it
/// (removing the guard).
pub const SECRET_SDDL: &str = "D:P(A;;KA;;;SY)(A;;SD;;;BA)";
/// Its folder under Program Files, and its program there.
pub const DIR_NAME: &str = "Tessera Guard";
pub const EXE_NAME: &str = "tessera-guard.exe";

/// The registry key: SYSTEM and Administrators may change it, signed-in
/// users only read it. Nothing inherited from above.
pub const KEY_SDDL: &str = "D:P(A;OICI;KA;;;SY)(A;OICI;KA;;;BA)(A;OICI;KR;;;AU)";
/// The program's folder: the same for files (read and run for users).
pub const DIR_SDDL: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FRFX;;;BU)";
/// The pipe: SYSTEM and Administrators fully; users signed in here may
/// connect, read and write (0x120083: read and write data, read
/// attributes, read control, synchronize), never create instances of it.
pub const PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x120083;;;IU)";
/// Where anyone signed in here can stop, without the pipe (it can't be
/// crowded out): a semaphore users may only release (0x2), each release a
/// Stop. Its full name ends in a random part chosen when the guard starts
/// (anyone may make objects under `Global\`, so a fixed name could be
/// taken first) and is published in [`STATE_KEY`] (`StopName`).
pub const STOP_SEMAPHORE: &str = r"Global\TesseraGuardStop";
pub const STOP_SEMAPHORE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x2;;;IU)";
/// A Stop counted elsewhere raises the guard's count by at most this much
/// at once (a caller can't push it to its end).
pub const MAX_JUMP: u32 = 1024;

/// The longest request or answer, in bytes.
pub const MAX_MESSAGE: usize = 4096;

/// Windows' integrity levels (the last part of the label's SID).
pub const MEDIUM: u32 = 0x2000;
pub const HIGH: u32 = 0x3000;

/// One request (one per connection).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Stopped or not, how many Stops so far, and the guard's own number
    /// for the latest one.
    Status,
    /// Stop everything here. Counted at least `at_least` (a Stop counted
    /// elsewhere first, brought here; within [`MAX_JUMP`]); always one more
    /// than before.
    Stop {
        #[serde(default)]
        at_least: u32,
    },
    /// Resume the Stop the guard numbered `generation` under its signing
    /// key `key` (its public part; both asked of it before the user
    /// approved): refused when another came since, even during the approval
    /// (the user approves again, knowing it), or when the guard's numbers
    /// started again under another key meanwhile.
    Resume {
        generation: u64,
        #[serde(default)]
        key: Option<String>,
    },
    /// Take the calling process (Tessera's machine) in: a job that nothing
    /// can leave, ended on a Stop and when the process ends.
    Adopt,
    /// Take in agent work the caller just started (still suspended): a
    /// process of the caller's user and session at the user's normal level.
    AdoptChild { pid: u32 },
    /// The guard's signed word on the Stop, for a server's `nonce` (see
    /// [`attest_message`]): what lets a server tell this computer's real
    /// machine, which can't act while stopped, from anything else.
    Attest { nonce: String },
}

/// The answer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    pub stopped: bool,
    pub stops: u32,
    #[serde(default)]
    pub generation: u64,
    /// The guard's public signing key (hex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// For `Attest`: the signature (hex) over [`attest_message`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Why not, in plain words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Who is asking, as Windows says (from the pipe itself, never from what
/// the request says).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Client {
    /// Known at all (an anonymous connection isn't).
    pub known: bool,
    /// The token was raised through Windows' administrator prompt.
    pub elevated: bool,
    /// A member of Administrators, enabled in this token.
    pub admin: bool,
    pub integrity: Option<u32>,
}

/// Whether `client` may make `request`. Status and Stop: anyone known
/// (stopping only ever stops). Resume: only a program the user raised
/// through Windows' administrator prompt. Adopt: only for a program at
/// the user's normal level (the guard checks it is the caller itself).
pub fn authorize(request: &Request, client: &Client) -> Result<(), &'static str> {
    if !client.known {
        return match request {
            Request::Status => Ok(()),
            _ => Err("Windows didn't say who is asking, so the guard does nothing for it"),
        };
    }
    match request {
        Request::Status | Request::Stop { .. } => Ok(()),
        Request::Attest { nonce } if nonce.is_empty() || nonce.len() > 128 => {
            Err("that isn't a nonce the guard signs")
        }
        Request::Attest { .. } => Ok(()),
        Request::Resume { .. } => {
            if client.elevated && client.admin && client.integrity.is_some_and(|i| i >= HIGH) {
                Ok(())
            } else {
                Err(
                    "resuming takes your approval in Windows' administrator prompt: press Resume on the Stop bar, in the app or run `tessera node resume`, and approve it there",
                )
            }
        }
        Request::Adopt | Request::AdoptChild { .. } => match client.integrity {
            Some(i) if i <= MEDIUM && !client.elevated => Ok(()),
            _ => Err("only Tessera's machine at your normal level is taken in"),
        },
    }
}

/// The Stop the guard keeps: `stops` (the count every part of Tessera
/// shares) and `generation`, the guard's own number for the latest Stop,
/// which only it raises (a Resume must name it exactly).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub stopped: bool,
    pub stops: u32,
    #[serde(default)]
    pub generation: u64,
}

impl State {
    /// Stopped, counted anew (and at least `at_least`, within
    /// [`MAX_JUMP`]); numbered anew.
    pub fn stop(&mut self, at_least: u32) {
        let next = self.stops.saturating_add(1);
        self.stops = next.max(at_least.min(self.stops.saturating_add(MAX_JUMP)));
        self.generation = self.generation.saturating_add(1);
        self.stopped = true;
    }

    /// Resumed, by someone who approved the Stop numbered `generation`:
    /// refused when another came since. Numbered anew too: a number is
    /// only ever signed as stopped or as not stopped, never both (servers
    /// remember only the newest number they saw).
    pub fn resume(&mut self, generation: u64) -> Result<(), &'static str> {
        if generation != self.generation {
            return Err(
                "another Stop came after the one you approved resuming: resume again if you still want to",
            );
        }
        // The last numbers are kept for Stops (one saturates there).
        if self.generation >= u64::MAX - 1 {
            return Err(
                "this Stop can't be numbered further: remove Tessera Guard and install it again",
            );
        }
        self.stopped = false;
        self.generation += 1;
        Ok(())
    }
}

/// The Stop as the guard keeps it (in the registry, or in its stand-in
/// file) with the public key (hex) of the signing key its numbers belong
/// to: a key signs only numbers of the record that names it, so no (key,
/// number) is ever signed both stopped and not stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    #[serde(flatten)]
    pub state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// Whether the record `saved` numbers for the signing key `public`.
pub fn belongs_to(saved: Option<&Saved>, public: &str) -> bool {
    saved.is_some_and(|s| s.key.as_deref() == Some(public))
}

/// Whether the guard may go on signing with the key `public`: its record
/// names it, and so does its stand-in file if there is one. A stand-in that
/// can't be read, or was written under another key (or none), may have held
/// Stops this key signed with higher numbers: the key goes (its numbers
/// could be signed again the other way).
pub fn keep_key(
    saved: Option<&Saved>,
    pending: &std::io::Result<Option<Saved>>,
    public: &str,
) -> bool {
    belongs_to(saved, public)
        && match pending {
            Ok(None) => true,
            Ok(Some(p)) => p.key.as_deref() == Some(public),
            Err(_) => false,
        }
}

/// The Stop the guard starts with, from its record `saved` (`None`: missing
/// or unreadable), its stand-in file `pending` (an error: unreadable), the
/// key in use `bound` and whether the numbers start again under a new key
/// (`renumbered`); and whether it must be saved (again). A stand-in's Stop
/// (one the registry didn't take, or kept under another key) is a Stop like
/// any other, numbered anew; with nothing readable or the numbers starting
/// again it is stopped (a Stop is never lost to a damaged record).
pub fn recover(
    saved: Option<Saved>,
    pending: std::io::Result<Option<Saved>>,
    bound: Option<&str>,
    renumbered: bool,
) -> (State, bool) {
    let readable = saved.is_some();
    // Saved again when it doesn't name the key in use (a new one, or none).
    let mut changed = renumbered || saved.as_ref().is_none_or(|s| s.key.as_deref() != bound);
    let mut state = saved.map(|s| s.state).unwrap_or_default();
    match pending {
        Ok(None) => {}
        Ok(Some(p)) => {
            changed = true;
            // Newer than the record, kept under another key (or none), or
            // counting more Stops: a Stop, numbered above both (the
            // stand-in's number may have been signed already), its count the
            // guard's own (kept whole, never capped as a count brought from
            // elsewhere is).
            if p.key.as_deref() != bound
                || p.state.generation > state.generation
                || p.state.stops > state.stops
            {
                state.generation = state.generation.max(p.state.generation);
                state.stops = state.stops.max(p.state.stops);
                state.stop(0);
            }
        }
        Err(_) => {
            changed = true;
            state.stop(0);
        }
    }
    if (!readable || renumbered) && !state.stopped {
        state.stop(0);
    }
    (state, changed)
}

/// A Stop counted into `state`, and the ending it sets in `ending` (when
/// the earliest Stop not yet enforced came, and the newest one's number:
/// what was taken in before it ends at the deadline). More Stops never put
/// the ending off; a Resume never takes it back.
pub fn stop_ending(
    state: &mut State,
    ending: &mut Option<(std::time::Instant, u64)>,
    at_least: u32,
) {
    state.stop(at_least);
    let since = ending.map_or_else(std::time::Instant::now, |(t, _)| t);
    *ending = Some((since, state.generation));
}

/// This computer's Stop with the guard installed: the guard's, unless a
/// Stop kept here (`local_stopped`, counted `local_stops`) is newer than
/// what the guard has (not brought to it yet). Resumes kept here count for
/// nothing: only the guard resumes (and a high count kept here without a
/// Stop stops nothing).
pub fn with_guard(local_stopped: bool, local_stops: u32, guard: State) -> State {
    let newer_here = local_stopped && local_stops > guard.stops;
    State {
        stopped: guard.stopped || newer_here,
        // The guard's count; one kept here only for a Stop it hasn't got.
        stops: if newer_here { local_stops } else { guard.stops },
        generation: guard.generation,
    }
}

/// Windows' User Account Control settings that decide whether a Resume
/// really took the user's approval (HKLM\…\Policies\System).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Uac {
    pub enable_lua: Option<u32>,
    pub secure_desktop: Option<u32>,
    pub admin_prompt: Option<u32>,
    pub user_prompt: Option<u32>,
    /// One of them couldn't be read (not merely missing): counted as weak.
    pub unreadable: bool,
}

/// Whether Windows' administrator prompt is something only the user can
/// answer, for a user who is an administrator (`admin`) or not. `Err`:
/// what to change, in plain words.
pub fn uac_ok(uac: Uac, admin: bool) -> Result<(), String> {
    if uac.unreadable {
        return Err("Windows' User Account Control settings couldn't be read here, so Tessera can't tell whether only you can approve a Resume".into());
    }
    // Unset values mean Windows' defaults: on, on the secure desktop.
    if uac.enable_lua.unwrap_or(1) == 0 {
        return Err("User Account Control is off on this computer, so any program could resume Tessera: turn it on (Control Panel → User Accounts → Change User Account Control settings) and restart the computer".into());
    }
    if uac.secure_desktop.unwrap_or(1) == 0 {
        return Err("Windows shows its administrator prompt where other programs can reach it (not on the secure desktop): set User Account Control to \"Always notify\" (Control Panel → User Accounts → Change User Account Control settings)".into());
    }
    if admin {
        // 1: ask for a password, 2: ask always, on the secure desktop. The
        // default (5) lets some of Windows' own programs raise themselves
        // without asking, which other programs can use to get around it.
        match uac.admin_prompt.unwrap_or(5) {
            1 | 2 => Ok(()),
            _ => Err("your account is an administrator and User Account Control isn't at \"Always notify\", so a program could approve itself: set it to \"Always notify\" (Control Panel → User Accounts → Change User Account Control settings)".into()),
        }
    } else {
        // 0: refused without asking (then only an administrator signing in
        // can resume); 1 or 3: an administrator's password on the secure
        // desktop.
        match uac.user_prompt.unwrap_or(3) {
            0 | 1 | 3 => Ok(()),
            _ => Err("Windows' prompt for an administrator's password isn't on the secure desktop: set User Account Control to \"Always notify\" (Control Panel → User Accounts → Change User Account Control settings)".into()),
        }
    }
}

/// What the guard signs for a server's `nonce`: the Stop as it is.
pub fn attest_message(nonce: &str, s: State) -> Vec<u8> {
    format!(
        "tessera-guard-attest-v1\n{nonce}\n{}\n{}\n{}",
        u8::from(s.stopped),
        s.stops,
        s.generation
    )
    .into_bytes()
}

/// `signature` (hex) is the guard with public key `key` (hex) saying `s`
/// for `nonce`.
pub fn verify_attestation(key: &str, nonce: &str, s: State, signature: &str) -> bool {
    match (unhex(key), unhex(signature)) {
        (Some(k), Some(sig)) => tessera_secure::sign::verify(&k, &attest_message(nonce, s), &sig),
        _ => false,
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || text.len() > 512 {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// The guard's public key (hex), `None` without a guard that answers.
pub fn public_key() -> Option<String> {
    #[cfg(windows)]
    {
        if !windows::installed() {
            return None;
        }
        windows::ask(&Request::Status, std::time::Duration::from_secs(2))
            .ok()
            .and_then(|r| r.key)
    }
    #[cfg(not(windows))]
    None
}

/// The guard's signed word on the Stop for `nonce`: the Stop and the
/// signature (hex). `None` without a guard that answers.
pub fn attest(nonce: &str) -> Option<(State, String)> {
    #[cfg(windows)]
    {
        let r = windows::ask(
            &Request::Attest {
                nonce: nonce.to_string(),
            },
            std::time::Duration::from_secs(3),
        )
        .ok()?;
        let signature = r.signature.filter(|_| r.ok)?;
        Some((
            State {
                stopped: r.stopped,
                stops: r.stops,
                generation: r.generation,
            },
            signature,
        ))
    }
    #[cfg(not(windows))]
    {
        let _ = nonce;
        None
    }
}

/// This computer's guard, as the machine needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Here {
    /// Not installed (or not Windows).
    Absent,
    /// Installed and answering: the Stop it keeps.
    Says(State),
    /// Installed but not answering (why).
    Silent(String),
}

/// What desktop control and commands need on Windows, said when they're
/// refused for want of it.
pub const NEEDED: &str = "commands and desktop control on Windows need Tessera Guard, which keeps your Stop where agents' programs can't undo it without your approval: install it in the Tessera app (Settings → Devices → This computer) or with `tessera guard install`; Windows asks an administrator once";

/// Asked as it is now (a short wait at most).
pub fn here() -> Here {
    #[cfg(windows)]
    {
        if !windows::installed() {
            return Here::Absent;
        }
        match windows::ask(&Request::Status, std::time::Duration::from_millis(800)) {
            Ok(r) if r.ok => Here::Says(State {
                stopped: r.stopped,
                stops: r.stops,
                generation: r.generation,
            }),
            Ok(r) => Here::Silent(r.error.unwrap_or_else(|| "it said no".into())),
            Err(e) => Here::Silent(e.to_string()),
        }
    }
    #[cfg(not(windows))]
    Here::Absent
}

/// Bring a Stop to the guard: first through its semaphore (nothing can
/// crowd that out; the guard counts it anew), then with the count kept here
/// (`at_least`) on its pipe. Whether it took it.
pub fn forward_stop(at_least: u32) -> bool {
    #[cfg(windows)]
    {
        if !windows::installed() {
            return false;
        }
        let signalled = windows::signal_stop();
        let counted = windows::ask(
            &Request::Stop { at_least },
            std::time::Duration::from_secs(2),
        )
        .is_ok_and(|r| r.stopped);
        signalled || counted
    }
    #[cfg(not(windows))]
    {
        let _ = at_least;
        false
    }
}

/// Ask the guard to take this process (the machine) in.
pub fn adopt_me() -> Result<(), String> {
    #[cfg(windows)]
    {
        match windows::ask(&Request::Adopt, std::time::Duration::from_secs(3)) {
            Ok(r) if r.ok => Ok(()),
            Ok(r) => Err(r.error.unwrap_or_else(|| "it said no".into())),
            Err(e) => Err(e.to_string()),
        }
    }
    #[cfg(not(windows))]
    Ok(())
}

/// How long an approval to resume counts once asked for: one given later
/// (a prompt left open) resumes nothing.
pub const APPROVAL_MS: u64 = 10 * 60 * 1000;

/// Ask the guard to take in agent work this process just started (`pid`,
/// still suspended): ended on a Stop whatever it does. Windows only.
pub fn adopt_child(pid: u32) -> Result<(), String> {
    #[cfg(windows)]
    {
        match windows::ask(
            &Request::AdoptChild { pid },
            std::time::Duration::from_secs(3),
        ) {
            Ok(r) if r.ok => Ok(()),
            Ok(r) => Err(r.error.unwrap_or_else(|| "it said no".into())),
            Err(e) => Err(e.to_string()),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        Ok(())
    }
}

/// Resume through Windows' administrator prompt: the guard is asked which
/// Stop it holds (its own number, not anything kept here), then its own
/// protected program is raised (never the user's copy of Tessera) to resume
/// exactly that one; the user approves it on the secure desktop. A Stop
/// during the approval, or an approval later than [`APPROVAL_MS`], resumes
/// nothing.
pub fn resume_with_approval() -> Result<(), String> {
    #[cfg(windows)]
    {
        let r = windows::ask(&Request::Status, std::time::Duration::from_secs(3))
            .map_err(|e| format!("Tessera Guard can't be asked: {e}"))?;
        if !r.stopped {
            return Ok(());
        }
        let program = windows::service_program()
            .map_err(|e| format!("Tessera Guard's program wasn't found: {e}"))?;
        let until = crate::util::now_ms() + APPROVAL_MS;
        match windows::run_elevated(
            &program,
            &match &r.key {
                // Hex only (from the guard itself).
                Some(key) if key.bytes().all(|b| b.is_ascii_hexdigit()) => format!(
                    "guard resume --generation {} --key {key} --until {until}",
                    r.generation
                ),
                _ => format!("guard resume --generation {} --until {until}", r.generation),
            },
            std::time::Duration::from_millis(APPROVAL_MS),
        ) {
            Ok(0) => Ok(()),
            Ok(_) => Err("Tessera Guard didn't resume (another Stop came, the approval came too late, or it couldn't save it): try again".into()),
            Err(e) => Err(e.to_string()),
        }
    }
    #[cfg(not(windows))]
    Err("Tessera Guard is for Windows".into())
}

/// User Account Control here is one only the user can answer (Windows).
pub fn uac_here() -> Result<(), String> {
    #[cfg(windows)]
    {
        uac_ok(windows::uac(), windows::user_is_admin())
    }
    #[cfg(not(windows))]
    Ok(())
}

/// Why desktop control and commands are refused on this computer for the
/// guard's sake (Windows), given whether the machine was taken in (`None`:
/// not asked, as for `tessera node status`).
pub fn blocked(here: &Here, taken_in: Option<&Result<(), String>>) -> Option<String> {
    match here {
        Here::Absent => cfg!(windows).then(|| NEEDED.to_string()),
        Here::Silent(why) => Some(format!(
            "Tessera Guard isn't answering on this computer ({why}), so nothing runs here"
        )),
        Here::Says(_) => match taken_in {
            // Said whether a restart of Tessera helps (a guard without its
            // signing key says what does).
            Some(Err(why)) if why.contains("restart") => {
                Some(format!("Tessera Guard didn't take this machine in: {why}"))
            }
            Some(Err(why)) => Some(format!(
                "Tessera Guard didn't take this machine in ({why}); restart Tessera on this computer"
            )),
            _ => uac_here().err(),
        },
    }
}

/// For `tessera node status` and the app (Windows; `None` elsewhere).
pub fn status_for(taken_in: Option<&Result<(), String>>) -> Option<tessera_proto::GuardStatus> {
    if !cfg!(windows) {
        return None;
    }
    let here = here();
    let blocked = blocked(&here, taken_in);
    let (installed, running, state) = match &here {
        Here::Absent => (false, false, State::default()),
        Here::Silent(_) => (true, false, State::default()),
        Here::Says(s) => (true, true, *s),
    };
    Some(tessera_proto::GuardStatus {
        installed,
        running,
        stopped: state.stopped,
        stops: state.stops,
        blocked,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(elevated: bool, admin: bool, integrity: u32) -> Client {
        Client {
            known: true,
            elevated,
            admin,
            integrity: Some(integrity),
        }
    }

    #[test]
    fn anyone_may_stop_and_only_an_approved_elevation_resumes() {
        let user = client(false, false, MEDIUM);
        let raised = client(true, true, HIGH);
        assert!(authorize(&Request::Stop { at_least: 0 }, &user).is_ok());
        assert!(authorize(&Request::Status, &user).is_ok());
        assert!(
            authorize(
                &Request::Resume {
                    generation: 1,
                    key: None,
                },
                &user
            )
            .is_err()
        );
        // An administrator's account, not raised (its usual, filtered token).
        assert!(
            authorize(
                &Request::Resume {
                    generation: 1,
                    key: None,
                },
                &client(false, false, MEDIUM)
            )
            .is_err()
        );
        // Raised but not an administrator, or not at high level: no.
        assert!(
            authorize(
                &Request::Resume {
                    generation: 1,
                    key: None,
                },
                &client(true, false, HIGH)
            )
            .is_err()
        );
        assert!(
            authorize(
                &Request::Resume {
                    generation: 1,
                    key: None,
                },
                &client(true, true, MEDIUM)
            )
            .is_err()
        );
        assert!(
            authorize(
                &Request::Resume {
                    generation: 1,
                    key: None,
                },
                &raised
            )
            .is_ok()
        );
        // Nobody known: only the status.
        let nobody = Client::default();
        assert!(authorize(&Request::Status, &nobody).is_ok());
        assert!(authorize(&Request::Stop { at_least: 0 }, &nobody).is_err());
        assert!(
            authorize(
                &Request::Resume {
                    generation: 9,
                    key: None,
                },
                &nobody
            )
            .is_err()
        );
    }

    #[test]
    fn only_a_normal_level_machine_is_taken_in() {
        assert!(authorize(&Request::Adopt, &client(false, false, MEDIUM)).is_ok());
        assert!(authorize(&Request::Adopt, &client(true, true, HIGH)).is_err());
        assert!(
            authorize(
                &Request::Adopt,
                &Client {
                    known: true,
                    ..Client::default()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn a_stop_always_counts_and_a_resume_must_name_the_latest() {
        let mut s = State::default();
        s.stop(0);
        assert_eq!(
            s,
            State {
                stopped: true,
                stops: 1,
                generation: 1
            }
        );
        s.stop(0);
        assert_eq!((s.stops, s.generation), (2, 2));
        // Brought from elsewhere with a higher count.
        s.stop(7);
        assert_eq!((s.stops, s.generation), (7, 3));
        // Approved for an earlier Stop: another came since.
        assert!(s.resume(2).is_err());
        // A number from the future names nothing.
        assert!(s.resume(4).is_err());
        assert!(s.stopped);
        assert!(s.resume(3).is_ok());
        assert!(!s.stopped);
        assert_eq!(s.generation, 4, "a Resume is numbered anew");
        // Approved again for the same Stop: it's no longer the newest.
        assert!(s.resume(3).is_err());
    }

    /// Servers remember only the newest number: so each number is signed
    /// as stopped or as not stopped, never both, whatever happens (a Stop,
    /// a Resume, a Stop imposed when a record can't be read).
    #[test]
    fn a_number_is_never_both_stopped_and_not() {
        let mut seen: std::collections::HashMap<u64, bool> = Default::default();
        let mut s = State::default();
        let mut note = |s: &State| {
            let was = seen.insert(s.generation, s.stopped);
            assert!(was.is_none_or(|w| w == s.stopped), "{s:?}");
        };
        note(&s);
        for _ in 0..3 {
            s.stop(0);
            note(&s);
            s.stop(0);
            note(&s);
            s.resume(s.generation).unwrap();
            note(&s);
        }
        // A Stop imposed on recovery (a stand-in record that can't be read)
        // is a Stop like any other.
        let resumed = s;
        let mut recovered = resumed;
        recovered.stop(0);
        note(&recovered);
        assert!(recovered.generation > resumed.generation);
        // At the very end: the last numbers only ever say stopped (a Stop
        // saturates there, so no Resume may number itself there either).
        for last in [u64::MAX - 1, u64::MAX] {
            let mut top = State {
                stopped: true,
                stops: 1,
                generation: last,
            };
            assert!(top.resume(last).is_err());
            assert!(top.stopped);
        }
        let mut near = State {
            stopped: true,
            stops: 1,
            generation: u64::MAX - 2,
        };
        near.resume(u64::MAX - 2).unwrap();
        assert_eq!(near.generation, u64::MAX - 1);
        near.stop(0);
        near.stop(0);
        assert_eq!((near.stopped, near.generation), (true, u64::MAX));
    }

    fn saved(stopped: bool, stops: u32, generation: u64, key: Option<&str>) -> Saved {
        Saved {
            state: State {
                stopped,
                stops,
                generation,
            },
            key: key.map(String::from),
        }
    }

    /// What the guard starts with, from its record and stand-in file.
    #[test]
    fn the_guard_starts_from_its_own_keys_record() {
        let k = Some("K");
        // Its own record, nothing else: as it was, nothing to save.
        let (s, changed) = recover(Some(saved(false, 3, 5, k)), Ok(None), k, false);
        assert_eq!((s.stopped, s.generation, changed), (false, 5, false));
        // Lost or unreadable: stopped, saved.
        let (s, changed) = recover(None, Ok(None), k, false);
        assert!(s.stopped && changed);
        // Numbers starting again under a new key: stopped, saved.
        let (s, changed) = recover(Some(saved(false, 3, 5, Some("old"))), Ok(None), k, true);
        assert!(s.stopped && changed && s.stops == 4);
        // A record written before it had a key (a new install): kept as it
        // is, saved again naming the key.
        let (s, changed) = recover(Some(saved(false, 2, 0, None)), Ok(None), k, false);
        assert_eq!((s.stopped, s.stops, changed), (false, 2, true));
        // A stand-in Stop newer than the record: a Stop, numbered anew,
        // its count kept.
        let (s, changed) = recover(
            Some(saved(false, 3, 5, k)),
            Ok(Some(saved(true, 9, 7, k))),
            k,
            false,
        );
        assert_eq!(
            (s.stopped, s.stops, s.generation, changed),
            (true, 10, 8, true)
        );
        // An older one: the record holds; the file goes (saved).
        let (s, changed) = recover(
            Some(saved(false, 3, 5, k)),
            Ok(Some(saved(true, 2, 4, k))),
            k,
            false,
        );
        assert_eq!((s.stopped, s.generation, changed), (false, 5, true));
        // One kept under another key (or none): a Stop all the same.
        let (s, _) = recover(
            Some(saved(false, 3, 5, k)),
            Ok(Some(saved(true, 1, 1, None))),
            k,
            false,
        );
        assert!(s.stopped && s.generation == 6);
        // ... and the key goes (see `keep_key`), as for an unreadable one.
        let foreign = Ok(Some(saved(true, 1, 1, None)));
        assert!(!keep_key(Some(&saved(false, 3, 5, k)), &foreign, "K"));
        let torn = Err(std::io::Error::other("torn"));
        assert!(!keep_key(Some(&saved(false, 3, 5, k)), &torn, "K"));
        let newer = Ok(Some(saved(true, 4, 7, k)));
        assert!(keep_key(Some(&saved(false, 3, 5, k)), &newer, "K"));
        // Unreadable: a Stop.
        let torn = Err(std::io::Error::other("torn"));
        let (s, _) = recover(Some(saved(false, 3, 5, k)), torn, k, false);
        assert!(s.stopped && s.generation == 6);
        // A lost record keeps a readable stand-in's count.
        let (s, _) = recover(None, Ok(Some(saved(true, 7, 3, k))), k, true);
        assert!(s.stopped && s.stops >= 7);
    }

    /// A key signs only numbers of the record that names it: a record lost
    /// for a moment (a new key, stopped, not saved) and read again later
    /// starts yet another key, never the one that signed in between.
    #[test]
    fn a_key_is_never_paired_with_another_keys_numbers() {
        let old = saved(false, 1, 1, Some("K"));
        assert!(belongs_to(Some(&old), "K"));
        // Unreadable for a moment: new key K2, stopped, signed (not saved).
        assert!(!belongs_to(None, "K"));
        let (s, _) = recover(None, Ok(None), Some("K2"), true);
        assert!(s.stopped);
        // Its save failed; restarted, the old record reads again: it doesn't
        // belong to K2 (so K2 goes too, and a third key starts stopped).
        assert!(!belongs_to(Some(&old), "K2"));
        let (s, _) = recover(Some(old), Ok(None), Some("K3"), true);
        assert!(s.stopped && s.generation == 2);
        // A record saved while the guard signed nothing (no key) never
        // belongs to a key found later.
        assert!(!belongs_to(Some(&saved(true, 1, 1, None)), "K"));
    }

    /// A guard across many Stops (some brought with a higher count),
    /// Resumes (some approved long after they were asked), registry
    /// failures, saves retried later, stand-in writes that fail (the old
    /// file stays) or are cut by a crash, stand-ins left behind, keys that
    /// can't be deleted or made, and restarts (some reading no record): no
    /// key ever signs a number both stopped and not stopped, no approval
    /// resumes under a key it wasn't given for, and no restart says fewer
    /// Stops than a store it could read kept.
    #[test]
    fn across_restarts_no_key_signs_a_number_both_ways() {
        #[derive(Default)]
        struct Sim {
            /// The registry as read now, and as it is on the disk (a write
            /// whose flush failed may be in one and not the other).
            registry: Option<Saved>,
            on_disk: Option<Saved>,
            flush_fails: bool,
            stand_in: Option<Saved>,
            torn: bool,
            /// The signing key kept in the registry (its number), if any.
            stored: Option<u32>,
            /// The key in use (`None`: it signs nothing).
            key: Option<u32>,
            keys: u32,
            state: State,
            unsaved: bool,
            /// Kept nowhere yet: nothing is signed.
            undurable: bool,
            signed: std::collections::HashMap<(u32, u64), bool>,
            failing: bool,
            beside_fails: bool,
            retire_fails: bool,
            delete_fails: bool,
            make_fails: bool,
            approval: Option<(Option<u32>, u64)>,
        }
        fn name(key: Option<u32>) -> Option<String> {
            key.map(|k| format!("K{k}"))
        }
        impl Sim {
            /// The guard answers a server (what it signs, if anything).
            fn sign(&mut self) {
                let Some(key) = self.key else {
                    return;
                };
                if self.undurable {
                    return;
                }
                let was = self
                    .signed
                    .insert((key, self.state.generation), self.state.stopped);
                assert!(
                    was.is_none_or(|w| w == self.state.stopped),
                    "key {key} number {} signed both ways",
                    self.state.generation
                );
            }
            fn retire(&mut self) {
                if !self.retire_fails {
                    self.stand_in = None;
                    self.torn = false;
                }
            }
            fn save(&mut self) -> bool {
                if self.failing {
                    return false;
                }
                let record = Some(Saved {
                    state: self.state,
                    key: name(self.key),
                });
                // Written, then flushed: a failed flush leaves it read as
                // written but maybe not on the disk (and counts as failed).
                self.registry = record.clone();
                if self.flush_fails {
                    return false;
                }
                self.on_disk = record;
                self.retire();
                true
            }
            /// The power goes: the registry as on the disk, then a restart.
            fn power_cut(&mut self) {
                self.registry = self.on_disk.clone();
                self.restart(false);
            }
            /// Save the Stop: the registry, else the stand-in, else nowhere
            /// (signed nothing until one takes it).
            fn persist(&mut self) {
                self.unsaved = !self.save();
                self.undurable = self.unsaved && !self.beside();
            }
            fn beside(&mut self) -> bool {
                if self.beside_fails {
                    return false;
                }
                self.stand_in = Some(Saved {
                    state: self.state,
                    key: name(self.key),
                });
                self.torn = false;
                true
            }
            fn stop(&mut self, at_least: u32) {
                self.state.stop(at_least);
                self.persist();
                self.sign();
            }
            /// A Stop the registry refused, cut by a crash while the stand-in
            /// was written: never answered, then a restart.
            fn crash_in_stop(&mut self) {
                self.state.stop(0);
                if !self.save() {
                    self.torn = true;
                }
                self.restart(false);
            }
            /// The enforcer's second.
            fn tick(&mut self) {
                if self.unsaved {
                    self.unsaved = !self.save();
                    // Saved, or kept beside at last: it may be signed.
                    if !self.unsaved || (self.undurable && self.beside()) {
                        self.undurable = false;
                    }
                } else if self.stand_in.is_some() || self.torn {
                    self.retire();
                }
            }
            fn ask(&mut self) {
                self.approval = Some((self.key, self.state.generation));
            }
            fn approve(&mut self) {
                let Some((key, generation)) = self.approval.take() else {
                    return;
                };
                if key != self.key {
                    return;
                }
                let before = self.state;
                if self.state.resume(generation).is_ok() {
                    if self.save() {
                        self.unsaved = false;
                        self.undurable = false;
                    } else {
                        // Maybe written all the same: a Stop above it.
                        let attempted = self.state.generation;
                        self.state = before;
                        self.state.generation = self.state.generation.max(attempted);
                        self.state.stop(0);
                        self.persist();
                    }
                    self.sign();
                }
            }
            fn make(&mut self) -> Option<u32> {
                if self.make_fails {
                    return None;
                }
                self.keys += 1;
                self.stored = Some(self.keys);
                self.stored
            }
            fn restart(&mut self, unreadable: bool) {
                let saved = if unreadable {
                    None
                } else {
                    self.registry.clone()
                };
                let pending = if self.torn {
                    Err(std::io::Error::other("torn"))
                } else {
                    Ok(self.stand_in.clone())
                };
                // The most Stops a store it can read kept.
                let kept = saved
                    .as_ref()
                    .map_or(0, |s| s.state.stops)
                    .max(match &pending {
                        Ok(Some(p)) => p.state.stops,
                        _ => 0,
                    });
                let (key, renumbered) = match self.stored {
                    Some(k) if keep_key(saved.as_ref(), &pending, &format!("K{k}")) => {
                        (Some(k), false)
                    }
                    None => (self.make(), false),
                    Some(_) if self.delete_fails => (None, true),
                    Some(_) => {
                        self.stored = None;
                        (self.make(), true)
                    }
                };
                self.key = key;
                let (state, changed) = recover(saved, pending, name(key).as_deref(), renumbered);
                assert!(
                    state.stops >= kept,
                    "a count kept was lost: {} < {kept}",
                    state.stops
                );
                let _ = changed;
                self.state = state;
                // Every start: saved before anything is signed.
                self.unsaved = true;
                self.undurable = true;
                self.sign();
            }
        }
        let seed_record = Some(Saved {
            state: State::default(),
            key: None,
        });
        let mut sim = Sim {
            registry: seed_record.clone(),
            on_disk: seed_record,
            ..Default::default()
        };
        sim.restart(false);
        sim.tick();
        // Found in review: Stops only the stand-in took, a restart, then a
        // Resume.
        sim.stop(0);
        sim.ask();
        sim.approve();
        sim.failing = true;
        sim.stop(0);
        sim.stop(0);
        sim.failing = false;
        sim.restart(false);
        sim.tick();
        sim.ask();
        sim.approve();
        assert!(!sim.state.stopped);
        // An approval asked before a restart that started a new key.
        sim.stop(0);
        sim.ask();
        sim.stop(0);
        sim.restart(true);
        sim.approve();
        assert!(
            sim.state.stopped,
            "an approval for another key resumes nothing"
        );
        sim.tick();
        // Counts brought from elsewhere, kept only in the stand-in: kept
        // whole across a restart and the Resume after it.
        sim.failing = true;
        sim.stop(sim.state.stops + 1024);
        sim.stop(sim.state.stops + 1024);
        let high = sim.state.stops;
        sim.failing = false;
        sim.restart(false);
        sim.tick();
        sim.ask();
        sim.approve();
        assert!(!sim.state.stopped && sim.state.stops > high);
        // Found in review: the record unreadable for a moment and the key
        // not deletable (signing nothing), a high count only the key-less
        // stand-in took, then a restart that reads both.
        sim.stop(0);
        sim.ask();
        sim.approve();
        sim.delete_fails = true;
        sim.restart(true);
        sim.failing = true;
        sim.stop(sim.state.stops + 1024);
        let high = sim.state.stops;
        sim.restart(false);
        assert!(sim.state.stops > high, "the stand-in's count kept");
        sim.failing = false;
        sim.delete_fails = false;
        sim.restart(false);
        sim.tick();
        // Found in review: Stops no store took, after an older stand-in: not
        // signed, so a restart and a Resume can't sign their numbers the
        // other way.
        sim.failing = true;
        sim.stop(0);
        sim.beside_fails = true;
        sim.stop(0);
        sim.stop(0);
        sim.restart(false);
        sim.failing = false;
        sim.beside_fails = false;
        sim.tick();
        sim.ask();
        sim.approve();
        // Found in review: a Stop written to the registry but not flushed,
        // the stand-in failing too, a restart reading it, then the power
        // going and a Resume.
        sim.flush_fails = true;
        sim.beside_fails = true;
        sim.stop(0);
        sim.restart(false);
        sim.sign();
        sim.power_cut();
        sim.flush_fails = false;
        sim.beside_fails = false;
        sim.tick();
        sim.ask();
        sim.approve();
        // Found in review: a Resume written but not flushed: a Stop above it
        // holds, across the power going.
        sim.stop(0);
        sim.ask();
        sim.flush_fails = true;
        sim.approve();
        assert!(sim.state.stopped, "a Resume not saved resumes nothing");
        sim.flush_fails = false;
        sim.power_cut();
        assert!(sim.state.stopped);
        sim.tick();
        // And many more, mixed (a fixed sequence, the same every run).
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        for _ in 0..60_000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let at_least = if seed.is_multiple_of(4) {
                sim.state.stops.saturating_add((seed >> 32) as u32 % 3000)
            } else {
                0
            };
            match seed % 18 {
                0 | 1 => sim.stop(at_least),
                2 => sim.ask(),
                3 | 4 => sim.approve(),
                5 => sim.failing = !sim.failing,
                6 => sim.retire_fails = !sim.retire_fails,
                7 => sim.delete_fails = (seed >> 20).is_multiple_of(4),
                8 => sim.beside_fails = (seed >> 24).is_multiple_of(3),
                9 => sim.make_fails = (seed >> 28).is_multiple_of(6),
                10 | 11 => sim.tick(),
                12 => sim.restart(false),
                13 => sim.restart(seed.is_multiple_of(3)),
                14 => sim.crash_in_stop(),
                15 => sim.flush_fails = (seed >> 36).is_multiple_of(3),
                16 => sim.power_cut(),
                _ => sim.sign(),
            }
        }
        assert!(sim.keys > 2 && sim.signed.len() > 1000);
    }

    /// Every Stop (a Resume that couldn't be saved among them) sets what it
    /// ends: the earliest moment holds, the newest number counts.
    #[test]
    fn every_stop_sets_what_it_ends() {
        let mut s = State::default();
        let mut ending = None;
        stop_ending(&mut s, &mut ending, 0);
        let (first, n1) = ending.unwrap();
        assert_eq!(n1, s.generation);
        // Resumed within the grace: the ending stays.
        s.resume(s.generation).unwrap();
        assert!(ending.is_some());
        // A Resume that couldn't be saved: a Stop above it, which ends what
        // was taken in before it (the machine taken in while stopped too).
        let before = State { stopped: true, ..s };
        let attempted = s.generation;
        s = before;
        s.generation = s.generation.max(attempted);
        stop_ending(&mut s, &mut ending, 0);
        let (t, n2) = ending.unwrap();
        assert_eq!(t, first, "the earliest moment holds");
        assert!(n2 > n1 && n2 == s.generation && s.stopped);
        // With no ending pending, a new one.
        let mut fresh = None;
        stop_ending(&mut s, &mut fresh, 0);
        assert_eq!(fresh.unwrap().1, s.generation);
    }

    #[test]
    fn nobody_pushes_the_count_to_its_end() {
        let mut s = State::default();
        s.stop(u32::MAX);
        assert_eq!(s.stops, MAX_JUMP);
        let mut top = State {
            stopped: false,
            stops: u32::MAX,
            generation: u64::MAX,
        };
        top.stop(0);
        // Saturates (never wraps to an old number).
        assert_eq!((top.stops, top.generation), (u32::MAX, u64::MAX));
        assert!(top.stopped);
    }

    #[test]
    fn with_the_guard_only_it_resumes() {
        let guard = State {
            stopped: true,
            stops: 4,
            generation: 4,
        };
        // A Resume written next to the machine (count 4 or above) counts
        // for nothing: the guard says stopped.
        assert!(with_guard(false, 4, guard).stopped);
        assert!(with_guard(false, 99, guard).stopped);
        let resumed = State {
            stopped: false,
            stops: 4,
            generation: 4,
        };
        assert!(!with_guard(false, 4, resumed).stopped);
        // A Stop counted here the guard hasn't got yet stops.
        let l = with_guard(true, 5, resumed);
        assert!(l.stopped && l.stops == 5);
        // A high count kept here without a Stop (written by anyone) doesn't,
        // nor counts.
        let high = with_guard(false, u32::MAX, resumed);
        assert!(!high.stopped && high.stops == 4);
    }

    #[test]
    fn the_administrator_prompt_must_be_one_only_the_user_answers() {
        let defaults = Uac::default();
        // An administrator at Windows' default level: not enough.
        assert!(uac_ok(defaults, true).is_err());
        // A standard account at the defaults: an administrator's password
        // on the secure desktop.
        assert!(uac_ok(defaults, false).is_ok());
        let always = Uac {
            admin_prompt: Some(2),
            ..Uac::default()
        };
        assert!(uac_ok(always, true).is_ok());
        let off = Uac {
            enable_lua: Some(0),
            admin_prompt: Some(2),
            ..Uac::default()
        };
        assert!(uac_ok(off, true).unwrap_err().contains("off"));
        let not_secure = Uac {
            secure_desktop: Some(0),
            admin_prompt: Some(2),
            ..Uac::default()
        };
        assert!(uac_ok(not_secure, true).is_err());
        let unknown = Uac {
            admin_prompt: Some(2),
            unreadable: true,
            ..Uac::default()
        };
        assert!(uac_ok(unknown, true).is_err());
        assert!(uac_ok(unknown, false).is_err());
        assert!(
            uac_ok(
                Uac {
                    user_prompt: Some(2),
                    ..Uac::default()
                },
                false
            )
            .is_err()
        );
    }

    #[test]
    fn what_blocks_commands_and_the_screen() {
        let says = Here::Says(State::default());
        assert!(
            blocked(&Here::Silent("down".into()), None)
                .unwrap()
                .contains("isn't answering")
        );
        let failed: Result<(), String> = Err("refused".into());
        assert!(
            blocked(&says, Some(&failed))
                .unwrap()
                .contains("didn't take this machine in")
        );
        // Elsewhere than Windows there is no guard to need.
        if !cfg!(windows) {
            assert_eq!(blocked(&Here::Absent, None), None);
            assert_eq!(blocked(&says, Some(&Ok(()))), None);
            assert!(status_for(None).is_none());
        }
    }

    #[test]
    fn a_signed_word_holds_only_for_its_nonce_and_its_stop() {
        let (private, public) = tessera_secure::sign::generate().unwrap();
        let s = State {
            stopped: false,
            stops: 3,
            generation: 5,
        };
        let sig = hex(&tessera_secure::sign::sign(&private, &attest_message("n1", s)).unwrap());
        let key = hex(&public);
        assert!(verify_attestation(&key, "n1", s, &sig));
        // Another nonce (a replay), another Stop, another key: no.
        assert!(!verify_attestation(&key, "n2", s, &sig));
        let stopped = State { stopped: true, ..s };
        assert!(!verify_attestation(&key, "n1", stopped, &sig));
        let (_, other) = tessera_secure::sign::generate().unwrap();
        assert!(!verify_attestation(&hex(&other), "n1", s, &sig));
        assert!(!verify_attestation("zz", "n1", s, &sig));
        assert_eq!(
            unhex(&hex(&[0, 1, 254, 255])).unwrap(),
            vec![0, 1, 254, 255]
        );
        assert!(unhex("abc").is_none());
    }

    #[test]
    fn requests_and_answers_read_back() {
        for r in [
            Request::Status,
            Request::Stop { at_least: 3 },
            Request::Resume {
                generation: 3,
                key: Some("ab".into()),
            },
            Request::Adopt,
        ] {
            let text = serde_json::to_string(&r).unwrap();
            assert!(text.len() < MAX_MESSAGE);
            assert_eq!(serde_json::from_str::<Request>(&text).unwrap(), r);
        }
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"op":"stop"}"#).unwrap(),
            Request::Stop { at_least: 0 }
        );
        assert!(serde_json::from_str::<Request>(r#"{"op":"resume"}"#).is_err());
    }
}
