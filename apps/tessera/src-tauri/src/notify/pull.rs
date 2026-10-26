//! A phone checking in while Tessera is closed or asleep.
//!
//! Android freezes an app soon after it leaves the screen, and Tessera uses
//! no third-party push service, so a background job of our plugin
//! (`plugins/background`, `PullJob`) wakes up about every 15 minutes (and
//! when a scheduled message is due), loads this library and calls [`pull`]
//! through JNI. It connects to the active server the same way the app does
//! (end-to-end encrypted to the pinned key, the saved proxy headers), asks
//! what this device missed, marks it handled and hands Kotlin the
//! notifications to show. Every decision is made here; Kotlin only shows.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tessera_proto::{PendingNotifications, UserNotification};

use super::{Note, Settings, Stored, TITLE_MAX, clip, valid_id};

/// One notification for Kotlin (the plugin's `PostArgs`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Post {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_id: Option<u64>,
    pub tag: String,
    pub session_id: Option<String>,
    pub urgent: bool,
    pub title: String,
    pub body: String,
    pub public_body: String,
    pub agent: bool,
    pub silent: bool,
    pub private: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sub: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<u64>,
}

/// What Kotlin does after a check.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Pulled {
    pub posts: Vec<Post>,
    /// Notification tags to remove.
    pub withdraw: Vec<String>,
    /// Check again then (a scheduled message is due).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wake_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ack_server: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ack_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suppressed: Vec<u64>,
}

/// Remembered between checks: the chats already said to be waiting (so a
/// pending approval is announced once, not every 15 minutes).
#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    server_scope: String,
    #[serde(default)]
    accepted: Vec<u64>,
    #[serde(default)]
    waiting: Vec<String>,
}

impl State {
    fn select_server(&mut self, scope: String) {
        if self.server_scope != scope {
            *self = Self {
                server_scope: scope,
                ..Self::default()
            };
        }
    }

    fn remember(&mut self, ids: &[u64]) {
        for id in ids {
            if !self.accepted.contains(id) {
                if self.accepted.len() >= 512 {
                    self.accepted.remove(0);
                }
                self.accepted.push(*id);
            }
        }
    }

    fn replay(&self, out: &mut Pulled) {
        out.posts.retain(|post| {
            if let Some(id) = post.notification_id.filter(|id| self.accepted.contains(id)) {
                out.suppressed.push(id);
                false
            } else {
                true
            }
        });
    }
}

static PULL_STATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
const STATE_FILE: &str = "notify-pull.json";

pub(super) fn post_of(n: &Note) -> Post {
    Post {
        notification_id: n.notification_id,
        private: n.private,
        tag: n.key.clone(),
        session_id: n.session.clone(),
        urgent: n.urgent(),
        title: n.title.clone(),
        body: n.body.clone(),
        public_body: n.public_body().into(),
        agent: n.kind == super::Kind::Agent,
        silent: n.silent,
        sub: n.from.clone(),
        when: n.when,
    }
}

