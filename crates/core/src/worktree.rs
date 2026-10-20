//! Git worktrees that isolate a subagent's edits. The subagent works in a
//! private, detached checkout of the parent's repository (inside the
//! repository's own git directory, `.git/tessera/worktrees/<id>`) that
//! starts from the parent's current state, uncommitted changes included.
//! When it finishes, its changes are kept as a binary patch (an artifact)
//! and the checkout is removed; the parent then applies or discards the
//! patch. A checkout is re-made from the patch to continue the subagent.
//!
//! Every live checkout is listed in the store, so one left behind by a
//! crash is saved as a patch and removed at the next start.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

tokio::task_local! {
    static ADMISSION: crate::tools::admission::ActionAdmission;
}

/// Agent-requested setup/apply only: host collection/rollback is outside.
/// Every git start and blocking copy checks the same queued admission.
/// Already-started git is cancelled if its authority changes.
/// No gate is kept across an await.
pub(crate) async fn admitted<F: std::future::Future>(
    admission: crate::tools::admission::ActionAdmission,
    future: F,
) -> F::Output {
    ADMISSION.scope(admission, future).await
}

fn admission() -> crate::tools::admission::ActionAdmission {
    ADMISSION.try_with(Clone::clone).unwrap_or_default()
}

async fn cleanup<F: std::future::Future>(future: F) -> F::Output {
    admitted(Default::default(), future).await
}

/// Where an isolated subagent's work stands.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeStatus {
    /// Requested; the checkout is made when the subagent gets a slot.
    #[default]
    New,
    /// The checkout exists and the subagent is (or was last) working in it.
    Active,
    /// Finished with changes waiting for `apply` or `discard`.
    Pending,
    /// Finished without changing anything.
    Empty,
    Applied,
    Discarded,
}

/// Engine-private state of an isolated subagent (persisted with its session).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WorktreeState {
    /// Top level of the parent's repository, where changes are applied.
    pub repo: String,
    /// The parent's working directory relative to `repo`; the subagent
    /// works in the same subdirectory of its checkout.
    pub subdir: String,
    /// The checkout.
    pub path: String,
    /// Commit the checkout was made from.
    pub head: String,
    /// Tree of the starting state (`head` plus the parent's uncommitted
    /// changes): the subagent's changes are everything since.
    pub base: String,
    /// Artifact holding the changes as a binary git patch.
    pub patch: Option<String>,
    /// Changed paths, relative to `repo`.
    pub files: Vec<String>,
    pub added: u64,
    pub removed: u64,
    pub status: WorktreeStatus,
    /// Checked repository/metadata identities retained across continuations.
    pub repo_scope: Option<crate::tools::file_object::DirectoryScope>,
    pub git_scope: Option<crate::tools::file_object::DirectoryScope>,
    pub common_scope: Option<crate::tools::file_object::DirectoryScope>,
    /// Active checkout's working folder (rebound only after authorized recreation).
    pub cwd_scope: Option<crate::tools::file_object::DirectoryScope>,
}

impl WorktreeState {
    /// "3 files, +12 −4".
    pub fn stat(&self) -> String {
        let n = self.files.len();
        format!(
            "{n} file{}, +{} −{}",
            if n == 1 { "" } else { "s" },
            self.added,
            self.removed
        )
    }

    /// Changed paths for a summary line (at most `max`).
    pub fn file_list(&self, max: usize) -> String {
        let mut s = self
            .files
            .iter()
            .take(max)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if self.files.len() > max {
            s.push_str(&format!(" and {} more", self.files.len() - max));
        }
        s
    }
}

/// The work tree enclosing `dir` (the nearest directory with a `.git`),
/// found without running git.
pub fn find_top(dir: &Path) -> Option<PathBuf> {
    let mut d = Some(dir);
    while let Some(p) = d {
        if p.join(".git").exists() {
            return Some(p.to_path_buf());
        }
        d = p.parent();
    }
    None
}

