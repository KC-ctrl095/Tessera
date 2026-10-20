//! Poll-scoped breadcrumbs for the watchdog. Only fixed operation names:
//! no arguments, session titles, paths, messages, tokens or tool output.
//! A future may move between workers, so a guard lives for one poll only.
//! Reporting never takes a lock that the stuck code might own.

use std::future::Future;
use std::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

pub struct Label(pub &'static str);
pub static REQUEST: Label = Label("request:other");
pub static LIST: Label = Label("request:ListSessions");
pub static SNAPSHOT: Label = Label("request:GetSession");
pub static SEND: Label = Label("request:Send");
pub static UPDATE: Label = Label("request:UpdateSession");
pub static TURN: Label = Label("agent:turn");
pub static CLAUDE: Label = Label("agent:claude-code");
pub static TOOL: Label = Label("tool:execute");
pub static SCHEDULER: Label = Label("task:scheduler");
pub static CHECKPOINT: Label = Label("task:background-checkpoint");
pub static SAVE: Label = Label("session:save");

/// Known tool names only. An extension's name can contain user input, so
/// unknown tools use the fixed fallback rather than entering the report.
pub fn tool(name: &str) -> &'static Label {
    match name {
        "bash" => &Label("tool:bash"),
        "read" => &Label("tool:read"),
        "write" => &Label("tool:write"),
        "edit" => &Label("tool:edit"),
        "grep" => &Label("tool:grep"),
        "glob" => &Label("tool:glob"),
        "browser" => &Label("tool:browser"),
        "machines" => &Label("tool:machines"),
        "web_fetch" => &Label("tool:web_fetch"),
        "web_search" => &Label("tool:web_search"),
        "note" => &Label("tool:note"),
        "recall" => &Label("tool:recall"),
        "agent" => &Label("tool:agent"),
        "agents" => &Label("tool:agents"),
        "task" => &Label("tool:task"),
        _ => &TOOL,
    }
}

const CAPACITY: usize = 256;
struct Slot {
    tid: AtomicU64,
    active: AtomicPtr<Label>,
    parent: AtomicPtr<Label>,
    last: AtomicPtr<Label>,
}
impl Slot {
    const fn new() -> Self {
        Self {
            tid: AtomicU64::new(0),
            active: AtomicPtr::new(std::ptr::null_mut()),
            parent: AtomicPtr::new(std::ptr::null_mut()),
            last: AtomicPtr::new(std::ptr::null_mut()),
        }
    }
}
static SLOTS: [Slot; CAPACITY] = [const { Slot::new() }; CAPACITY];
static NEXT: AtomicUsize = AtomicUsize::new(0);
static MISSED: AtomicUsize = AtomicUsize::new(0);
thread_local! {
    static SLOT: ThreadSlot = ThreadSlot::new();
}

// Reuse slots when blocking-pool threads exit: this must still work after
// months of thread churn, not just for the first 256 threads of a process.
struct ThreadSlot(Option<usize>);
impl ThreadSlot {
    fn new() -> Self {
        let start = NEXT.fetch_add(1, Relaxed);
        #[cfg(target_os = "linux")]
        let tid = unsafe { libc::syscall(libc::SYS_gettid) as u64 };
        for offset in 0..CAPACITY {
            let i = start.wrapping_add(offset) % CAPACITY;
            #[cfg(not(target_os = "linux"))]
            let tid = i as u64 + 1;
            if SLOTS[i]
                .tid
                .compare_exchange(0, tid, std::sync::atomic::Ordering::Acquire, Relaxed)
                .is_ok()
            {
                return Self(Some(i));
            }
        }
        MISSED.fetch_add(1, Relaxed);
        Self(None)
    }
}
impl Drop for ThreadSlot {
    fn drop(&mut self) {
        if let Some(i) = self.0 {
            let s = &SLOTS[i];
            s.active.store(std::ptr::null_mut(), Relaxed);
            s.parent.store(std::ptr::null_mut(), Relaxed);
            s.last.store(std::ptr::null_mut(), Relaxed);
            s.tid.store(0, std::sync::atomic::Ordering::Release);
        }
    }
}
fn slot() -> Option<&'static Slot> {
    SLOT.with(|s| s.0.map(|i| &SLOTS[i]))
}

pub struct Guard {
    slot: Option<&'static Slot>,
    previous: *mut Label,
    parent: *mut Label,
    // A guard must not move to a different worker between enter and drop.
    _local: std::marker::PhantomData<std::rc::Rc<()>>,
}

