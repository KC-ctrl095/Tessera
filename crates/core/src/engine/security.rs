//! Settings → Privacy & safety, the sandbox the agent's commands get, the settings
//! agents may change with approval, and credentials typed into a chat.
//!
//! A credential in a recognisable format is taken out of the user's message
//! before it is stored or sent anywhere ([`Inner::capture_secrets`]); the
//! agent moves the rest with `save_secret` ([`Inner::save_chat_secret`]),
//! which rewrites every stored item of the chat that holds the value.
//! Reasoning a provider signed has to be replayed verbatim while its turn
//! runs, so that is rewritten when the turn ends.
//!
//! Where it goes is the user's choice (`secret_saving`): Passwords & keys at
//! once, or kept for that chat alone, with a card asking whether to save it
//! ("Ask me first", answered by [`Inner::decide_chat_secret`]) or not
//! ("Don't save"). The model gets only `$NAME` in every case.

use super::*;
use crate::auth::{SecretPlace, SecretScope, SecretUse, SecretUser};
use crate::config::{SandboxMode, SecretMachines, SecretSaving};
use crate::tools::secrets::{ChatSecret, Code, find_codes, find_known};
use tessera_proto::ChatSecretChoice;

/// A folder only the user can enter, made if missing.
#[cfg(unix)]
fn make_private_dir(d: &Path) {
    use std::os::unix::fs::DirBuilderExt;
    let _ = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(d);
}

/// Saved-secret notices start with this; the `$NAME`s in them are the
/// variables the agent uses (clients may render them as a card).
pub(crate) const SAVED_PREFIX: &str = "Saved to Passwords & keys: ";
/// The same for secrets kept for one chat alone.
pub(crate) const KEPT_PREFIX: &str = "Kept for this chat only: ";
/// A secret kept for one chat that waits for the user's answer: clients
/// show it as a card with Save and Don't save (`decide_chat_secret`).
pub(crate) const ASK_PREFIX: &str = "Save to Passwords & keys? ";
/// Where the last one-time code the user sent in a chat is kept.
const ONE_TIME_CODE: &str = "ONE_TIME_CODE";

/// A credential from a message, as stored.
struct Stored {
    name: String,
    /// What the message gets instead (`NAME`, `NAME_OTP`), without `$`.
    var: String,
    /// Stored just now (not a value typed again).
    fresh: bool,
}

/// `base`, else `base_2`, `base_3`… whichever no secret has.
fn free_name(taken: &[tessera_proto::SecretInfo], base: &str) -> String {
    std::iter::once(base.to_string())
        .chain((2..).map(|i| format!("{base}_{i}")))
        .find(|n| !taken.iter().any(|s| &s.name == n))
        .expect("a free name")
}

/// `Shop & Co.` → `SHOP_CO` (a secret's name after the site it is for).
fn secret_name(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_uppercase());
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    out.trim_end_matches('_').chars().take(40).collect()
}

/// Most folder rules a user can set (each is a mount in the sandbox).
const MAX_FOLDERS: usize = 32;

/// (value, `$VARIABLE`) pairs to swap in a chat's history.
type Swaps = Vec<(String, String)>;

/// What to go over again when each session's turn ends.
fn later() -> &'static Mutex<HashMap<String, Swaps>> {
    static L: std::sync::OnceLock<Mutex<HashMap<String, Swaps>>> = std::sync::OnceLock::new();
    L.get_or_init(Default::default)
}

// Secure entry is app-only; the completed tool card contains metadata alone.
impl Inner {
    /// Structured tool metadata is text too. Binary image payloads and signed
    /// provider reasoning stay outside this JSON-only boundary.
    pub(super) fn redact_metadata(&self, value: &mut Value) {
        crate::tools::redaction::value(
            value,
            &self.redaction_secrets(),
            self.config.read().security.redact_patterns,
        );
    }

    pub(super) fn redact_entry(&self, body: &mut EntryBody) {
        match body {
            EntryBody::ToolResult {
                content, details, ..
            } => {
                self.redact_parts(content);
                if let Some(details) = details {
                    self.redact_metadata(details);
                }
            }
            EntryBody::Assistant { content, .. } => {
                for part in content {
                    if let AssistantPart::ToolCall { arguments, .. } = part {
                        self.redact_metadata(arguments);
                    }
                }
            }
            _ => {}
        }
    }
    fn save_entered_secret(
        &self,
        session_id: &str,
        call_id: &str,
        fields: std::collections::BTreeMap<String, String>,
        chat_only: bool,
    ) -> Result<(), ApiError> {
        let _saving = self.secret_entry.lock();
        let rt = self.rt(session_id)?;
        if !self
            .claude_capabilities(&rt.st.lock())
            .iter()
            .any(|n| n == "save_secret")
        {
            return Err(bad("Password entry is not available to this chat."));
        }
        let entry = self
            .store
            .entries(session_id, 0)
            .map_err(internal)?
            .into_iter()
            .find_map(|e| match e.body {
                EntryBody::ToolResult {
                    call_id: id,
                    tool,
                    is_error: false,
                    details: Some(d),
                    ..
                } if id == call_id && tool == "save_secret" => d.get("secret_entry").cloned(),
                _ => None,
            })
            .ok_or_else(|| bad("No secure password entry card for this call."))?;
        if entry.get("saved").and_then(Value::as_bool) == Some(true) {
            return Err(bad("This card was already saved."));
        }
        let name = entry["name"]
            .as_str()
            .ok_or_else(|| bad("Missing credential name."))?;
        let title = entry["title"].as_str().unwrap_or(name);
        let kind = if entry["kind"] == "login" {
            SecretKind::Login
        } else {
            SecretKind::Token
        };
        let allowed: &[&str] = if kind == SecretKind::Token {
            &["value"]
        } else {
            &["username", "password", "url"]
        };
        if fields.is_empty()
            || fields
                .iter()
                .any(|(k, v)| !allowed.contains(&k.as_str()) || v.trim().is_empty())
            || (kind == SecretKind::Token && !fields.contains_key("value"))
            || (kind == SecretKind::Login && !fields.contains_key("password"))
        {
            return Err(bad("Fill in the key or login password."));
        }
        if crate::tools::secrets::claude_credential(
            name,
            title,
            fields.get("url").map_or("", String::as_str),
        ) || fields
            .values()
            .any(|v| v.contains("sk-ant-oat") || v.contains("sk-ant-sid"))
        {
            return Err(bad(
                "Claude sign-in belongs to your own Claude CLI, not Tessera's password library.",
            ));
        }
        let hosts = fields
            .get("url")
            .and_then(|u| url::Url::parse(u).ok())
            .and_then(|u| u.host_str().map(str::to_string))
            .into_iter()
            .collect();
        let mut secret = ChatSecret {
            name: name.into(),
            kind,
            description: title.into(),
            hosts,
            fields: Default::default(),
            chat_only,
        };
        let saving = self.check_chat_secret(session_id, &secret).map_err(bad)?;
        secret.chat_only |= saving == SecretSaving::ChatOnly;
        secret.description = crate::tools::secrets::chat_secret_title(
            Some(title),
            name,
            kind,
            &fields,
            &secret.hosts,
            &[],
        );
        secret.fields = fields;
        self.store_chat_secret(session_id, secret).map_err(bad)?;
        let mut entries = self.store.entries(session_id, 0).map_err(internal)?;
        if let Some(e) = entries
            .iter_mut()
            .find(|e| matches!(&e.body, EntryBody::ToolResult { call_id: id, .. } if id == call_id))
        {
            if let EntryBody::ToolResult {
                details: Some(d), ..
            } = &mut e.body
            {
                d["secret_entry"]["saved"] = json!(true);
            }
            self.store.replace_entry(session_id, e).map_err(internal)?;
            let _ = self.events.send(Event::EntriesChanged {
                session_id: session_id.into(),
                entries: vec![e.clone()],
            });
        }
        self.activity_resolved(&self.root_session(session_id), "secret");
        self.save(&rt);
        Ok(())
    }
    pub(super) fn security_request(&self, req: Request) -> ApiResult {
        match req {
            Request::GetSecurity => to_value(self.security_info()),
            Request::SetSecurity {
                sandbox,
                folders,
                ask_outside,
                redact_patterns,
                save_secrets_from_chat,
                secret_saving,
                secret_machines,
                secret_machines_picked,
                bot_handoff,
                default_permission,
            } => {
                let folders = folders.map(|f| self.check_folders(f)).transpose()?;
                // Only machines this server knows (a removed one can't come back this way).
                let picked = secret_machines_picked.map(|ids| {
                    let known = self.machine_list();
                    let mut ids: Vec<String> = ids
                        .into_iter()
                        .filter(|id| known.iter().any(|m| &m.id == id))
                        .collect();
                    ids.sort();
                    ids.dedup();
                    ids
                });
                {
                    let mut c = self.config.write();
                    let s = &mut c.security;
                    if let Some(v) = sandbox {
                        s.sandbox = v;
                    }
                    if let Some(v) = folders {
                        s.folders = v;
                    }
                    if let Some(v) = ask_outside {
                        s.ask_outside = v;
                    }
                    if let Some(v) = redact_patterns {
                        s.redact_patterns = v;
                    }
                    if let Some(v) = save_secrets_from_chat {
                        s.save_secrets_from_chat = v;
                    }
                    if let Some(v) = secret_saving {
                        s.secret_saving = v;
                    }
                    if let Some(v) = secret_machines {
                        s.secret_machines = v;
                    }
                    if let Some(v) = picked {
                        s.secret_machines_picked = v;
                    }
                    if let Some(v) = bot_handoff {
                        s.bot_handoff = v;
                    }
                    // The user's own devices only: agents have no request for
                    // it, and their `settings` tool can't reach `security.*`.
                    if let Some(v) = default_permission {
                        s.default_permission = Some(v);
                    }
                    c.save(&self.paths).map_err(internal)?;
                }
                self.invalidate_prompts();
                to_value(self.security_info())
            }
            _ => Err(bad("not a security request")),
        }
    }