/// Untracked files copied into a new checkout: at most this much in total.
const UNTRACKED_BUDGET: u64 = 5 * 1024 * 1024;

/// The agent's command sandbox (as `bash` gets it), which Tessera's own git
/// commands here run in too. The repository, its config and its attributes
/// are the agent's to change, so a filter or diff driver set up there runs
/// with no more than the agent's own commands get, and without the
/// server's credentials in its environment.
pub type Sandbox<'a> = Option<&'a (crate::sandbox::Spec, bool)>;

async fn git(
    dir: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
    sb: Sandbox<'_>,
) -> Result<Vec<u8>, String> {
    let cwd = crate::tools::file_object::DirectoryScope::capture(dir)?.open()?;
    git_in_directory(cwd, args, stdin, sb).await
}

async fn git_in_directory(
    cwd: crate::tools::file_object::ProcessDirectory,
    args: &[&str],
    stdin: Option<&[u8]>,
    sb: Sandbox<'_>,
) -> Result<Vec<u8>, String> {
    let prepared = cwd.prepare(sb)?;
    // -C with an absolute path would undo descriptor entry in the child.
    let mut argv: Vec<std::ffi::OsString> = Vec::new();
    // Nothing of the user's runs as a side effect (hooks), and nothing
    // waits for a terminal.
    argv.extend(
        [
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
        ]
        .into_iter()
        .chain(args.iter().copied())
        .map(Into::into),
    );
    let (program, argv) = cwd.argv(prepared.as_ref(), "git".into(), argv);
    let mut cmd = tokio::process::Command::new(program);
    crate::platform::no_window(&mut cmd);
    cmd.args(argv)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    crate::tools::bash::withhold_env(&mut cmd);
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            crate::tools::bash::yield_to_server();
            Ok(())
        });
    }
    let held = cwd.apply(&mut cmd, prepared);
    let admission = admission();
    let mut child = admission
        .run(|| crate::platform::spawn(&mut cmd, crate::platform::Start::Agent))
        .map_err(|e| {
            format!(
                "cannot run git: {}",
                crate::tools::file_object::launch_error(&e)
            )
        })?;
    let _held = held.after_spawn();
    let tree = crate::tools::bash::ChildTree::capture(child.id().unwrap_or(0));
    if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let data = data.to_vec();
        tokio::spawn(async move {
            let _ = pipe.write_all(&data).await;
        });
    }
    let output = child.wait_with_output();
    tokio::pin!(output);
    let out = tokio::select! {
        biased;
        _ = admission.changed() => {
            tree.stop();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(3), &mut output).await;
            return Err("The chat's permissions changed; worktree setup was cancelled.".into());
        }
        out = tokio::time::timeout(
            std::time::Duration::from_secs(300), &mut output,
        ) => out,
    }
    .map_err(|_| format!("git {} took too long", args[0]))?
    .map_err(|e| format!("git {}: {e}", args[0]))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .take(8)
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!(
            "git {}: {}",
            args[0],
            crate::util::truncate(&err, 800)
        ));
    }
    tree.disarm();
    Ok(out.stdout)
}

async fn git_line(dir: &Path, args: &[&str], sb: Sandbox<'_>) -> Result<String, String> {
    Ok(String::from_utf8_lossy(&git(dir, args, None, sb).await?)
        .trim()
        .to_string())
}

/// Patches are made by git itself: never by an external diff program or a
/// text conversion the repository's config names.
const PLAIN_DIFF: [&str; 4] = ["--binary", "--no-renames", "--no-ext-diff", "--no-textconv"];

/// A repository, as git sees it from some directory inside.
pub struct Repo {
    pub top: PathBuf,
    /// The shared git directory (`.git` of the main checkout).
    pub common: PathBuf,
    pub git_dir: PathBuf,
    pub head: String,
}

