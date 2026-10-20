//! File objects kept open from authorization through use.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

pub(crate) struct Opened {
    pub file: File,
    pub path: PathBuf,
}

impl Opened {
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::open_as(path, false)
    }

    /// `held`: on Windows a folder opened this way can't be renamed, moved or
    /// deleted while it is open (no `FILE_SHARE_DELETE`). NTFS is documented
    /// to refuse renaming the folders above an open one too; that part has
    /// not been tried on Windows yet.
    fn open_as(path: &Path, held: bool) -> io::Result<Self> {
        let path = crate::platform::canonicalize(path)?;
        let file = open_plain(&path, path.is_dir(), held)?;
        let opened = Self { file, path };
        opened.check_path()?;
        let meta = opened.file.metadata()?;
        if meta.file_type().is_symlink() || (!meta.is_file() && !meta.is_dir()) {
            return Err(io::Error::other(
                "Only regular files and folders can be opened.",
            ));
        }
        Ok(opened)
    }

    /// Server-owned blob ids may select only an opened object in their store.
    pub fn open_in(path: &Path, root: &Path) -> io::Result<Self> {
        let dir = Self::open(root)?;
        if !dir.file.metadata()?.is_dir() {
            return Err(io::Error::other("A store folder is required."));
        }
        let file = Self::open(path)?;
        dir.check_path()?;
        if !file.path.starts_with(&dir.path) || !file.file.metadata()?.is_file() {
            return Err(io::Error::other("The file is outside its store."));
        }
        file.check_path()?;
        Ok(file)
    }

    pub fn check_path(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        let current = fd_path(&self.file)?;
        #[cfg(windows)]
        let current = crate::platform::windows::real_path(&self.file)
            .ok_or_else(|| io::Error::other("Cannot check the opened file location."))?;
        #[cfg(all(not(target_os = "linux"), not(windows)))]
        let current = {
            use std::os::unix::fs::MetadataExt;
            let a = self.file.metadata()?;
            let b = std::fs::metadata(&self.path)?;
            if a.dev() != b.dev() || a.ino() != b.ino() {
                return Err(io::Error::other("The file object changed; try again."));
            }
            crate::platform::canonicalize(&self.path)?
        };
        if current != self.path {
            return Err(io::Error::other("The file location changed; try again."));
        }
        Ok(())
    }

    pub fn listing_path(&self) -> PathBuf {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            PathBuf::from(format!("/proc/self/fd/{}", self.file.as_raw_fd()))
        }
        #[cfg(not(target_os = "linux"))]
        self.path.clone()
    }

    /// Bounded synchronous descriptor read, after the caller authorizes `path`.
    pub fn read_bounded(&self, max: u64) -> io::Result<Vec<u8>> {
        self.check_path()?;
        let meta = self.file.metadata()?;
        if !meta.is_file() || meta.len() > max {
            return Err(io::Error::other(
                "Not a regular file within the size limit.",
            ));
        }
        let mut bytes = Vec::new();
        (&self.file)
            .take(max.saturating_add(1))
            .read_to_end(&mut bytes)?;
        self.check_path()?;
        if bytes.len() as u64 > max {
            return Err(io::Error::other("The file exceeded the size limit."));
        }
        Ok(bytes)
    }

    pub async fn bytes(&self) -> io::Result<Vec<u8>> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        self.check_path()?;
        let mut file = tokio::fs::File::from_std(self.file.try_clone()?);
        file.seek(std::io::SeekFrom::Start(0)).await?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).await?;
        self.check_path()?;
        Ok(bytes)
    }
}

/// Persisted authority for a directory, not a pathname that may later point
/// somewhere else. Reopening verifies both the original location and object.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct DirectoryScope {
    path: PathBuf,
    device: u64,
    object: u64,
}

impl DirectoryScope {
    pub fn capture(path: &Path) -> Result<Self, String> {
        let opened = Opened::open(path).map_err(|e| e.to_string())?;
        if opened.path != path || !opened.file.metadata().is_ok_and(|m| m.is_dir()) {
            return Err("The authorized working folder changed; start a new helper.".into());
        }
        let (device, object) = directory_identity(&opened.file).map_err(|e| e.to_string())?;
        Ok(Self {
            path: opened.path,
            device,
            object,
        })
    }

    pub fn verify(&self) -> Result<(), String> {
        self.open_verified(false).map(|_| ())
    }

    /// Retain the verified object through process launch, not just its name.
    pub fn open(&self) -> Result<ProcessDirectory, String> {
        ProcessDirectory::from_opened(self.open_verified(true)?)
    }

    fn open_verified(&self, held: bool) -> Result<Opened, String> {
        let opened = Opened::open_as(&self.path, held).map_err(|e| e.to_string())?;
        let (device, object) = directory_identity(&opened.file).map_err(|e| e.to_string())?;
        if opened.path != self.path || device != self.device || object != self.object {
            return Err("The authorized working folder changed; start a new helper.".into());
        }
        Ok(opened)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(unix)]
fn directory_identity(file: &File) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata()?;
    Ok((meta.dev(), meta.ino()))
}

#[cfg(windows)]
fn directory_identity(file: &File) -> io::Result<(u64, u64)> {
    crate::platform::windows::file_identity(file)
        .ok_or_else(|| io::Error::other("Cannot verify the working folder identity."))
}

/// A process's working folder, held open from its check until the process
/// has started. Unix children enter the held object (`fchdir`), or, inside a
/// sandbox, its path in the sandbox's view checked against the held object;
/// never just a name that may have been replaced meanwhile. On Windows the
/// held folder can't be renamed or replaced while the process enters it by
/// name.
pub struct ProcessDirectory {
    path: PathBuf,
    #[cfg(any(unix, windows))]
    opened: Opened,
    #[cfg(unix)]
    name: std::ffi::CString,
    #[cfg(unix)]
    identity: (u64, u64),
    /// The folder's path on a mapped network drive when its real path is a
    /// `\\server\share` one (cmd.exe can't start there): the drive the chat
    /// names, else one of this session's drives that leads to the same path.
    #[cfg(windows)]
    enter_as: Option<PathBuf>,
}

/// Keep through native entry: shared nsenter may return from `spawn` before
/// its launcher starts. Holding to process exit prevents inode reuse while
/// it is stopped. Windows only needs the hold until `spawn` returns.
#[must_use = "keep the working folder held through process entry"]
pub struct DirectoryHold {
    #[cfg(any(unix, windows))]
    _held: std::sync::Arc<File>,
}

impl DirectoryHold {
    /// Call after successful spawn. Keep this in the process's owning task
    /// on Unix; Windows has already entered cwd, so release its rename lock.
    pub fn after_spawn(self) -> Option<Self> {
        #[cfg(unix)]
        {
            Some(self)
        }
        #[cfg(not(unix))]
        {
            None
        }
    }
}

/// A launch whose working folder was refused (replaced, moved, or hidden
/// inside the sandbox), in plain words; other start errors as they are.
pub fn launch_error(e: &io::Error) -> String {
    #[cfg(unix)]
    if e.raw_os_error() == Some(libc::ESTALE) {
        return FOLDER_CHANGED.into();
    }
    e.to_string()
}

#[cfg(unix)]
const FOLDER_CHANGED: &str = "The command's working folder is not the folder that was checked: it was replaced or moved, or the command sandbox hides it. Nothing was run.";

const OUTSIDE_ROOT: &str = "The helper command folder must remain inside its bound working folder; select another helper cwd explicitly.";

impl ProcessDirectory {
    pub fn open(path: &Path) -> Result<Self, String> {
        #[cfg(unix)]
        {
            Self::from_opened(Opened::open_as(path, true).map_err(|e| e.to_string())?)
        }
        #[cfg(windows)]
        {
            let mut dir =
                Self::from_opened(Opened::open_as(path, true).map_err(|e| e.to_string())?)?;
            // The same held folder, entered by the drive letter the chat
            // names; the drive mapping itself is the user's session.
            let unc = dir.path.as_os_str().to_string_lossy().starts_with(r"\\");
            let drive = !path.as_os_str().to_string_lossy().starts_with(r"\\");
            if unc && drive && path.is_absolute() {
                dir.enter_as = Some(path.to_path_buf());
            }
            Ok(dir)
        }
        #[cfg(not(any(unix, windows)))]
        {
            Ok(Self {
                path: path.to_path_buf(),
            })
        }
    }

