//! Direct ConPTY (Windows 10 1809+) with non-inherited pipes and a mandatory Job.
//! Pipe IO and ClosePseudoConsole never block a Tokio worker. ConPTY can block
//! during close until its output is drained, so the reader stays alive in discard
//! mode until that call returns. Every error path uses the same cleanup.
use super::{choose_shell, clamp_size, environment_from, quote_wide, shell_start};
use parking_lot::Mutex;
use std::{
    ffi::OsString,
    io,
    mem::size_of,
    os::windows::ffi::OsStrExt,
    os::windows::io::AsRawHandle,
    path::PathBuf,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use tokio::sync::{Mutex as AsyncMutex, mpsc as async_mpsc, oneshot};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0},
    Storage::FileSystem::{ReadFile, WriteFile},
    System::{
        Console::{COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole},
        IO::CancelSynchronousIo,
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
        Pipes::{CreatePipe, PeekNamedPipe},
        Threading::{
            CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
            DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
            InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, ResumeThread, STARTUPINFOEXW,
            TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
        },
    },
};

struct Handle(HANDLE);
// Each pipe has exactly one IO thread; process and Job handles support concurrent
// queries. Owning handles stay alive until every use of their Arc is over.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
impl Handle {
    fn new(raw: HANDLE) -> io::Result<Self> {
        if raw.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(raw))
        }
    }
}