pub async fn repo(dir: &Path, sb: Sandbox<'_>) -> Result<Repo, String> {
    let top = PathBuf::from(git_line(dir, &["rev-parse", "--show-toplevel"], sb).await?);
    let git_dir = PathBuf::from(git_line(dir, &["rev-parse", "--absolute-git-dir"], sb).await?);
    let common = PathBuf::from(git_line(dir, &["rev-parse", "--git-common-dir"], sb).await?);
    let common = if common.is_absolute() {
        common
    } else {
        dir.join(common)
    };
    let head = git_line(
        dir,
        &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
        sb,
    )
    .await
    .map_err(|_| "the repository has no commits yet; commit once, then retry".to_string())?;
    Ok(Repo {
        top: crate::platform::canonicalize(&top).map_err(|e| e.to_string())?,
        git_dir: crate::platform::canonicalize(&git_dir).map_err(|e| e.to_string())?,
        common: crate::platform::canonicalize(&common).map_err(|e| e.to_string())?,
        head,
    })
}

/// Where subagent `id`'s checkout of `repo` lives.
pub fn path_for(repo: &Repo, id: &str) -> PathBuf {
    repo.common.join("tessera").join("worktrees").join(id)
}

/// Make a detached checkout at `path` holding the repository's current
/// state: HEAD, uncommitted changes and (up to a size budget) untracked
/// files. Returns the tree of that state and how many untracked files
/// didn't fit.
pub async fn create(repo: &Repo, path: &Path, sb: Sandbox<'_>) -> Result<(String, usize), String> {
    admission().check()?;
    // git makes the folders leading to `path` itself, in the sandbox.
    let p = path.to_string_lossy().to_string();
    let setup = async {
        git(
            &repo.top,
            &["worktree", "add", "--detach", "--quiet", &p, &repo.head],
            None,
            sb,
        )
        .await?;
        fill(repo, path, sb).await
    }
    .await;
    if setup.is_err() && path.exists() {
        cleanup(remove(&repo.top, path, sb)).await;
    }
    setup
}

async fn fill(repo: &Repo, path: &Path, sb: Sandbox<'_>) -> Result<(String, usize), String> {
    let mut args = vec!["diff", "HEAD"];
    args.extend(PLAIN_DIFF);
    let dirty = git(&repo.top, &args, None, sb).await?;
    if !dirty.is_empty() {
        git(
            path,
            &["apply", "--binary", "--whitespace=nowarn", "-"],
            Some(&dirty),
            sb,
        )
        .await?;
    }
    let others = git(
        &repo.top,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        None,
        sb,
    )
    .await?;
    let rels: Vec<PathBuf> = others
        .split(|b| *b == 0)
        .filter(|r| !r.is_empty())
        .map(|r| PathBuf::from(String::from_utf8_lossy(r).to_string()))
        .collect();
    let (top, to) = (repo.top.clone(), path.to_path_buf());
    let admission = admission();
    let skipped =
        tokio::task::spawn_blocking(move || admission.run(|| Ok(copy_untracked(&top, &to, &rels))))
            .await
            .map_err(|e| format!("copying untracked files failed: {e}"))?
            .map_err(|e| e.to_string())?;
    let base = snapshot_tree(path, sb).await?;
    Ok((base, skipped))
}