/// What to show for what the server says this device missed, the ids to
/// mark handled, and the waiting chats to remember.
pub(crate) fn decide(
    settings: &Settings,
    p: &PendingNotifications,
    announced: &[String],
    quiet: bool,
) -> (Pulled, Vec<u64>, Vec<String>) {
    let mut out = Pulled {
        withdraw: p
            .seen
            .iter()
            .map(|id| super::agent_key(*id))
            .chain(p.withdraw.iter().cloned())
            .collect(),
        wake_at: p.next_due,
        ..Default::default()
    };
    let mut ack: Vec<u64> = p.new.iter().map(|n| n.id).collect();
    if !settings.enabled {
        out.posts.extend(
            p.new
                .iter()
                .filter(|n| n.kind == "test" && !n.seen)
                .filter_map(|n| super::server_note(settings, n).map(|note| post_of(&note))),
        );
        return (out, ack, Vec::new());
    }
    let show = |n: &UserNotification| {
        let mut note = super::server_note(settings, n)?;
        note.silent |= !note.server_policy && quiet && !n.urgent;
        Some(post_of(&note))
    };
    out.posts.extend(
        p.new
            .iter()
            .filter(|n| !n.seen && !tessera_proto::is_protocol_notification(n))
            .filter_map(show),
    );
    // Legacy bursts may coalesce. Modern policy already groups exact repeats,
    // so distinct first occurrences and urgent revisions all reach the OS.
    let independent = p
        .new
        .iter()
        .filter(|n| !n.reason.is_empty() || n.urgent)
        .map(|n| n.id)
        .collect::<std::collections::HashSet<_>>();
    let mut keys = std::collections::HashSet::new();
    out.posts.reverse();
    out.posts.retain(|post| {
        post.notification_id
            .is_some_and(|id| independent.contains(&id))
            || keys.insert(post.tag.clone())
    });
    out.posts.reverse();
    ack.retain(|id| {
        !out.posts
            .iter()
            .any(|post| post.notification_id == Some(*id))
    });
    let mut waiting = Vec::new();
    for w in &p.waiting {
        if !valid_id(&w.session_id) {
            continue;
        }
        waiting.push(w.session_id.clone());
        if !settings.approvals
            || announced.contains(&w.session_id)
            || out
                .posts
                .iter()
                .any(|p| p.session_id.as_deref() == Some(w.session_id.as_str()))
        {
            continue;
        }
        let place = if w.private {
            "Private chat".to_string()
        } else if w.title.trim().is_empty() {
            "Untitled chat".to_string()
        } else {
            clip(&w.title, TITLE_MAX)
        };
        let mut note = Note::new(
            &w.session_id,
            Some(&w.session_id),
            super::Kind::Approval,
            "Tessera needs your approval",
            place,
        );
        if w.private || !settings.previews {
            note.generic();
        }
        note.silent = quiet;
        out.posts.push(post_of(&note));
    }
    // Answered (here or elsewhere) since: the approval notification goes.
    out.withdraw
        .extend(announced.iter().filter(|s| !waiting.contains(s)).cloned());
    (out, ack, waiting)
}

/// Check the active server for what this device missed. `dir` is the app's
/// data directory (`servers.json`, `notifications.json`).
pub(crate) fn pull(dir: &Path) -> Pulled {
    // One check at a time (the periodic job and a wake-up may overlap).
    let _one = PULL_STATE.lock().unwrap_or_else(|e| e.into_inner());
    let read = |name: &str| std::fs::read(dir.join(name)).ok();
    let stored: Stored = read("notifications.json")
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let config: crate::ClientConfig = read("servers.json")
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let Some(server) = config
        .active
        .as_deref()
        .and_then(|id| config.servers.iter().find(|s| s.id == id))
        .filter(|s| !s.token.is_empty())
    else {
        return Pulled::default();
    };
    let mut state: State = read(STATE_FILE)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            return Pulled {
                error: Some(e.to_string()),
                ..Default::default()
            };
        }
    };
    let config = tessera_client::ServerConfig {
        endpoints: server.endpoints.clone(),
        headers: server.headers.clone(),
        token: server.token.clone(),
        server_key: server.server_key.clone(),
    };
    let scope = format!("{}:{}", server.id, server.server_key);
    state.select_server(scope);
    let settings = stored.settings.clone();
    let result = rt.block_on(async move {
        let client = tessera_client::Client::connect(config);
        let done = async {
            client
                .wait_connected(Duration::from_secs(20))
                .await
                .map_err(|e| e.to_string())?;
            let v = client
                .call_timeout(
                    tessera_proto::Request::PendingNotificationsV2,
                    Duration::from_secs(15),
                )
                .await;
            // Old servers know only explicit notifications. No automatic payload
            // reaches an old policy on either side of this mixed-version route.
            let v = match v {
                Ok(v) => v,
                Err(_) => client
                    .call_timeout(
                        tessera_proto::Request::PendingNotifications,
                        Duration::from_secs(15),
                    )
                    .await
                    .map_err(|e| e.message)?,
            };
            let p: PendingNotifications = serde_json::from_value(v).map_err(|e| e.to_string())?;
            let quiet = settings.quiet_now();
            let (mut out, ack, waiting) = decide(&settings, &p, &state.waiting, quiet);
            // Kotlin acknowledges only accepted posts; the remainder is
            // deliberate suppression, scoped to this saved server identity.
            out.ack_server = Some(server.id.clone());
            out.ack_key = Some(server.server_key.clone());
            out.suppressed = ack;
            state.replay(&mut out);
            Ok::<_, String>((
                out,
                State {
                    server_scope: state.server_scope.clone(),
                    accepted: state.accepted.clone(),
                    waiting,
                },
            ))
        }
        .await;
        client.close();
        done
    });
    match result {
        Ok((out, state)) => {
            if let Ok(data) = serde_json::to_vec(&state) {
                let _ = crate::private_file::write_verified(&dir.join(STATE_FILE), &data);
            }
            out
        }
        Err(e) => Pulled {
            error: Some(e),
            ..Default::default()
        },
    }
}