    // ---- stored secrets: who may use them, and where --------------------

    pub(super) fn secret_request(&self, req: Request) -> ApiResult {
        match req {
            Request::ListSecrets => to_value(self.auth.list_secrets()),
            Request::SaveChatSecret {
                session_id,
                call_id,
                fields,
                chat_only,
            } => {
                self.save_entered_secret(&session_id, &call_id, fields.0, chat_only)?;
                Ok(Value::Null)
            }
            Request::SaveSecret {
                name,
                kind,
                description,
                fields,
                remove_fields,
                ask,
                hosts,
                bots,
                on_server,
                on_machines,
                new_name,
                machines_default,
            } => {
                if crate::tools::secrets::claude_credential(
                    &name,
                    &description,
                    fields.get("url").map_or("", String::as_str),
                ) || fields
                    .values()
                    .any(|v| v.contains("sk-ant-oat") || v.contains("sk-ant-sid"))
                {
                    return Err(bad(
                        "Claude sign-in belongs to your own Claude CLI, not Tessera's password library.",
                    ));
                }
                // A kept one's card and the chat name it: it keeps its name
                // until it is saved.
                if new_name.as_deref().is_some_and(|n| n.trim() != name.trim())
                    && self.kept_for(name.trim()).is_some()
                {
                    return Err(bad(format!(
                        "{} is kept for one chat only: save it to Passwords & keys first, then rename it",
                        name.trim()
                    )));
                }
                let info = self
                    .auth
                    .save_secret_scoped(
                        name.trim(),
                        kind,
                        &description,
                        fields,
                        &remove_fields,
                        ask,
                        hosts,
                        SecretScope {
                            bots,
                            on_server,
                            on_machines,
                            new_name,
                            chat: None,
                            machines_default,
                        },
                    )
                    .map_err(|e| bad(e.to_string()))?;
                if info.name != name.trim() {
                    self.secret_renamed_for_bots(name.trim(), Some(&info.name));
                }
                self.invalidate_prompts();
                to_value(info)
            }
            Request::DecideChatSecret {
                session_id,
                name,
                choice,
            } => {
                self.decide_chat_secret(&session_id, &name, choice)?;
                Ok(Value::Null)
            }
            Request::SetSecretTitle { name, title } => {
                let info = self
                    .auth
                    .set_secret_title(name.trim(), &title)
                    .map_err(|e| bad(e.to_string()))?;
                self.invalidate_prompts();
                to_value(info)
            }
            Request::SuggestSecretTitles => {
                let machines: Vec<String> =
                    self.machine_list().into_iter().map(|m| m.name).collect();
                to_value(crate::tools::secrets::suggest_titles(
                    &self.auth.list_secrets(),
                    &machines,
                ))
            }
            Request::SetSecret { name, value } => {
                if crate::tools::secrets::claude_credential(&name, "", "")
                    || value.contains("sk-ant-oat")
                    || value.contains("sk-ant-sid")
                {
                    return Err(bad(
                        "Claude sign-in belongs to your own Claude CLI, not Tessera's password library.",
                    ));
                }
                self.auth
                    .set_secret(name.trim(), &value)
                    .map_err(|e| bad(e.to_string()))?;
                self.invalidate_prompts();
                Ok(Value::Null)
            }
            Request::DeleteSecret { name } => {
                // One kept for a chat: its card there says it's gone.
                if let Some(chat) = self.kept_for(&name) {
                    self.decide_chat_secret(&chat, &name, ChatSecretChoice::Forget)?;
                    return Ok(Value::Null);
                }
                if self.auth.delete_secret(&name).map_err(internal)? {
                    self.secret_renamed_for_bots(&name, None);
                }
                self.invalidate_prompts();
                Ok(Value::Null)
            }
            _ => Err(bad("not a secret request")),
        }
    }

    /// The chat a stored secret is kept for alone, if it is.
    fn kept_for(&self, name: &str) -> Option<String> {
        self.auth
            .list_secrets()
            .into_iter()
            .find(|s| s.name == name)
            .and_then(|s| s.chat)
    }

    /// `user` (a bot's chat) just saved `name` for every chat: that bot may
    /// use it, whatever it gets of the others.
    fn let_bot_use(&self, user: &SecretUser, name: &str) {
        let Some(id) = user.bot() else {
            return;
        };
        let changed = {
            let mut bots = self.bots.write();
            let Some(b) = bots.iter_mut().find(|b| b.id == id) else {
                return;
            };
            // A bot made before the setting uses all of them already.
            let Some(s) = b.secrets.as_mut() else {
                return;
            };
            if s.each.get(name).copied().unwrap_or(s.default) {
                return;
            }
            if s.default {
                s.each.remove(name);
            } else {
                s.each.insert(name.to_string(), true);
            }
            b.clone()
        };
        self.bot_secrets_changed(changed);
    }

    /// A saved password was renamed (`to`) or deleted (`None`): bots' own
    /// choices about it follow it, so none sticks to a name nothing has or
    /// passes to a new password that reuses the name.
    fn secret_renamed_for_bots(&self, from: &str, to: Option<&str>) {
        let changed: Vec<Bot> = {
            let mut bots = self.bots.write();
            bots.iter_mut()
                .filter_map(|b| {
                    let s = b.secrets.as_mut()?;
                    let on = s.each.remove(from)?;
                    if let Some(to) = to {
                        s.each.insert(to.to_string(), on);
                    }
                    Some(b.clone())
                })
                .collect()
        };
        for b in changed {
            self.bot_secrets_changed(b);
        }
    }

    fn bot_secrets_changed(&self, bot: Bot) {
        if let Err(e) = self.store.put_bot(&bot) {
            tracing::warn!("saving bot {}: {e}", bot.id);
        }
        let _ = self.events.send(Event::BotUpdated { bot });
    }

    /// What a session acts as when it uses secrets: nobody in a private
    /// chat or a helper; its bot in a bot's chat, a channel (the bot
    /// speaking), a bot's scheduled duty or a subagent of any of those
    /// (subagents carry their root's bot); else an ordinary chat. Either
    /// way in its chat (a subagent: its root chat), whose own secrets it
    /// may use too.
    pub(super) fn secret_user(&self, st: &RtState) -> SecretUser {
        if st.info.private || st.info.internal {
            return SecretUser::Nobody;
        }
        let user = match self.speaker_bot(st) {
            Some(b) => SecretUser::Bot(b.id, b.secrets),
            None => SecretUser::Chat,
        };
        let chat = match (&st.info.kind, &st.info.parent_id) {
            (SessionKind::Subagent, Some(parent)) => self.root_session(parent),
            _ => st.info.id.clone(),
        };
        SecretUser::InChat {
            chat,
            user: Box::new(user),
        }
    }

    /// The secrets a command in `session_id` names that it may use at
    /// `place`. Every tool that passes secrets on goes through here.
    pub(super) fn secret_env(
        &self,
        session_id: &str,
        command: &str,
        place: SecretPlace,
    ) -> SecretUse {
        let user = match self.rt(session_id) {
            Ok(rt) => self.secret_user(&rt.st.lock()),
            Err(_) => SecretUser::Nobody,
        };
        self.auth
            .secrets_for(command, &user, place, &self.secret_default_machines())
    }