/// Copy the untracked files `rels` from `top` into the checkout `to`, up to
/// [`UNTRACKED_BUDGET`]; returns how many were left out. This runs outside
/// the agent's sandbox while the agent can still change both trees, so it
/// never goes through a link: a folder swapped for one mid-copy is skipped.
#[cfg(unix)]
fn copy_untracked(top: &Path, to: &Path, rels: &[PathBuf]) -> usize {
    use crate::util::beneath;
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    let (Ok(src_root), Ok(dst_root)) = (beneath::open_dir(top), beneath::open_dir(to)) else {
        return rels.len();
    };
    let mut budget = UNTRACKED_BUDGET;
    let mut skipped = 0;
    for rel in rels {
        let copied = (|| -> std::io::Result<bool> {
            let (Some(name), parent) = (rel.file_name(), rel.parent().unwrap_or(Path::new("")))
            else {
                return Ok(false);
            };
            let from_dir = beneath::dir(&src_root, parent, false)?;
            let st = beneath::lstat_at(&from_dir, name)?;
            let kind = st.st_mode & libc::S_IFMT;
            if kind == libc::S_IFLNK {
                let target = beneath::read_link_at(&from_dir, name)?;
                beneath::symlink_at(&target, &beneath::dir(&dst_root, parent, true)?, name)?;
                return Ok(true);
            }
            if kind != libc::S_IFREG || st.st_size as u64 > budget {
                return Ok(false);
            }
            // Non-blocking: a pipe put there meanwhile can't stall the copy.
            let from = beneath::open_at(&from_dir, name, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
            let meta = from.metadata()?;
            if !meta.is_file() || meta.len() > budget {
                return Ok(false);
            }
            let mut out = beneath::open_at(
                &beneath::dir(&dst_root, parent, true)?,
                name,
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                meta.permissions().mode() & 0o777,
            )?;
            let n = std::io::copy(&mut from.take(budget), &mut out)?;
            budget -= n;
            Ok(true)
        })()
        .unwrap_or(false);
        if !copied {
            skipped += 1;
        }
    }
    skipped
}

#[cfg(not(unix))]
fn copy_untracked(top: &Path, to: &Path, rels: &[PathBuf]) -> usize {
    let mut budget = UNTRACKED_BUDGET;
    let mut skipped = 0;
    for rel in rels {
        let (from, to) = (top.join(rel), to.join(rel));
        let copied = std::fs::symlink_metadata(&from)
            .ok()
            .filter(|m| m.is_file() && m.len() <= budget)
            .is_some_and(|m| {
                if let Some(parent) = to.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let ok = std::fs::copy(&from, &to).is_ok();
                if ok {
                    budget -= m.len();
                }
                ok
            });
        if !copied {
            skipped += 1;
        }
    }
    skipped
}

/// The tree of the checkout's current files (the index is left as HEAD, so
/// `git status` there shows what differs from the commit, as it would for
/// the parent).
pub async fn snapshot_tree(path: &Path, sb: Sandbox<'_>) -> Result<String, String> {
    git(path, &["add", "-A"], None, sb).await?;
    let tree = git_line(path, &["write-tree"], sb).await?;
    git(path, &["reset", "-q"], None, sb).await?;
    Ok(tree)
}

/// Re-make a removed checkout as the subagent left it: the starting state
/// plus its `patch`.
pub async fn recreate(
    top: &Path,
    path: &Path,
    head: &str,
    base: &str,
    patch: Option<&[u8]>,
    sb: Sandbox<'_>,
) -> Result<(), String> {
    admission().check()?;
    if path.exists() {
        remove(top, path, sb).await;
    }
    let p = path.to_string_lossy().to_string();
    let restored = async {
        git(
            top,
            &[
                "worktree",
                "add",
                "--detach",
                "--no-checkout",
                "--quiet",
                &p,
                head,
            ],
            None,
            sb,
        )
        .await?;
        git(path, &["read-tree", "-u", "--reset", base], None, sb)
            .await
            .map_err(|e| format!("its starting state is gone from the repository ({e})"))?;
        git(path, &["reset", "-q"], None, sb).await?;
        if let Some(patch) = patch.filter(|p| !p.is_empty()) {
            git(
                path,
                &["apply", "--binary", "--whitespace=nowarn", "-"],
                Some(patch),
                sb,
            )
            .await?;
        }
        Ok::<_, String>(())
    }
    .await;
    if restored.is_err() {
        cleanup(remove(top, path, sb)).await;
    }
    restored
}

/// What changed in a checkout since `base`.
pub struct Diff {
    pub patch: Vec<u8>,
    pub files: Vec<String>,
    pub added: u64,
    pub removed: u64,
}

pub async fn diff(path: &Path, base: &str, sb: Sandbox<'_>) -> Result<Diff, String> {
    git(path, &["add", "-A"], None, sb).await?;
    let mut args = vec!["diff", "--cached"];
    args.extend(PLAIN_DIFF);
    args.push(base);
    let patch = git(path, &args, None, sb).await?;
    let numstat = git(
        path,
        &[
            "diff",
            "--cached",
            "--numstat",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            "-z",
            base,
        ],
        None,
        sb,
    )
    .await?;
    let _ = git(path, &["reset", "-q"], None, sb).await;
    let (mut files, mut added, mut removed) = (Vec::new(), 0, 0);
    for rec in numstat.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let rec = String::from_utf8_lossy(rec);
        let mut parts = rec.splitn(3, '\t');
        let (a, r, name) = (parts.next(), parts.next(), parts.next());
        if let Some(name) = name {
            added += a.and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            removed += r.and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            files.push(name.to_string());
        }
    }
    Ok(Diff {
        patch,
        files,
        added,
        removed,
    })
}