/// `dev.tessera.background.Native.pullNative(dir)`: the check, as JSON.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tessera_background_Native_pullNative<'l>(
    mut env: jni::JNIEnv<'l>,
    _class: jni::objects::JClass<'l>,
    dir: jni::objects::JString<'l>,
) -> jni::sys::jstring {
    let dir: String = env.get_string(&dir).map(Into::into).unwrap_or_default();
    let out = if dir.is_empty() {
        Pulled::default()
    } else {
        pull(Path::new(&dir))
    };
    let json = serde_json::to_string(&out).unwrap_or_else(|_| "{}".into());
    env.new_string(json)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

// The same persist-before-network boundary is used by JNI and the regression.
#[cfg(any(target_os = "android", all(test, unix)))]
fn receipt_then_ack(
    dir: &Path,
    scope: String,
    ids: &[u64],
    ack: impl FnOnce(),
) -> std::io::Result<()> {
    let _one = PULL_STATE.lock().unwrap_or_else(|e| e.into_inner());
    let state_file = dir.join(STATE_FILE);
    let mut state: State = std::fs::read(&state_file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    state.select_server(scope);
    state.remember(ids);
    let data = serde_json::to_vec(&state).map_err(std::io::Error::other)?;
    crate::private_file::write_verified(&state_file, &data)?;
    ack();
    Ok(())
}
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tessera_background_Native_ackNative<'l>(
    mut env: jni::JNIEnv<'l>,
    _class: jni::objects::JClass<'l>,
    dir: jni::objects::JString<'l>,
    json: jni::objects::JString<'l>,
) {
    let dir: String = env.get_string(&dir).map(Into::into).unwrap_or_default();
    let json: String = env.get_string(&json).map(Into::into).unwrap_or_default();
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Receipt {
        server: String,
        key: String,
        #[serde(default)]
        delivered: Vec<u64>,
        ids: Vec<u64>,
    }
    let Ok(receipt) = serde_json::from_str::<Receipt>(&json) else {
        return;
    };
    if receipt.ids.is_empty() || receipt.ids.len() > 1024 {
        return;
    }
    let Ok(config) = std::fs::read(Path::new(&dir).join("servers.json")).and_then(|b| {
        serde_json::from_slice::<crate::ClientConfig>(&b).map_err(std::io::Error::other)
    }) else {
        return;
    };
    let Some(server) = config.servers.iter().find(|s| {
        Some(&s.id) == config.active.as_ref()
            && s.id == receipt.server
            && s.server_key == receipt.key
    }) else {
        return;
    };
    let Ok(rt) = tokio::runtime::Runtime::new() else {
        return;
    };
    let _ = receipt_then_ack(
        Path::new(&dir),
        format!("{}:{}", server.id, server.server_key),
        &receipt.ids,
        || {
            rt.block_on(async {
                let client = tessera_client::Client::connect(tessera_client::ServerConfig {
                    endpoints: server.endpoints.clone(),
                    headers: server.headers.clone(),
                    token: server.token.clone(),
                    server_key: server.server_key.clone(),
                });
                if client.wait_connected(Duration::from_secs(15)).await.is_ok()
                    && let Some(epoch) = client.connected_epoch()
                {
                    let _ = client
                        .call_timeout_in_epoch(
                            epoch,
                            tessera_proto::Request::ReportNotificationDelivery {
                                ids: receipt.delivered.clone(),
                            },
                            Duration::from_secs(15),
                        )
                        .await;
                    let _ = client
                        .call_timeout_in_epoch(
                            epoch,
                            tessera_proto::Request::AckNotifications {
                                ids: receipt.ids.clone(),
                            },
                            Duration::from_secs(15),
                        )
                        .await;
                }
                client.close();
            })
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessera_proto::WaitingChat;
    #[cfg(unix)]
    #[test]
    fn protected_ancestor_receipt_reaches_network_ack_after_durable_save() {
        use std::io::{Read, Write};
        let dir = crate::private_file::ProtectedAppDir::new();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        receipt_then_ack(&dir.app, "server-a:key-a".into(), &[73], || {
            let saved: State =
                serde_json::from_slice(&std::fs::read(dir.app.join(STATE_FILE)).unwrap()).unwrap();
            assert_eq!(saved.accepted, [73]);
            let mut socket = std::net::TcpStream::connect(address).unwrap();
            socket
                .write_all(
                    &serde_json::to_vec(&tessera_proto::Request::AckNotifications {
                        ids: vec![73],
                    })
                    .unwrap(),
                )
                .unwrap();
        })
        .unwrap();
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut bytes = Vec::new();
        socket.read_to_end(&mut bytes).unwrap();
        assert!(
            matches!(serde_json::from_slice::<tessera_proto::Request>(&bytes).unwrap(), tessera_proto::Request::AckNotifications { ids } if ids == [73])
        );
    }
    #[test]
    fn accepted_pull_replay_reacks_without_posting_and_ids_are_server_scoped_bounded() {
        let mut state = State::default();
        state.select_server("server-a:key-a".into());
        state.remember(&[7]);
        let pending = PendingNotifications {
            new: vec![message(7, "s1"), message(8, "s1")],
            seen: Vec::new(),
            withdraw: Vec::new(),
            next_due: None,
            waiting: Vec::new(),
        };
        let (mut out, ack, _) = decide(&Settings::default(), &pending, &[], false);
        // Match pull(): carry deliberate burst suppression into the replay
        // result before accepted receipts are added to it.
        out.suppressed = ack;
        state.replay(&mut out);
        assert_eq!(out.posts.len(), 1, "unaccepted post remains replayable");
        assert_eq!(out.posts[0].notification_id, Some(8));
        assert!(
            out.suppressed.contains(&7),
            "accepted post re-Acks even if previous Ack failed"
        );
        let bytes = serde_json::to_vec(&state).unwrap();
        let mut state: State = serde_json::from_slice(&bytes).unwrap();
        state.select_server("server-a:key-a".into());
        assert_eq!(state.accepted, [7]);
        state.remember(&(0..600).collect::<Vec<_>>());
        assert_eq!(state.accepted.len(), 512);
        state.select_server("server-b:key-b".into());
        assert!(state.accepted.is_empty());
        let (mut out, _, _) = decide(&Settings::default(), &pending, &[], false);
        state.replay(&mut out);
        assert_eq!(out.posts.len(), 1);
    }

    fn message(id: u64, session: &str) -> UserNotification {
        UserNotification {
            id,
            title: "Build finished".into(),
            body: "All tests pass.\nThe release is in out/.".into(),
            session_id: Some(session.into()),
            from: "Release bot".into(),
            urgent: false,
            created_at: 1,
            due_at: 2,
            sent: true,
            seen: false,
            kind: String::new(),
            quiet: false,
            private: false,
            source_key: String::new(),
            ..Default::default()
        }
    }

    #[test]
    fn missed_messages_are_shown_once_and_seen_ones_removed() {
        let p = PendingNotifications {
            new: vec![message(7, "s1")],
            seen: vec![3],
            withdraw: Vec::new(),
            next_due: Some(99),
            waiting: vec![
                WaitingChat {
                    session_id: "s2".into(),
                    title: "Deploy".into(),
                    private: false,
                },
                WaitingChat {
                    session_id: "s3".into(),
                    title: "secret plans".into(),
                    private: true,
                },
            ],
        };
        let settings = Settings::default();
        let (out, ack, waiting) = decide(&settings, &p, &["s3".into(), "gone".into()], false);
        assert!(ack.is_empty());
        assert_eq!(waiting, ["s2", "s3"]);
        assert_eq!(out.wake_at, Some(99));
        // n3 was seen elsewhere; "gone" isn't waiting any more.
        assert_eq!(out.withdraw, ["n3", "gone"]);
        assert_eq!(out.posts.len(), 2, "{:?}", out.posts);
        let m = &out.posts[0];
        assert_eq!(
            (m.tag.as_str(), m.agent, m.urgent, m.sub.as_deref()),
            ("s1", true, false, None)
        );
        assert_eq!(m.body, "New activity. Open Tessera for details.");
        assert_eq!(m.public_body, "A message for you");
        // s3 was already announced; s2 is new: its title, never content.
        let a = &out.posts[1];
        assert_eq!(
            (a.tag.as_str(), a.urgent, a.agent, a.body.as_str()),
            ("s2", true, false, "New activity. Open Tessera for details.")
        );

        // Quiet hours: silent. Notifications off: nothing shown, still handled.
        let (out, ..) = decide(&settings, &p, &[], true);
        assert!(out.posts.iter().all(|p| p.silent));
        let off = Settings {
            enabled: false,
            ..Settings::default()
        };
        let (out, ack, _) = decide(&off, &p, &[], false);
        assert!(out.posts.is_empty());
        assert_eq!(ack, [7]);
    }

    #[test]
    fn a_closed_phone_hears_of_a_failed_run_under_its_scheduled_tasks_setting() {
        let failed = UserNotification {
            title: "Scheduled task failed".into(),
            body: "Nightly tests: the build broke".into(),
            from: "Scheduled tasks".into(),
            kind: "job".into(),
            ..message(9, "s9")
        };
        let p = PendingNotifications {
            new: vec![failed],
            seen: vec![],
            withdraw: Vec::new(),
            next_due: None,
            waiting: vec![],
        };
        let (out, ack, _) = decide(&Settings::default(), &p, &[], false);
        assert!(ack.is_empty());
        assert_eq!(out.posts.len(), 1);
        // Automatic: no title, body or sender without previews.
        assert_eq!(
            (
                out.posts[0].agent,
                out.posts[0].title.as_str(),
                out.posts[0].body.as_str(),
                out.posts[0].sub.as_deref()
            ),
            (
                false,
                "Tessera",
                "New activity. Open Tessera for details.",
                None
            )
        );
        let off = Settings {
            jobs: false,
            ..Settings::default()
        };
        let (out, ack, _) = decide(&off, &p, &[], false);
        assert!(out.posts.is_empty());
        assert_eq!(ack, [9], "handled all the same");
    }

    #[test]
    fn private_pull_payloads_are_generic_without_a_session_cache_even_with_previews() {
        for previews in [false, true] {
            for kind in ["job", "approval", "helper", "future-kind", ""] {
                let alert = UserNotification {
                    private: true,
                    kind: kind.into(),
                    title: "SECRET_TITLE".into(),
                    body: "SECRET_BODY".into(),
                    from: "SECRET_SUBTITLE".into(),
                    ..message(912, "unknown")
                };
                let p = PendingNotifications {
                    new: vec![alert],
                    waiting: vec![WaitingChat {
                        session_id: "waiting".into(),
                        title: "SECRET_WAITING_TITLE".into(),
                        private: true,
                    }],
                    seen: vec![],
                    withdraw: Vec::new(),
                    next_due: None,
                };
                let (out, _, _) = decide(
                    &Settings {
                        previews,
                        ..Settings::default()
                    },
                    &p,
                    &[],
                    false,
                );
                assert_eq!(out.posts.len(), if kind == "helper" { 1 } else { 2 });
                for post in out.posts {
                    assert_eq!(
                        (post.title.as_str(), post.body.as_str(), post.sub.as_deref()),
                        ("Tessera", "New activity. Open Tessera for details.", None)
                    );
                    assert!(!serde_json::to_string(&post).unwrap().contains("SECRET"));
                }
            }
        }
    }

    #[test]
    fn server_levels_and_urgent_test_survive_closed_app_preferences() {
        let mut test = message(91, "s1");
        test.kind = "test".into();
        test.source_key = "system_test".into();
        test.urgent = true;
        test.reason = "you asked for this alert".into();
        let mut digest = message(92, "s1");
        digest.kind = "digest".into();
        digest.source_key = "system_digest".into();
        digest.level = tessera_proto::NotificationLevel::Digest;
        digest.reason = "scheduled summary, without sound".into();
        let snapshot = PendingNotifications {
            new: vec![test, digest],
            seen: vec![],
            withdraw: vec![],
            next_due: None,
            waiting: vec![],
        };
        let (posts, _, _) = decide(&Settings::default(), &snapshot, &[], true);
        assert_eq!(posts.posts.len(), 2);
        assert!(!posts.posts[0].silent, "urgent test cannot be quieted");
        assert!(posts.posts[1].silent, "digest is always silent");
        let off = Settings {
            enabled: false,
            ..Settings::default()
        };
        let (posts, _, _) = decide(&off, &snapshot, &[], true);
        assert_eq!(
            posts.posts.len(),
            1,
            "explicit test still reaches an opted-out device"
        );
        assert_eq!(posts.posts[0].notification_id, Some(91));
        let mut first = message(93, "s1");
        first.reason = "a reply you asked for is ready".into();
        let second = UserNotification {
            id: 94,
            title: "A different first occurrence".into(),
            ..first.clone()
        };
        let burst = PendingNotifications {
            new: vec![first, second],
            ..snapshot
        };
        let (posts, _, _) = decide(&Settings::default(), &burst, &[], false);
        assert_eq!(
            posts.posts.len(),
            2,
            "modern policy must not coalesce different first occurrences"
        );
    }
    #[test]
    fn no_server_or_notifications_off_means_no_check() {
        let dir = std::env::temp_dir().join(format!("tessera-pull-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = pull(&dir);
        assert!(out.posts.is_empty() && out.error.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_json_is_what_kotlin_reads() {
        let mut n = Note::agent(&message(5, "s1"));
        n.silent = true;
        let v = serde_json::to_value(post_of(&n)).unwrap();
        for k in [
            "tag",
            "sessionId",
            "urgent",
            "title",
            "body",
            "publicBody",
            "agent",
            "silent",
            "sub",
            "when",
        ] {
            assert!(v.get(k).is_some(), "{k}: {v}");
        }
        let v = serde_json::to_value(Pulled {
            wake_at: Some(1),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(v["wakeAt"], 1);
    }
}