    /// The machines (ids) where saved passwords that don't choose for
    /// themselves may be used (`[security] secret_machines`).
    pub(super) fn secret_default_machines(&self) -> Vec<String> {
        let (mode, picked) = {
            let c = self.config.read();
            (
                c.security.secret_machines,
                c.security.secret_machines_picked.clone(),
            )
        };
        match mode {
            SecretMachines::Off => Vec::new(),
            SecretMachines::Picked => picked,
            SecretMachines::All => self.machine_list().into_iter().map(|m| m.id).collect(),
        }
    }

    /// A machine was removed: it isn't picked for saved passwords any more
    /// (turned on again, it starts without).
    pub(super) fn forget_secret_machine(&self, id: &str) {
        let mut c = self.config.write();
        if c.security.secret_machines_picked.iter().any(|m| m == id) {
            c.security.secret_machines_picked.retain(|m| m != id);
            if let Err(e) = c.save(&self.paths) {
                tracing::warn!("saving the config: {e}");
            }
        }
    }

    /// `bot` may use stored secrets that `session_id` may not.
    pub(super) fn bot_has_own_secrets(&self, session_id: &str, bot: &str) -> bool {
        let (Some(b), Ok(rt)) = (self.find_bot(bot), self.rt(session_id)) else {
            return false;
        };
        let me = self.secret_user(&rt.st.lock());
        self.auth
            .usable_beyond(&SecretUser::Bot(b.id, b.secrets), &me)
    }

    /// The `# Secrets` block for this session: only what it may use.
    pub(super) fn secrets_prompt(&self, st: &RtState) -> Option<String> {
        self.secrets_block(st, None)
    }

    /// [`Self::secrets_prompt`] with at most `cap` of them, the most
    /// recently used first, and a line saying how to list the rest (a lean
    /// start's, where `save_secret` lists them).
    pub(super) fn secrets_block(&self, st: &RtState, cap: Option<usize>) -> Option<String> {
        let mut secrets = self
            .auth
            .secret_vars(&self.secret_user(st), &self.secret_default_machines());
        if secrets.is_empty() {
            return None;
        }
        // Machine names, only when one may be used on a machine.
        let machines = if secrets.iter().any(|s| !s.on_machines.is_empty()) {
            self.machine_list()
        } else {
            Vec::new()
        };
        for s in &mut secrets {
            s.on_machines = s
                .on_machines
                .iter()
                .filter_map(|id| machines.iter().find(|m| &m.id == id))
                .map(|m| m.name.clone())
                .collect();
        }
        secrets.retain(|s| s.on_server || !s.on_machines.is_empty());
        if secrets.is_empty() {
            return None;
        }
        let more = match cap {
            Some(cap) if secrets.len() > cap => {
                // Stable: never-used ones stay in name order.
                secrets.sort_by_key(|s| std::cmp::Reverse(s.last_used.unwrap_or(0)));
                let more = secrets.len() - cap;
                secrets.truncate(cap);
                secrets.sort_by(|a, b| a.name.cmp(&b.name));
                more
            }
            _ => 0,
        };
        let mut block = crate::prompt::secrets_block(&secrets);
        if more > 0 {
            block.push_str(&format!(
                "- …and {more} more: `save_secret` with `action: \"list\"` lists them all\n"
            ));
        }
        Some(block)
    }

    pub(super) fn security_info(&self) -> SecurityInfo {
        let (c, shared) = {
            let cfg = self.config.read();
            (cfg.security.clone(), cfg.shared_permission())
        };
        let mut protected: Vec<String> = self
            .protected_paths()
            .roots
            .iter()
            .filter(|r| r.exists())
            .map(|r| r.display().to_string())
            .collect();
        protected.dedup();
        SecurityInfo {
            sandbox: c.sandbox,
            sandbox_status: crate::sandbox::describe(),
            sandbox_complete: crate::sandbox::support().complete(),
            sandbox_hint: crate::sandbox::hint(),
            protected,
            folders: c.folders,
            ask_outside: c.ask_outside,
            redact_patterns: c.redact_patterns,
            require_e2ee: c.require_e2ee,
            save_secrets_from_chat: c.save_secrets_from_chat,
            secret_saving: c.secret_saving,
            secret_machines: c.secret_machines,
            secret_machines_picked: c.secret_machines_picked,
            bot_handoff: c.bot_handoff,
            default_permission: shared,
        }
    }

    /// Full paths of existing folders, outside Tessera's own; later entries
    /// for the same folder win.
    fn check_folders(&self, list: Vec<FolderAccess>) -> Result<Vec<FolderAccess>, ApiError> {
        let protected = self.protected_paths().canonical();
        let mut out: Vec<FolderAccess> = Vec::new();
        for f in list {
            let raw = f.path.trim();
            let path = crate::util::expand_home(raw);
            if !path.is_absolute() {
                return Err(bad(format!("`{raw}` is not a full path")));
            }
            let canon = crate::platform::canonicalize(&path)
                .ok()
                .filter(|p| p.is_dir())
                .ok_or_else(|| bad(format!("there is no folder `{raw}`")))?;
            if canon.parent().is_none() {
                return Err(bad(
                    "the whole disk can't be a folder rule; turn off asking before writing elsewhere instead",
                ));
            }
            if protected.roots.iter().any(|r| canon.starts_with(r)) {
                return Err(bad(format!(
                    "`{raw}` is inside Tessera's own data or settings, which agents never get"
                )));
            }
            let path = canon.display().to_string();
            out.retain(|o| o.path != path);
            out.push(FolderAccess {
                path,
                write: f.write,
            });
        }
        if out.len() > MAX_FOLDERS {
            return Err(bad(format!("at most {MAX_FOLDERS} folders")));
        }
        Ok(out)
    }

    pub(super) fn write_access(&self) -> crate::tools::WriteAccess {
        let c = self.config.read();
        crate::tools::WriteAccess {
            folders: c
                .security
                .folders
                .iter()
                .map(|f| {
                    (
                        crate::tools::canonical(&crate::util::expand_home(&f.path)),
                        f.write,
                    )
                })
                .collect(),
            ask_outside: c.security.ask_outside,
        }
    }

    /// What "Allow for this session" remembers for an approval `key` in
    /// this chat: each part of it ([`crate::tools::key_parts`]), and in a
    /// channel under the speaking bot's name, so one bot's answer never
    /// covers another bot's actions.
    pub(super) fn session_keys(&self, st: &RtState, key: &str) -> Vec<String> {
        let bot = is_room(&st.info).then(|| self.speaker_id(st)).flatten();
        crate::tools::key_parts(key)
            .into_iter()
            .map(|k| match &bot {
                Some(b) => format!("bot:{b}:{k}"),
                None => k,
            })
            .collect()
    }

    /// The user already allowed all of `keys` for this session.
    pub(super) fn session_allowed(st: &RtState, keys: &[String]) -> bool {
        keys.iter().all(|k| st.internal.allowed.contains(k))
    }

    pub(super) fn allow_for_session(st: &mut RtState, keys: Vec<String>) {
        for k in keys {
            if !st.internal.allowed.contains(&k) {
                st.internal.allowed.push(k);
            }
        }
    }

    /// How to confine commands: what to hide and keep, the folder rules,
    /// and whether running unconfined is refused. `None` = don't sandbox.
    pub(super) fn sandbox_spec(&self) -> Option<(crate::sandbox::Spec, bool)> {
        let (mode, folders) = {
            let c = self.config.read();
            (c.security.sandbox, c.security.folders.clone())
        };
        if mode == SandboxMode::Off {
            return None;
        }
        let p = self.protected_paths();
        // Files attached to chats: each chat's commands see only its own
        // ([`Self::session_sandbox`]).
        let uploads = self.paths.uploads_dir();
        // What stays visible (and what is hidden) must exist before a view
        // is made.
        #[cfg(unix)]
        for d in p
            .except
            .iter()
            .filter(|d| p.roots.iter().any(|r| d.starts_with(r)))
            .chain([&uploads])
        {
            make_private_dir(d);
        }
        Some((
            crate::sandbox::Spec {
                hide: p
                    .roots
                    .into_iter()
                    .chain(crate::sandbox::session_hides())
                    .chain([uploads.clone()])
                    .collect(),
                keep: p
                    .except
                    .into_iter()
                    .filter(|e| *e != uploads)
                    .chain(crate::sandbox::session_keeps())
                    .collect(),
                folders: crate::sandbox::protect_startup(
                    with_program_kept(
                        folders
                            .iter()
                            .map(|f| (crate::util::expand_home(&f.path), f.write))
                            .collect(),
                        crate::node::server_program_paths(dirs::home_dir().as_deref()),
                    ),
                    crate::node::server_startup_paths(dirs::home_dir().as_deref()),
                ),
                writable: None,
                group: String::new(),
            },
            mode == SandboxMode::Strict,
        ))
    }

