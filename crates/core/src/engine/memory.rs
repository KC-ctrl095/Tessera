//! Where the server's memory goes (`memory_report`, shown by `tessera
//! doctor`): the kernel's and the allocator's counts beside the sizes of the
//! buffers the server keeps (events waiting for slow connections, caches,
//! background commands, chats held in memory).

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::Inner;
use crate::memstats::{HeapStats, ProcessMemory};
use crate::util::human_bytes;

/// What one connection holds, kept up to date by the server while it lives.
#[derive(Debug)]
pub struct ConnStats {
    pub device: String,
    /// A machine (`tessera node`): it gets no chat events.
    pub machine: AtomicBool,
    started: std::time::Instant,
    /// Events the shared queue still keeps for it.
    pub waiting: AtomicUsize,
    /// Bytes ready to go out to it but not sent yet.
    pub queued_bytes: AtomicUsize,
    pub sent_events: AtomicU64,
    pub sent_bytes: AtomicU64,
    /// Times it fell behind and was asked to fetch its chats again.
    pub resyncs: AtomicU64,
}

/// Connections alive now (dropped ones fall out on the next look).
static CONNECTIONS: Mutex<Vec<Weak<ConnStats>>> = Mutex::new(Vec::new());
/// Events sent to connections and their bytes, for the average size.
static EVENT_BYTES: AtomicU64 = AtomicU64::new(0);
static EVENT_COUNT: AtomicU64 = AtomicU64::new(0);

impl ConnStats {
    /// Count a connection for the report while the returned value lives.
    pub fn register(device: &str, machine: bool) -> Arc<Self> {
        let s = Arc::new(Self {
            device: device.to_string(),
            machine: AtomicBool::new(machine),
            started: std::time::Instant::now(),
            waiting: AtomicUsize::new(0),
            queued_bytes: AtomicUsize::new(0),
            sent_events: AtomicU64::new(0),
            sent_bytes: AtomicU64::new(0),
            resyncs: AtomicU64::new(0),
        });
        let mut all = CONNECTIONS.lock();
        all.retain(|w| w.strong_count() > 0);
        all.push(Arc::downgrade(&s));
        s
    }