fn pipe() -> io::Result<(Handle, Handle)> {
    let (mut read, mut write) = (ptr::null_mut(), ptr::null_mut());
    // Null security attributes: neither end is inheritable, ever.
    if unsafe { CreatePipe(&mut read, &mut write, ptr::null(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((Handle(read), Handle(write)))
}
fn job() -> io::Result<Handle> {
    let job = Handle::new(unsafe { CreateJobObjectW(ptr::null(), ptr::null()) })?;
    let limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
        BasicLimitInformation:
            windows_sys::Win32::System::JobObjects::JOBOBJECT_BASIC_LIMIT_INFORMATION {
                LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                ..Default::default()
            },
        ..Default::default()
    };
    if unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}

struct Attributes {
    storage: Vec<usize>,
    initialized: bool,
}
impl Attributes {
    fn pointer(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr().cast()
    }
    fn new(console: HPCON) -> io::Result<Self> {
        let mut bytes = 0;
        unsafe {
            InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut bytes);
        }
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut out = Self {
            storage: vec![0; bytes.div_ceil(size_of::<usize>())],
            initialized: false,
        };
        if unsafe { InitializeProcThreadAttributeList(out.pointer(), 1, 0, &mut bytes) } == 0 {
            return Err(io::Error::last_os_error());
        }
        out.initialized = true;
        if unsafe {
            UpdateProcThreadAttribute(
                out.pointer(),
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                console as *const _,
                size_of::<HPCON>(),
                ptr::null_mut(),
                ptr::null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(out)
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        if self.initialized {
            unsafe {
                DeleteProcThreadAttributeList(self.pointer());
            }
        }
    }
}

pub struct Account {
    pub home: PathBuf,
    pub shell: PathBuf,
}
pub fn account() -> io::Result<Account> {
    let home = std::env::var_os("USERPROFILE")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .filter(|h| h.is_absolute() && h.is_dir())
        .ok_or_else(|| io::Error::other("this account has no usable home folder"))?;
    // Never execute a shell from the working directory or a relative PATH entry.
    let shell = choose_shell(|name| {
        std::env::var_os("PATH")
            .and_then(|paths| {
                std::env::split_paths(&paths)
                    .filter(|p| p.is_absolute())
                    .map(|p| p.join(name))
                    .find(|p| p.is_file())
            })
            .or_else(|| {
                let system = PathBuf::from(std::env::var_os("SystemRoot")?).join("System32");
                let path = match name {
                    "powershell.exe" => system.join("WindowsPowerShell/v1.0/powershell.exe"),
                    "cmd.exe" => system.join("cmd.exe"),
                    _ => return None,
                };
                (path.is_absolute() && path.is_file()).then_some(path)
            })
    })
    .ok_or_else(|| io::Error::other("no PowerShell or cmd shell found on this account"))?;
    Ok(Account { home, shell })
}
impl Account {
    pub fn shell_start(&self) -> (OsString, Vec<OsString>) {
        shell_start(&self.shell)
    }
    pub fn environment(&self) -> Vec<(OsString, OsString)> {
        environment_from(std::env::vars_os(), &self.home, &self.shell)
    }
}

/// Mirrors the Unix spawn contract, but cannot accept confinement or a UID switch.
/// Windows terminals deliberately run only as this process's local account.
pub struct Start {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
    pub sandbox: Option<crate::sandbox::Prepared>,
    pub cols: u16,
    pub rows: u16,
}
pub struct Spawned {
    pub pty: Pty,
    pub child: Child,
    pub pid: u32,
}
pub struct Child {
    handle: Arc<Handle>,
}
impl Child {
    pub async fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        use std::os::windows::process::ExitStatusExt;
        loop {
            match unsafe { WaitForSingleObject(self.handle.0, 0) } {
                WAIT_OBJECT_0 => {
                    let mut code = 0;
                    if unsafe { GetExitCodeProcess(self.handle.0, &mut code) } == 0 {
                        return Err(io::Error::last_os_error());
                    }
                    return Ok(std::process::ExitStatus::from_raw(code));
                }
                WAIT_FAILED => return Err(io::Error::last_os_error()),
                _ => tokio::time::sleep(Duration::from_millis(25)).await,
            }
        }
    }
}

struct Console(HPCON);
impl Drop for Console {
    fn drop(&mut self) {
        unsafe {
            ClosePseudoConsole(self.0);
        }
    }
}
struct Resources {
    console: Console,
    job: Handle,
    size: COORD,
}
// HPCON is an owning scalar; only the protected resize or cleanup thread uses it.
unsafe impl Send for Resources {}
struct Write {
    data: Vec<u8>,
    done: oneshot::Sender<io::Result<()>>,
}
struct Reader {
    rx: async_mpsc::Receiver<Vec<u8>>,
    kept: Vec<u8>,
    at: usize,
}
struct Inner {
    resources: Mutex<Option<Resources>>,
    // 0 normal, 1 drain/discard during close, 2 stop after console closed.
    state: Arc<AtomicU8>,
    threads: Mutex<Vec<thread::JoinHandle<()>>>,
    read: AsyncMutex<Reader>,
    write: mpsc::SyncSender<Write>,
}
#[derive(Clone)]
pub struct Pty(Arc<Inner>);
impl Inner {
    fn close(&self) {
        let Some(resources) = self.resources.lock().take() else {
            return;
        };
        self.state.store(1, Ordering::SeqCst);
        // Mandatory Job contains every child. Never kill by a reusable PID.
        unsafe {
            TerminateJobObject(resources.job.0, 1);
        }
        let threads = std::mem::take(&mut *self.threads.lock());
        let state = self.state.clone();
        for t in &threads {
            unsafe {
                CancelSynchronousIo(t.as_raw_handle());
            }
        }
        // An unbounded thread spawn is not an unbounded IO queue: one cleanup
        // per terminal, bounded by Host's count limits, and no Tokio blocking.
        let cleanup = move || {
            drop(resources.console);
            state.store(2, Ordering::SeqCst);
            for t in threads {
                unsafe {
                    CancelSynchronousIo(t.as_raw_handle());
                }
                let _ = t.join();
            }
            drop(resources.job);
        };
        // If a cleanup thread cannot be allocated, dropping its captured
        // Console still closes it while the independent reader drains output.
        let _ = thread::Builder::new()
            .name("terminal-close".into())
            .spawn(cleanup);
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.close();
    }
}
impl Pty {
    pub fn close(&self) {
        self.0.close();
    }
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let (cols, rows) = clamp_size(cols, rows);
        let mut resources = self.0.resources.lock();
        let r = resources
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "terminal closed"))?;
        let status = unsafe {
            ResizePseudoConsole(
                r.console.0,
                COORD {
                    X: cols as i16,
                    Y: rows as i16,
                },
            )
        };
        if status < 0 {
            return Err(io::Error::other(format!(
                "ConPTY resize failed ({status:#x})"
            )));
        }
        r.size = COORD {
            X: cols as i16,
            Y: rows as i16,
        };
        Ok(())
    }
    pub fn redraw(&self) {
        let resources = self.0.resources.lock();
        let Some(r) = resources.as_ref() else { return };
        // A reconnect may have no new dimensions. Trigger a resize event and
        // restore the size without injecting any command or keyboard input.
        let temporary = COORD {
            X: if r.size.X < 1000 {
                r.size.X + 1
            } else {
                r.size.X - 1
            },
            Y: r.size.Y,
        };
        unsafe {
            ResizePseudoConsole(r.console.0, temporary);
            ResizePseudoConsole(r.console.0, r.size);
        }
    }
    pub fn at_prompt(&self) -> Option<bool> {
        super::prompt_state()
    }
    pub async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let mut reader = self.0.read.lock().await;
        if reader.at == reader.kept.len() {
            let Some(data) = reader.rx.recv().await else {
                return Ok(0);
            };
            reader.kept = data;
            reader.at = 0;
        }
        let n = buf.len().min(reader.kept.len() - reader.at);
        buf[..n].copy_from_slice(&reader.kept[reader.at..reader.at + n]);
        reader.at += n;
        Ok(n)
    }
    pub async fn write_all(&self, data: &[u8]) -> io::Result<()> {
        if data.len() > crate::terminal::MAX_INPUT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "terminal input exceeds 64 KB",
            ));
        }
        let (done, completion) = oneshot::channel();
        let mut message = Write {
            data: data.to_vec(),
            done,
        };
        loop {
            if self.0.state.load(Ordering::SeqCst) != 0 {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "terminal closed"));
            }
            match self.0.write.try_send(message) {
                Ok(()) => break,
                Err(mpsc::TrySendError::Full(m)) => {
                    message = m;
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "terminal input closed",
                    ));
                }
            }
        }
        completion.await.unwrap_or_else(|_| {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "terminal input closed",
            ))
        })
    }
}