    #[cfg(any(unix, windows))]
    fn from_opened(opened: Opened) -> Result<Self, String> {
        if !opened.file.metadata().is_ok_and(|m| m.is_dir()) {
            return Err("The working folder is not a directory.".into());
        }
        #[cfg(unix)]
        {
            use std::os::fd::{AsRawFd, FromRawFd};
            use std::os::unix::ffi::OsStrExt;
            let mut opened = opened;
            // Standard input/output/error are set up in the child before its
            // hooks run: a held folder there would be overwritten.
            if opened.file.as_raw_fd() < 3 {
                let fd = unsafe { libc::fcntl(opened.file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
                if fd < 0 {
                    return Err(io::Error::last_os_error().to_string());
                }
                opened.file = unsafe { File::from_raw_fd(fd) };
            }
            let name = std::ffi::CString::new(opened.path.as_os_str().as_bytes())
                .map_err(|e| e.to_string())?;
            Ok(Self {
                identity: directory_identity(&opened.file).map_err(|e| e.to_string())?,
                path: opened.path.clone(),
                opened,
                name,
            })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                enter_as: crate::platform::windows::mapped_drive_path(&opened.path),
                path: opened.path.clone(),
                opened,
            })
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The folder at `path` (canonical, beneath this one), reached from this
    /// held folder: on Unix one name at a time from its descriptor without
    /// following links, so it lies inside this very folder even if a name
    /// was replaced meanwhile; on Windows by name while this folder is held
    /// in place, its final location checked.
    pub fn open_beneath(&self, path: &Path) -> Result<Self, String> {
        let rel = path.strip_prefix(&self.path).map_err(|_| OUTSIDE_ROOT)?;
        if rel
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(OUTSIDE_ROOT.into());
        }
        #[cfg(unix)]
        {
            let mut file = self.opened.file.try_clone().map_err(|e| e.to_string())?;
            for c in rel.components() {
                file = at(&file, c.as_os_str(), libc::O_RDONLY | libc::O_DIRECTORY, 0)
                    .map_err(|e| format!("{OUTSIDE_ROOT} ({e})"))?;
            }
            let opened = Opened {
                file,
                path: path.to_path_buf(),
            };
            opened
                .check_path()
                .map_err(|e| format!("{OUTSIDE_ROOT} ({e})"))?;
            Self::from_opened(opened)
        }
        #[cfg(windows)]
        {
            let opened = Opened::open_as(path, true).map_err(|e| e.to_string())?;
            if opened.path != path || !opened.path.starts_with(&self.path) {
                return Err(OUTSIDE_ROOT.into());
            }
            Self::from_opened(opened)
        }
        #[cfg(not(any(unix, windows)))]
        {
            Ok(Self {
                path: path.to_path_buf(),
            })
        }
    }

    pub fn prepare(
        &self,
        spec: Option<&(crate::sandbox::Spec, bool)>,
    ) -> Result<Option<crate::sandbox::Prepared>, String> {
        crate::sandbox::prepare(spec, &self.path)
    }

    pub fn argv(
        &self,
        sandbox: Option<&crate::sandbox::Prepared>,
        program: std::ffi::OsString,
        args: Vec<std::ffi::OsString>,
    ) -> (std::ffi::OsString, Vec<std::ffi::OsString>) {
        match sandbox {
            #[cfg(unix)]
            Some(s) => {
                use std::os::fd::AsRawFd;
                s.directory_argv(program, args, self.opened.file.as_raw_fd(), self.identity)
            }
            #[cfg(not(unix))]
            Some(s) => s.argv(program, args),
            None => (program, args),
        }
    }

    /// Install last, after the launcher's other pre_exec hooks. Only syscalls
    /// occur in the forked child; the host descriptor never reaches exec in
    /// shared mode. Keep the returned hold through entry (see after_spawn).
    pub fn apply(
        self,
        cmd: &mut tokio::process::Command,
        sandbox: Option<crate::sandbox::Prepared>,
    ) -> DirectoryHold {
        #[cfg(unix)]
        {
            let env_error = sandbox
                .as_ref()
                .and_then(|s| s.protect_environment(cmd.as_std_mut()).err())
                .map(|e| e.raw_os_error().unwrap_or(libc::EIO));
            let held = std::sync::Arc::new(self.opened.file);
            let for_child = held.clone();
            let name = self.name;
            let identity = self.identity;
            unsafe {
                cmd.pre_exec(move || {
                    use std::os::fd::AsRawFd;
                    if let Some(code) = env_error {
                        return Err(io::Error::from_raw_os_error(code));
                    }
                    if let Some(s) = &sandbox {
                        s.enter()?;
                        return match s.directory_entry() {
                            crate::sandbox::DirectoryEntry::Reopen => {
                                Self::enter_visible_fd(&name, identity)
                            }
                            crate::sandbox::DirectoryEntry::Values(checked) => {
                                if checked != for_child.as_raw_fd() {
                                    return Err(io::Error::from_raw_os_error(libc::EINVAL));
                                }
                                // Before nsenter enters the PID namespace:
                                // no host directory reaches a visible process.
                                if libc::close(for_child.as_raw_fd()) != 0 {
                                    return Err(io::Error::last_os_error());
                                }
                                Ok(())
                            }
                        };
                    }
                    if libc::fchdir(for_child.as_raw_fd()) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            DirectoryHold { _held: held }
        }
        #[cfg(windows)]
        {
            let _ = sandbox;
            cmd.current_dir(self.enter_as.as_deref().unwrap_or(&self.path));
            DirectoryHold {
                _held: std::sync::Arc::new(self.opened.file),
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = sandbox;
            cmd.current_dir(self.path);
            DirectoryHold {}
        }
    }

    /// Mount-based hiding/read-only views require an fd opened *inside* the
    /// namespace. It must be the same still-pinned inode, or launch fails
    /// with `ESTALE` ([`launch_error`] says what happened). There is no
    /// pathname chdir and no authority from a replacement target.
    #[cfg(all(test, unix))]
    unsafe fn enter_visible(&self) -> io::Result<()> {
        unsafe { Self::enter_visible_fd(&self.name, self.identity) }
    }

    #[cfg(unix)]
    #[allow(clippy::unnecessary_cast)] // dev_t differs between Unix targets
    unsafe fn enter_visible_fd(name: &std::ffi::CStr, identity: (u64, u64)) -> io::Result<()> {
        unsafe {
            let fd = libc::open(
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            );
            if fd < 0 {
                return Err(io::Error::from_raw_os_error(libc::ESTALE));
            }
            let mut visible: libc::stat = std::mem::zeroed();
            let result = if libc::fstat(fd, &mut visible) != 0 {
                Err(io::Error::last_os_error())
            } else if visible.st_dev as u64 != identity.0 || visible.st_ino as u64 != identity.1 {
                Err(io::Error::from_raw_os_error(libc::ESTALE))
            } else if libc::fchdir(fd) != 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            };
            libc::close(fd);
            result
        }
    }
}

#[cfg(target_os = "linux")]
fn fd_path(file: &File) -> io::Result<PathBuf> {
    use std::os::fd::AsRawFd;
    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

#[cfg(unix)]
fn at(dir: &File, name: &std::ffi::OsStr, flags: i32, mode: u32) -> io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(name.as_bytes())?;
    // The descriptor pins the parent. O_NOFOLLOW rejects a replaced final link.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            mode,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Walk from a pinned root, rejecting links at every component. This also
/// works on kernels without openat2; it has the same no-symlinks/beneath rule.
#[cfg(target_os = "linux")]
fn open_plain(path: &Path, directory: bool, _held: bool) -> io::Result<File> {
    let mut file = File::open("/")?;
    let components: Vec<_> = path
        .strip_prefix("/")
        .map_err(io::Error::other)?
        .components()
        .collect();
    for (i, component) in components.iter().enumerate() {
        let std::path::Component::Normal(name) = component else {
            return Err(io::Error::other(
                "A resolved absolute file path is required.",
            ));
        };
        let dir_flag = if directory || i + 1 < components.len() {
            libc::O_DIRECTORY
        } else {
            0
        };
        file = at(&file, name, libc::O_RDONLY | libc::O_NONBLOCK | dir_flag, 0)?;
    }
    Ok(file)
}

#[cfg(not(target_os = "linux"))]
fn open_plain(path: &Path, directory: bool, held: bool) -> io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_SHARE_READ: u32 = 0x1;
        const FILE_SHARE_WRITE: u32 = 0x2;
        options.custom_flags(
            FILE_FLAG_OPEN_REPARSE_POINT
                | if directory {
                    FILE_FLAG_BACKUP_SEMANTICS
                } else {
                    0
                },
        );
        // Without FILE_SHARE_DELETE nobody can rename, move or delete a held
        // folder while this handle is open (NTFS also refuses renaming the
        // folders above it, as documented; not yet tried on Windows).
        if held && directory {
            options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
        }
    }
    let _ = (directory, held);
    options.open(path)
}

pub(crate) struct Destination {
    dir: Opened,
    name: std::ffi::OsString,
    pub path: PathBuf,
    admission: super::admission::ActionAdmission,
    cwd_scope: Option<DirectoryScope>,
}

impl Destination {
    #[cfg(all(test, target_os = "linux"))]
    pub fn prepare(
        path: &Path,
        authorize: impl Fn(&Path) -> Result<(), String>,
    ) -> Result<Self, String> {
        Self::prepare_guarded(
            path,
            authorize,
            super::admission::ActionAdmission::default(),
        )
    }

    pub fn prepare_guarded(
        path: &Path,
        authorize: impl Fn(&Path) -> Result<(), String>,
        admission: super::admission::ActionAdmission,
    ) -> Result<Self, String> {
        Self::prepare_scoped(path, authorize, admission, None)
    }

    pub fn prepare_scoped(
        path: &Path,
        authorize: impl Fn(&Path) -> Result<(), String>,
        admission: super::admission::ActionAdmission,
        cwd_scope: Option<DirectoryScope>,
    ) -> Result<Self, String> {
        let verify_scope = || {
            if let Some(scope) = &cwd_scope {
                scope.verify().map_err(io::Error::other)?;
            }
            Ok::<_, io::Error>(())
        };
        verify_scope().map_err(|e| e.to_string())?;
        admission.check()?;
        let path = super::canonical(path);
        authorize(&path)?;
        let parent = path.parent().ok_or("A parent folder is required.")?;
        let name = path
            .file_name()
            .ok_or("A file name is required.")?
            .to_os_string();
        #[cfg(target_os = "linux")]
        let dir = {
            use std::os::fd::AsRawFd;
            use std::os::unix::ffi::OsStrExt;
            let mut file = File::open("/").map_err(|e| e.to_string())?;
            let mut current = PathBuf::from("/");
            for component in parent
                .strip_prefix("/")
                .map_err(|e| e.to_string())?
                .components()
            {
                let std::path::Component::Normal(part) = component else {
                    return Err("A resolved absolute path is required.".into());
                };
                if fd_path(&file).map_err(|e| e.to_string())? != current {
                    return Err("The parent folder moved; try again.".into());
                }
                current.push(part);
                let next = at(&file, part, libc::O_RDONLY | libc::O_DIRECTORY, 0);
                file = match next {
                    Ok(f) => f,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        // Authorization was checked before any folder is made;
                        // mkdirat never follows a name inserted in the meantime.
                        authorize(&path)?;
                        let part_c =
                            std::ffi::CString::new(part.as_bytes()).map_err(|e| e.to_string())?;
                        admission
                            .run(|| {
                                verify_scope()?;
                                if unsafe {
                                    libc::mkdirat(file.as_raw_fd(), part_c.as_ptr(), 0o755)
                                } != 0
                                {
                                    let e = io::Error::last_os_error();
                                    if e.kind() != io::ErrorKind::AlreadyExists {
                                        return Err(e);
                                    }
                                }
                                Ok(())
                            })
                            .map_err(|e| e.to_string())?;
                        at(&file, part, libc::O_RDONLY | libc::O_DIRECTORY, 0)
                            .map_err(|e| e.to_string())?
                    }
                    Err(e) => return Err(e.to_string()),
                };
            }
            Opened {
                file,
                path: parent.into(),
            }
        };
        #[cfg(not(target_os = "linux"))]
        let dir = {
            admission
                .run(|| {
                    verify_scope()?;
                    std::fs::create_dir_all(parent)
                })
                .map_err(|e| e.to_string())?;
            Opened::open(parent).map_err(|e| e.to_string())?
        };
        dir.check_path().map_err(|e| e.to_string())?;
        authorize(&path)?;
        Ok(Self {
            dir,
            name,
            path,
            admission,
            cwd_scope,
        })
    }

    fn verify_scope(&self) -> io::Result<()> {
        if let Some(scope) = &self.cwd_scope {
            scope.verify().map_err(io::Error::other)?;
        }
        Ok(())
    }

    fn existing(&self) -> io::Result<Option<File>> {
        self.verify_scope()?;
        self.dir.check_path()?;
        #[cfg(target_os = "linux")]
        let result = at(
            &self.dir.file,
            &self.name,
            libc::O_RDONLY | libc::O_NONBLOCK,
            0,
        );
        #[cfg(not(target_os = "linux"))]
        let result = {
            let opened = Opened::open(&self.dir.path.join(&self.name));
            opened.and_then(|f| {
                if f.path != self.path {
                    return Err(io::Error::other(
                        "The destination is now a link; try again.",
                    ));
                }
                Ok(f.file)
            })
        };
        match result {
            Ok(f) if f.metadata()?.is_file() => {
                let opened = Opened {
                    file: f,
                    path: self.path.clone(),
                };
                opened.check_path()?;
                Ok(Some(opened.file))
            }
            Ok(_) => Err(io::Error::other("The destination is not a regular file.")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn write(&self, expected: Option<&str>, text: &str) -> io::Result<()> {
        self.admission
            .run(|| self.write_content(expected.map(str::as_bytes), text.as_bytes(), true))
    }

    pub fn write_bytes(&self, expected: Option<&[u8]>, bytes: &[u8]) -> io::Result<()> {
        self.admission
            .run(|| self.write_content(expected, bytes, false))
    }

    fn write_content(
        &self,
        expected: Option<&[u8]>,
        content: &[u8],
        text_mode: bool,
    ) -> io::Result<()> {
        self.write_content_checked(expected, content, text_mode, || {})
    }

    fn write_content_checked(
        &self,
        expected: Option<&[u8]>,
        content: &[u8],
        text_mode: bool,
        before_compare: impl FnOnce(),
    ) -> io::Result<()> {
        let existing = self.existing()?;
        let permissions = existing
            .as_ref()
            .map(|f| f.metadata().map(|m| m.permissions()))
            .transpose()?;
        before_compare();
        if let Some(mut file) = existing {
            let matches = if let Some(expected) = expected {
                // Fixed-size chunks and one extra byte: no allocation or read
                // can grow with a racing destination beyond the expected size.
                let mut buf = [0; 8192];
                let mut matched = true;
                for part in expected.chunks(buf.len()) {
                    if file.read_exact(&mut buf[..part.len()]).is_err()
                        || buf[..part.len()] != *part
                    {
                        matched = false;
                        break;
                    }
                }
                matched && file.read(&mut buf[..1])? == 0
            } else if text_mode {
                // The legacy binary-to-text replacement has no text expected;
                // bound its UTF-8 check by the whole-write operation maximum.
                const MAX: u64 = 16 << 20;
                let mut bytes = Vec::new();
                file.take(MAX + 1).read_to_end(&mut bytes)?;
                if bytes.len() as u64 > MAX {
                    return Err(io::Error::other(
                        "The destination grew beyond the write size limit; read it again.",
                    ));
                }
                std::str::from_utf8(&bytes).is_err()
            } else {
                false
            };
            if !matches {
                return Err(io::Error::other(
                    "The file size or content changed since it was read; try again.",
                ));
            }
        } else if expected.is_some() {
            return Err(io::Error::other("The file disappeared; try again."));
        }
        self.verify_scope()?;
        self.dir.check_path()?;
        let temp = std::ffi::OsString::from(format!(".tessera-write-{}", crate::util::new_id()));
        #[cfg(target_os = "linux")]
        let mut file = at(
            &self.dir.file,
            &temp,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )?;
        #[cfg(not(target_os = "linux"))]
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.dir.path.join(&temp))?;
        let result = (|| {
            file.write_all(content)?;
            if let Some(p) = permissions {
                file.set_permissions(p)?;
            }
            file.sync_all()?;
            self.verify_scope()?;
            self.dir.check_path()?;
            #[cfg(target_os = "linux")]
            {
                use std::os::fd::AsRawFd;
                use std::os::unix::ffi::OsStrExt;
                let from = std::ffi::CString::new(temp.as_bytes())?;
                let to = std::ffi::CString::new(self.name.as_bytes())?;
                let fd = self.dir.file.as_raw_fd();
                if unsafe { libc::renameat(fd, from.as_ptr(), fd, to.as_ptr()) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                self.dir.file.sync_all()?;
            }
            #[cfg(not(target_os = "linux"))]
            {
                if crate::platform::canonicalize(&self.dir.path)? != self.dir.path {
                    return Err(io::Error::other("The destination folder changed."));
                }
                std::fs::rename(self.dir.path.join(&temp), &self.path)?;
            }
            Ok(())
        })();
        if result.is_err() {
            #[cfg(target_os = "linux")]
            {
                use std::os::fd::AsRawFd;
                use std::os::unix::ffi::OsStrExt;
                if let Ok(name) = std::ffi::CString::new(temp.as_bytes()) {
                    unsafe {
                        libc::unlinkat(self.dir.file.as_raw_fd(), name.as_ptr(), 0);
                    }
                }
            }
            #[cfg(not(target_os = "linux"))]
            let _ = std::fs::remove_file(self.dir.path.join(&temp));
        }
        result
    }

    pub fn delete(&self, expected: &[u8]) -> io::Result<()> {
        self.admission.run(|| self.delete_admitted(expected))
    }

    fn delete_admitted(&self, expected: &[u8]) -> io::Result<()> {
        let Some(mut file) = self.existing()? else {
            return Ok(());
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        if bytes != expected {
            return Err(io::Error::other("The file changed since it was read."));
        }
        self.verify_scope()?;
        self.dir.check_path()?;
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            use std::os::unix::ffi::OsStrExt;
            let name = std::ffi::CString::new(self.name.as_bytes())?;
            if unsafe { libc::unlinkat(self.dir.file.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                return Err(io::Error::last_os_error());
            }
            self.dir.file.sync_all()?;
        }
        #[cfg(not(target_os = "linux"))]
        std::fs::remove_file(&self.path)?;
        Ok(())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pinned_process_entry_does_not_follow_a_name_changed_after_command_preparation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("selected");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&path).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(path.join("marker"), "checked").unwrap();
        std::fs::write(outside.join("marker"), "outside").unwrap();
        let directory = DirectoryScope::capture(&path).unwrap().open().unwrap();
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", "cat marker; printf original > launched"]);
        let _hold = directory.apply(&mut cmd, None);
        std::fs::rename(&path, temp.path().join("saved")).unwrap();
        symlink(&outside, &path).unwrap();
        let out = cmd.output().await.unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, b"checked");
        assert!(temp.path().join("saved/launched").is_file());
        assert!(!outside.join("launched").exists());
    }

    #[tokio::test]
    async fn namespace_visible_entry_checks_the_pinned_inode_and_never_enters_a_replacement() {
        for link in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("selected");
            let outside = temp.path().join("outside");
            std::fs::create_dir(&path).unwrap();
            std::fs::create_dir(&outside).unwrap();
            let directory = DirectoryScope::capture(&path).unwrap().open().unwrap();
            let mut cmd = tokio::process::Command::new("/bin/sh");
            cmd.args(["-c", "printf bad > launched"]);
            // Exercise the post-mount syscall path without requiring userns
            // availability. No side effects can run before its inode check.
            unsafe {
                cmd.pre_exec(move || directory.enter_visible());
            }
            std::fs::rename(&path, temp.path().join("saved")).unwrap();
            if link {
                symlink(&outside, &path).unwrap();
            } else {
                std::fs::create_dir(&path).unwrap();
            }
            let err = cmd.output().await.unwrap_err();
            assert_eq!(launch_error(&err), FOLDER_CHANGED);
            assert!(!path.join("launched").exists() && !outside.join("launched").exists());
        }
    }

    fn test_sandbox_spec(work: &Path) -> Option<crate::sandbox::Spec> {
        crate::sandbox::test_mode("trusted directory launcher regressions")?;
        Some(crate::sandbox::Spec {
            folders: vec![
                (work.parent().unwrap().to_path_buf(), false),
                (work.to_path_buf(), true),
            ],
            group: format!("directory regression {}", work.display()),
            ..Default::default()
        })
    }
    /// Start `script` in `directory` inside the sandbox `spec` the way every
    /// launcher does (prepare, argv, apply), after `swap`: its exit code and
    /// output, or the start error.
    async fn sandboxed(
        directory: ProcessDirectory,
        spec: &crate::sandbox::Spec,
        script: &str,
        swap: impl FnOnce(),
    ) -> io::Result<(Option<i32>, String, String)> {
        let sandbox = directory.prepare(Some(&(spec.clone(), false))).unwrap();
        assert!(sandbox.is_some(), "the sandbox runs here");
        let (program, args) = directory.argv(
            sandbox.as_ref(),
            "/bin/sh".into(),
            vec!["-c".into(), script.into()],
        );
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args);
        let _hold = directory.apply(&mut cmd, sandbox);
        swap();
        let out = cmd.output().await?;
        Ok((
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ))
    }

    /// Refused before anything ran: a start error (own namespaces) or the
    /// shared namespace's check (exit 126 and why).
    fn refused(result: io::Result<(Option<i32>, String, String)>) {
        match result {
            Err(e) => assert_eq!(launch_error(&e), FOLDER_CHANGED),
            Ok((code, out, err)) => {
                assert_eq!(
                    code,
                    Some(crate::sandbox::FOLDER_CHANGED_STATUS),
                    "{out}{err}"
                );
                assert!(
                    out.is_empty() && err.contains("Nothing was run"),
                    "{out}{err}"
                );
            }
        }
    }

    /// The working folder held from its check is entered in the sandbox's
    /// own view, in both modes (own namespaces with Landlock ABI 2+, a chat's
    /// shared namespace below that: `TESSERA_TEST_LANDLOCK_ABI=1`): paths
    /// relative to it see the hidden folders hidden, the read-only rules
    /// read-only (a writable folder inside a read-only one stays usable) and
    /// the sandbox's own `/proc`. A hidden working folder, or one replaced
    /// after its check, runs nothing.
    #[tokio::test]
    async fn pinned_entry_inside_a_namespace_preserves_readonly_and_hidden_views() {
        if crate::sandbox::test_mode("pinned_entry_inside_a_namespace").is_none() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let work = base.join("work");
        let hidden = base.join("hidden");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&hidden).unwrap();
        std::fs::create_dir_all(base.join("other")).unwrap();
        std::fs::write(work.join("marker"), "checked").unwrap();
        std::fs::write(hidden.join("secret"), "hidden sentinel").unwrap();
        std::fs::write(base.join("ro-file"), "unchanged").unwrap();
        let spec = crate::sandbox::Spec {
            hide: vec![hidden.clone()],
            folders: vec![(base.clone(), false), (work.clone(), true)],
            group: "pinned entry views".into(),
            ..Default::default()
        };
        let up = "../".repeat(work.components().count() - 1);
        let script = format!(
            "cat marker; echo; echo made > made && echo wrote-work; \
             cat ../hidden/secret 2>/dev/null || echo hidden-unreadable; \
             [ ../hidden -ef '{h}' ] && echo hidden-same-view; \
             echo x > ../ro-file 2>/dev/null && echo WROTE-RO || echo refused-ro; \
             [ {up}proc -ef /proc ] && echo proc-same-view; true",
            h = hidden.display()
        );
        let directory = DirectoryScope::capture(&work).unwrap().open().unwrap();
        let (code, out, err) = sandboxed(directory, &spec, &script, || {}).await.unwrap();
        assert_eq!(code, Some(0), "{out}{err}");
        for want in [
            "checked",
            "wrote-work",
            "hidden-unreadable",
            "hidden-same-view",
            "refused-ro",
            "proc-same-view",
        ] {
            assert!(out.contains(want), "{want}: {out}{err}");
        }
        assert!(
            !out.contains("WROTE-RO") && !out.contains("sentinel"),
            "{out}"
        );
        assert_eq!(
            std::fs::read_to_string(work.join("made")).unwrap(),
            "made\n"
        );
        assert_eq!(
            std::fs::read_to_string(base.join("ro-file")).unwrap(),
            "unchanged"
        );

        // A working folder the sandbox hides: nothing runs (not in the empty
        // view, not in the real folder).
        let directory = ProcessDirectory::open(&hidden).unwrap();
        refused(sandboxed(directory, &spec, "echo ran > launched", || {}).await);
        assert!(!hidden.join("launched").exists());

        // Replaced after its check: nothing runs in either folder.
        let other = base.join("other");
        let directory = DirectoryScope::capture(&other).unwrap().open().unwrap();
        let saved = base.join("saved");
        let swap = || {
            std::fs::rename(&other, &saved).unwrap();
            std::fs::create_dir(&other).unwrap();
        };
        refused(sandboxed(directory, &spec, "echo ran > launched", swap).await);
        assert!(!other.join("launched").exists() && !saved.join("launched").exists());

        // Even a symlink back to the original held inode is not an entry by
        // the checked name; refuse it in both real namespace lanes.
        std::fs::remove_dir(&other).unwrap();
        std::fs::rename(&saved, &other).unwrap();
        let directory = DirectoryScope::capture(&other).unwrap().open().unwrap();
        let link = || {
            std::fs::rename(&other, &saved).unwrap();
            symlink(&saved, &other).unwrap();
        };
        refused(sandboxed(directory, &spec, "echo ran > launched", link).await);
        assert!(!saved.join("launched").exists());
    }

    /// The exact native shared launcher, without needing namespaces. A high
    /// held descriptor is closed before entry, and replacement or final
    /// symlinks are refused (including a link to the held object).
    #[tokio::test]
    async fn the_native_shared_launcher_checks_identity_without_a_host_descriptor() {
        use std::os::fd::{AsRawFd, FromRawFd};
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let work = base.join("work");
        std::fs::create_dir(&work).unwrap();
        let held = || {
            let mut opened = Opened::open(&work).unwrap();
            let fd = unsafe { libc::fcntl(opened.file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 40) };
            assert!(fd >= 40);
            opened.file = unsafe { File::from_raw_fd(fd) };
            ProcessDirectory::from_opened(opened).unwrap()
        };
        let run = |directory: ProcessDirectory| async {
            let p = crate::sandbox::joined_for_tests(&work.display().to_string());
            let fd = directory.opened.file.as_raw_fd();
            let (_, args) = directory.argv(Some(&p), "/bin/sh".into(), vec!["-c".into(),
                format!("pwd; for n in 3 4 5 6 7 8 9 {fd}; do [ -d /proc/self/fd/$n ] && echo LEAKED; done; true").into()]);
            let mut cmd = tokio::process::Command::new(&args[2]);
            cmd.args(&args[3..]);
            let _held = directory.apply(&mut cmd, Some(p));
            cmd.output().await.unwrap()
        };
        let out = run(held()).await;
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            work.display().to_string()
        );
        let directory = held();
        std::fs::rename(&work, base.join("saved")).unwrap();
        std::fs::create_dir(&work).unwrap();
        let out = run(directory).await;
        assert_eq!(
            out.status.code(),
            Some(crate::sandbox::FOLDER_CHANGED_STATUS)
        );
        assert!(
            out.stdout.is_empty()
                && String::from_utf8_lossy(&out.stderr).contains("Nothing was run")
        );
        std::fs::remove_dir(&work).unwrap();
        let directory = ProcessDirectory::open(&base.join("saved")).unwrap();
        symlink(base.join("saved"), &work).unwrap();
        let out = run(directory).await;
        assert_eq!(
            out.status.code(),
            Some(crate::sandbox::FOLDER_CHANGED_STATUS)
        );
        assert!(out.stdout.is_empty());
    }