    /// The sandbox for one chat's commands (and its subagents'): the
    /// files attached to that chat are the only uploads they see, and
    /// where commands share a process namespace the chat has its own.
    pub(super) fn session_sandbox(&self, session_id: &str) -> Option<(crate::sandbox::Spec, bool)> {
        let (mut spec, strict) = self.sandbox_spec()?;
        let root = self.root_session(session_id);
        let uploads = self.paths.uploads_dir();
        let own = uploads.join(&root);
        #[cfg(unix)]
        make_private_dir(&own);
        spec.keep.push(own);
        // A subagent's own files (a signature it asked for).
        let sub = uploads.join(session_id);
        if session_id != root && sub.is_dir() {
            spec.keep.push(sub);
        }
        spec.group = root;
        Some((spec, strict))
    }

    /// `path` is in the attached files of another chat than `session_id`'s.
    pub(super) fn other_chats_upload(&self, session_id: &str, path: &Path) -> bool {
        let uploads = crate::tools::canonical(&self.paths.uploads_dir());
        let Ok(rel) = crate::tools::canonical(path)
            .strip_prefix(&uploads)
            .map(Path::to_path_buf)
        else {
            return false;
        };
        let Some(first) = rel.components().next() else {
            return false;
        };
        let first = first.as_os_str().to_string_lossy();
        first != session_id && first != self.root_session(session_id)
    }

    /// What the agent is told about the sandbox and its limits.
    pub(super) fn sandbox_prompt(&self) -> Option<String> {
        let c = self.config.read().security.clone();
        let s = crate::sandbox::support();
        if c.sandbox == SandboxMode::Off || !s.available {
            return None;
        }
        let mut text = "# Sandbox\n`bash` keeps user rights: Tessera data/settings hidden, skills read-only (`skill_manage`/`edit`), no desktop (D-Bus, user systemctl, tmux, GPG) or sudo/setuid. Give root/session commands to the user.".to_string();
        if s.complete() && s.landlock < 2 {
            text.push_str(" Commands see only processes Tessera's commands started.");
        }
        let ro: Vec<&str> = c
            .folders
            .iter()
            .filter(|f| !f.write)
            .map(|f| f.path.as_str())
            .take(8)
            .collect();
        if !ro.is_empty() {
            text.push_str(&format!(" Read-only for you: {}.", ro.join(", ")));
        }
        text.push_str(" Change Tessera's settings only with `settings`.");
        Some(text)
    }

    // ---- settings agents may change -------------------------------------

    /// Tessera's settings without credentials (header and environment
    /// values are hidden, known secrets scrubbed).
    pub(super) fn settings_text(&self) -> String {
        let mut c = self.config.read().clone();
        for p in &mut c.providers {
            p.headers.values_mut().for_each(|v| *v = "[hidden]".into());
        }
        for m in &mut c.mcp_servers {
            m.env.values_mut().for_each(|v| *v = "[hidden]".into());
            m.headers.values_mut().for_each(|v| *v = "[hidden]".into());
        }
        let text = toml::to_string_pretty(&c).unwrap_or_default();
        // Passwords in addresses (`https://user:pass@host`).
        static USERINFO: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let userinfo =
            USERINFO.get_or_init(|| regex::Regex::new(r"://[^/\s:@]+:[^/\s@]+@").unwrap());
        let text = userinfo.replace_all(&text, "://[hidden]@");
        format!(
            "{}\n# `settings set` can change: default_model, utility_model, models.*, prices.*, pricing.*, agent.* (not agent.permission), context.*, memory.*, skills.disabled, lsp.*, web.search, web.chain, browser.enabled/headless/idle_minutes/max_tabs/freeze_seconds/scale, server.name, and this chat's chat.helpers/helper_steps/helper_minutes. Everything else is the user's (Settings in the app).",
            crate::tools::redact(&text, &self.redaction_secrets()).trim_end()
        )
    }

    /// Set one setting the user approved (`path` already checked).
    pub(super) fn change_setting(&self, path: &[String], value: &Value) -> Result<String, String> {
        if let Some(why) = crate::tools::settings::refusal(path) {
            return Err(why);
        }
        let key = path.join(".");
        let mut root = toml::Value::try_from(&*self.config.read()).map_err(|e| e.to_string())?;
        let new = if value.is_null() {
            None
        } else {
            Some(
                toml::Value::try_from(value)
                    .map_err(|e| format!("not a value for `{key}`: {e}"))?,
            )
        };
        let mut at = &mut root;
        for (i, seg) in path.iter().enumerate() {
            let table = at
                .as_table_mut()
                .ok_or_else(|| format!("`{}` is not a section", path[..i].join(".")))?;
            if i + 1 == path.len() {
                match &new {
                    Some(v) => {
                        table.insert(seg.clone(), v.clone());
                    }
                    None => {
                        table.remove(seg);
                    }
                }
                break;
            }
            at = table
                .entry(seg.clone())
                .or_insert_with(|| toml::Value::Table(Default::default()));
        }
        let cfg: Config = root
            .try_into()
            .map_err(|e| format!("`{key}` can't be set to {value}: {e}"))?;
        cfg.save(&self.paths).map_err(|e| e.to_string())?;
        if path.first().is_some_and(|p| p == "lsp") {
            *self.lsp.write() = cfg
                .lsp
                .enabled
                .then(|| crate::lsp::LspManager::new(cfg.lsp.clone()));
        }
        *self.config.write() = cfg;
        self.refresh_helper_gates();
        self.invalidate_prompts();
        Ok(match new {
            Some(_) => format!(
                "Set `{key}` to {value}. New requests use it; browser settings apply after Tessera restarts."
            ),
            None => format!("Removed `{key}` (back to its default)."),
        })
    }

    // ---- secrets typed into a chat --------------------------------------

