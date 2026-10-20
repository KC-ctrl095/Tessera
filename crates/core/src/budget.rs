//! How much this machine can spare, read once, and the defaults that follow
//! from it. Only defaults: a value the user set in `config.toml` (or in the
//! apps) always wins. A big machine gets what it always got; a small one
//! (a Raspberry Pi, an old laptop, a 1 GB VPS) gets limits that keep the
//! server well clear of running out of memory.

use std::sync::OnceLock;

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

/// Below this the machine counts as small: the server's own web view
/// browser (350–430 MB for one to three tabs, 394 MB on disk) is off by
/// default. A "2 GB" board reports about 1.8 GiB and keeps it.
pub const SMALL_RAM: u64 = 1700 * MIB;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budget {
    /// Memory this machine (or the container it runs in) has, in bytes.
    /// `None` when it couldn't be read: today's defaults apply.
    pub ram: Option<u64>,
    /// Under [`SMALL_RAM`].
    pub small: bool,
    /// Events kept for connections that read slowly (see `Engine::open`).
    pub event_ring: usize,
    /// Parsed chat histories kept in memory (bytes; 0 = none).
    pub history_cache: usize,
    /// File versions kept for edit checks (bytes).
    pub file_cache: usize,
    /// Finished background commands kept for the `job` tool.
    pub kept_jobs: usize,
    /// How big the database's write-ahead log may stay between writes.
    pub wal_limit: u64,
    /// `[browser] enabled`: web views run in a browser on this server.
    pub web_views: bool,
    /// `[browser] max_tabs`.
    pub max_tabs: usize,
    /// `[agent] max_subagents`.
    pub max_subagents: usize,
    /// `[lsp] max_servers`.
    pub lsp_servers: usize,
    /// `[lsp] max_memory_mb`.
    pub lsp_memory_mb: u64,
}

impl Budget {
    /// The defaults for a machine with `ram` bytes (`None`: unknown).
    pub fn for_ram(ram: Option<u64>) -> Self {
        let Some(r) = ram.filter(|r| *r > 0) else {
            return Self::for_ram(Some(64 * GIB)).with_ram(None);
        };
        let small = r < SMALL_RAM;
        let tiny = r < GIB;
        Self {
            ram: Some(r),
            small,
            event_ring: if small { 1024 } else { 2048 },
            history_cache: if tiny {
                0
            } else {
                (r / 512).min(24 * MIB) as usize
            },
            file_cache: (r / 128).clamp(8 * MIB, 64 * MIB) as usize,
            kept_jobs: if small { 8 } else { 24 },
            wal_limit: if small { 4 * MIB } else { 16 * MIB },
            web_views: !small,
            // A real page takes 0.3–1.3 GB (measured with 12 pages using 10 GB);
            // a blank one about 400 MB. When memory runs low anyway, the
            // browser closes the pages nobody looks at (`memory_watch`).
            max_tabs: (r / GIB).clamp(2, 16) as usize,
            max_subagents: if tiny {
                1
            } else if small {
                2
            } else {
                4
            },
            lsp_servers: if tiny {
                1
            } else if small {
                2
            } else {
                4
            },
            lsp_memory_mb: if small {
                r / 4 / MIB
            } else if r >= 8 * GIB {
                8192
            } else {
                r / 2 / MIB
            },
        }
    }

    fn with_ram(mut self, ram: Option<u64>) -> Self {
        self.ram = ram;
        self
    }
}

/// This machine's budget, worked out on first use.
pub fn get() -> &'static Budget {
    static B: OnceLock<Budget> = OnceLock::new();
    B.get_or_init(|| Budget::for_ram(machine_ram()))
}

/// Memory available to this process: the machine's, or its container's
/// limit when that is lower. `TESSERA_MEMORY_MB` overrides both (a
/// container whose limit can't be seen, or trying the small-machine
/// defaults on a big one).
pub fn machine_ram() -> Option<u64> {
    if let Some(mb) = std::env::var("TESSERA_MEMORY_MB")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|mb| *mb > 0)
    {
        return Some(mb * MIB);
    }
    let total = mem_total();
    match (total, cgroup_limit()) {
        (Some(t), Some(c)) => Some(t.min(c)),
        (t, c) => t.or(c),
    }
}

#[cfg(target_os = "linux")]
fn mem_total() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    meminfo_total(&text)
}

#[cfg(not(target_os = "linux"))]
fn mem_total() -> Option<u64> {
    None
}

/// Memory the host or this process's cgroup can still give out now,
/// whichever is lower. Parent limits and cgroup-namespace roots count too.
#[cfg(target_os = "linux")]
pub fn mem_available() -> Option<u64> {
    let host = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|text| meminfo_field(&text, "MemAvailable:"));
    let group = std::fs::read_to_string("/proc/self/cgroup")
        .ok()
        .and_then(|own| cgroup_memory(std::path::Path::new("/sys/fs/cgroup"), &own).1);
    lower(host, group)
}

#[cfg(not(target_os = "linux"))]
pub fn mem_available() -> Option<u64> {
    None
}

/// Below this much free memory the server makes room (web view pages
/// nobody looks at close): an eighth of the memory, at least 1 GiB.
pub fn low_memory_mark() -> u64 {
    (get().ram.unwrap_or(0) / 8).max(GIB)
}