/// Whether `patch` applies cleanly to the files in `top`; the reason if not.
pub async fn check(top: &Path, patch: &[u8], sb: Sandbox<'_>) -> Result<(), String> {
    git(
        top,
        &["apply", "--check", "--binary", "--whitespace=nowarn", "-"],
        Some(patch),
        sb,
    )
    .await
    .map(|_| ())
    .map_err(|e| e.trim_start_matches("git apply: ").to_string())
}

/// Apply `patch` to the files in `top` (not staged).
pub async fn apply(top: &Path, patch: &[u8], sb: Sandbox<'_>) -> Result<(), String> {
    git(
        top,
        &["apply", "--binary", "--whitespace=nowarn", "-"],
        Some(patch),
        sb,
    )
    .await
    .map(|_| ())
}

/// Remove a checkout and git's record of it. Never fails: anything left
/// over is deleted directly (only inside a `tessera/worktrees` folder).
pub async fn remove(top: &Path, path: &Path, sb: Sandbox<'_>) {
    let p = path.to_string_lossy().to_string();
    let _ = git(
        top,
        &["worktree", "remove", "--force", "--force", &p],
        None,
        sb,
    )
    .await;
    if path.exists() && is_ours(path) {
        let (admission, path) = (admission(), path.to_path_buf());
        let _ =
            tokio::task::spawn_blocking(move || admission.run(|| std::fs::remove_dir_all(path)))
                .await;
    }
    let _ = git(top, &["worktree", "prune"], None, sb).await;
}

/// Inside a `tessera/worktrees` folder, reached without links (the agent
/// could otherwise point `.git/tessera` somewhere else).
fn is_ours(path: &Path) -> bool {
    let Some(worktrees) = path.parent() else {
        return false;
    };
    worktrees.ends_with("tessera/worktrees")
        && [path, worktrees, worktrees.parent().unwrap_or(worktrees)]
            .iter()
            .all(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()))
}

// ---- registry of live checkouts (crash cleanup) --------------------------------

static REGISTRY: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
const KEY: &str = "worktrees";

#[derive(Serialize, Deserialize)]
pub struct Registered {
    pub path: String,
    pub repo: String,
}

pub fn registered(store: &crate::store::Store) -> Vec<Registered> {
    store
        .kv_get(KEY)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default()
}

fn update(store: &crate::store::Store, f: impl FnOnce(&mut Vec<Registered>)) {
    let _guard = REGISTRY.lock();
    let mut list = registered(store);
    f(&mut list);
    if let Ok(v) = serde_json::to_string(&list) {
        let _ = store.kv_set(KEY, &v);
    }
}