fn reader_thread(pipe: Handle, state: Arc<AtomicU8>, tx: async_mpsc::Sender<Vec<u8>>) {
    let mut pending = None;
    let mut buf = [0u8; 16 << 10];
    loop {
        match state.load(Ordering::SeqCst) {
            2 => return,
            1 => pending = None,
            _ => {
                if let Some(data) = pending.take() {
                    match tx.try_send(data) {
                        Ok(()) => (),
                        Err(async_mpsc::error::TrySendError::Full(data)) => {
                            pending = Some(data);
                            thread::sleep(Duration::from_millis(10));
                            continue;
                        }
                        Err(async_mpsc::error::TrySendError::Closed(_)) => {
                            state.store(1, Ordering::SeqCst);
                        }
                    }
                }
            }
        }
        let mut available = 0;
        if unsafe {
            PeekNamedPipe(
                pipe.0,
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                &mut available,
                ptr::null_mut(),
            )
        } == 0
        {
            return;
        }
        if available == 0 {
            thread::sleep(Duration::from_millis(10));
            continue;
        }
        let mut read = 0;
        if unsafe {
            ReadFile(
                pipe.0,
                buf.as_mut_ptr(),
                available.min(buf.len() as u32),
                &mut read,
                ptr::null_mut(),
            )
        } == 0
            || read == 0
        {
            return;
        }
        if state.load(Ordering::SeqCst) == 0 {
            pending = Some(buf[..read as usize].to_vec());
        }
    }
}
fn writer_thread(pipe: Handle, state: Arc<AtomicU8>, rx: mpsc::Receiver<Write>) {
    while state.load(Ordering::SeqCst) == 0 {
        let message = match rx.recv_timeout(Duration::from_millis(25)) {
            Ok(m) => m,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => return,
        };
        let result = (|| {
            let mut at = 0;
            while at < message.data.len() {
                if state.load(Ordering::SeqCst) != 0 {
                    return Err(io::Error::new(io::ErrorKind::BrokenPipe, "terminal closed"));
                }
                let mut n = 0;
                if unsafe {
                    WriteFile(
                        pipe.0,
                        message.data[at..].as_ptr(),
                        (message.data.len() - at).min(u32::MAX as usize) as u32,
                        &mut n,
                        ptr::null_mut(),
                    )
                } == 0
                {
                    return Err(io::Error::last_os_error());
                }
                if n == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "terminal stopped reading",
                    ));
                }
                at += n as usize;
            }
            Ok(())
        })();
        let _ = message.done.send(result);
    }
}
fn wide(s: &std::ffi::OsStr) -> io::Result<Vec<u16>> {
    let mut v: Vec<u16> = s.encode_wide().collect();
    if v.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "terminal path or environment contains a zero character",
        ));
    }
    v.push(0);
    Ok(v)
}