pub fn enter(label: &'static Label) -> Guard {
    let slot = slot();
    let mut previous = std::ptr::null_mut();
    let mut parent = std::ptr::null_mut();
    if let Some(s) = slot {
        previous = s.active.swap(std::ptr::from_ref(label).cast_mut(), Relaxed);
        parent = s.parent.swap(previous, Relaxed);
        s.last.store(std::ptr::from_ref(label).cast_mut(), Relaxed);
    }
    Guard {
        slot,
        previous,
        parent,
        _local: std::marker::PhantomData,
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(s) = self.slot {
            s.active.store(self.previous, Relaxed);
            s.parent.store(self.parent, Relaxed);
        }
    }
}

pub async fn monitor<F: Future>(label: &'static Label, future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    futures::future::poll_fn(|cx| {
        let _guard = enter(label);
        future.as_mut().poll(cx)
    })
    .await
}

fn name(ptr: *mut Label) -> &'static str {
    // All stored pointers came from &'static Label and never expire.
    unsafe { ptr.as_ref() }.map_or("none", |label| label.0)
}

pub fn report() -> String {
    let began = Instant::now();
    let mut out = String::from(
        "Worker operations (best-effort; active means inside a poll, not an awaited task):\n",
    );
    for s in &SLOTS {
        let tid = s.tid.load(Relaxed);
        if tid == 0 {
            continue;
        }
        #[cfg(target_os = "linux")]
        if !std::path::Path::new(&format!("/proc/self/task/{tid}")).exists() {
            continue;
        }
        let active = s.active.load(Relaxed);
        if active.is_null() {
            out.push_str(&format!(
                "  {tid}: between monitored polls; last={}\n",
                name(s.last.load(Relaxed))
            ));
        } else {
            out.push_str(&format!(
                "  {tid}: active={} parent={}\n",
                name(active),
                name(s.parent.load(Relaxed))
            ));
        }
    }
    let extra = MISSED.load(Relaxed);
    if extra > 0 {
        out.push_str(&format!(
            "  {extra} threads could not register at the breadcrumb limit\n"
        ));
    }
    out.push_str(&format!(
        "  snapshot took {} ms; unmonitored code may still be waiting\n\n",
        began.elapsed().as_millis()
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_operations_restore_the_enclosing_poll() {
        let s = slot().unwrap();
        let outer = enter(&TURN);
        assert_eq!(name(s.active.load(Relaxed)), "agent:turn");
        {
            let _inner = enter(&SAVE);
            assert_eq!(name(s.active.load(Relaxed)), "session:save");
            assert_eq!(name(s.parent.load(Relaxed)), "agent:turn");
        }
        assert_eq!(name(s.active.load(Relaxed)), "agent:turn");
        drop(outer);
        assert!(s.active.load(Relaxed).is_null());
    }
    #[test]
    fn exited_threads_release_slots_beyond_a_full_registry_lifetime() {
        for _ in 0..CAPACITY + 8 {
            std::thread::spawn(|| {
                let _guard = enter(&TURN);
                assert!(slot().is_some());
            })
            .join()
            .unwrap();
        }
        assert!(slot().is_some());
    }

    #[test]
    fn unknown_tool_names_cannot_put_user_text_in_a_report() {
        let _guard = enter(tool("fixture-secret /private/path"));
        let text = report();
        assert!(text.contains("active=tool:execute"));
        assert!(!text.contains("fixture-secret") && !text.contains("/private/path"));
    }

    #[test]
    fn panic_drops_the_active_operation() {
        let s = slot().unwrap();
        let _ = std::panic::catch_unwind(|| {
            let _guard = enter(&TURN);
            panic!("synthetic unwind");
        });
        assert!(s.active.load(Relaxed).is_null());
    }

    #[tokio::test]
    async fn pending_and_cancelled_futures_do_not_leave_a_worker_active() {
        let s = slot().unwrap();
        let mut f = Box::pin(monitor(&TOOL, std::future::pending::<()>()));
        futures::future::poll_fn(|cx| {
            assert!(f.as_mut().poll(cx).is_pending());
            assert!(s.active.load(Relaxed).is_null());
            std::task::Poll::Ready(())
        })
        .await;
        drop(f);
        assert!(s.active.load(Relaxed).is_null());
        assert!(report().contains("last=tool:execute"));
    }
}