#[cfg(any(target_os = "linux", test))]
fn meminfo_field(text: &str, field: &str) -> Option<u64> {
    let line = text.lines().find(|l| l.starts_with(field))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

#[cfg(any(target_os = "linux", test))]
fn meminfo_total(text: &str) -> Option<u64> {
    meminfo_field(text, "MemTotal:")
}

/// The lowest memory limit on this process's cgroup and its parents
/// (cgroup v2), or the v1 memory controller's limit.
#[cfg(target_os = "linux")]
fn cgroup_limit() -> Option<u64> {
    let own = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    cgroup_memory(std::path::Path::new("/sys/fs/cgroup"), &own).0
}

#[cfg(any(target_os = "linux", test))]
fn lower(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

#[cfg(any(target_os = "linux", test))]
fn cgroup_memory(root: &std::path::Path, own: &str) -> (Option<u64>, Option<u64>) {
    let mut limit = None;
    let mut available = None;
    let mut keep = |dir: &std::path::Path, max: &str, current: &str| {
        let cap = read_limit(&dir.join(max));
        limit = lower(limit, cap);
        let used = std::fs::read_to_string(dir.join(current))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok());
        let headroom = cap.zip(used).map(|(cap, used)| cap.saturating_sub(used));
        available = lower(available, headroom);
    };
    if let Some(path) = own.lines().find_map(|l| l.strip_prefix("0::")) {
        let relative = std::path::Path::new(path.trim_start_matches('/'));
        if relative
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return (None, None);
        }
        let mut dir = root.join(relative);
        loop {
            keep(&dir, "memory.max", "memory.current");
            // Includes root itself: `0::/` is common in cgroup namespaces.
            if dir == root || !dir.pop() {
                break;
            }
        }
    } else {
        keep(
            &root.join("memory"),
            "memory.limit_in_bytes",
            "memory.usage_in_bytes",
        );
    }
    (limit, available)
}

#[cfg(not(target_os = "linux"))]
fn cgroup_limit() -> Option<u64> {
    None
}

/// A cgroup memory limit file: `max` (or v1's huge "unlimited") is none.
#[cfg(any(target_os = "linux", test))]
fn read_limit(file: &std::path::Path) -> Option<u64> {
    let v: u64 = std::fs::read_to_string(file).ok()?.trim().parse().ok()?;
    (v < (1 << 60)).then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_big_machine_keeps_todays_defaults() {
        let b = Budget::for_ram(Some(15 * GIB));
        assert!(!b.small && b.web_views);
        assert_eq!(
            (b.max_tabs, b.max_subagents, b.lsp_servers, b.lsp_memory_mb),
            (15, 4, 4, 8192)
        );
        assert_eq!((b.file_cache, b.history_cache), (64 << 20, 24 << 20));
        assert_eq!((b.kept_jobs, b.wal_limit), (24, 16 << 20));
        // Unknown: the same as a very big one (64 GiB).
        let unknown = Budget::for_ram(None);
        assert_eq!(unknown.ram, None);
        let big = Budget::for_ram(Some(64 * GIB));
        assert_eq!(unknown.with_ram(big.ram), big);
        assert_eq!(big.max_tabs, 16);
    }

    #[test]
    fn small_machines_get_small_limits() {
        // A Raspberry Pi Zero 2 W: 512 MB, about 430 MiB reported.
        let zero = Budget::for_ram(Some(430 * MIB));
        assert!(zero.small && !zero.web_views);
        assert_eq!(
            (zero.max_subagents, zero.lsp_servers, zero.max_tabs),
            (1, 1, 2)
        );
        assert_eq!(zero.history_cache, 0);
        assert_eq!(zero.file_cache, 8 << 20);
        assert_eq!(zero.lsp_memory_mb, 107);
        assert_eq!(
            (zero.kept_jobs, zero.wal_limit, zero.event_ring),
            (8, 4 << 20, 1024)
        );
        // 1.5 GB: still small, a little more room.
        let mid = Budget::for_ram(Some(1500 * MIB));
        assert!(mid.small && !mid.web_views);
        assert_eq!(
            (mid.max_subagents, mid.lsp_servers, mid.max_tabs),
            (2, 2, 2)
        );
        assert_eq!(mid.history_cache, 3_000 * 1024);
        // A "2 GB" board reports about 1.8 GiB: web views stay on.
        let two = Budget::for_ram(Some(1850 * MIB));
        assert!(!two.small && two.web_views);
        assert_eq!(two.max_tabs, 2);
        assert_eq!(two.lsp_memory_mb, 925);
    }

    #[test]
    fn cgroup_namespace_root_and_parent_pressure_count() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("memory.max"), (2 * GIB).to_string()).unwrap();
        std::fs::write(root.join("memory.current"), (1900 * MIB).to_string()).unwrap();
        assert_eq!(
            cgroup_memory(root, "0::/\n"),
            (Some(2 * GIB), Some(148 * MIB))
        );
        assert_eq!(
            lower(Some(40 * GIB), cgroup_memory(root, "0::/").1),
            Some(148 * MIB)
        );
        std::fs::create_dir(root.join("child")).unwrap();
        std::fs::write(root.join("child/memory.max"), "max").unwrap();
        std::fs::write(root.join("child/memory.current"), "0").unwrap();
        assert_eq!(
            cgroup_memory(root, "0::/child"),
            (Some(2 * GIB), Some(148 * MIB))
        );
        std::fs::write(root.join("memory.current"), (3 * GIB).to_string()).unwrap();
        assert_eq!(cgroup_memory(root, "0::/").1, Some(0));
        assert_eq!(cgroup_memory(root, "0::/../../outside"), (None, None));
    }

    #[test]
    fn reads_meminfo() {
        let text = "MemTotal:       16384000 kB\nMemFree:  100 kB\n";
        assert_eq!(meminfo_total(text), Some(16_384_000 * 1024));
        assert_eq!(meminfo_total("nothing"), None);
        let text = "MemTotal: 16384000 kB\nMemAvailable:    2048000 kB\n";
        assert_eq!(meminfo_field(text, "MemAvailable:"), Some(2_048_000 * 1024));
        assert_eq!(meminfo_field("MemTotal: 1 kB", "MemAvailable:"), None);
    }
}