pub fn spawn(start: Start) -> io::Result<Spawned> {
    if start.sandbox.is_some() {
        return Err(io::Error::other(
            "Windows terminals cannot be sandboxed or change accounts",
        ));
    }
    let (cols, rows) = clamp_size(start.cols, start.rows);
    let program = wide(&start.program)?;
    let dir = wide(start.cwd.as_os_str())?;
    let mut command = Vec::new();
    for arg in std::iter::once(&start.program).chain(start.args.iter()) {
        let wide = wide(arg)?;
        if !command.is_empty() {
            command.push(b' ' as u16);
        }
        command.extend(quote_wide(&wide[..wide.len() - 1]));
    }
    command.push(0);
    let mut environment = Vec::new();
    let mut env = start.env;
    env.sort_by_key(|(k, _)| k.to_string_lossy().to_ascii_uppercase());
    for (key, value) in env {
        if super::withheld(&key.to_string_lossy()) {
            continue;
        }
        let key = wide(&key)?;
        if key[..key.len() - 1].contains(&(b'=' as u16)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid environment key",
            ));
        }
        environment.extend(&key[..key.len() - 1]);
        environment.push(b'=' as u16);
        environment.extend(wide(&value)?);
    }
    environment.push(0);
    if environment.len() == 1 {
        environment.push(0);
    }
    let job = job()?;
    let (input, write) = pipe()?;
    let (read, output) = pipe()?;
    let mut console = 0;
    let status = unsafe {
        CreatePseudoConsole(
            COORD {
                X: cols as i16,
                Y: rows as i16,
            },
            input.0,
            output.0,
            0,
            &mut console,
        )
    };
    if status < 0 {
        return Err(io::Error::other(format!(
            "Windows terminals need ConPTY (Windows 10 1809 or newer): {status:#x}"
        )));
    }
    drop((input, output));
    let state = Arc::new(AtomicU8::new(0));
    // No more than 5 * 16KiB can wait outside Host's existing output window.
    let (tx, rx) = async_mpsc::channel(4);
    let (write_tx, write_rx) = mpsc::sync_channel(1);
    let pty = Pty(Arc::new(Inner {
        resources: Mutex::new(Some(Resources {
            console: Console(console),
            job,
            size: COORD {
                X: cols as i16,
                Y: rows as i16,
            },
        })),
        state: state.clone(),
        threads: Mutex::new(Vec::new()),
        read: AsyncMutex::new(Reader {
            rx,
            kept: Vec::new(),
            at: 0,
        }),
        write: write_tx,
    }));
    let s = state.clone();
    let reader = thread::Builder::new()
        .name("terminal-output".into())
        .spawn(move || reader_thread(read, s, tx))?;
    pty.0.threads.lock().push(reader);
    let writer = thread::Builder::new()
        .name("terminal-input".into())
        .spawn(move || writer_thread(write, state, write_rx))?;
    pty.0.threads.lock().push(writer);
    let mut attributes = Attributes::new(console)?;
    let startup = STARTUPINFOEXW {
        StartupInfo: windows_sys::Win32::System::Threading::STARTUPINFOW {
            cb: size_of::<STARTUPINFOEXW>() as u32,
            ..Default::default()
        },
        lpAttributeList: attributes.pointer(),
    };
    let mut process = PROCESS_INFORMATION::default();
    // No std handles or service descriptors are inherited; only ConPTY's opaque
    // attribute is supplied. The process cannot run before Job assignment.
    if unsafe {
        CreateProcessW(
            program.as_ptr(),
            command.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr().cast(),
            dir.as_ptr(),
            &startup.StartupInfo,
            &mut process,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let child = Arc::new(Handle(process.hProcess));
    let main_thread = Handle(process.hThread);
    let resources = pty.0.resources.lock();
    let job = &resources.as_ref().expect("console just created").job;
    if unsafe { AssignProcessToJobObject(job.0, child.0) } == 0 {
        let error = io::Error::last_os_error();
        unsafe {
            TerminateProcess(child.0, 1);
        }
        return Err(io::Error::other(format!(
            "could not contain the terminal's programs in a Windows Job: {error}"
        )));
    }
    if unsafe { ResumeThread(main_thread.0) } == u32::MAX {
        let error = io::Error::last_os_error();
        unsafe {
            TerminateProcess(child.0, 1);
        }
        return Err(error);
    }
    drop(resources);
    drop(main_thread);
    Ok(Spawned {
        pty,
        child: Child { handle: child },
        pid: process.dwProcessId,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn conpty_unicode_resize_and_close() {
        let _dog = crate::terminal::watchdog("Windows ConPTY smoke test", 30);
        let mut a = account().unwrap();
        let dir = tempfile::tempdir().unwrap();
        a.home = dir.path().to_owned();
        let (program, mut args) = a.shell_start();
        let is_cmd = a
            .shell
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .eq_ignore_ascii_case("cmd.exe");
        if !is_cmd {
            args.insert(0, "-NoProfile".into());
        }
        let mut spawned = spawn(Start {
            program,
            args,
            cwd: dir.path().to_owned(),
            env: a.environment(),
            sandbox: None,
            cols: 80,
            rows: 24,
        })
        .unwrap();
        spawned.pty.resize(0, u16::MAX).unwrap();
        spawned
            .pty
            .write_all(if is_cmd {
                "echo TESSERA_^ユニコード\r\n".as_bytes()
            } else {
                "Write-Output ('TESSERA_' + 'ユニコード')\r\n".as_bytes()
            })
            .await
            .unwrap();
        let mut output = Vec::new();
        let mut buf = [0; 4096];
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let n = spawned.pty.read(&mut buf).await.unwrap();
                assert_ne!(n, 0);
                output.extend(&buf[..n]);
                if String::from_utf8_lossy(&output).contains("TESSERA_ユニコード") {
                    break;
                }
            }
        })
        .await
        .unwrap();
        spawned.pty.close();
        tokio::time::timeout(Duration::from_secs(5), spawned.child.wait())
            .await
            .unwrap()
            .unwrap();
    }
}