    /// Production launcher selection can be tested without substituting its
    /// implementation: the queue supplies the built CLI path explicitly.
    fn launcher_command(args: &[std::ffi::OsString]) -> tokio::process::Command {
        let exe = std::env::var_os("TESSERA_TEST_LAUNCHER_EXE").unwrap_or_else(|| args[2].clone());
        let mut cmd = tokio::process::Command::new(exe);
        cmd.args(&args[3..]);
        cmd
    }

    #[tokio::test]
    async fn loader_variables_reach_only_the_program_not_trusted_entry_or_refusal() {
        let temp = tempfile::tempdir().unwrap();
        let work = temp.path().canonicalize().unwrap();
        let library = work.join("constructor.so");
        let marker = work.join("loaded");
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/launcher_constructor.c");
        assert!(
            std::process::Command::new("cc")
                .args(["-shared", "-fPIC", "-o"])
                .arg(&library)
                .arg(source)
                .status()
                .unwrap()
                .success()
        );
        let run = |directory: ProcessDirectory, p: crate::sandbox::Prepared| {
            let (program, args) = directory.argv(Some(&p), "/bin/true".into(), vec![]);
            let mut cmd = tokio::process::Command::new(program);
            cmd.args(args);
            cmd.env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("LD_PRELOAD", &library)
                .env("LD_AUDIT", &library)
                .env("GLIBC_TUNABLES", "glibc.malloc.perturb=13")
                .env("TESSERA_CONSTRUCTOR_MARKER", &marker);
            let hold = directory.apply(&mut cmd, Some(p));
            (cmd, hold)
        };
        let directory = ProcessDirectory::open(&work).unwrap();
        let p = crate::sandbox::joined_for_tests(&work.display().to_string());
        let (mut cmd, _hold) = run(directory, p);
        let env: Vec<_> = cmd
            .as_std()
            .get_envs()
            .filter_map(|(k, v)| {
                v.map(|v| {
                    (
                        k.to_string_lossy().into_owned(),
                        v.to_string_lossy().into_owned(),
                    )
                })
            })
            .collect();
        assert!(
            env.iter().all(|(k, _)| [
                "PATH",
                "LANG",
                "LC_ALL",
                "TESSERA_INTERNAL_ENV_FD",
                "TESSERA_INTERNAL_SUPERVISION",
            ]
            .contains(&k.as_str())),
            "{env:?}"
        );
        assert!(
            !cmd.as_std()
                .get_args()
                .any(|a| a.to_string_lossy().contains("constructor.so"))
        );
        let out = cmd.output().await.unwrap();
        assert!(out.status.success(), "{out:?}");
        let loaded = std::fs::read_to_string(&marker).unwrap();
        assert!(
            loaded.lines().all(|l| l.starts_with("/bin/true ")),
            "trusted loader ran constructor: {loaded}"
        );
        std::fs::remove_file(&marker).unwrap();
        let target = work.join("target");
        std::fs::create_dir(&target).unwrap();
        let directory = ProcessDirectory::open(&target).unwrap();
        let p = crate::sandbox::joined_for_tests(&target.display().to_string());
        let (mut cmd, _hold) = run(directory, p);
        std::fs::rename(&target, work.join("saved")).unwrap();
        std::fs::create_dir(&target).unwrap();
        let out = cmd.output().await.unwrap();
        assert_eq!(
            out.status.code(),
            Some(crate::sandbox::FOLDER_CHANGED_STATUS)
        );
        assert!(!marker.exists(), "constructor ran before refused entry");

        // Same loader protection in an actual direct/shared mount lane.
        if let Some(spec) = test_sandbox_spec(&work) {
            let directory = ProcessDirectory::open(&work).unwrap();
            let p = directory
                .prepare(Some(&(spec.clone(), true)))
                .unwrap()
                .unwrap();
            let (mut cmd, _hold) = run(directory, p);
            assert!(cmd.output().await.unwrap().status.success());
            let loaded = std::fs::read_to_string(&marker).unwrap();
            assert!(
                loaded.lines().all(|l| l.starts_with("/bin/true ")),
                "{loaded}"
            );
            std::fs::remove_file(&marker).unwrap();
            let directory = ProcessDirectory::open(&target).unwrap();
            let p = directory.prepare(Some(&(spec, true))).unwrap().unwrap();
            let (mut cmd, _hold) = run(directory, p);
            std::fs::rename(&target, work.join("saved-again")).unwrap();
            std::fs::create_dir(&target).unwrap();
            match cmd.output().await {
                Err(e) => assert_eq!(e.raw_os_error(), Some(libc::ESTALE)),
                Ok(out) => assert_eq!(
                    out.status.code(),
                    Some(crate::sandbox::FOLDER_CHANGED_STATUS)
                ),
            }
            assert!(!marker.exists());
        }
        // The nsenter loader is tested too, even on a non-namespace run.
        let directory = ProcessDirectory::open(&work).unwrap();
        let p = crate::sandbox::joined_for_tests(&work.display().to_string());
        let (_, args) = directory.argv(Some(&p), "/bin/true".into(), vec![]);
        let mut cmd = tokio::process::Command::new("/usr/bin/nsenter");
        cmd.args(args);
        cmd.env_clear()
            .env("LD_PRELOAD", &library)
            .env("TESSERA_CONSTRUCTOR_MARKER", &marker);
        let _hold = directory.apply(&mut cmd, Some(p));
        assert!(cmd.output().await.unwrap().status.success());
        let loaded = std::fs::read_to_string(&marker).unwrap();
        assert!(
            loaded.lines().all(|l| l.starts_with("/bin/true ")),
            "{loaded}"
        );
    }

