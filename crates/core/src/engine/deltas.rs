//! Streamed replies arrive in pieces of a few characters, often a hundred a
//! second. Each piece used to go out as its own event: for every connection
//! a JSON text, a sealed frame and a wake-up (measured on a slow server: 0.5
//! ms of CPU per piece for one watcher, 0.1 ms more per extra one). Pieces
//! that arrive within [`WINDOW`] of the first unsent one now go out as one
//! event. A tool call that starts, the end of the stream and errors go out
//! at once, and the order never changes. The chat's live state follows the
//! events exactly, so a device that fetches the chat meanwhile and then
//! reads the next event sees each piece once.

use std::time::Duration;

use tessera_proto::Event;
use tokio::sync::{broadcast, mpsc};

use super::{Rt, apply_delta};
use crate::llm::LlmEvent;

/// How long a piece may wait for more (a reader doesn't notice 40 ms; the
/// apps draw at most once a frame anyway).
pub(super) const WINDOW: Duration = Duration::from_millis(40);

/// Add `ev` to `pending` when it continues it (the same text, reasoning or
/// tool call); otherwise give it back.
pub(super) fn merge(pending: &mut Option<LlmEvent>, ev: LlmEvent) -> Option<LlmEvent> {
    match (pending.as_mut(), ev) {
        (
            Some(LlmEvent::Text { index, text }),
            LlmEvent::Text {
                index: i,
                text: more,
            },
        ) if *index == i => {
            text.push_str(&more);
            None
        }
        (
            Some(LlmEvent::Thinking { index, text }),
            LlmEvent::Thinking {
                index: i,
                text: more,
            },
        ) if *index == i => {
            text.push_str(&more);
            None
        }
        (
            Some(LlmEvent::ToolCall {
                index,
                id,
                name,
                arguments,
            }),
            LlmEvent::ToolCall {
                index: i,
                id: id2,
                name: name2,
                arguments: more,
            },
        ) if *index == i && (id2.is_empty() || *id == id2) => {
            if name.is_empty() {
                *name = name2;
            }
            arguments.push_str(&more);
            None
        }
        (_, ev) => Some(ev),
    }
}

/// Batches what comes in `index` order and says when to send.
#[derive(Default)]
pub(super) struct Batcher {
    pending: Option<LlmEvent>,
    due: Option<tokio::time::Instant>,
    /// The tool call the last piece belonged to.
    last_call: Option<u32>,
}

impl Batcher {
    /// Take `ev`: what must go out now, in order (at most two events).
    pub(super) fn push(&mut self, ev: LlmEvent) -> Vec<LlmEvent> {
        let Some(ev) = merge(&mut self.pending, ev) else {
            return Vec::new();
        };
        // Something else: what waited goes first.
        let mut out: Vec<LlmEvent> = self.pending.take().into_iter().collect();
        self.due = None;
        let starts_call =
            matches!(&ev, LlmEvent::ToolCall { index, .. } if self.last_call != Some(*index));
        self.last_call = match &ev {
            LlmEvent::ToolCall { index, .. } => Some(*index),
            _ => None,
        };
        if starts_call {
            out.push(ev);
        } else {
            self.pending = Some(ev);
            self.due = Some(tokio::time::Instant::now() + WINDOW);
        }
        out
    }

    /// When what waits must go out.
    pub(super) fn due(&self) -> Option<tokio::time::Instant> {
        self.due
    }

    /// What waits, now.
    pub(super) fn take(&mut self) -> Option<LlmEvent> {
        self.due = None;
        self.pending.take()
    }
}