    /// Take credentials in a recognisable format out of a message the user
    /// is sending, before it is stored or seen, and put their `$NAME` in
    /// their place: saved as secrets (named by what they are), or kept for
    /// this chat alone when the user wants to be asked first or not to save
    /// them (`secret_saving`). Private chats keep nothing, so there they are
    /// only removed. Returns the notices to show after the message.
    pub(super) fn capture_secrets(
        &self,
        session_id: &str,
        content: &mut [ContentPart],
    ) -> Result<Vec<(NoticeLevel, String)>, ApiError> {
        let mut notes = Vec::new();
        // Claude sign-in never enters a transcript or a vault, regardless of
        // the user's ordinary service-key saving preference.
        let mut removed_claude = false;
        for part in content.iter_mut() {
            if let ContentPart::Text { text } = part {
                for f in find_known(text)
                    .into_iter()
                    .rev()
                    .filter(|f| f.label == "Claude sign-in token")
                {
                    text.replace_range(f.start..f.end, "[removed: Claude sign-in token]");
                    removed_claude = true;
                }
            }
        }
        if removed_claude {
            notes.push((NoticeLevel::Warning, "Removed a Claude sign-in token without saving it. Sign in only through your own Claude CLI; Tessera never collects its credentials.".into()));
        }
        let (on, saving) = {
            let c = self.config.read();
            (c.security.save_secrets_from_chat, c.security.secret_saving)
        };
        if !on {
            return Ok(notes);
        }
        let rt = self.rt(session_id)?;
        let (private, user) = {
            let st = rt.st.lock();
            (st.info.private, self.secret_user(&st))
        };
        // In a bot's chat, what was saved before this message: one saved
        // now for every chat is that bot's to use too (not one it added a
        // field to, which it may not have been allowed).
        let before: Option<std::collections::HashSet<String>> = user.bot().map(|_| {
            self.auth
                .list_secrets()
                .into_iter()
                .map(|s| s.name)
                .collect()
        });
        // "Ask me first" and "Don't save": new ones stay with this chat.
        let hold = match saving {
            SecretSaving::Auto => None,
            _ => user.chat().map(str::to_string),
        };
        // What was kept, what it is, and whether it is a one-time code.
        let mut caught: Vec<(Stored, &str, bool)> = Vec::new();
        let mut removed: Vec<&str> = Vec::new();
        // Stored already, for bots this chat isn't (or for another chat).
        let mut hidden = false;
        let mut device = false;
        for part in content.iter_mut() {
            let ContentPart::Text { text } = part else {
                continue;
            };
            let found = find_known(text);
            if found.is_empty() {
                continue;
            }
            let mut out = String::with_capacity(text.len());
            let mut at = 0;
            for f in found {
                out.push_str(&text[at..f.start]);
                let value = &text[f.start..f.end];
                at = f.end;
                let stored = match f.name {
                    None => {
                        device |= f.label == "Tessera device token";
                        None
                    }
                    Some(_) if private => None,
                    Some(base) => {
                        match self.store_found(base, f.hosts, value, &user, hold.as_deref()) {
                            Ok(Some(s)) => Some(s),
                            Ok(None) => {
                                hidden = true;
                                out.push_str(crate::auth::HIDDEN_SECRET);
                                continue;
                            }
                            Err(e) => {
                                tracing::warn!("could not keep a {} from a chat: {e}", f.label);
                                None
                            }
                        }
                    }
                };
                match stored {
                    Some(s) => {
                        out.push_str(&format!("${}", s.var));
                        if !caught.iter().any(|(c, ..)| c.name == s.name) {
                            caught.push((s, f.label, false));
                        }
                    }
                    None => {
                        out.push_str(&format!("[removed: {}]", f.label));
                        if f.name.is_some() {
                            removed.push(f.label);
                        }
                    }
                }
            }
            out.push_str(&text[at..]);
            *text = out;
        }
        // Two-factor secrets: a setup key goes to a login (the agent types the
        // current code as `$NAME_OTP`), a one-time code to `$ONE_TIME_CODE`.
        for part in content.iter_mut() {
            let ContentPart::Text { text } = part else {
                continue;
            };
            let found = find_codes(text);
            if found.is_empty() {
                continue;
            }
            let mut out = String::with_capacity(text.len());
            let mut at = 0;
            for f in found {
                out.push_str(&text[at..f.start]);
                at = f.end;
                let (label, one_time) = match f.code {
                    Code::Setup { .. } => ("two-factor key", false),
                    Code::OneTime(_) => ("one-time code", true),
                };
                let stored = if private {
                    None
                } else {
                    self.store_code(&f.code, &user, hold.as_deref())
                        .unwrap_or_else(|e| {
                            tracing::warn!("could not keep a {label} from a chat: {e}");
                            None
                        })
                };
                match stored {
                    Some(s) => {
                        out.push_str(&format!("${}", s.var));
                        if !caught.iter().any(|(c, ..)| c.name == s.name) {
                            caught.push((s, label, one_time));
                        }
                    }
                    None => {
                        out.push_str(&format!("[removed: {label}]"));
                        removed.push(label);
                    }
                }
            }
            out.push_str(&text[at..]);
            *text = out;
        }
        // Values already in the store, typed again: their names will do
        // (for one this chat may not use, not even the name).
        let stored = self.auth.typed_values(&user);
        let mut named: Vec<String> = Vec::new();
        for part in content.iter_mut() {
            if let ContentPart::Text { text } = part
                && let Some(r) = redact_text(text, &stored)
            {
                for (v, n) in &stored {
                    if !text.contains(v.as_str()) {
                        continue;
                    }
                    if n == crate::auth::HIDDEN_SECRET {
                        hidden = true;
                    } else if !named.contains(n) {
                        named.push(n.clone());
                    }
                }
                *text = r;
            }
        }
        // Known formats kept before: named too, nothing new to save.
        for (s, ..) in caught.iter().filter(|(s, ..)| !s.fresh) {
            let v = format!("${}", s.var);
            if !named.contains(&v) {
                named.push(v);
            }
        }
        caught.retain(|(s, ..)| s.fresh);
        // Saved for every chat from a bot's chat: that bot may use it.
        if let (None, Some(before)) = (&hold, &before) {
            for (s, ..) in caught.iter().filter(|(s, ..)| !before.contains(&s.name)) {
                self.let_bot_use(&user, &s.name);
            }
        }
        if !named.is_empty() {
            notes.push((
                NoticeLevel::Info,
                format!(
                    "Your message held the stored {} {}; the agent got the name instead.",
                    if named.len() == 1 {
                        "secret"
                    } else {
                        "secrets"
                    },
                    named.join(", ")
                ),
            ));
        }
        if !caught.is_empty() {
            self.invalidate_prompts();
            let listed = |c: &[&(Stored, &str, bool)]| -> (Vec<(String, String)>, Vec<String>) {
                c.iter()
                    .map(|(s, what, _)| ((s.name.clone(), what.to_string()), format!("${}", s.var)))
                    .unzip()
            };
            let all: Vec<&(Stored, &str, bool)> = caught.iter().collect();
            if hold.is_none() {
                let (items, vars) = listed(&all);
                notes.push((
                    NoticeLevel::Info,
                    listed_notice(SAVED_PREFIX, &items, &vars, "your message"),
                ));
            } else {
                // A card for each to answer; a one-time code is good for
                // minutes, so it isn't worth a question.
                let (ask, keep): (Vec<_>, Vec<_>) = all
                    .into_iter()
                    .partition(|(_, _, one_time)| saving == SecretSaving::Ask && !one_time);
                if !keep.is_empty() {
                    let (items, vars) = listed(&keep);
                    notes.push((
                        NoticeLevel::Info,
                        listed_notice(KEPT_PREFIX, &items, &vars, "your message"),
                    ));
                }
                for (s, what, _) in ask {
                    notes.push((
                        NoticeLevel::Info,
                        format!(
                            "{ASK_PREFIX}{} ({what}) was taken out of your message and is kept for this chat only until you decide; the agent uses it as ${} without seeing the value.",
                            s.name, s.var
                        ),
                    ));
                }
            }
        }
        if !removed.is_empty() {
            let why = if private {
                "private chats don't keep secrets"
            } else {
                "it could not be saved"
            };
            notes.push((
                NoticeLevel::Warning,
                format!(
                    "Took {} out of your message: {why}. Add it in Settings → Privacy & safety → Passwords & keys to use it.",
                    removed.join(", ")
                ),
            ));
        }
        if hidden {
            notes.push((
                NoticeLevel::Info,
                "Took a saved password or key out of your message: only certain bots may use it, or another chat keeps it, so this chat got neither the value nor its name.".into(),
            ));
        }
        if device {
            notes.push((
                NoticeLevel::Warning,
                "Took a Tessera device token out of your message: anyone who has one controls this server. If it was shared anywhere, remove that device in Settings → Devices.".into(),
            ));
        }
        Ok(notes)
    }

    pub(super) fn post_notes(&self, session_id: &str, notes: Vec<(NoticeLevel, String)>) {
        if let Ok(rt) = self.rt(session_id) {
            for (level, text) in notes {
                self.notice(&rt, level, text);
            }
        }
    }

    /// The secret holding `value`: an existing token with that value, or a
    /// new one named `base` (`base_2`… when taken), kept for the chat `hold`
    /// alone when given. `None`: it is stored already, and `user` may not
    /// use it.
    fn store_found(
        &self,
        base: &str,
        hosts: &[&str],
        value: &str,
        user: &SecretUser,
        hold: Option<&str>,
    ) -> anyhow::Result<Option<Stored>> {
        if let Some((name, usable)) = self.auth.token_named(value, user) {
            return Ok(usable.then(|| Stored {
                var: name.clone(),
                name,
                fresh: false,
            }));
        }
        let name = free_name(&self.auth.list_secrets(), base);
        let hosts: Vec<String> = hosts.iter().map(|h| h.to_string()).collect();
        // What it is ("GitHub token", "API key 2"), not where it came from;
        // `base` is a known format's name, so this is its label.
        let title = crate::tools::secrets::suggest_title(&name, SecretKind::Token, &hosts, &[]);
        self.auth.save_secret_scoped(
            &name,
            SecretKind::Token,
            &title,
            std::collections::BTreeMap::from([("value".to_string(), value.to_string())]),
            &[],
            true,
            hosts,
            SecretScope {
                chat: hold.map(|c| Some(c.to_string())),
                ..Default::default()
            },
        )?;
        Ok(Some(Stored {
            var: name.clone(),
            name,
            fresh: true,
        }))
    }