    #[tokio::test]
    async fn the_program_inherits_no_private_environment_or_supervision_descriptors() {
        let temp = tempfile::tempdir().unwrap();
        let directory = ProcessDirectory::open(temp.path()).unwrap();
        let p = crate::sandbox::joined_for_tests(&directory.path().display().to_string());
        let (_, args) = directory.argv(Some(&p), "/bin/sh".into(), vec!["-c".into(),
            "echo token:$PRIVATE_TOKEN; echo fdvar:${TESSERA_INTERNAL_ENV_FD-unset}; for f in /proc/self/fd/*; do readlink \"$f\"; done; true".into()]);
        let mut cmd = launcher_command(&args);
        cmd.env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("PRIVATE_TOKEN", "only-program");
        let _held = directory.apply(&mut cmd, Some(p));
        let out = cmd.output().await.unwrap();
        assert!(out.status.success(), "{out:?}");
        let output = String::from_utf8_lossy(&out.stdout);
        assert!(
            output.contains("token:only-program") && output.contains("fdvar:unset"),
            "{output}"
        );
        assert!(
            !output.contains("tessera-program-env") && !output.contains("pidfd"),
            "{output}"
        );
        // Only what any child of this test gets survives (its stdio pipes,
        // and whatever a CI runner hands down): never a supervision pipe,
        // which is CLOEXEC. A plain child started the same way is the baseline.
        let pipes = |text: &str| text.lines().filter(|l| l.starts_with("pipe:")).count();
        let plain = tokio::process::Command::new("/bin/sh")
            .args([
                "-c",
                "for f in /proc/self/fd/*; do readlink \"$f\"; done; true",
            ])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .output()
            .await
            .unwrap();
        let baseline = pipes(&String::from_utf8_lossy(&plain.stdout));
        assert!(pipes(&output) <= baseline, "{output}\nbaseline: {baseline}");
    }