    /// An event of `bytes` went out on this connection.
    pub fn sent_event(&self, bytes: usize) {
        self.sent_events.fetch_add(1, Ordering::Relaxed);
        self.sent_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
        EVENT_COUNT.fetch_add(1, Ordering::Relaxed);
        EVENT_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryReport {
    pub pid: u32,
    pub uptime_secs: u64,
    pub process: Option<ProcessMemory>,
    pub heap: Option<HeapStats>,
    /// Memory the machine (or its container) has.
    pub ram: Option<u64>,
    /// Small-machine defaults apply (see `budget.rs`).
    pub small_machine: bool,
    pub event_queue: QueueStats,
    pub connections: Vec<ConnectionMemory>,
    pub history_cache: CacheStats,
    pub file_cache: CacheStats,
    pub jobs: JobStats,
    pub chats: ChatStats,
    /// SQLite's own allocations (page cache, statements).
    pub sqlite_bytes: u64,
    pub browser_running: bool,
    pub browser_tabs: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct QueueStats {
    pub capacity: usize,
    /// Events kept until every connection has taken them.
    pub waiting: usize,
    pub listeners: usize,
    /// Average size of an event as sent (JSON), since the start.
    pub average_event_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionMemory {
    pub device: String,
    pub machine: bool,
    pub seconds: u64,
    pub waiting_events: usize,
    pub queued_bytes: u64,
    pub sent_events: u64,
    pub sent_bytes: u64,
    pub resyncs: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheStats {
    pub items: usize,
    pub bytes: u64,
    pub budget: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct JobStats {
    pub running: usize,
    pub finished: usize,
    pub output_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatStats {
    pub loaded: usize,
    pub running: usize,
    /// Their frozen system prompts (kept for prompt caching).
    pub prompt_bytes: u64,
    /// Replies being written right now.
    pub streaming_bytes: u64,
}

impl Inner {
    pub(super) async fn memory_report(&self) -> MemoryReport {
        let budget = crate::budget::get();
        let names: std::collections::HashMap<String, String> = self
            .store
            .devices(None)
            .unwrap_or_default()
            .into_iter()
            .map(|d| (d.id, d.name))
            .collect();
        let connections = CONNECTIONS
            .lock()
            .iter()
            .filter_map(Weak::upgrade)
            .map(|c| ConnectionMemory {
                device: names.get(&c.device).cloned().unwrap_or(c.device.clone()),
                machine: c.machine.load(Ordering::Relaxed),
                seconds: c.started.elapsed().as_secs(),
                waiting_events: c.waiting.load(Ordering::Relaxed),
                queued_bytes: c.queued_bytes.load(Ordering::Relaxed) as u64,
                sent_events: c.sent_events.load(Ordering::Relaxed),
                sent_bytes: c.sent_bytes.load(Ordering::Relaxed),
                resyncs: c.resyncs.load(Ordering::Relaxed),
            })
            .collect();
        let mut chats = ChatStats::default();
        for rt in self.live_sessions() {
            let st = rt.st.lock();
            chats.loaded += 1;
            chats.running += st.running as usize;
            chats.prompt_bytes += st.internal.system_prompt.as_ref().map_or(0, |p| p.len()) as u64
                + st.internal
                    .bot_prompts
                    .values()
                    .map(|p| p.len() as u64)
                    .sum::<u64>();
            if let Some(s) = &st.streaming {
                chats.streaming_bytes += s
                    .content
                    .iter()
                    .map(|p| serde_json::to_string(p).map_or(0, |t| t.len()) as u64)
                    .sum::<u64>();
            }
        }
        let (history_items, history_bytes) = self.store.history_cache_stats();
        let (file_items, file_bytes) = self.files.stats();
        let (running, finished, output) = crate::tools::bash::job_stats();
        let browser = self.browser.info().await;
        let count = EVENT_COUNT.load(Ordering::Relaxed);
        MemoryReport {
            pid: std::process::id(),
            uptime_secs: uptime_secs(),
            process: crate::memstats::process(),
            heap: crate::memstats::heap(),
            ram: budget.ram,
            small_machine: budget.small,
            event_queue: QueueStats {
                capacity: event_capacity(),
                waiting: self.events.len(),
                listeners: self.events.receiver_count(),
                average_event_bytes: EVENT_BYTES.load(Ordering::Relaxed) / count.max(1),
            },
            connections,
            history_cache: CacheStats {
                items: history_items,
                bytes: history_bytes as u64,
                budget: crate::store::history_cache_budget() as u64,
            },
            file_cache: CacheStats {
                items: file_items,
                bytes: file_bytes as u64,
                budget: crate::tools::filecache::max_bytes() as u64,
            },
            jobs: JobStats {
                running,
                finished,
                output_bytes: output as u64,
            },
            chats,
            sqlite_bytes: sqlite_memory(),
            browser_running: browser.running,
            browser_tabs: browser.tabs,
        }
    }
}

/// After a burst of work glibc keeps the memory it freed (48 MB private
/// after three long chats where 25 MB were left in use, measured): once the
/// server has had nothing running for [`QUIET`], it is given back, once.
#[derive(Default)]
pub(super) struct Quiet {
    since: Option<std::time::Instant>,
    trimmed: bool,
}

/// How long nothing must run before freed memory is given back.
pub(super) const QUIET: std::time::Duration = std::time::Duration::from_secs(60);

impl Quiet {
    /// Look now (the scheduler's rounds): `busy` when a chat is working.
    /// True when memory was given back.
    pub(super) fn look(&mut self, busy: bool) -> bool {
        if busy {
            self.since = None;
            self.trimmed = false;
            return false;
        }
        let since = *self.since.get_or_insert_with(std::time::Instant::now);
        if self.trimmed || since.elapsed() < QUIET {
            return false;
        }
        self.trimmed = true;
        // Not worth it with under 1 MB held.
        crate::memstats::heap().is_none_or(|h| h.free_held > 1 << 20) && crate::memstats::trim()
    }

    /// When to look next (`None`: not until something runs again).
    pub(super) fn next_look(&self) -> Option<std::time::Instant> {
        if self.trimmed {
            return None;
        }
        Some(self.since.unwrap_or_else(std::time::Instant::now) + QUIET)
    }
}

/// How many events the shared queue keeps for listeners that fall behind
/// (server connections take theirs at once, however slowly their device
/// reads: see the server's `connection`). It used to be 8192, which a
/// stalled listener could fill with up to 32 KB tool results each.
pub(super) fn event_capacity() -> usize {
    crate::budget::get().event_ring
}

fn sqlite_memory() -> u64 {
    // SAFETY: a read of SQLite's global allocation counter.
    let used = unsafe { rusqlite::ffi::sqlite3_memory_used() };
    used.max(0) as u64
}

/// How long this process has run.
fn uptime_secs() -> u64 {
    #[cfg(target_os = "linux")]
    {
        let since_boot = std::fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|t| t.split_whitespace().next()?.parse::<f64>().ok());
        // Field 22 of /proc/self/stat: start time in clock ticks after boot
        // (counted after the command name, which may hold spaces).
        let started = std::fs::read_to_string("/proc/self/stat")
            .ok()
            .and_then(|t| {
                let rest = t.rsplit_once(')')?.1;
                rest.split_whitespace().nth(19)?.parse::<f64>().ok()
            });
        // SAFETY: sysconf only reads a system constant.
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64;
        if let (Some(up), Some(st)) = (since_boot, started) {
            return (up - st / hz).max(0.0) as u64;
        }
        0
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

/// "1 chat", "3 chats".
fn n(count: u64, what: &str) -> String {
    format!("{count} {what}{}", if count == 1 { "" } else { "s" })
}

fn duration(secs: u64) -> String {
    match secs {
        s if s < 120 => format!("{s} s"),
        s if s < 7200 => format!("{} min", s / 60),
        s if s < 172_800 => format!("{} h {} min", s / 3600, s / 60 % 60),
        s => format!("{} days", s / 86_400),
    }
}

impl MemoryReport {
    /// The report in plain sentences, one fact a line.
    pub fn describe(&self) -> Vec<String> {
        let b = human_bytes;
        let mut out = Vec::new();
        out.push(format!(
            "Server process {}, running for {}.",
            self.pid,
            duration(self.uptime_secs)
        ));
        if let Some(ram) = self.ram {
            out.push(format!(
                "This machine has {} of memory{}.",
                b(ram),
                if self.small_machine {
                    ": small-machine defaults apply (see the Small machines section of the docs)"
                } else {
                    ""
                }
            ));
        }
        if let Some(p) = &self.process {
            out.push(format!(
                "Memory: {} private to the server, {} resident in all ({} of it program files), {} at most so far; {} threads.",
                b(p.private),
                b(p.resident),
                b(p.file),
                b(p.peak),
                p.threads
            ));
            if p.swapped > 0 {
                out.push(format!("Swapped out: {}.", b(p.swapped)));
            }
        }
        if let Some(h) = &self.heap {
            let arenas = if h.arenas > 0 {
                format!(
                    " in {} arena{}",
                    h.arenas,
                    if h.arenas == 1 { "" } else { "s" }
                )
            } else {
                String::new()
            };
            out.push(format!(
                "Allocator: {} in use{arenas}, {} freed but still held ({} of that at the top of the heap).",
                b(h.in_use),
                b(h.free_held),
                b(h.top_free)
            ));
        }
        let q = &self.event_queue;
        out.push(format!(
            "Event queue: {} of {} waiting for a slow connection (about {}), {} listening; events average {}.",
            n(q.waiting as u64, "event"),
            q.capacity,
            b(q.waiting as u64 * q.average_event_bytes),
            q.listeners,
            b(q.average_event_bytes)
        ));
        if self.connections.is_empty() {
            out.push("Connections: none.".into());
        }
        for c in &self.connections {
            out.push(format!(
                "Connection from {}{}, open {}: {} waiting, {} ready to send; {} ({}) sent{}.",
                c.device,
                if c.machine { " (machine)" } else { "" },
                duration(c.seconds),
                n(c.waiting_events as u64, "event"),
                b(c.queued_bytes),
                n(c.sent_events, "event"),
                b(c.sent_bytes),
                if c.resyncs > 0 {
                    format!(", fell behind {}", n(c.resyncs, "time"))
                } else {
                    String::new()
                }
            ));
        }
        let h = &self.history_cache;
        out.push(format!(
            "Chat history cache: {}, {} (up to {}).",
            n(h.items as u64, "chat"),
            b(h.bytes),
            b(h.budget)
        ));
        let f = &self.file_cache;
        out.push(format!(
            "File cache: {} in {} (up to {}).",
            b(f.bytes),
            n(f.items as u64, "chat"),
            b(f.budget)
        ));
        let j = &self.jobs;
        out.push(format!(
            "Background commands kept: {} running, {} finished, holding {} of output.",
            j.running,
            j.finished,
            b(j.output_bytes)
        ));
        let c = &self.chats;
        out.push(format!(
            "Chats in memory: {} ({} working), {} of saved system prompts, {} of replies being written.",
            c.loaded,
            c.running,
            b(c.prompt_bytes),
            b(c.streaming_bytes)
        ));
        out.push(format!("Database (SQLite): {}.", b(self.sqlite_bytes)));
        out.push(if self.browser_running {
            format!(
                "Web view browser: running with {} (its memory is its own, not counted above).",
                n(self.browser_tabs as u64, "tab")
            )
        } else {
            "Web view browser: not running.".into()
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_reads_plainly() {
        let r = MemoryReport {
            pid: 7,
            uptime_secs: 4500,
            ram: Some(1 << 30),
            small_machine: true,
            process: Some(ProcessMemory {
                resident: 48 << 20,
                private: 30 << 20,
                threads: 5,
                ..Default::default()
            }),
            connections: vec![ConnectionMemory {
                device: "Phone".into(),
                resyncs: 2,
                ..Default::default()
            }],
            ..Default::default()
        };
        let text = r.describe().join("\n");
        assert!(text.contains("running for 75 min"), "{text}");
        assert!(text.contains("small-machine defaults apply"));
        assert!(text.contains("30.0 MB private to the server"), "{text}");
        assert!(text.contains("Connection from Phone"));
        assert!(text.contains("fell behind 2 times"));
        let _ = ConnStats::register("d", false);
    }

    #[test]
    fn memory_goes_back_once_after_a_quiet_minute() {
        let mut q = Quiet::default();
        assert!(!q.look(true));
        assert!(q.next_look().is_some(), "busy: look again later");
        assert!(!q.look(false), "quiet only just now");
        let at = q.next_look().unwrap();
        assert!(at > std::time::Instant::now() + QUIET - std::time::Duration::from_secs(2));
        q.since = Some(std::time::Instant::now() - QUIET);
        q.look(false);
        assert!(
            q.trimmed && q.next_look().is_none(),
            "once, then nothing to do"
        );
        assert!(!q.look(false));
        q.look(true);
        assert!(
            !q.trimmed && q.next_look().is_some(),
            "work again: again later"
        );
    }
}