/// Apply `ev` to the chat's live reply and tell the devices.
pub(super) fn send(rt: &Rt, events: &broadcast::Sender<Event>, sid: &str, mut ev: LlmEvent) {
    // A partial JSON string cannot be safely scrubbed (escapes and secret
    // values can cross deltas). Publish arguments with the completed, redacted
    // entry instead. Execution uses the provider's separate final response.
    if let LlmEvent::ToolCall { arguments, .. } = &mut ev {
        arguments.clear();
    }
    let deltas = {
        let mut st = rt.st.lock();
        // A check's reply is protocol until committed. Real work/answers are
        // still published as entries, but no transient mark reaches a device.
        let quiet = st.internal.completion_pending.is_some();
        let Some(s) = st.streaming.as_mut() else {
            return;
        };
        let before: String = s
            .content
            .iter()
            .filter_map(|p| match p {
                tessera_proto::AssistantPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .flat_map(str::chars)
            .take(32)
            .collect();
        let held = crate::tools::meta::possible_check_ack(&before);
        let delta = apply_delta(s, &ev);
        if quiet {
            return;
        }
        let after: String = s
            .content
            .iter()
            .filter_map(|p| match p {
                tessera_proto::AssistantPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .flat_map(str::chars)
            .take(32)
            .collect();
        if matches!(ev, LlmEvent::Text { .. }) {
            if crate::tools::meta::possible_check_ack(&after) {
                return;
            }
            if held {
                s.content
                    .iter()
                    .enumerate()
                    .filter_map(|(i, p)| match p {
                        tessera_proto::AssistantPart::Text { text } => {
                            Some(tessera_proto::StreamDelta::Text {
                                index: i as u32,
                                text: text.clone(),
                            })
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            } else {
                vec![delta]
            }
        } else {
            vec![delta]
        }
    };
    for delta in deltas {
        let _ = events.send(Event::StreamDelta {
            session_id: sid.to_string(),
            delta,
        });
    }
}

/// Release held text only once it is known not to be an acknowledgement.
/// Called before StreamEnded by both engines, so terminal runs see it too.
pub(super) fn finish(rt: &Rt, events: &broadcast::Sender<Event>, sid: &str) {
    let st = rt.st.lock();
    let Some(s) = &st.streaming else { return };
    let text: String = s
        .content
        .iter()
        .filter_map(|p| match p {
            tessera_proto::AssistantPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    if text.is_empty() || crate::tools::meta::is_check_ack(&text) {
        return;
    }
    let prefix: String = text.chars().take(32).collect();
    if st.internal.completion_pending.is_some() || crate::tools::meta::possible_check_ack(&prefix) {
        for (i, p) in s.content.iter().enumerate() {
            if let tessera_proto::AssistantPart::Text { text } = p {
                let _ = events.send(Event::StreamDelta {
                    session_id: sid.into(),
                    delta: tessera_proto::StreamDelta::Text {
                        index: i as u32,
                        text: text.clone(),
                    },
                });
            }
        }
    }
}

/// Mirror one model response's pieces with `emit` (see [`send`]),
/// batched as the module notes say. Ends, with nothing left waiting, when
/// `rx` closes.
pub(super) async fn forward(
    mut rx: mpsc::UnboundedReceiver<LlmEvent>,
    mut emit: impl FnMut(LlmEvent),
) {
    let mut batch = Batcher::default();
    loop {
        let next = match batch.due() {
            Some(at) => tokio::select! {
                ev = rx.recv() => ev,
                _ = tokio::time::sleep_until(at) => {
                    if let Some(ev) = batch.take() {
                        emit(ev);
                    }
                    continue;
                }
            },
            None => rx.recv().await,
        };
        let Some(ev) = next else { break };
        for ev in batch.push(ev) {
            emit(ev);
        }
    }
    if let Some(ev) = batch.take() {
        emit(ev);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn text(i: u32, t: &str) -> LlmEvent {
        LlmEvent::Text {
            index: i,
            text: t.into(),
        }
    }

    fn call(i: u32, id: &str, args: &str) -> LlmEvent {
        LlmEvent::ToolCall {
            index: i,
            id: id.into(),
            name: if id.is_empty() { "" } else { "bash" }.into(),
            arguments: args.into(),
        }
    }

    fn show(evs: &[LlmEvent]) -> Vec<String> {
        evs.iter()
            .map(|e| match e {
                LlmEvent::Text { index, text } => format!("text{index}:{text}"),
                LlmEvent::Thinking { index, text } => format!("think{index}:{text}"),
                LlmEvent::ToolCall {
                    index,
                    id,
                    name,
                    arguments,
                } => format!("call{index}:{id}:{name}:{arguments}"),
            })
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn pieces_go_out_together_and_in_order() {
        let mut b = Batcher::default();
        let mut sent = Vec::new();
        for ev in [
            LlmEvent::Thinking {
                index: 0,
                text: "hm".into(),
            },
            LlmEvent::Thinking {
                index: 0,
                text: "m".into(),
            },
            text(1, "Hel"),
            text(1, "lo"),
            text(1, " there"),
        ] {
            sent.extend(b.push(ev));
        }
        // The reasoning went out when the text began; the text waits.
        assert_eq!(show(&sent), ["think0:hmm"]);
        assert!(b.due().is_some());
        // A tool call flushes the text and goes out at once; its further
        // argument pieces are batched, a new call is not.
        sent.extend(b.push(call(2, "c1", "{\"comm")));
        sent.extend(b.push(call(2, "", "and\":")));
        sent.extend(b.push(call(2, "", "\"ls\"}")));
        sent.extend(b.push(call(3, "c2", "{}")));
        sent.extend(b.take());
        assert_eq!(
            show(&sent),
            [
                "think0:hmm",
                "text1:Hello there",
                "call2:c1:bash:{\"comm",
                "call2:::and\":\"ls\"}",
                "call3:c2:bash:{}"
            ]
        );
        assert!(b.take().is_none() && b.due().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn forward_sends_a_window_at_a_time_and_everything_at_the_end() {
        let (tx, rx) = mpsc::unbounded_channel();
        let got = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let sink = got.clone();
        let task = tokio::spawn(forward(rx, move |ev| sink.lock().push(ev)));
        // 100 pieces a second for half a second: about 13 events, not 50.
        for i in 0..50 {
            tx.send(text(0, &format!("w{i} "))).unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        drop(tx);
        task.await.unwrap();
        let got = got.lock();
        assert!((10..=17).contains(&got.len()), "{} events", got.len());
        let all: String = show(&got)
            .iter()
            .map(|s| s.trim_start_matches("text0:"))
            .collect();
        let want: String = (0..50).map(|i| format!("w{i} ")).collect();
        assert_eq!(all, want);
    }
}