    /// Pause AFTER wait reaped the requested leader and BEFORE the watchdog
    /// completion byte. Even with no ordinary target left, the live watchdog
    /// anchors that exact group; no delayed negative-PGID kill can hit reuse.
    #[tokio::test]
    async fn the_watchdog_anchors_the_group_through_the_reap_to_notify_window() {
        use std::os::unix::process::ExitStatusExt;
        for script in ["exit 0", "kill -TERM $$"] {
            let temp = tempfile::tempdir().unwrap();
            let work = temp.path().canonicalize().unwrap();
            let directory = ProcessDirectory::open(&work).unwrap();
            let p = if std::env::var("TESSERA_TEST_SANDBOX").as_deref() == Ok("shared") {
                directory
                    .prepare(Some(&(test_sandbox_spec(&work).unwrap(), true)))
                    .unwrap()
                    .unwrap()
            } else {
                crate::sandbox::joined_for_tests(&work.display().to_string())
            };
            let (program, args) =
                directory.argv(Some(&p), "/bin/sh".into(), vec!["-c".into(), script.into()]);
            let mut cmd = tokio::process::Command::new(program);
            cmd.args(args)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("TESSERA_TEST_PAUSE_AFTER_REAP", work.join("reaped"))
                .kill_on_drop(true);
            let _held = directory.apply(&mut cmd, Some(p));
            let mut child = cmd.spawn().unwrap();
            let launcher = child.id().unwrap();
            let anchor = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    if std::fs::read_to_string(format!("/proc/{launcher}/status"))
                        .is_ok_and(|s| s.lines().any(|l| l.starts_with("State:\tT")))
                    {
                        let children = std::fs::read_to_string(format!(
                            "/proc/{launcher}/task/{launcher}/children"
                        ))
                        .unwrap();
                        let pids: Vec<u32> = children
                            .split_whitespace()
                            .filter_map(|s| s.parse().ok())
                            .collect();
                        assert_eq!(pids.len(), 2, "leader not reaped: {children}");
                        assert!(
                            pids.iter()
                                .all(|p| crate::sandbox::supervisor_auxiliary(*p))
                        );
                        break pids[0];
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert!(crate::sandbox::supervisor_auxiliary(anchor));
            assert_eq!(
                crate::sandbox::program_pid(launcher),
                launcher,
                "anchor is never the program/status/PTY id"
            );
            let stat = std::fs::read_to_string(format!("/proc/{anchor}/stat")).unwrap();
            let group: u32 = stat
                .rsplit_once(')')
                .unwrap()
                .1
                .split_whitespace()
                .nth(2)
                .unwrap()
                .parse()
                .unwrap();
            assert_ne!(
                group, anchor,
                "watchdog must join target group, not its own"
            );
            assert!(
                !Path::new(&format!("/proc/{group}")).exists(),
                "target leader is already reaped"
            );
            // Job/TTY/polite signals must not stop/end the group's last anchor.
            for signal in [
                libc::SIGHUP,
                libc::SIGINT,
                libc::SIGQUIT,
                libc::SIGTERM,
                libc::SIGTSTP,
                libc::SIGTTIN,
                libc::SIGTTOU,
                libc::SIGWINCH,
            ] {
                assert_eq!(unsafe { libc::kill(anchor as i32, signal) }, 0);
            }
            let mut sentinel = tokio::process::Command::new("/bin/sleep")
                .arg("90")
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            assert!(
                std::fs::read_to_string(format!("/proc/{anchor}/status")).is_ok_and(|s| !s
                    .lines()
                    .any(|l| l.starts_with("State:\tT") || l.starts_with("State:\tZ")))
            );
            assert_eq!(unsafe { libc::kill(launcher as i32, libc::SIGCONT) }, 0);
            let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
                .await
                .unwrap()
                .unwrap();
            if script == "exit 0" {
                assert!(status.success());
            } else {
                assert_eq!(status.signal(), Some(libc::SIGTERM));
            }
            assert!(
                sentinel.try_wait().unwrap().is_none(),
                "cleanup hit unrelated work"
            );
            sentinel.kill().await.unwrap();
            sentinel.wait().await.unwrap();
        }
    }
    fn supervision_plan(directory: &ProcessDirectory, work: &Path) -> crate::sandbox::Prepared {
        if std::env::var("TESSERA_TEST_SANDBOX").as_deref() == Ok("shared") {
            directory
                .prepare(Some(&(test_sandbox_spec(work).unwrap(), true)))
                .unwrap()
                .unwrap()
        } else {
            crate::sandbox::joined_for_tests(&work.display().to_string())
        }
    }

    async fn registered_anchors(launcher: u32) -> Vec<u32> {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let pids: Vec<u32> =
                    std::fs::read_to_string(format!("/proc/{launcher}/task/{launcher}/children"))
                        .unwrap_or_default()
                        .split_whitespace()
                        .filter_map(|p| p.parse().ok())
                        .filter(|p| crate::sandbox::supervisor_auxiliary(*p))
                        .collect();
                if pids.len() == 2 {
                    break pids;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
    fn group_members(anchor: u32) -> Vec<u32> {
        let group = |pid| {
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|s| {
                    s.rsplit_once(')')?
                        .1
                        .split_whitespace()
                        .nth(2)?
                        .parse::<u32>()
                        .ok()
                })
        };
        let wanted = group(anchor).unwrap();
        std::fs::read_dir("/proc")
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.parse().ok())
            .filter(|p| group(*p) == Some(wanted))
            .collect()
    }
    async fn confirmed_stopped(pid: u32) {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !std::fs::read_to_string(format!("/proc/{pid}/status"))
                .is_ok_and(|s| s.lines().any(|l| l.starts_with("State:\tT")))
            {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("SIGSTOP was not confirmed");
    }
    async fn completely_gone(pids: &[u32]) {
        tokio::time::timeout(std::time::Duration::from_secs(8), async {
            while pids
                .iter()
                .any(|p| Path::new(&format!("/proc/{p}/stat")).exists())
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("owned work remains (including zombies)");
    }

    #[tokio::test]
    async fn a_watchdog_stopped_before_readiness_refuses_within_a_bound() {
        for (index, loss) in [(0, false), (0, true), (1, false), (1, true)] {
            let temp = tempfile::tempdir().unwrap();
            let work = temp.path().canonicalize().unwrap();
            let directory = ProcessDirectory::open(&work).unwrap();
            let p = supervision_plan(&directory, &work);
            let script = format!("touch '{}'", work.join("must-not-exec").display());
            let (program, args) =
                directory.argv(Some(&p), "/bin/sh".into(), vec!["-c".into(), script.into()]);
            let mut cmd = tokio::process::Command::new(program);
            cmd.args(args)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("TESSERA_TEST_WATCHDOG_STOP_READY", index.to_string())
                .kill_on_drop(true);
            let _hold = directory.apply(&mut cmd, Some(p));
            let mut child = cmd.spawn().unwrap();
            let launcher = child.id().unwrap();
            let anchors = registered_anchors(launcher).await;
            confirmed_stopped(anchors[index]).await;
            if loss {
                child.kill().await.unwrap();
            }
            let status = tokio::time::timeout(std::time::Duration::from_secs(8), child.wait())
                .await
                .unwrap()
                .unwrap();
            if loss {
                assert!(!status.success());
            } else {
                assert_eq!(status.code(), Some(126));
            }
            assert!(!work.join("must-not-exec").exists());
            completely_gone(&anchors).await;
        }
    }

    #[tokio::test]
    async fn stopped_dead_and_lost_launcher_watchdogs_have_bounded_group_cleanup() {
        for (index, mode) in [0, 1]
            .into_iter()
            .flat_map(|i| ["stopped", "dead", "stopped-launcher-loss"].map(|m| (i, m)))
        {
            let temp = tempfile::tempdir().unwrap();
            let work = temp.path().canonicalize().unwrap();
            let directory = ProcessDirectory::open(&work).unwrap();
            let p = supervision_plan(&directory, &work);
            let (program, args) = directory.argv(
                Some(&p),
                "/bin/sh".into(),
                vec!["-c".into(), "sleep 90 & wait".into()],
            );
            let mut cmd = tokio::process::Command::new(program);
            cmd.args(args)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .kill_on_drop(true);
            let _hold = directory.apply(&mut cmd, Some(p));
            let mut child = cmd.spawn().unwrap();
            let launcher = child.id().unwrap();
            let anchors = registered_anchors(launcher).await;
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let pids = group_members(anchors[0]);
            assert!(pids.len() >= 4, "leader and descendant missing: {pids:?}");
            let signal = if mode == "dead" {
                libc::SIGKILL
            } else {
                libc::SIGSTOP
            };
            // Pause the launcher first so it cannot resume the watchdog before
            // the regression has actually observed its stopped state.
            assert_eq!(unsafe { libc::kill(launcher as i32, libc::SIGSTOP) }, 0);
            confirmed_stopped(launcher).await;
            assert_eq!(unsafe { libc::kill(anchors[index] as i32, signal) }, 0);
            if signal == libc::SIGSTOP {
                confirmed_stopped(anchors[index]).await;
            }
            if mode == "stopped-launcher-loss" {
                assert_eq!(unsafe { libc::kill(launcher as i32, libc::SIGKILL) }, 0);
            } else {
                assert_eq!(unsafe { libc::kill(launcher as i32, libc::SIGCONT) }, 0);
            }
            let status = tokio::time::timeout(std::time::Duration::from_secs(8), child.wait())
                .await
                .unwrap()
                .unwrap();
            assert!(!status.success());
            completely_gone(&pids).await;
        }
    }

    #[tokio::test]
    async fn dead_fallback_before_leader_release_cleans_owned_work_and_spares_unrelated_work() {
        let temp = tempfile::tempdir().unwrap();
        let work = temp.path().canonicalize().unwrap();
        let directory = ProcessDirectory::open(&work).unwrap();
        let p = supervision_plan(&directory, &work);
        let (program, args) = directory.argv(
            Some(&p),
            "/bin/sh".into(),
            vec![
                "-c".into(),
                "sleep 90 >/dev/null 2>&1 & kill -STOP $$; exit 0".into(),
            ],
        );
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .kill_on_drop(true);
        let _hold = directory.apply(&mut cmd, Some(p));
        let mut child = cmd.spawn().unwrap();
        let launcher = child.id().unwrap();
        let anchors = registered_anchors(launcher).await;
        let leader = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let pid = crate::sandbox::program_pid(launcher);
                if pid != launcher {
                    break pid;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        confirmed_stopped(leader).await;
        // Stop the launcher before letting the leader exit. Its real child
        // stays unreaped through completion, exactly as in production.
        assert_eq!(unsafe { libc::kill(launcher as i32, libc::SIGSTOP) }, 0);
        confirmed_stopped(launcher).await;
        assert_eq!(unsafe { libc::kill(leader as i32, libc::SIGCONT) }, 0);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !std::fs::read_to_string(format!("/proc/{leader}/status"))
                .is_ok_and(|s| s.lines().any(|l| l.starts_with("State:\tZ")))
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("production leader must still be unreaped");
        let pids = group_members(anchors[0]);
        assert_eq!(
            pids.len(),
            4,
            "unreaped leader, two anchors and background work: {pids:?}"
        );
        assert!(pids.contains(&leader) && anchors.iter().all(|p| pids.contains(p)));
        let mut sentinel = tokio::process::Command::new("/bin/sleep")
            .arg("90")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        assert_eq!(unsafe { libc::kill(anchors[1] as i32, libc::SIGKILL) }, 0);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !std::fs::read_to_string(format!("/proc/{}/status", anchors[1]))
                .is_ok_and(|s| s.lines().any(|l| l.starts_with("State:\tZ")))
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fallback death must precede leader release");
        assert_eq!(unsafe { libc::kill(launcher as i32, libc::SIGCONT) }, 0);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(8), child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
        completely_gone(&pids).await;
        assert!(
            sentinel.try_wait().unwrap().is_none(),
            "unrelated group killed"
        );
        sentinel.kill().await.unwrap();
        sentinel.wait().await.unwrap();
    }

    #[tokio::test]
    async fn stopped_fallback_after_primary_exit_is_cleaned_on_launcher_loss() {
        let temp = tempfile::tempdir().unwrap();
        let work = temp.path().canonicalize().unwrap();
        let directory = ProcessDirectory::open(&work).unwrap();
        let p = supervision_plan(&directory, &work);
        let (program, args) = directory.argv(
            Some(&p),
            "/bin/sh".into(),
            vec!["-c".into(), "sleep 90 >/dev/null 2>&1 & exit 0".into()],
        );
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env(
                "TESSERA_TEST_PAUSE_AFTER_PRIMARY",
                work.join("primary-exited"),
            )
            .kill_on_drop(true);
        let _hold = directory.apply(&mut cmd, Some(p));
        let mut child = cmd.spawn().unwrap();
        let launcher = child.id().unwrap();
        let anchors = registered_anchors(launcher).await;
        confirmed_stopped(launcher).await;
        assert!(work.join("primary-exited").exists());
        let pids = group_members(anchors[1]);
        assert_eq!(unsafe { libc::kill(anchors[1] as i32, libc::SIGSTOP) }, 0);
        confirmed_stopped(anchors[1]).await;
        child.kill().await.unwrap();
        child.wait().await.unwrap();
        completely_gone(&pids).await;
    }

    #[tokio::test]
    async fn failed_anchor_construction_releases_every_created_child() {
        use std::os::fd::AsRawFd;
        for fail in [0, 1, 2] {
            let temp = tempfile::tempdir().unwrap();
            let work = temp.path().canonicalize().unwrap();
            let (identities, probe) = std::os::unix::net::UnixDatagram::pair().unwrap();
            identities.set_nonblocking(true).unwrap();
            let directory = ProcessDirectory::open(&work).unwrap();
            let p = supervision_plan(&directory, &work);
            let (program, args) = directory.argv(
                Some(&p),
                "/bin/sh".into(),
                vec!["-c".into(), "touch must-not-exec".into()],
            );
            let mut cmd = tokio::process::Command::new(program);
            cmd.args(args)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("TESSERA_TEST_ANCHOR_FAIL_AFTER", fail.to_string())
                .env(
                    "TESSERA_TEST_ANCHOR_IDENTITIES_FD",
                    probe.as_raw_fd().to_string(),
                )
                .kill_on_drop(true);
            let _hold = directory.apply(&mut cmd, Some(p));
            let error = cmd.spawn().unwrap_err();
            assert_eq!(error.raw_os_error(), Some(libc::EAGAIN));
            assert!(!work.join("must-not-exec").exists());
            let mut created = Vec::new();
            loop {
                let mut bytes = [0; std::mem::size_of::<i32>()];
                match identities.recv(&mut bytes) {
                    Ok(n) => {
                        assert_eq!(n, bytes.len());
                        let pid = i32::from_ne_bytes(bytes);
                        assert!(pid > 0);
                        created.push(pid as u32);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => panic!("construction identity stream failed: {e}"),
                }
            }
            assert_eq!(created.len(), fail, "every created anchor must be reported");
            completely_gone(&created).await; // Existence includes zombie state Z.
        }
    }

    #[tokio::test]
    async fn a_joined_peer_stopping_the_trusted_preexec_child_has_bounded_refusal() {
        // This regression always uses two commands in the SAME real joined PID
        // namespace, even when the other focused tests use the direct lane.
        let temp = tempfile::tempdir().unwrap();
        let work = temp.path().canonicalize().unwrap();
        let Some(spec) = test_sandbox_spec(&work) else {
            return;
        };
        let directory = ProcessDirectory::open(&work).unwrap();
        let peer_plan = crate::sandbox::shared_for_tests(&spec, &work);
        let script = "while [ ! -s preexec ]; do sleep 0.01; done; pid=$(cat preexec); kill -STOP $pid; while ! grep -q '^State:.*T' /proc/$pid/status; do sleep 0.01; done; touch confirmed-stopped";
        let (program, args) = directory.argv(
            Some(&peer_plan),
            "/bin/sh".into(),
            vec!["-c".into(), script.into()],
        );
        let mut peer = tokio::process::Command::new(program);
        peer.args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .kill_on_drop(true);
        let _peer_hold = directory.apply(&mut peer, Some(peer_plan));
        let mut peer = peer.spawn().unwrap();
        let directory = ProcessDirectory::open(&work).unwrap();
        let plan = crate::sandbox::shared_for_tests(&spec, &work);
        let (program, args) = directory.argv(
            Some(&plan),
            "/bin/sh".into(),
            vec!["-c".into(), "touch must-not-exec".into()],
        );
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("TESSERA_TEST_PREEXEC_STOP", work.join("preexec"))
            .kill_on_drop(true);
        let _hold = directory.apply(&mut cmd, Some(plan));
        let out = tokio::time::timeout(std::time::Duration::from_secs(8), cmd.output())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(out.status.code(), Some(126), "{out:?}");
        assert!(work.join("confirmed-stopped").exists());
        assert!(!work.join("must-not-exec").exists());
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), peer.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
    }
    #[tokio::test]
    async fn a_program_named_tessera_anchor_is_selected_counted_and_traversed() {
        let temp = tempfile::tempdir().unwrap();
        let work = temp.path().canonicalize().unwrap();
        let named = work.join("tessera-anchor");
        std::fs::copy("/bin/sleep", &named).unwrap();
        let directory = ProcessDirectory::open(&work).unwrap();
        let p = supervision_plan(&directory, &work);
        let (program, args) = directory.argv(Some(&p), named.into_os_string(), vec!["90".into()]);
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .kill_on_drop(true);
        let _hold = directory.apply(&mut cmd, Some(p));
        let mut child = cmd.spawn().unwrap();
        let launcher = child.id().unwrap();
        let anchors = registered_anchors(launcher).await;
        let requested = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let pid = crate::sandbox::program_pid(launcher);
                if pid != launcher
                    && std::fs::read_to_string(format!("/proc/{pid}/comm"))
                        .is_ok_and(|s| s.trim() == "tessera-anchor")
                {
                    break pid;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(format!("/proc/{requested}/comm"))
                .unwrap()
                .trim(),
            "tessera-anchor"
        );
        assert!(!crate::sandbox::supervisor_auxiliary(requested));
        let user_count = group_members(anchors[0])
            .into_iter()
            .filter(|pid| !crate::sandbox::supervisor_auxiliary(*pid))
            .count();
        assert_eq!(user_count, 1, "only the actual program is counted");
        assert!(crate::node::exec::has_descendant_for_tests(
            launcher, requested
        ));
        child.kill().await.unwrap();
        child.wait().await.unwrap();
    }
    /// Launcher kill, Stop-style group kill and timeout-style kill all end
    /// the requested child and its descendant. Include a stopped launcher
    /// while its child is running, and exit/signal status propagation.
    #[tokio::test]
    async fn native_supervision_ends_children_and_descendants_on_stop_or_timeout() {
        for mode in ["kill-launcher", "stop-timeout", "group-stop"] {
            let temp = tempfile::tempdir().unwrap();
            let work = temp.path().canonicalize().unwrap();
            let directory = ProcessDirectory::open(&work).unwrap();
            let p = if std::env::var("TESSERA_TEST_SANDBOX").as_deref() == Ok("shared") {
                let spec = test_sandbox_spec(&work).unwrap();
                directory.prepare(Some(&(spec, true))).unwrap().unwrap()
            } else {
                crate::sandbox::joined_for_tests(&work.display().to_string())
            };
            let script = format!(
                "sleep 90 & echo $$ $! > '{}'; wait",
                work.join("pids").display()
            );
            let (program, args) =
                directory.argv(Some(&p), "/bin/sh".into(), vec!["-c".into(), script.into()]);
            let mut cmd = tokio::process::Command::new(program);
            cmd.args(args);
            cmd.env_clear()
                .env("PATH", "/usr/bin:/bin")
                .kill_on_drop(true);
            #[cfg(unix)]
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setsid() < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let _hold = directory.apply(&mut cmd, Some(p));
            let mut child = cmd.spawn().unwrap();
            let pid = child.id().unwrap();
            let pids = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    if let Ok(s) = std::fs::read_to_string(work.join("pids"))
                        && s.split_whitespace().count() == 2
                    {
                        break s;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            let requested = crate::sandbox::program_pid(pid);
            let descendants =
                std::fs::read_to_string(format!("/proc/{requested}/task/{requested}/children"))
                    .unwrap();
            assert!(!descendants.trim().is_empty(), "no descendant for {pids}");
            let pids = format!("{requested} {descendants}");
            if mode == "stop-timeout" {
                assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGSTOP) }, 0);
            }
            if mode == "group-stop" {
                crate::tools::bash::kill_group(pid);
            } else {
                child.start_kill().unwrap();
            }
            child.wait().await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let gone = pids.split_whitespace().all(|pid| {
                        std::fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |s| {
                            s.rsplit_once(") ")
                                .is_some_and(|(_, tail)| tail.starts_with("Z "))
                        })
                    });
                    if gone {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("{mode} left running descendants: {pids}"));
        }
        for (script, expected) in [("exit 23", 23), ("kill -TERM $$", 128 + libc::SIGTERM)] {
            let temp = tempfile::tempdir().unwrap();
            let directory = ProcessDirectory::open(temp.path()).unwrap();
            let p = crate::sandbox::joined_for_tests(&directory.path().display().to_string());
            let (_, args) =
                directory.argv(Some(&p), "/bin/sh".into(), vec!["-c".into(), script.into()]);
            let mut cmd = launcher_command(&args);
            cmd.env_clear();
            let _hold = directory.apply(&mut cmd, Some(p));
            use std::os::unix::process::ExitStatusExt;
            let status = cmd.status().await.unwrap();
            assert_eq!(
                status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap()),
                expected
            );
        }
    }
    /// LSP memory accounting and terminal foreground checks must follow the
    /// new native supervisor to the requested program, as they did nsenter.
    #[tokio::test]
    async fn the_native_launcher_supervision_finds_the_requested_process() {
        let temp = tempfile::tempdir().unwrap();
        let directory = ProcessDirectory::open(temp.path()).unwrap();
        let p = crate::sandbox::joined_for_tests(&directory.path().display().to_string());
        let (_, args) = directory.argv(
            Some(&p),
            "/bin/sh".into(),
            vec!["-c".into(), "kill -STOP $$; exit 0".into()],
        );
        let mut cmd = tokio::process::Command::new(&args[2]);
        cmd.args(&args[3..]).kill_on_drop(true);
        let _held = directory.apply(&mut cmd, Some(p));
        let mut child = cmd.spawn().unwrap();
        let supervisor = child.id().unwrap();
        let program = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let pid = crate::sandbox::program_pid(supervisor);
                if pid != supervisor
                    && std::fs::read_to_string(format!("/proc/{pid}/status"))
                        .is_ok_and(|s| s.lines().any(|l| l.starts_with("State:\tT")))
                {
                    break pid;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(unsafe { libc::kill(program as i32, libc::SIGCONT) }, 0);
        assert!(child.wait().await.unwrap().success());
    }
    /// Refusal does not invoke printf, [, cd, exec or any interpreter at all.
    /// Both real namespace lanes receive hostile exported functions/startup
    /// files that would leave a visible side effect if anything imported them.
    #[tokio::test]
    async fn a_refused_launch_never_runs_imported_functions_or_shell_startup() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let work = base.join("work");
        let target = base.join("target");
        std::fs::create_dir(&work).unwrap();
        std::fs::create_dir(&target).unwrap();
        let Some(spec) = test_sandbox_spec(&work) else {
            return;
        };
        let marker = work.join("imported-function-ran");
        let startup = work.join("startup");
        std::fs::write(&startup, format!("echo startup > '{}'\n", marker.display())).unwrap();
        let directory = ProcessDirectory::open(&target).unwrap();
        let plan = directory.prepare(Some(&(spec, true))).unwrap();
        let (program, args) = directory.argv(
            plan.as_ref(),
            "/bin/sh".into(),
            vec!["-c".into(), "echo command-ran".into()],
        );
        std::fs::rename(&target, base.join("saved-target")).unwrap();
        std::fs::create_dir(&target).unwrap();
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .env("BASH_ENV", &startup)
            .env("ENV", &startup);
        for function in ["printf", "[", "cd", "exec", "eval", "unset", "test", "echo"] {
            cmd.env(
                format!("BASH_FUNC_{function}%%"),
                format!(
                    "() {{ /bin/echo imported > '{}'; return 0; }}",
                    marker.display()
                ),
            );
        }
        let _held = directory.apply(&mut cmd, plan);
        let out = cmd.output().await;
        assert!(!marker.exists(), "an imported function/startup ran");
        match out {
            Err(e) => assert_eq!(e.raw_os_error(), Some(libc::ESTALE)),
            Ok(out) => {
                assert_eq!(
                    out.status.code(),
                    Some(crate::sandbox::FOLDER_CHANGED_STATUS)
                );
                assert!(out.stdout.is_empty());
                assert!(String::from_utf8_lossy(&out.stderr).contains("Nothing was run"));
            }
        }
    }

    /// While the trusted launcher is stopped before entry, a concurrent
    /// same-group command tries the old slot, original fd, all process fds
    /// and cwd. The shared launcher must not even be visible in that group's
    /// PID namespace. Direct mode checks a stopped requested command too.
    #[tokio::test]
    async fn a_same_group_command_cannot_capture_a_stopped_launchers_host_folder() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let work = base.join("work");
        let target = base.join("target");
        std::fs::create_dir(&work).unwrap();
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("host-secret"), "host-only").unwrap();
        let Some(mut spec) = test_sandbox_spec(&work) else {
            return;
        };
        spec.hide.push(target.clone());
        let directory = ProcessDirectory::open(&target).unwrap();
        use std::os::fd::AsRawFd;
        let host_fd = directory.opened.file.as_raw_fd();
        let plan = directory.prepare(Some(&(spec.clone(), true))).unwrap();
        let stop = format!("kill -STOP $$; : '{}'; exit 0", base.display());
        let (program, args) = directory.argv(
            plan.as_ref(),
            "/bin/sh".into(),
            vec!["-c".into(), stop.clone().into()],
        );
        let launcher = args
            .iter()
            .position(|a| a == "--tessera-enter-directory")
            .map(|i| args[i - 1].to_string_lossy().into_owned());
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .env("TESSERA_TEST_DIRECTORY_STOP", "1")
            .kill_on_drop(true);
        let _held = directory.apply(&mut cmd, plan);
        let spawned = cmd.spawn();
        // Direct mode refuses a hidden cwd in pre_exec, before any visible
        // process exists. Use the allowed cwd to inspect a stopped command.
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => {
                assert_eq!(e.raw_os_error(), Some(libc::ESTALE));
                let directory = ProcessDirectory::open(&work).unwrap();
                let plan = directory.prepare(Some(&(spec.clone(), true))).unwrap();
                let (program, args) = directory.argv(
                    plan.as_ref(),
                    "/bin/sh".into(),
                    vec!["-c".into(), stop.into()],
                );
                let mut cmd = tokio::process::Command::new(program);
                cmd.args(args).kill_on_drop(true);
                let _held = directory.apply(&mut cmd, plan);
                cmd.spawn().unwrap()
            }
        };
        let pid = child.id().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
                if status.lines().any(|line| line.starts_with("State:\tT")) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let trust_check = launcher
            .map(|exe| format!("awk -v path='{exe}' '$5 == path && $6 ~ /(^|,)ro(,|$)/ {{ ro=1 }} END {{ exit !ro }}' /proc/self/mountinfo || echo WRITABLE-LAUNCHER\n"))
            .unwrap_or_default();
        let attack = trust_check
            + &format!(
                "VICTIM={pid}\nTOKEN='{}'\nHOST_FD={host_fd}\nTARGET='{}'\n",
                base.display(),
                target.strip_prefix("/").unwrap().display(),
            )
            + r#"