    /// Keep a two-factor secret: in Passwords & keys, or for the chat `hold`
    /// alone when given.
    ///
    /// A setup key joins the login of the same name (named after the site it
    /// is for), else becomes a login of its own (`TWO_FACTOR` when nothing
    /// names it); the agent types the current code as `$NAME_OTP`. A
    /// one-time code replaces the last one (`ONE_TIME_CODE`, or the chat's
    /// own): it is only good for minutes, so there is never more than one.
    fn store_code(
        &self,
        code: &Code,
        user: &SecretUser,
        hold: Option<&str>,
    ) -> anyhow::Result<Option<Stored>> {
        let all = self.auth.list_secrets();
        // Kept for the chat, or for any (also a code another chat kept before).
        let scope = || SecretScope {
            chat: Some(hold.map(str::to_string)),
            ..Default::default()
        };
        match code {
            Code::OneTime(digits) => {
                let name = match hold {
                    // The shared one, never a code another chat keeps to itself.
                    None => all
                        .iter()
                        .find(|s| s.chat.is_none() && s.name.starts_with(ONE_TIME_CODE))
                        .map(|s| s.name.clone())
                        .unwrap_or_else(|| free_name(&all, ONE_TIME_CODE)),
                    Some(c) => all
                        .iter()
                        .find(|s| s.chat.as_deref() == Some(c) && s.name.starts_with(ONE_TIME_CODE))
                        .map(|s| s.name.clone())
                        .unwrap_or_else(|| free_name(&all, ONE_TIME_CODE)),
                };
                self.auth.save_secret_scoped(
                    &name,
                    SecretKind::Token,
                    "The last one-time code you sent in a chat",
                    std::collections::BTreeMap::from([("value".to_string(), digits.clone())]),
                    &[],
                    false,
                    Vec::new(),
                    scope(),
                )?;
                Ok(Some(Stored {
                    var: name.clone(),
                    name,
                    fresh: true,
                }))
            }
            Code::Setup {
                seed,
                issuer,
                account,
            } => {
                let base = issuer
                    .as_deref()
                    .map(secret_name)
                    .filter(|n| crate::auth::valid_secret_name(n).is_ok())
                    .unwrap_or_else(|| "TWO_FACTOR".to_string());
                // The same key again, where this chat may use it: nothing new to keep.
                if let Some(s) = all.iter().find(|s| {
                    s.kind == SecretKind::Login
                        && s.fields.iter().any(|f| f == "totp")
                        && s.chat.as_deref().is_none_or(|c| user.chat() == Some(c))
                        && self.auth.field_is(&s.name, "totp", seed)
                }) {
                    return Ok(Some(Stored {
                        name: s.name.clone(),
                        var: format!("{}_OTP", s.name),
                        fresh: false,
                    }));
                }
                let free = |n: &str| !all.iter().any(|s| s.name == n);
                // A saved login of that name without a key gets this one (its
                // other fields stay); one kept for a chat gets a login of its own.
                if hold.is_none()
                    && let Some(s) = all.iter().find(|s| {
                        s.name == base
                            && s.kind == SecretKind::Login
                            && s.chat.is_none()
                            && !s.fields.iter().any(|f| f == "totp")
                    })
                {
                    self.auth.save_secret(
                        &s.name,
                        SecretKind::Login,
                        &s.description,
                        std::collections::BTreeMap::from([("totp".to_string(), seed.clone())]),
                        &[],
                        s.ask,
                        s.hosts.clone(),
                    )?;
                    return Ok(Some(Stored {
                        name: s.name.clone(),
                        var: format!("{}_OTP", s.name),
                        fresh: true,
                    }));
                }
                let name = std::iter::once(base.clone())
                    .chain(std::iter::once(format!("{base}_2FA")))
                    .chain((2..).map(|i| format!("{base}_2FA_{i}")))
                    .find(|n| free(n))
                    .expect("a free name");
                let mut fields =
                    std::collections::BTreeMap::from([("totp".to_string(), seed.clone())]);
                if let Some(a) = account {
                    fields.insert("username".to_string(), a.clone());
                }
                let what = issuer
                    .as_deref()
                    .map(|i| format!(" for {i}"))
                    .unwrap_or_default();
                self.auth.save_secret_scoped(
                    &name,
                    SecretKind::Login,
                    &format!("Two-factor key{what}"),
                    fields,
                    &[],
                    false,
                    Vec::new(),
                    scope(),
                )?;
                Ok(Some(Stored {
                    var: format!("{name}_OTP"),
                    name,
                    fresh: true,
                }))
            }
        }
    }

    /// The user's answer about `name`, a secret kept for the chat
    /// `session_id` alone (from its card, or Passwords & keys): saved for
    /// any chat, kept for this one, or deleted. The card changes to say so.
    pub(super) fn decide_chat_secret(
        &self,
        session_id: &str,
        name: &str,
        choice: ChatSecretChoice,
    ) -> Result<(), ApiError> {
        let info = self
            .auth
            .list_secrets()
            .into_iter()
            .find(|s| s.name == name)
            .ok_or_else(|| {
                ApiError::new(ErrorCode::NotFound, format!("{name} isn't kept anymore"))
            })?;
        let Some(chat) = info.chat.clone() else {
            return Err(bad(format!("{name} is in Passwords & keys already")));
        };
        if chat != session_id && self.root_session(session_id) != chat {
            return Err(bad(format!("{name} is kept for another chat")));
        }
        match choice {
            ChatSecretChoice::Save => {
                self.auth
                    .save_secret_scoped(
                        name,
                        info.kind,
                        &info.description,
                        std::collections::BTreeMap::new(),
                        &[],
                        info.ask,
                        info.hosts.clone(),
                        SecretScope {
                            chat: Some(None),
                            ..Default::default()
                        },
                    )
                    .map_err(|e| bad(e.to_string()))?;
                // The bot of the chat it was kept for keeps it.
                if let Ok(rt) = self.rt(&chat) {
                    let user = self.secret_user(&rt.st.lock());
                    self.let_bot_use(&user, name);
                }
            }
            ChatSecretChoice::ChatOnly => {}
            ChatSecretChoice::Forget => {
                self.auth.delete_secret(name).map_err(internal)?;
            }
        }
        self.invalidate_prompts();
        for id in [chat.as_str(), session_id] {
            self.settle_card(id, name, choice);
            if id == session_id {
                break;
            }
        }
        Ok(())
    }

    /// The chat's notices about `name` alone (the card waiting for the
    /// user's answer, or "kept for this chat only") now say what they chose.
    fn settle_card(&self, session_id: &str, name: &str, choice: ChatSecretChoice) {
        let Ok(entries) = self.store.entries(session_id, 0) else {
            return;
        };
        let mut changed = Vec::new();
        for mut e in entries {
            let EntryBody::Notice { text, .. } = &mut e.body else {
                continue;
            };
            let Some((n, what, from, vars)) = kept_notice(text) else {
                continue;
            };
            if n != name {
                continue;
            }
            let item = [(n.to_string(), what.to_string())];
            let vars = [vars.to_string()];
            let new = match choice {
                ChatSecretChoice::Save => listed_notice(SAVED_PREFIX, &item, &vars, from),
                ChatSecretChoice::ChatOnly => listed_notice(KEPT_PREFIX, &item, &vars, from),
                ChatSecretChoice::Forget => {
                    format!(
                        "Forgot {n} ({what}): it was taken out of {from} and isn't kept anywhere."
                    )
                }
            };
            if new == *text {
                continue;
            }
            *text = new;
            match self.store.replace_entry(session_id, &e) {
                Ok(()) => changed.push(e),
                Err(err) => tracing::error!("rewriting a notice of {session_id}: {err}"),
            }
        }
        if !changed.is_empty() {
            let _ = self.events.send(Event::EntriesChanged {
                session_id: session_id.to_string(),
                entries: changed,
            });
        }
    }