pub fn register(store: &crate::store::Store, path: &Path, repo: &Path) {
    let path = path.to_string_lossy().to_string();
    let repo = repo.to_string_lossy().to_string();
    update(store, |l| {
        if !l.iter().any(|r| r.path == path) {
            l.push(Registered { path, repo });
        }
    });
}

pub fn unregister(store: &crate::store::Store, path: &str) {
    update(store, |l| l.retain(|r| r.path != path));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn git_launch_enters_the_verified_repo_not_the_swapped_name() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("selected");
        let outside = temp.path().join("outside");
        for dir in [&path, &outside] {
            std::fs::create_dir(dir).unwrap();
            sh(dir, &["init", "-q"]);
        }
        std::fs::write(path.join("checked.txt"), "checked").unwrap();
        std::fs::write(outside.join("outside.txt"), "outside").unwrap();
        let directory = crate::tools::file_object::DirectoryScope::capture(&path)
            .unwrap()
            .open()
            .unwrap();
        std::fs::rename(&path, temp.path().join("saved")).unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        git_in_directory(directory, &["add", "checked.txt"], None, None)
            .await
            .unwrap();
        assert_eq!(
            git_line(
                &temp.path().join("saved"),
                &["diff", "--cached", "--name-only"],
                None
            )
            .await
            .unwrap(),
            "checked.txt"
        );
        assert!(
            git_line(&outside, &["diff", "--cached", "--name-only"], None)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            git(&path, &["status", "--porcelain"], None, None)
                .await
                .is_err(),
            "a redirected canonical git cwd must not be recaptured"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn worktree_git_launch_creates_in_the_pinned_repo_after_a_name_swap() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("selected");
        let outside = temp.path().join("outside");
        for dir in [&path, &outside] {
            std::fs::create_dir(dir).unwrap();
            sh(dir, &["init", "-q"]);
            sh(dir, &["commit", "--allow-empty", "-qm", "base"]);
        }
        let directory = crate::tools::file_object::DirectoryScope::capture(&path)
            .unwrap()
            .open()
            .unwrap();
        std::fs::rename(&path, temp.path().join("saved")).unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        git_in_directory(
            directory,
            &["worktree", "add", "--detach", "relative-checkout"],
            None,
            None,
        )
        .await
        .unwrap();
        assert!(temp.path().join("saved/relative-checkout/.git").is_file());
        assert!(!outside.join("relative-checkout").exists());
        sh(
            &temp.path().join("saved"),
            &["worktree", "remove", "--force", "relative-checkout"],
        );
    }

    fn sh(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    #[tokio::test]
    async fn checkout_diff_apply_and_recreate() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().canonicalize().unwrap();
        sh(&top, &["init", "-q"]);
        std::fs::write(top.join("a.txt"), "one\n").unwrap();
        std::fs::write(top.join("gone.txt"), "bye\n").unwrap();
        sh(&top, &["add", "."]);
        sh(&top, &["commit", "-qm", "init"]);
        // Uncommitted and untracked work is carried into the checkout.
        std::fs::write(top.join("a.txt"), "one\ndirty\n").unwrap();
        std::fs::write(top.join("new.txt"), "untracked\n").unwrap();
        let r = repo(&top, None).await.unwrap();
        let wt = path_for(&r, "child1");
        let (base, skipped) = create(&r, &wt, None).await.unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(
            std::fs::read_to_string(wt.join("a.txt")).unwrap(),
            "one\ndirty\n"
        );
        assert_eq!(
            std::fs::read_to_string(wt.join("new.txt")).unwrap(),
            "untracked\n"
        );
        // Nothing changed yet.
        assert!(diff(&wt, &base, None).await.unwrap().files.is_empty());
        std::fs::write(wt.join("a.txt"), "one\ndirty\nchild\n").unwrap();
        std::fs::write(wt.join("made.txt"), "by child\n").unwrap();
        std::fs::remove_file(wt.join("gone.txt")).unwrap();
        let d = diff(&wt, &base, None).await.unwrap();
        assert_eq!(d.files, ["a.txt", "gone.txt", "made.txt"]);
        assert_eq!((d.added, d.removed), (2, 1));
        // The parent's files are untouched until the patch is applied.
        assert_eq!(
            std::fs::read_to_string(top.join("a.txt")).unwrap(),
            "one\ndirty\n"
        );
        remove(&r.top, &wt, None).await;
        assert!(!wt.exists());
        // Re-made as the child left it.
        recreate(&r.top, &wt, &r.head, &base, Some(&d.patch), None)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(wt.join("made.txt")).unwrap(),
            "by child\n"
        );
        assert!(!wt.join("gone.txt").exists());
        remove(&r.top, &wt, None).await;
        check(&top, &d.patch, None).await.unwrap();
        apply(&top, &d.patch, None).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(top.join("a.txt")).unwrap(),
            "one\ndirty\nchild\n"
        );
        assert!(top.join("made.txt").exists() && !top.join("gone.txt").exists());
        // A second time it conflicts, and nothing is touched.
        let err = check(&top, &d.patch, None).await.unwrap_err();
        assert!(err.contains("a.txt") || err.contains("made.txt"), "{err}");
        let listed = git_line(&top, &["worktree", "list"], None).await.unwrap();
        assert_eq!(listed.lines().count(), 1, "{listed}");
    }

    /// A repository can name programs for git to run (a filter, a diff
    /// driver). Tessera's own git commands run them only as the agent's
    /// own commands would run: in its sandbox, never as diff programs.
    #[tokio::test]
    async fn programs_the_repository_names_run_in_the_agents_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (top, private, out) = (root.join("repo"), root.join("private"), root.join("out"));
        for d in [&top, &private, &out] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(private.join("secret"), "sk-kept-private").unwrap();
        sh(&top, &["init", "-q"]);
        std::fs::write(top.join("a.txt"), "one\n").unwrap();
        sh(&top, &["add", "."]);
        sh(&top, &["commit", "-qm", "init"]);
        let copied = out.join("copied");
        let differ = out.join("differ");
        sh(
            &top,
            &[
                "config",
                "filter.copy.clean",
                &format!(
                    "cat '{}' >> '{}' 2>/dev/null; cat",
                    private.join("secret").display(),
                    copied.display()
                ),
            ],
        );
        sh(
            &top,
            &[
                "config",
                "diff.external",
                &format!("touch '{}'", differ.display()),
            ],
        );
        sh(
            &top,
            &[
                "config",
                "diff.show.textconv",
                &format!("touch '{}'; cat", differ.display()),
            ],
        );
        std::fs::write(top.join(".gitattributes"), "*.txt filter=copy diff=show\n").unwrap();
        std::fs::write(top.join("a.txt"), "one\ntwo\n").unwrap();
        let run = |sb: Option<(crate::sandbox::Spec, bool)>| {
            let (top, copied) = (top.clone(), copied.clone());
            async move {
                let _ = std::fs::remove_file(&copied);
                let r = repo(&top, sb.as_ref()).await.unwrap();
                let wt = path_for(&r, "child");
                let (base, _) = create(&r, &wt, sb.as_ref()).await.unwrap();
                std::fs::write(wt.join("a.txt"), "one\ntwo\nthree\n").unwrap();
                let d = diff(&wt, &base, sb.as_ref()).await.unwrap();
                assert_eq!(d.files, ["a.txt"]);
                remove(&r.top, &wt, sb.as_ref()).await;
                std::fs::read_to_string(&copied).unwrap_or_default()
            }
        };
        // No diff program ever runs, sandbox or not.
        let unconfined = run(None).await;
        assert!(!differ.exists(), "an external diff program ran");
        // Without a sandbox the filter runs as the agent's commands then
        // do: with the user's access.
        assert!(unconfined.contains("sk-kept-private"), "{unconfined}");
        if !crate::sandbox::support().available {
            eprintln!("sandbox unavailable here: {}", crate::sandbox::describe());
            return;
        }
        let spec = crate::sandbox::Spec {
            hide: vec![private.clone()],
            ..Default::default()
        };
        let confined = run(Some((spec, false))).await;
        assert!(!confined.contains("sk-kept-private"), "{confined}");
        assert!(!differ.exists(), "an external diff program ran");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn permission_change_cancels_pending_setup_and_cleans_its_checkout() {
        use crate::tools::admission::{ActionAdmission, PermissionCeiling};
        use std::os::unix::fs::PermissionsExt;
        use tessera_proto::PermissionMode;
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().join("repo");
        std::fs::create_dir(&top).unwrap();
        sh(&top, &["init", "-q"]);
        std::fs::write(top.join("a.txt"), "before\n").unwrap();
        std::fs::write(top.join(".gitattributes"), "*.txt filter=fixture\n").unwrap();
        sh(&top, &["add", "."]);
        sh(&top, &["commit", "-qm", "fixture"]);
        let marker = dir.path().join("filter-started");
        let filter = dir.path().join("filter.sh");
        std::fs::write(
            &filter,
            format!("#!/bin/sh\ntouch '{}'\nsleep 60\ncat\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&filter, std::fs::Permissions::from_mode(0o755)).unwrap();
        sh(
            &top,
            &[
                "config",
                "filter.fixture.clean",
                &filter.display().to_string(),
            ],
        );
        std::fs::write(top.join("a.txt"), "after\n").unwrap();
        let repo = repo(&top, None).await.unwrap();
        let path = path_for(&repo, "fixture");
        let ceiling = PermissionCeiling::new(PermissionMode::Auto);
        let admission = ActionAdmission::capture(vec![ceiling.clone()], None);
        let setup_path = path.clone();
        let setup =
            tokio::spawn(
                async move { admitted(admission, create(&repo, &setup_path, None)).await },
            );
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !marker.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        ceiling.store(PermissionMode::ReadOnly);
        let err = tokio::time::timeout(std::time::Duration::from_secs(5), setup)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(err.contains("cancelled"), "{err}");
        assert!(!path.exists());
        assert_eq!(
            git_line(&top, &["worktree", "list"], None)
                .await
                .unwrap()
                .lines()
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn stale_worktree_admission_refuses_git_creation_and_recreation() {
        use crate::tools::admission::{ActionAdmission, PermissionCeiling};
        use tessera_proto::PermissionMode;
        let dir = tempfile::tempdir().unwrap();
        sh(dir.path(), &["init", "-q"]);
        sh(dir.path(), &["commit", "--allow-empty", "-qm", "fixture"]);
        let repo = repo(dir.path(), None).await.unwrap();
        let path = path_for(&repo, "fixture");
        let ceiling = PermissionCeiling::new(PermissionMode::Auto);
        let admission = ActionAdmission::capture(vec![ceiling.clone()], None);
        ceiling.store(PermissionMode::ReadOnly);
        ceiling.store(PermissionMode::Auto);
        let err = admitted(admission.clone(), create(&repo, &path, None))
            .await
            .unwrap_err();
        assert!(err.contains("permissions changed"), "{err}");
        assert!(!path.exists());
        let err = admitted(
            admission,
            recreate(&repo.top, &path, &repo.head, &repo.head, None, None),
        )
        .await
        .unwrap_err();
        assert!(err.contains("permissions changed"), "{err}");
        assert!(!path.exists());
        assert_eq!(
            git_line(&repo.top, &["worktree", "list"], None)
                .await
                .unwrap()
                .lines()
                .count(),
            1
        );
    }
    #[tokio::test]
    async fn outside_a_repository_there_is_nothing_to_isolate() {
        let dir = tempfile::tempdir().unwrap();
        assert!(repo(dir.path(), None).await.is_err());
    }
}