# A host PID is not visible in the shared namespace. Only signal a matching
# test process, never an unrelated process with the same numeric PID.
case "$(tr '\0' ' ' < /proc/$VICTIM/cmdline 2>/dev/null)" in
    *"$TOKEN"*) echo VISIBLE; kill -STOP $VICTIM 2>/dev/null ;;
esac
# Try the old slot, the original fd, every group process's fd, and cwd too.
for q in /proc/$VICTIM/fd/9 /proc/$VICTIM/fd/$HOST_FD /proc/[0-9]*/fd/* /proc/[0-9]*/cwd; do
    if [ -d "$q" ]; then
        (
            exec 8<"$q"
            for rel in host-secret target/host-secret "$TARGET/host-secret"; do
                [ -r "/proc/self/fd/8/$rel" ] && echo CAPTURED
            done
        ) 2>/dev/null
    fi
done
echo INSPECTED
"#;
        let out = sandboxed(
            ProcessDirectory::open(&work).unwrap(),
            &spec,
            &attack,
            || {},
        )
        .await
        .unwrap();
        assert_eq!(out.0, Some(0), "{out:?}");
        assert!(!out.1.contains("CAPTURED"), "{out:?}");
        assert!(
            !out.1.contains("WRITABLE-LAUNCHER"),
            "trusted executable must be read-only: {out:?}"
        );
        assert!(out.1.contains("INSPECTED"), "{out:?}");
        if std::env::var("TESSERA_TEST_SANDBOX").as_deref() == Ok("shared") {
            assert!(
                !out.1.contains("VISIBLE"),
                "trusted launcher stays outside the PID namespace: {out:?}"
            );
        }
        // Resume only our own host child. Namespace peers cannot do this for
        // the shared launcher: it becomes visible only after the checked entry.
        assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGCONT) }, 0);
        let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap();
        if std::env::var("TESSERA_TEST_SANDBOX").as_deref() == Ok("shared") {
            assert_eq!(status.code(), Some(crate::sandbox::FOLDER_CHANGED_STATUS));
        } else {
            assert!(status.success(), "{status:?}");
        }
    }

    /// A helper's command folder below its bound folder is reached from the
    /// held folder itself, one name at a time: a replaced root or a link on
    /// the way is refused, never followed.
    #[test]
    fn a_command_folder_is_opened_beneath_the_held_root() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let root = base.join("root");
        let outside = base.join("outside");
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::create_dir_all(outside.join("b")).unwrap();
        let held = DirectoryScope::capture(&root).unwrap().open().unwrap();
        let sub = held.open_beneath(&root.join("a/b")).unwrap();
        assert_eq!(sub.path(), root.join("a/b"));
        // Not below the root, or not a plain path below it.
        assert!(held.open_beneath(&base.join("outside/b")).is_err());
        assert!(held.open_beneath(&root.join("a/../../outside")).is_err());
        // A link on the way is refused even when it leads back inside.
        std::fs::rename(root.join("a"), root.join("real-a")).unwrap();
        symlink(root.join("real-a"), root.join("a")).unwrap();
        assert!(held.open_beneath(&root.join("a/b")).is_err());
        std::fs::remove_file(root.join("a")).unwrap();
        symlink(&outside, root.join("a")).unwrap();
        assert!(held.open_beneath(&root.join("a/b")).is_err());
        // The root replaced by another real folder after its check: the
        // subfolder is not taken from the replacement.
        std::fs::remove_file(root.join("a")).unwrap();
        std::fs::rename(root.join("real-a"), root.join("a")).unwrap();
        std::fs::rename(&root, base.join("saved-root")).unwrap();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        assert!(held.open_beneath(&root.join("a/b")).is_err());
    }

    #[test]
    fn directory_authority_roundtrips_and_rejects_replacement_by_link_or_new_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("selected");
        let other = dir.path().join("other");
        std::fs::create_dir(&path).unwrap();
        std::fs::create_dir(&other).unwrap();
        let scope = DirectoryScope::capture(&path).unwrap();
        let scope: DirectoryScope =
            serde_json::from_str(&serde_json::to_string(&scope).unwrap()).unwrap();
        assert!(scope.verify().is_ok());
        std::fs::rename(&path, dir.path().join("saved")).unwrap();
        symlink(&other, &path).unwrap();
        assert!(scope.verify().is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(scope.verify().is_err());
        std::fs::remove_dir(&path).unwrap();
        std::fs::rename(dir.path().join("saved"), &path).unwrap();
        assert!(scope.verify().is_ok());
    }
    use std::os::unix::fs::symlink;

    #[tokio::test]
    async fn opened_read_keeps_the_checked_file_when_a_link_replaces_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        let other = dir.path().join("other.txt");
        std::fs::write(&path, "checked notes").unwrap();
        std::fs::write(&other, "different notes").unwrap();
        let opened = Opened::open(&path).unwrap();
        std::fs::rename(&path, dir.path().join("saved.txt")).unwrap();
        symlink(&other, &path).unwrap();
        assert!(opened.bytes().await.is_err());
        // No reopening by pathname: the descriptor still holds the old object.
        let mut content = String::new();
        (&opened.file).read_to_string(&mut content).unwrap();
        assert_eq!(content, "checked notes");
    }

    #[test]
    fn directory_and_final_links_inserted_after_check_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("work");
        let other = dir.path().join("other");
        std::fs::create_dir(&parent).unwrap();
        std::fs::create_dir(&other).unwrap();
        let path = parent.join("notes.txt");
        std::fs::write(&path, "old").unwrap();
        let destination = Destination::prepare(&path, |_| Ok(())).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::write(other.join("notes.txt"), "private notes").unwrap();
        symlink(other.join("notes.txt"), &path).unwrap();
        assert!(destination.write(Some("old"), "new").is_err());
        assert_eq!(
            std::fs::read_to_string(other.join("notes.txt")).unwrap(),
            "private notes"
        );
        std::fs::rename(&parent, dir.path().join("saved")).unwrap();
        symlink(&other, &parent).unwrap();
        assert!(destination.write(None, "new").is_err());
        assert_eq!(
            std::fs::read_to_string(other.join("notes.txt")).unwrap(),
            "private notes"
        );
    }

    #[test]
    fn late_same_inode_growth_is_rejected_before_destination_comparison_allocates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("growing.txt");
        std::fs::write(&path, "old").unwrap();
        let opened = Opened::open(&path).unwrap();
        let expected = opened.read_bounded(16 << 20).unwrap();
        let destination = Destination::prepare(&path, |_| Ok(())).unwrap();
        let grown = 1 << 30;
        let error = destination
            .write_content_checked(Some(&expected), b"new", true, || {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_len(grown)
                    .unwrap();
            })
            .unwrap_err();
        assert!(
            error.to_string().contains("size or content changed"),
            "{error}"
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), grown);
        assert_eq!(
            opened.read_bounded(3).unwrap_err().kind(),
            io::ErrorKind::Other
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
    #[test]
    fn ordinary_nested_writes_are_atomic_and_keep_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new/sub/notes.txt");
        let destination = Destination::prepare(&path, |_| Ok(())).unwrap();
        destination.write(None, "first").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        destination.write(Some("first"), "second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o640);
        assert!(destination.write(Some("first"), "third").is_err());
        // Full write can still replace non-text files; edit remains text-only.
        let binary = dir.path().join("binary.bin");
        std::fs::write(&binary, [0xff, 0xfe]).unwrap();
        let destination = Destination::prepare(&binary, |_| Ok(())).unwrap();
        destination.write(None, "text").unwrap();
        assert_eq!(std::fs::read_to_string(binary).unwrap(), "text");
    }

    #[cfg(unix)]
    #[test]
    fn blob_and_skill_store_reads_reject_swapped_final_and_parent_links() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store");
        let outside = dir.path().join("outside");
        std::fs::create_dir(&store).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("asset"), "protected asset sentinel").unwrap();
        symlink(outside.join("asset"), store.join("asset")).unwrap();
        assert!(Opened::open_in(&store.join("asset"), &store).is_err());
        let nested = store.join("nested");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(nested.join("asset"), "ordinary").unwrap();
        std::fs::rename(&nested, store.join("saved")).unwrap();
        symlink(&outside, &nested).unwrap();
        assert!(Opened::open_in(&nested.join("asset"), &store).is_err());
        let opened = Opened::open_in(&store.join("saved/asset"), &store).unwrap();
        assert_eq!(opened.read_bounded(100).unwrap(), b"ordinary");
    }

    #[cfg(unix)]
    #[test]
    fn binary_restore_and_delete_remain_bound_to_the_authorized_parent() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        let outside = dir.path().join("outside");
        std::fs::create_dir(&work).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(work.join("x"), b"old\0").unwrap();
        std::fs::write(outside.join("x"), b"protected\0").unwrap();
        let destination = Destination::prepare(&work.join("x"), |_| Ok(())).unwrap();
        std::fs::rename(&work, dir.path().join("saved")).unwrap();
        symlink(&outside, &work).unwrap();
        assert!(destination.write_bytes(Some(b"old\0"), b"new\0").is_err());
        assert!(destination.delete(b"old\0").is_err());
        assert_eq!(std::fs::read(outside.join("x")).unwrap(), b"protected\0");
        let destination = Destination::prepare(&dir.path().join("saved/x"), |_| Ok(())).unwrap();
        destination.write_bytes(Some(b"old\0"), b"new\0").unwrap();
        destination.delete(b"new\0").unwrap();
        assert!(!dir.path().join("saved/x").exists());
    }

    #[test]
    fn binary_commit_and_delete_keep_revision_admission_after_object_binding() {
        use super::super::admission::{ActionAdmission, PermissionCeiling};
        use tessera_proto::PermissionMode;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x");
        std::fs::write(&path, b"old\0").unwrap();
        let ceiling = PermissionCeiling::new(PermissionMode::Auto);
        let destination = Destination::prepare_scoped(
            &path,
            |_| Ok(()),
            ActionAdmission::capture(vec![ceiling.clone()], None),
            Some(DirectoryScope::capture(dir.path()).unwrap()),
        )
        .unwrap();
        ceiling.store(PermissionMode::ReadOnly);
        ceiling.store(PermissionMode::Auto);
        assert_eq!(
            destination
                .write_bytes(Some(b"old\0"), b"new\0")
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            destination.delete(b"old\0").unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"old\0");
    }

    #[test]
    fn commit_checks_cloned_helper_scope_even_when_the_destination_did_not_move() {
        let dir = tempfile::tempdir().unwrap();
        let selected = dir.path().join("selected");
        let allowed = dir.path().join("allowed");
        std::fs::create_dir(&selected).unwrap();
        std::fs::create_dir(&allowed).unwrap();
        let path = allowed.join("x");
        std::fs::write(&path, b"old\0").unwrap();
        let destination = Destination::prepare_scoped(
            &path,
            |_| Ok(()),
            super::super::admission::ActionAdmission::default(),
            Some(DirectoryScope::capture(&selected).unwrap()),
        )
        .unwrap();
        let saved = dir.path().join("saved");
        std::fs::rename(&selected, &saved).unwrap();
        std::fs::create_dir(&selected).unwrap();
        assert!(destination.write_bytes(Some(b"old\0"), b"new\0").is_err());
        assert!(destination.delete(b"old\0").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"old\0");
        std::fs::remove_dir(&selected).unwrap();
        std::fs::rename(saved, &selected).unwrap();
        destination.write_bytes(Some(b"old\0"), b"new\0").unwrap();
        destination.delete(b"new\0").unwrap();
        assert!(!path.exists());
    }
}