    /// Whether `save_secret` may store `s` from `session_id`, and how the
    /// user wants credentials saved (with "Ask me first" the tool asks them
    /// before it saves to Passwords & keys).
    pub(super) fn check_chat_secret(
        &self,
        session_id: &str,
        s: &ChatSecret,
    ) -> Result<SecretSaving, String> {
        let (on, saving) = {
            let c = self.config.read();
            (c.security.save_secrets_from_chat, c.security.secret_saving)
        };
        if !on {
            return Err("Saving secrets from chats is turned off (Settings → Privacy & safety). Ask the user to add it in Settings → Privacy & safety → Passwords & keys.".into());
        }
        let rt = self.rt(session_id).map_err(|e| e.message)?;
        if rt.st.lock().info.private {
            return Err("Private chats don't keep secrets.".into());
        }
        crate::auth::valid_secret_name(&s.name).map_err(|e| e.to_string())?;
        if crate::tools::secrets::claude_credential(&s.name, &s.description, "")
            || s.fields
                .values()
                .any(|v| v.contains("sk-ant-oat") || v.contains("sk-ant-sid"))
        {
            return Err("Claude sign-in belongs to the user's own Claude CLI, not Tessera's password library.".into());
        }
        if self.auth.list_secrets().iter().any(|i| i.name == s.name) {
            return Err(format!("The name {} is taken. Pick another name.", s.name));
        }
        let entries = self
            .store
            .entries(session_id, 0)
            .map_err(|e| e.to_string())?;
        let typed: String = entries
            .iter()
            .filter_map(|e| match &e.body {
                EntryBody::User {
                    content,
                    author: None,
                } => Some(plain_text(content)),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        // Only what the user typed: nothing from pages, files or tools can
        // be planted in (or replace) their secrets this way.
        for (f, v) in s.fields.iter().filter(|(f, _)| f.as_str() != "url") {
            if v.trim().is_empty() || !typed.contains(v.as_str()) {
                return Err(format!(
                    "That {f} isn't in the user's own messages in this chat. Save only what the user typed, copied exactly."
                ));
            }
        }
        Ok(saving)
    }

    /// `save_secret`: store what the user typed (in Passwords & keys, or for
    /// this chat alone with `chat_only`) and rewrite the chat's history so
    /// later requests never carry it.
    pub(super) fn save_chat_secret(
        &self,
        session_id: &str,
        s: ChatSecret,
    ) -> Result<String, String> {
        self.check_chat_secret(session_id, &s)?;
        self.store_chat_secret(session_id, s)
    }

    /// Secure app entry: the value was never a model argument or message.
    fn store_chat_secret(&self, session_id: &str, s: ChatSecret) -> Result<String, String> {
        let rt = self.rt(session_id).map_err(|e| e.message)?;
        let user = self.secret_user(&rt.st.lock());
        let hold = s
            .chat_only
            .then(|| user.chat().map(str::to_string))
            .flatten();
        self.auth
            .save_secret_scoped(
                &s.name,
                s.kind,
                &s.description,
                s.fields.clone(),
                &[],
                true,
                s.hosts,
                SecretScope {
                    chat: hold.clone().map(Some),
                    ..Default::default()
                },
            )
            .map_err(|e| e.to_string())?;
        if hold.is_none() {
            self.let_bot_use(&user, &s.name);
        }
        let var = |f: &str| match s.kind {
            SecretKind::Token => format!("${}", s.name),
            _ => format!("${}_{}", s.name, f.to_ascii_uppercase()),
        };
        let mut swaps: Vec<(String, String)> = s
            .fields
            .iter()
            .filter(|(f, v)| f.as_str() != "url" && v.chars().count() >= 3)
            .map(|(f, v)| (v.clone(), var(f)))
            .collect();
        swaps.sort_by_key(|(v, _)| std::cmp::Reverse(v.len()));
        let exposed = self.redact_history(&rt, &swaps, true);
        self.invalidate_prompts_preserving_claude((!exposed).then_some(session_id));
        if exposed {
            let mut st = rt.st.lock();
            st.internal.claude_history_epoch = st.internal.claude_history_epoch.wrapping_add(1);
            st.internal.claude_code = None;
            st.internal.claude_code_bots.clear();
        } else {
            rt.st.lock().internal.claude_secure_entry = true;
        }
        let claude = self
            .providers
            .kind_of(&self.effective_model(&rt.st.lock()).provider)
            == Some(ProviderKind::ClaudeCode);
        if claude {
            if exposed {
                self.notice(&rt, NoticeLevel::Warning,
                    "Saved securely in Tessera. If this password was sent as plain text earlier, it stays in Claude Code's own history, which Tessera never edits. The next Claude turn starts from Tessera's sanitized history.");
            } else {
                self.notice(&rt, NoticeLevel::Info, "Saved securely in Tessera.");
            }
        }
        let vars: Vec<String> = s.fields.keys().map(|f| var(f)).collect();
        let what = match s.kind {
            SecretKind::Token => "token",
            SecretKind::Login => "login",
            SecretKind::Custom => "custom",
        };
        let prefix = if hold.is_some() {
            KEPT_PREFIX
        } else {
            SAVED_PREFIX
        };
        if exposed || !claude {
            self.notice(
                &rt,
                NoticeLevel::Info,
                listed_notice(
                    prefix,
                    &[(s.name.clone(), what.into())],
                    &vars,
                    "the conversation",
                ),
            );
        }
        Ok(if hold.is_some() {
            format!(
                "Kept for this chat only as {} ({what}): the user doesn't want it in Passwords & keys. The values are gone from this conversation; in this chat use {} in commands (the user may be asked before its first use).",
                s.name,
                vars.join(", ")
            )
        } else {
            format!(
                "Saved as {} ({what}). The values are gone from this conversation; from now on use {} in commands (the user may be asked before its first use).",
                s.name,
                vars.join(", ")
            )
        })
    }

    /// Reasoning held back while its turn ran: rewrite it now.
    pub(super) fn finish_redactions(&self, rt: &Rt) {
        let id = rt.st.lock().info.id.clone();
        let Some(swaps) = later().lock().remove(&id) else {
            return;
        };
        self.redact_history(rt, &swaps, false);
    }

    /// Replace each value with its variable everywhere in the chat: stored
    /// items, title, preview, task ledger and queued messages.
    /// `hold_signed` (mid-turn): leave the latest reply's signed reasoning,
    /// and go over everything again in
    /// [`finish_redactions`](Self::finish_redactions).
    fn redact_history(&self, rt: &Rt, swaps: &[(String, String)], hold_signed: bool) -> bool {
        let id = rt.st.lock().info.id.clone();
        let Ok(entries) = self.store.entries(&id, 0) else {
            // Without readable history we cannot prove it was card-only.
            return true;
        };
        let latest = entries
            .iter()
            .rev()
            .find(|e| matches!(e.body, EntryBody::Assistant { .. }))
            .map(|e| e.seq);
        let mut changed = Vec::new();
        let mut exposed = false;
        for mut e in entries {
            // Include signed reasoning even if rewriting it must wait until
            // the turn ends. Only a boolean survives this transient check.
            exposed |= redact_body(&mut e.body.clone(), swaps, false);
            let hold = hold_signed && Some(e.seq) == latest;
            if redact_body(&mut e.body, swaps, hold) {
                match self.store.replace_entry(&id, &e) {
                    Ok(()) => changed.push(e),
                    Err(err) => tracing::error!("rewriting an entry of {id}: {err}"),
                }
            }
        }
        // Once more when the turn ends: held reasoning, and anything the
        // agent still repeats before then.
        if hold_signed && exposed {
            later()
                .lock()
                .entry(id.clone())
                .or_default()
                .extend(swaps.iter().cloned());
        }
        {
            let mut st = rt.st.lock();
            let info = &mut st.info;
            for s in [&mut info.title, &mut info.preview] {
                if let Some(r) = redact_text(s, swaps) {
                    *s = r;
                }
            }
            if let Some(task) = st.info.task.as_mut()
                && let Ok(mut v) = serde_json::to_value(&*task)
                && redact_value(&mut v, swaps)
                && let Ok(t) = serde_json::from_value(v)
            {
                *task = t;
            }
            if let Some(task) = st.internal.ledger_shown.as_mut()
                && let Ok(mut v) = serde_json::to_value(&*task)
                && redact_value(&mut v, swaps)
                && let Ok(t) = serde_json::from_value(v)
            {
                *task = t;
            }
            // A checkpoint being written may have read the secret.
            st.pre = None;
            let RtState {
                steer,
                followups,
                send_now,
                ..
            } = &mut *st;
            for w in steer
                .iter_mut()
                .chain(followups.iter_mut())
                .chain(send_now.iter_mut())
            {
                redact_parts(&mut w.content, swaps);
            }
        }
        self.save(rt);
        if !changed.is_empty() {
            let _ = self.events.send(Event::EntriesChanged {
                session_id: id,
                entries: changed,
            });
        }
        exposed
    }
}

/// The folder rules with Tessera's program and its service files (`kept`)
/// read-only on top: no writable rule inside them counts.
pub(super) fn with_program_kept(
    mut folders: Vec<(PathBuf, bool)>,
    kept: Vec<PathBuf>,
) -> Vec<(PathBuf, bool)> {
    folders.retain(|(p, write)| !(*write && kept.iter().any(|k| p.starts_with(k))));
    folders.extend(kept.into_iter().map(|k| (k, false)));
    folders
}

/// "Saved to Passwords & keys: NAME (what), …. It was taken out of …; the
/// agent uses it as $VAR, … without seeing the value." (or "Kept for this
/// chat only: …", with that prefix).
fn listed_notice(prefix: &str, items: &[(String, String)], vars: &[String], from: &str) -> String {
    let list = items
        .iter()
        .map(|(n, what)| format!("{n} ({what})"))
        .collect::<Vec<_>>()
        .join(", ");
    let (it, they) = if items.len() == 1 {
        ("It was", "it")
    } else {
        ("They were", "them")
    };
    format!(
        "{prefix}{list}. {it} taken out of {from}; the agent uses {they} as {} without seeing the value.",
        vars.join(", ")
    )
}

/// A notice about one secret kept for a chat: the card waiting for the
/// user's answer, or "Kept for this chat only" (one item). Its name, what it
/// is, what it was taken out of, and the variables the agent uses.
fn kept_notice(text: &str) -> Option<(&str, &str, &str, &str)> {
    if let Some(rest) = text.strip_prefix(ASK_PREFIX) {
        let (head, tail) = rest.split_once(" was taken out of your message")?;
        let (name, what) = head.split_once(" (")?;
        let vars = tail
            .split_once("; the agent uses it as ")?
            .1
            .strip_suffix(" without seeing the value.")?;
        return Some((name, what.strip_suffix(')')?, "your message", vars));
    }
    let rest = text.strip_prefix(KEPT_PREFIX)?;
    let (head, tail) = rest.split_once(". It was taken out of ")?;
    let (name, what) = head.split_once(" (")?;
    let what = what.strip_suffix(')')?;
    // Several at once ("A (x), B (y)") are left as they are.
    if what.contains('(') || name.contains(' ') {
        return None;
    }
    let (from, vars) = tail.split_once("; the agent uses it as ")?;
    Some((
        name,
        what,
        from,
        vars.strip_suffix(" without seeing the value.")?,
    ))
}

/// `text` with every whole occurrence of each value replaced (a value that
/// starts or ends with a letter or digit must not be part of a longer word).
fn redact_text(text: &str, swaps: &[(String, String)]) -> Option<String> {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = text.to_string();
    let mut changed = false;
    for (value, var) in swaps {
        if value.is_empty() || !out.contains(value.as_str()) {
            continue;
        }
        let first = value.chars().next().is_some_and(word);
        let last = value.chars().last().is_some_and(word);
        let mut next = String::with_capacity(out.len());
        let mut at = 0;
        for (i, _) in out.match_indices(value.as_str()) {
            if i < at {
                continue;
            }
            let before = out[..i].chars().last();
            let after = out[i + value.len()..].chars().next();
            if (first && before.is_some_and(word)) || (last && after.is_some_and(word)) {
                continue;
            }
            next.push_str(&out[at..i]);
            next.push_str(var);
            at = i + value.len();
            changed = true;
        }
        next.push_str(&out[at..]);
        out = next;
    }
    changed.then_some(out)
}

fn redact_parts(parts: &mut [ContentPart], swaps: &[(String, String)]) -> bool {
    let mut changed = false;
    for p in parts {
        if let ContentPart::Text { text } = p
            && let Some(r) = redact_text(text, swaps)
        {
            *text = r;
            changed = true;
        }
    }
    changed
}

/// Every string in a JSON value (tool arguments, details).
fn redact_value(v: &mut Value, swaps: &[(String, String)]) -> bool {
    match v {
        Value::String(s) => match redact_text(s, swaps) {
            Some(r) => {
                *s = r;
                true
            }
            None => false,
        },
        Value::Array(a) => {
            let mut changed = false;
            for x in a {
                changed |= redact_value(x, swaps);
            }
            changed
        }
        Value::Object(o) => {
            let mut changed = false;
            for x in o.values_mut() {
                changed |= redact_value(x, swaps);
            }
            changed
        }
        _ => false,
    }
}

/// Rewrite one stored item; true if anything changed. With `hold`, signed
/// reasoning that mentions a value is left as is (its provider needs it
/// verbatim until the turn ends).
fn redact_body(body: &mut EntryBody, swaps: &[(String, String)], hold: bool) -> bool {
    match body {
        EntryBody::User { content, .. } => redact_parts(content, swaps),
        EntryBody::ToolResult {
            content, details, ..
        } => {
            let a = redact_parts(content, swaps);
            let b = details.as_mut().is_some_and(|d| redact_value(d, swaps));
            a || b
        }
        EntryBody::Compaction { summary, .. } | EntryBody::Notice { text: summary, .. } => {
            match redact_text(summary, swaps) {
                Some(r) => {
                    *summary = r;
                    true
                }
                None => false,
            }
        }
        EntryBody::Assistant { content, .. } => {
            let mut changed = false;
            for p in content {
                match p {
                    AssistantPart::Text { text } => {
                        if let Some(r) = redact_text(text, swaps) {
                            *text = r;
                            changed = true;
                        }
                    }
                    AssistantPart::ToolCall { arguments, .. } => {
                        changed |= redact_value(arguments, swaps);
                    }
                    AssistantPart::Thinking { text, opaque } => {
                        let in_opaque = opaque.as_ref().is_some_and(|o| {
                            let s = o.to_string();
                            swaps.iter().any(|(v, _)| s.contains(v.as_str()))
                        });
                        let new_text = redact_text(text, swaps);
                        if new_text.is_none() && !in_opaque {
                            continue;
                        }
                        if hold && opaque.is_some() {
                            continue;
                        }
                        if let Some(r) = new_text {
                            *text = r;
                        }
                        // Provider data that can't be rewritten is dropped:
                        // that reasoning is then simply not replayed.
                        *opaque = None;
                        changed = true;
                    }
                }
            }
            changed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_replaced_whole() {
        let swaps = vec![
            ("testpw2".to_string(), "$BANK_PASSWORD".to_string()),
            ("ann".to_string(), "$BANK_USERNAME".to_string()),
        ];
        assert_eq!(
            redact_text("login ann / testpw2, annual testpw22 (ann)", &swaps).unwrap(),
            "login $BANK_USERNAME / $BANK_PASSWORD, annual testpw22 ($BANK_USERNAME)"
        );
        assert_eq!(redact_text("nothing here", &swaps), None);
        let mut v = json!({"name": "BANK", "fields": {"password": "testpw2"}, "n": 3});
        assert!(redact_value(&mut v, &swaps));
        assert_eq!(v["fields"]["password"], "$BANK_PASSWORD");
        // Signed reasoning waits when asked to, else loses its signature.
        let thinking = || EntryBody::Assistant {
            content: vec![AssistantPart::Thinking {
                text: "password testpw2".into(),
                opaque: Some(json!({"signature": "abc"})),
            }],
            model: ModelRef {
                provider: "p".into(),
                model: "m".into(),
            },
            usage: Usage::default(),
            stop: StopReason::ToolUse,
            error: None,
            author: None,
        };
        let mut body = thinking();
        assert!(!redact_body(&mut body, &swaps, true));
        assert!(redact_body(&mut body, &swaps, false));
        let EntryBody::Assistant { content, .. } = &body else {
            unreachable!()
        };
        assert_eq!(
            content[0],
            AssistantPart::Thinking {
                text: "password $BANK_PASSWORD".into(),
                opaque: None
            }
        );
    }

    #[test]
    fn notices_about_one_kept_secret_are_read_back() {
        let card = format!(
            "{ASK_PREFIX}GITHUB_TOKEN (GitHub token) was taken out of your message and is kept for this chat only until you decide; the agent uses it as $GITHUB_TOKEN without seeing the value."
        );
        assert_eq!(
            kept_notice(&card),
            Some((
                "GITHUB_TOKEN",
                "GitHub token",
                "your message",
                "$GITHUB_TOKEN"
            ))
        );
        let login = listed_notice(
            KEPT_PREFIX,
            &[("BANK".into(), "login".into())],
            &["$BANK_PASSWORD, $BANK_USERNAME".into()],
            "the conversation",
        );
        assert_eq!(
            kept_notice(&login),
            Some((
                "BANK",
                "login",
                "the conversation",
                "$BANK_PASSWORD, $BANK_USERNAME"
            ))
        );
        let two = listed_notice(
            KEPT_PREFIX,
            &[("A".into(), "x".into()), ("B".into(), "y".into())],
            &["$A".into(), "$B".into()],
            "your message",
        );
        assert_eq!(kept_notice(&two), None);
        let saved = listed_notice(
            SAVED_PREFIX,
            &[("A".into(), "x".into())],
            &["$A".into()],
            "your message",
        );
        assert_eq!(kept_notice(&saved), None);
    }

    #[test]
    fn notices_name_the_variables() {
        let one = listed_notice(
            SAVED_PREFIX,
            &[("ANTHROPIC_API_KEY".into(), "Anthropic API key".into())],
            &["$ANTHROPIC_API_KEY".into()],
            "your message",
        );
        assert_eq!(
            one,
            "Saved to Passwords & keys: ANTHROPIC_API_KEY (Anthropic API key). It was taken out of your message; the agent uses it as $ANTHROPIC_API_KEY without seeing the value."
        );
    }
}
