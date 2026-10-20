//! What this process's memory is made of, as far as it can tell itself:
//! the kernel's counts (the server marks itself non-dumpable, so other
//! processes can't read its `smaps`) and the C allocator's own books
//! (bytes in use versus freed but still held). `tessera doctor` shows them.

use serde::{Deserialize, Serialize};

/// The kernel's view of this process (`/proc/self/status`), in bytes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProcessMemory {
    /// Resident: in RAM now, shared library pages included.
    pub resident: u64,
    /// Private (anonymous) pages in RAM: what this process alone costs.
    pub private: u64,
    /// Pages of files (the program itself, libraries) in RAM.
    pub file: u64,
    /// The most it was ever resident.
    pub peak: u64,
    pub virtual_size: u64,
    pub swapped: u64,
    pub threads: u32,
}

pub fn process() -> Option<ProcessMemory> {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/self/status").ok()?;
        Some(parse_status(&text))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_status(text: &str) -> ProcessMemory {
    let num = |key: &str| -> u64 {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
            .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
            .unwrap_or(0)
    };
    let kb = |key: &str| num(key) * 1024;
    ProcessMemory {
        resident: kb("VmRSS"),
        private: kb("RssAnon"),
        file: kb("RssFile"),
        peak: kb("VmHWM"),
        virtual_size: kb("VmSize"),
        swapped: kb("VmSwap"),
        threads: num("Threads") as u32,
    }
}

/// The C allocator's books (glibc), in bytes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HeapStats {
    /// Handed out and not freed yet.
    pub in_use: u64,
    /// Freed by the program but still held by the allocator (it reuses it
    /// before asking the system for more; `malloc_trim` gives it back).
    pub free_held: u64,
    /// Of `free_held`, at the very top of the main heap (given back by
    /// itself once it passes the trim threshold).
    pub top_free: u64,
    /// Big blocks mapped one by one (part of `in_use`).
    pub mapped: u64,
    /// Separate heaps the allocator keeps for threads (each holds its own
    /// free memory). 0 when unknown.
    pub arenas: u32,
}

/// The allocator's books, where the allocator says (glibc on Linux).
pub fn heap() -> Option<HeapStats> {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        glibc::heap()
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    {
        None
    }
}

/// Fewer allocator arenas: glibc gives every thread that meets contention
/// a heap of its own (up to 8 per core), and each keeps its freed memory.
/// Two are enough for the server's few worker threads (measured: private
/// memory after a burst of work 48 → 36 MB, +1.7% CPU). An explicit
/// `MALLOC_ARENA_MAX` in the environment wins. Call before threads start.
pub fn limit_arenas() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    if std::env::var_os("MALLOC_ARENA_MAX").is_none() {
        // SAFETY: mallopt only adjusts allocator parameters.
        unsafe {
            libc::mallopt(libc::M_ARENA_MAX, 2);
        }
    }
}

/// Give memory freed by the program back to the system (glibc keeps it
/// otherwise, in the middle of its heaps too). Returns whether there was
/// something to give back. Takes a few milliseconds on a large heap.
pub fn trim() -> bool {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: malloc_trim is safe to call at any time.
        unsafe { libc::malloc_trim(0) == 1 }
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    {
        false
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod glibc {
    use super::HeapStats;

    /// `mallinfo2` came with glibc 2.33; release builds run on 2.28 too, so
    /// it is looked up rather than linked (`mallinfo`'s counters wrap at
    /// 4 GB, which is fine below that).
    pub fn heap() -> Option<HeapStats> {
        // SAFETY: looking up a symbol by a NUL-terminated name.
        let f = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"mallinfo2".as_ptr()) };
        let mut h = if f.is_null() {
            // SAFETY: plain call into the allocator.
            let m = unsafe { libc::mallinfo() };
            let u = |v: libc::c_int| v as u32 as u64;
            HeapStats {
                in_use: u(m.uordblks) + u(m.hblkhd),
                free_held: u(m.fordblks),
                top_free: u(m.keepcost),
                mapped: u(m.hblkhd),
                arenas: 0,
            }
        } else {
            // SAFETY: `mallinfo2` has exactly this signature.
            let f: extern "C" fn() -> libc::mallinfo2 = unsafe { std::mem::transmute(f) };
            let m = f();
            HeapStats {
                in_use: (m.uordblks + m.hblkhd) as u64,
                free_held: m.fordblks as u64,
                top_free: m.keepcost as u64,
                mapped: m.hblkhd as u64,
                arenas: 0,
            }
        };
        h.arenas = arenas().unwrap_or(0);
        Some(h)
    }

    /// Arenas in use, counted from `malloc_info`'s report (one `<heap>` each).
    fn arenas() -> Option<u32> {
        let mut buf: *mut libc::c_char = std::ptr::null_mut();
        let mut len: libc::size_t = 0;
        // SAFETY: open_memstream allocates `buf`, which we free after the
        // stream is closed; malloc_info writes its XML report into it.
        unsafe {
            let f = libc::open_memstream(&mut buf, &mut len);
            if f.is_null() {
                return None;
            }
            let ok = libc::malloc_info(0, f) == 0;
            libc::fclose(f);
            let text = if ok && !buf.is_null() {
                let bytes = std::slice::from_raw_parts(buf as *const u8, len);
                Some(String::from_utf8_lossy(bytes).into_owned())
            } else {
                None
            };
            libc::free(buf as *mut libc::c_void);
            text.map(|t| t.matches("<heap nr=").count() as u32)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_kernels_counts() {
        let text = "Name:\ttessera\nVmHWM:\t   21384 kB\nVmRSS:\t   21000 kB\nRssAnon:\t    6600 kB\nRssFile:\t   14400 kB\nVmSize:\t  310000 kB\nThreads:\t5\n";
        let m = parse_status(text);
        assert_eq!(m.resident, 21000 * 1024);
        assert_eq!(m.private, 6600 * 1024);
        assert_eq!(m.peak, 21384 * 1024);
        assert_eq!(m.threads, 5);
        assert_eq!(m.swapped, 0);
    }

    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn the_allocator_tells_what_it_holds() {
        let h = heap().expect("glibc reports");
        assert!(h.in_use > 0);
        assert!(h.arenas >= 1, "{h:?}");
        let _ = trim();
        assert!(process().is_some_and(|p| p.resident > 0 && p.threads >= 1));
    }
}