#[cfg(all(test, windows))]
mod windows_directory_tests {
    use super::*;

    /// A helper's folder is held from its check until its process has
    /// started: it can't be renamed, nor a folder above it, and a folder put
    /// in its place after the check is refused.
    #[test]
    fn helper_process_entry_holds_the_checked_folder_in_place() {
        let temp = tempfile::tempdir().unwrap();
        let base = crate::platform::canonicalize(temp.path()).unwrap();
        let path = base.join("selected");
        std::fs::create_dir_all(path.join("sub")).unwrap();
        let scope = DirectoryScope::capture(&path).unwrap();
        let directory = scope.open().unwrap();
        assert_eq!(directory.path(), path);
        assert!(std::fs::rename(&path, base.join("moved")).is_err());
        assert!(
            crate::platform::windows::mapped_drive_path(&path).is_none(),
            "a local folder is entered by its own path"
        );
        let sub = directory.open_beneath(&path.join("sub")).unwrap();
        assert!(std::fs::rename(path.join("sub"), path.join("moved")).is_err());
        assert!(directory.open_beneath(&base.join("elsewhere")).is_err());
        let mut cmd = tokio::process::Command::new("cmd");
        let hold = sub.apply(&mut cmd, None);
        assert_eq!(
            cmd.as_std().get_current_dir(),
            Some(path.join("sub").as_path())
        );
        drop(hold);
        drop(directory);
        std::fs::rename(&path, base.join("saved")).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(scope.open().is_err(), "another folder at the same place");
        assert!(scope.verify().is_err());
        assert!(
            ProcessDirectory::open(&path).is_ok(),
            "ordinary chats enter the folder there now"
        );
    }
}
