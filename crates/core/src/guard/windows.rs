//! Tessera Guard on Windows: the service (its pipe, the Stop in the
//! registry, the jobs it holds), installing and removing it, and the client
//! side the machine, the command line and the app use.
//!
//! Every client checks that the pipe it reached is the service's own (its
//! process id), and talks at identification level only, so a program that
//! took the pipe's name first can neither lie about the Stop nor act as a
//! caller who raised itself through Windows' administrator prompt.

use super::{
    Client, DIR_NAME, DIR_SDDL, EXE_NAME, KEY_SDDL, MAX_MESSAGE, PIPE_NAME, PIPE_SDDL, Reply,
    Request, SERVICE_DISPLAY, SERVICE_NAME, STATE_KEY, Saved, State, Uac, recover,
};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_BROKEN_PIPE, ERROR_CALL_NOT_IMPLEMENTED,
    ERROR_CANCELLED, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
    ERROR_SERVICE_ALREADY_RUNNING, ERROR_SERVICE_CANNOT_ACCEPT_CTRL, ERROR_SERVICE_DOES_NOT_EXIST,
    ERROR_SERVICE_NOT_ACTIVE, ERROR_TOO_MANY_POSTS, GENERIC_READ, GENERIC_WRITE, GetLastError,
    HANDLE, INVALID_HANDLE_VALUE, LocalFree, NO_ERROR, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
    SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    ACL, CheckTokenMembership, CreateWellKnownSid, DACL_SECURITY_INFORMATION, EqualSid,
    GetLengthSid, GetSecurityDescriptorDacl, GetSidSubAuthority, GetSidSubAuthorityCount,
    GetTokenInformation, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    RevertToSelf, SECURITY_ATTRIBUTES, SECURITY_MAX_SID_SIZE, TOKEN_ELEVATION,
    TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_USER, TokenElevation, TokenElevationType,
    TokenElevationTypeFull, TokenElevationTypeLimited, TokenIntegrityLevel, TokenUser,
    WinBuiltinAdministratorsSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_ALWAYS, CreateDirectoryW, CreateFileW, DeleteFileW, FILE_ATTRIBUTE_NORMAL,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
    FILE_FLAG_SEQUENTIAL_SCAN, FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_READ,
    FILE_WRITE_DATA, FlushFileBuffers, GetFileAttributesW, INVALID_FILE_ATTRIBUTES, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX, ReadFile, RemoveDirectoryW, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
    SYNCHRONIZE, WRITE_DAC, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    GetNamedPipeClientSessionId, GetNamedPipeServerProcessId, ImpersonateNamedPipeClient,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
    PIPE_WAIT, WaitNamedPipeW,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_ALL_ACCESS, KEY_QUERY_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegFlushKey,
    RegGetValueW, RegOpenKeyExW, RegSetKeySecurity, RegSetKeyValueW,
};
use windows_sys::Win32::System::RemoteDesktop::{ProcessIdToSessionId, WTSQueryUserToken};
use windows_sys::Win32::System::Services::{
    ChangeServiceConfig2W, ChangeServiceConfigW, CloseServiceHandle, ControlService,
    CreateServiceW, DeleteService, OpenSCManagerW, OpenServiceW, QUERY_SERVICE_CONFIGW,
    QueryServiceConfigW, QueryServiceStatusEx, RegisterServiceCtrlHandlerExW, SC_ACTION,
    SC_ACTION_RESTART, SC_HANDLE, SC_MANAGER_ALL_ACCESS, SC_MANAGER_CONNECT,
    SC_STATUS_PROCESS_INFO, SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP, SERVICE_ALL_ACCESS,
    SERVICE_AUTO_START, SERVICE_CONFIG_DESCRIPTION, SERVICE_CONFIG_FAILURE_ACTIONS,
    SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP,
    SERVICE_DESCRIPTIONW, SERVICE_ERROR_NORMAL, SERVICE_FAILURE_ACTIONSW, SERVICE_NO_CHANGE,
    SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START_PENDING,
    SERVICE_STATUS, SERVICE_STATUS_HANDLE, SERVICE_STATUS_PROCESS, SERVICE_STOP_PENDING,
    SERVICE_STOPPED, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus,
    StartServiceCtrlDispatcherW, StartServiceW,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateSemaphoreW, GetCurrentProcess, GetCurrentThread, GetExitCodeProcess,
    OpenProcess, OpenProcessToken, OpenSemaphoreW, OpenThreadToken,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    ReleaseSemaphore, SEMAPHORE_MODIFY_STATE, SetEvent, TerminateProcess, WaitForMultipleObjects,
    WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::{
    SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

// ---- small pieces ------------------------------------------------------------

fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.as_ref().encode_wide().chain(Some(0)).collect()
}

fn os_error(code: u32) -> io::Error {
    io::Error::from_raw_os_error(code as i32)
}

/// A kernel handle, closed when dropped.
struct Owned(HANDLE);

// SAFETY: a kernel handle may be used and closed from any thread.
unsafe impl Send for Owned {}
// SAFETY: as above; the calls made through it are thread-safe.
unsafe impl Sync for Owned {}

impl Owned {
    fn new(h: HANDLE) -> Option<Self> {
        (!h.is_null() && h != INVALID_HANDLE_VALUE).then_some(Self(h))
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: a handle this owns.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// A service manager or service handle, closed when dropped.
struct Sc(SC_HANDLE);

impl Sc {
    fn new(h: SC_HANDLE) -> io::Result<Self> {
        if h.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(h))
        }
    }
}

impl Drop for Sc {
    fn drop(&mut self) {
        // SAFETY: a handle this owns.
        unsafe {
            CloseServiceHandle(self.0);
        }
    }
}

/// A security descriptor made from SDDL, freed when dropped.
struct Sd(PSECURITY_DESCRIPTOR);

impl Sd {
    fn new(sddl: &str) -> io::Result<Self> {
        let text = wide(sddl);
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: a NUL-terminated string; the descriptor is freed in Drop.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(sd))
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }

    fn dacl(&self) -> io::Result<*mut ACL> {
        let (mut present, mut defaulted) = (0, 0);
        let mut dacl: *mut ACL = std::ptr::null_mut();
        // SAFETY: a valid descriptor; the DACL points into it.
        let ok =
            unsafe { GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted) };
        if ok == 0 || present == 0 {
            return Err(io::Error::other(
                "the security descriptor has no access list",
            ));
        }
        Ok(dacl)
    }

    /// Put this descriptor's access list on `path` (a file or folder),
    /// replacing what it had and inheriting nothing.
    fn apply_to(&self, path: &Path) -> io::Result<()> {
        let name = wide(path);
        let dacl = self.dacl()?;
        // SAFETY: valid name and access list.
        let r = unsafe {
            SetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null(),
            )
        };
        if r != NO_ERROR {
            return Err(os_error(r));
        }
        Ok(())
    }
}

impl Drop for Sd {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe {
            LocalFree(self.0);
        }
    }
}

/// A text value of HKEY_LOCAL_MACHINE\`key`.
fn machine_text(key: &str, name: &str) -> Option<String> {
    let (key, name) = (wide(key), wide(name));
    let mut buf = vec![0u16; 1024];
    let mut bytes = (buf.len() * 2) as u32;
    // SAFETY: `buf` holds `bytes` bytes.
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if r != NO_ERROR {
        return None;
    }
    let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// A number value of HKEY_LOCAL_MACHINE\`key`: `Ok(None)` when it (or the
/// key) isn't there.
fn machine_number(key: &str, name: &str) -> io::Result<Option<u32>> {
    let (key, name) = (wide(key), wide(name));
    let mut value = 0u32;
    let mut bytes = 4u32;
    // SAFETY: `value` holds 4 bytes.
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut value as *mut u32).cast(),
            &mut bytes,
        )
    };
    match r {
        NO_ERROR => Ok(Some(value)),
        ERROR_FILE_NOT_FOUND => Ok(None),
        e => Err(os_error(e)),
    }
}

// ---- tokens ------------------------------------------------------------------

/// What a token says about its user.
struct TokenFacts {
    elevated: bool,
    admin: bool,
    integrity: Option<u32>,
    sid: Vec<u8>,
}

/// `token`'s facts (opened with TOKEN_QUERY).
unsafe fn token_facts(token: HANDLE) -> Option<TokenFacts> {
    // SAFETY: a token opened with TOKEN_QUERY; buffers sized for each class.
    unsafe {
        let mut len = 0u32;
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let elevated = GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        ) != 0
            && elevation.TokenIsElevated != 0;
        let mut label = [0u64; 16];
        let integrity = if GetTokenInformation(
            token,
            TokenIntegrityLevel,
            label.as_mut_ptr().cast(),
            std::mem::size_of_val(&label) as u32,
            &mut len,
        ) != 0
        {
            let sid = (*(label.as_ptr() as *const TOKEN_MANDATORY_LABEL))
                .Label
                .Sid;
            let count = *GetSidSubAuthorityCount(sid);
            (count > 0).then(|| *GetSidSubAuthority(sid, u32::from(count) - 1))
        } else {
            None
        };
        let mut user = [0u64; 32];
        if GetTokenInformation(
            token,
            TokenUser,
            user.as_mut_ptr().cast(),
            std::mem::size_of_val(&user) as u32,
            &mut len,
        ) == 0
        {
            return None;
        }
        let sid_ptr = (*(user.as_ptr() as *const TOKEN_USER)).User.Sid;
        let sid_len = GetLengthSid(sid_ptr) as usize;
        let sid = std::slice::from_raw_parts(sid_ptr as *const u8, sid_len).to_vec();
        let mut admins = [0u8; SECURITY_MAX_SID_SIZE as usize];
        let mut size = admins.len() as u32;
        let mut member = 0;
        let admin = CreateWellKnownSid(
            WinBuiltinAdministratorsSid,
            std::ptr::null_mut(),
            admins.as_mut_ptr().cast(),
            &mut size,
        ) != 0
            && CheckTokenMembership(token, admins.as_mut_ptr().cast(), &mut member) != 0
            && member != 0;
        Some(TokenFacts {
            elevated,
            admin,
            integrity,
            sid,
        })
    }
}

/// Browser folders: protected owner-only inheritable DACL, including existing
/// directories. Verify on the same non-reparse, non-delete-shared handle.
pub struct BrowserFolder(Owned);

pub fn private_browser_dir(path: &Path) -> io::Result<BrowserFolder> {
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, GetSecurityInfo, SetSecurityInfo,
    };
    use windows_sys::Win32::Security::{
        GetSecurityDescriptorControl, OWNER_SECURITY_INFORMATION, SE_DACL_PROTECTED,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, GetFileInformationByHandle, OPEN_EXISTING, READ_CONTROL,
        WRITE_DAC,
    };
    let (token, _hold) = own_token().ok_or_else(|| io::Error::other("no account token"))?;
    let facts = unsafe { token_facts(token) }.ok_or_else(|| io::Error::other("no account SID"))?;
    let sid = facts.sid.as_ptr() as PSID;
    let mut text = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut n = 0;
    unsafe {
        while *text.add(n) != 0 {
            n += 1;
        }
    }
    let user = unsafe { String::from_utf16_lossy(std::slice::from_raw_parts(text, n)) };
    unsafe {
        LocalFree(text.cast());
    }
    let descriptor = Sd::new(&format!("D:P(A;OICI;FA;;;{user})"))?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
        && !parent.exists()
    {
        private_browser_dir(parent)?;
    }
    // Reject every reparse ancestor; never follow a junction into somebody
    // else's profile. The returned handle prevents renaming this folder.
    for ancestor in path.ancestors() {
        not_a_link(ancestor)?;
    }
    let name = wide(path);
    if unsafe { CreateDirectoryW(name.as_ptr(), &descriptor.attributes()) } == 0
        && unsafe { GetLastError() } != ERROR_ALREADY_EXISTS
    {
        return Err(io::Error::last_os_error());
    }
    not_a_link(path)?;
    let handle = Owned::new(unsafe {
        CreateFileW(
            name.as_ptr(),
            READ_CONTROL | WRITE_DAC,
            FILE_SHARE_READ,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    })
    .ok_or_else(io::Error::last_os_error)?;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(handle.0, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
        || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(io::Error::other("browser folder is not a plain directory"));
    }
    let result = unsafe {
        SetSecurityInfo(
            handle.0,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            descriptor.dacl()?,
            std::ptr::null(),
        )
    };
    if result != NO_ERROR {
        return Err(os_error(result));
    }
    let (mut owner, mut acl, mut sd) = (
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    );
    let expected = descriptor.dacl()?;
    let result = unsafe {
        GetSecurityInfo(
            handle.0,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            &mut acl,
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    if result != NO_ERROR {
        return Err(os_error(result));
    }
    let check = unsafe {
        let (mut flags, mut revision) = (0, 0);
        !owner.is_null()
            && EqualSid(owner, sid) != 0
            && !acl.is_null()
            && GetSecurityDescriptorControl(sd, &mut flags, &mut revision) != 0
            && flags & SE_DACL_PROTECTED != 0
            && (*acl).AclSize == (*expected).AclSize
            && std::slice::from_raw_parts(acl.cast::<u8>(), (*acl).AclSize as usize)
                == std::slice::from_raw_parts(expected.cast::<u8>(), (*expected).AclSize as usize)
    };
    unsafe {
        LocalFree(sd);
    }
    if !check {
        return Err(io::Error::other(
            "browser folder isn't owned and protected by this account",
        ));
    }
    Ok(BrowserFolder(handle))
}

impl BrowserFolder {
    pub fn hold(&self) {
        let _ = self.0.0;
    }
}

#[cfg(test)]
mod browser_folder_tests {
    use super::*;
    #[test]
    fn private_browser_folder_repairs_existing_and_ignores_permissive_parent() {
        let root = tempfile::tempdir().unwrap();
        let permissive = Sd::new("D:P(A;OICI;FA;;;WD)").unwrap();
        permissive.apply_to(root.path()).unwrap();
        let child = root.path().join("browser");
        std::fs::create_dir(&child).unwrap();
        permissive.apply_to(&child).unwrap();
        // The function verifies protected, exact owner-only DACL internally.
        let held = private_browser_dir(&child).unwrap();
        held.hold();
        assert!(std::fs::rename(&child, root.path().join("swapped")).is_err());
        drop(held);
        let held = private_browser_dir(&root.path().join("fresh")).unwrap();
        held.hold();
        drop(held);
        let file = child.with_extension("txt");
        std::fs::write(&file, "plain").unwrap();
        assert!(private_browser_dir(&file).is_err());
    }
}

/// This process's token's facts.
fn own_token() -> Option<(HANDLE, Owned)> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: our own process; the token is closed by `Owned`.
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
    }
    Owned::new(token).map(|o| (o.0, o))
}

/// This program runs raised through Windows' administrator prompt.
pub fn elevated() -> bool {
    let Some((token, _owned)) = own_token() else {
        return false;
    };
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut len = 0u32;
    // SAFETY: a valid token; the buffer fits the class.
    unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        ) != 0
            && elevation.TokenIsElevated != 0
    }
}

/// The signed-in user (this program's) is an administrator: Windows gives
/// them a filtered token and raises it on approval (or this one is raised).
pub fn user_is_admin() -> bool {
    let Some((token, _owned)) = own_token() else {
        return true;
    };
    let mut kind = 0i32;
    let mut len = 0u32;
    // SAFETY: a valid token; the buffer fits the class.
    let typed = unsafe {
        GetTokenInformation(
            token,
            TokenElevationType,
            (&mut kind as *mut i32).cast(),
            4,
            &mut len,
        ) != 0
    };
    if typed && (kind == TokenElevationTypeLimited || kind == TokenElevationTypeFull) {
        return true;
    }
    // No split token (a standard account, or UAC off): a member or not.
    let mut admins = [0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut size = admins.len() as u32;
    let mut member = 0;
    // SAFETY: a buffer of the size given; NULL token: this thread's.
    unsafe {
        CreateWellKnownSid(
            WinBuiltinAdministratorsSid,
            std::ptr::null_mut(),
            admins.as_mut_ptr().cast(),
            &mut size,
        ) != 0
            && CheckTokenMembership(
                std::ptr::null_mut(),
                admins.as_mut_ptr().cast(),
                &mut member,
            ) != 0
            && member != 0
    }
}

/// Windows' User Account Control settings.
pub fn uac() -> Uac {
    const KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System";
    let read = |name| machine_number(KEY, name);
    let (a, b, c, d) = (
        read("EnableLUA"),
        read("PromptOnSecureDesktop"),
        read("ConsentPromptBehaviorAdmin"),
        read("ConsentPromptBehaviorUser"),
    );
    let unreadable = a.is_err() || b.is_err() || c.is_err() || d.is_err();
    Uac {
        enable_lua: a.ok().flatten(),
        secure_desktop: b.ok().flatten(),
        admin_prompt: c.ok().flatten(),
        user_prompt: d.ok().flatten(),
        unreadable,
    }
}

// ---- the service, as others see it ------------------------------------------

/// The guard's service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    /// Not installed.
    Missing,
    /// Installed, not running (starting or stopping counts as not).
    Down,
    Running {
        pid: u32,
    },
    /// Windows wouldn't say (counts as installed).
    Unknown,
}

pub fn service() -> Service {
    let name = wide(SERVICE_NAME);
    // SAFETY: plain calls; handles closed by `Sc`.
    unsafe {
        let Ok(scm) = Sc::new(OpenSCManagerW(
            std::ptr::null(),
            std::ptr::null(),
            SC_MANAGER_CONNECT,
        )) else {
            return Service::Unknown;
        };
        let svc = OpenServiceW(scm.0, name.as_ptr(), SERVICE_QUERY_STATUS);
        if svc.is_null() {
            return if GetLastError() == ERROR_SERVICE_DOES_NOT_EXIST {
                Service::Missing
            } else {
                Service::Unknown
            };
        }
        let svc = Sc(svc);
        let mut status: SERVICE_STATUS_PROCESS = std::mem::zeroed();
        let mut needed = 0u32;
        if QueryServiceStatusEx(
            svc.0,
            SC_STATUS_PROCESS_INFO,
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
            std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut needed,
        ) == 0
        {
            return Service::Unknown;
        }
        if status.dwCurrentState == SERVICE_RUNNING && status.dwProcessId != 0 {
            Service::Running {
                pid: status.dwProcessId,
            }
        } else {
            Service::Down
        }
    }
}

/// Installed (or Windows won't say: counted as installed).
pub fn installed() -> bool {
    service() != Service::Missing
}

/// The program the service runs (the protected copy): the one to raise for
/// a Resume, never the user's own copy of Tessera.
pub fn service_program() -> io::Result<PathBuf> {
    let name = wide(SERVICE_NAME);
    // SAFETY: plain calls; the buffer is sized as Windows asks.
    unsafe {
        let scm = Sc::new(OpenSCManagerW(
            std::ptr::null(),
            std::ptr::null(),
            SC_MANAGER_CONNECT,
        ))?;
        let svc = Sc::new(OpenServiceW(scm.0, name.as_ptr(), SERVICE_QUERY_CONFIG))?;
        let mut needed = 0u32;
        QueryServiceConfigW(svc.0, std::ptr::null_mut(), 0, &mut needed);
        let mut buf = vec![0u64; (needed as usize).div_ceil(8).max(1)];
        if QueryServiceConfigW(svc.0, buf.as_mut_ptr().cast(), needed, &mut needed) == 0 {
            return Err(io::Error::last_os_error());
        }
        let config = &*(buf.as_ptr() as *const QUERY_SERVICE_CONFIGW);
        let mut len = 0;
        while *config.lpBinaryPathName.add(len) != 0 {
            len += 1;
        }
        let line =
            String::from_utf16_lossy(std::slice::from_raw_parts(config.lpBinaryPathName, len));
        let program = crate::platform::split_windows_args(&line)
            .into_iter()
            .next()
            .ok_or_else(|| io::Error::other("the guard's service names no program"))?;
        Ok(PathBuf::from(program))
    }
}

// ---- asking the guard ----------------------------------------------------------

/// Run one overlapped read or write on `h`, waiting until `deadline`.
fn overlapped(
    h: HANDLE,
    deadline: Instant,
    start: impl FnOnce(*mut OVERLAPPED) -> i32,
) -> io::Result<u32> {
    // SAFETY: a manual-reset event closed by `Owned`; the OVERLAPPED lives
    // until the operation has finished (waited for, also when cancelled).
    unsafe {
        let event = Owned::new(CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()))
            .ok_or_else(io::Error::last_os_error)?;
        let mut ov: OVERLAPPED = std::mem::zeroed();
        ov.hEvent = event.0;
        let mut n = 0u32;
        if start(&mut ov) == 0 {
            let e = GetLastError();
            if e != ERROR_IO_PENDING {
                return Err(os_error(e));
            }
            let left = deadline
                .saturating_duration_since(Instant::now())
                .as_millis();
            if WaitForSingleObject(event.0, left.min(u128::from(u32::MAX - 1)) as u32)
                != WAIT_OBJECT_0
            {
                CancelIoEx(h, &ov);
                GetOverlappedResult(h, &ov, &mut n, 1);
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Tessera Guard didn't answer in time",
                ));
            }
        }
        if GetOverlappedResult(h, &ov, &mut n, 0) == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(n)
    }
}

fn write_all(h: HANDLE, data: &[u8], deadline: Instant) -> io::Result<()> {
    let mut done = 0;
    while done < data.len() {
        let rest = &data[done..];
        // SAFETY: `rest` outlives the operation (waited for in `overlapped`).
        let n = overlapped(h, deadline, |ov| unsafe {
            WriteFile(
                h,
                rest.as_ptr(),
                rest.len() as u32,
                std::ptr::null_mut(),
                ov,
            )
        })?;
        if n == 0 {
            return Err(io::Error::other("the pipe took nothing"));
        }
        done += n as usize;
    }
    Ok(())
}

/// One line (without its newline), at most [`MAX_MESSAGE`] bytes.
fn read_line(h: HANDLE, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        // SAFETY: `buf` outlives the operation (waited for in `overlapped`).
        let n = match overlapped(h, deadline, |ov| unsafe {
            ReadFile(
                h,
                buf.as_mut_ptr(),
                buf.len() as u32,
                std::ptr::null_mut(),
                ov,
            )
        }) {
            Ok(n) => n as usize,
            Err(e) if e.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) => 0,
            Err(e) => return Err(e),
        };
        if n == 0 {
            return Ok(out);
        }
        out.extend_from_slice(&buf[..n]);
        if let Some(i) = out.iter().position(|b| *b == b'\n') {
            out.truncate(i);
            return Ok(out);
        }
        if out.len() > MAX_MESSAGE {
            return Err(io::Error::other("the message is too long"));
        }
    }
}

/// Ask the guard. Its own pipe only (the service's process answers), and
/// at identification level (it can't act as this program).
pub fn ask(request: &Request, timeout: Duration) -> io::Result<Reply> {
    let Service::Running { pid } = service() else {
        return Err(io::Error::new(
            io::ErrorKind::NotConnected,
            "Tessera Guard isn't running",
        ));
    };
    let deadline = Instant::now() + timeout;
    let name = wide(PIPE_NAME);
    let pipe = loop {
        // SAFETY: a NUL-terminated name; the handle is owned.
        let h = unsafe {
            CreateFileW(
                name.as_ptr(),
                // Exactly what the pipe grants users (generic rights would ask
                // for more).
                FILE_READ_DATA | FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                std::ptr::null_mut(),
            )
        };
        if let Some(h) = Owned::new(h) {
            break h;
        }
        // SAFETY: plain calls.
        let e = unsafe { GetLastError() };
        let left = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u32;
        if e == ERROR_PIPE_BUSY && left > 0 {
            // SAFETY: as above.
            unsafe {
                WaitNamedPipeW(name.as_ptr(), left.min(1000));
            }
            continue;
        }
        return Err(os_error(e));
    };
    let mut server = 0u32;
    // SAFETY: a valid pipe handle.
    if unsafe { GetNamedPipeServerProcessId(pipe.0, &mut server) } == 0 || server != pid {
        return Err(io::Error::other(
            "something other than Tessera Guard answers on its pipe",
        ));
    }
    let mut text = serde_json::to_vec(request).map_err(io::Error::other)?;
    text.push(b'\n');
    write_all(pipe.0, &text, deadline)?;
    let line = read_line(pipe.0, deadline)?;
    serde_json::from_slice(&line).map_err(io::Error::other)
}

/// The guard's Stop, `None` when it can't be asked (not running, or not
/// answering).
pub fn status(timeout: Duration) -> Option<State> {
    let r = ask(&Request::Status, timeout).ok()?;
    r.ok.then_some(State {
        stopped: r.stopped,
        stops: r.stops,
        generation: r.generation,
    })
}

/// Run `program` with `params` raised: Windows asks (UAC, on the secure
/// desktop). Its exit code once it ends (within `wait`); `PermissionDenied`
/// when the user didn't approve.
pub fn run_elevated(program: &Path, params: &str, wait: Duration) -> io::Result<u32> {
    let (verb, file, params) = (wide("runas"), wide(program), wide(params));
    // SAFETY: a zeroed struct of the size Windows expects, with
    // NUL-terminated strings that outlive the call; the process handle is
    // owned.
    unsafe {
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = params.as_ptr();
        info.nShow = SW_HIDE;
        if ShellExecuteExW(&mut info) == 0 {
            let e = GetLastError();
            return Err(if e == ERROR_CANCELLED {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "it wasn't approved in Windows' administrator prompt",
                )
            } else {
                os_error(e)
            });
        }
        let process =
            Owned::new(info.hProcess).ok_or_else(|| io::Error::other("Windows started nothing"))?;
        if WaitForSingleObject(
            process.0,
            wait.as_millis().min(u128::from(u32::MAX - 1)) as u32,
        ) != WAIT_OBJECT_0
        {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the approved step didn't finish in time",
            ));
        }
        let mut code = 0u32;
        GetExitCodeProcess(process.0, &mut code);
        Ok(code)
    }
}

// ---- the service ---------------------------------------------------------------

/// The service's own state: the Stop, and the machines it holds.
struct Guard {
    kept: Mutex<Kept>,
    adopted: Mutex<Vec<Arc<Adopted>>>,
    /// Its signing key (PKCS#8) and public part (hex), the one its record
    /// names; `None`: it signs nothing (so nothing runs where it's needed).
    secret: Option<(Vec<u8>, String)>,
}

/// The Stop, whether it is saved, and the moment it began (the machines get
/// a moment to stop by themselves before the guard ends them).
struct Kept {
    state: State,
    /// Not saved yet: tried again each second until it is (it holds all the
    /// same).
    unsaved: bool,
    /// A stand-in Stop file that couldn't be removed after a save: tried
    /// again each second.
    pending_left: bool,
    /// The Stop as it is now is kept nowhere (the registry refused it and
    /// the stand-in file couldn't be written): the guard signs nothing until
    /// one of them has it. A number signed only from memory could be given
    /// out again, the other way, after a restart.
    undurable: bool,
    /// A Stop whose programs are still to be ended: when the earliest such
    /// Stop came, and the newest one's number. Ended at the deadline even if
    /// resumed meanwhile (a Resume lets new work in, never saves old work).
    ending: Option<(Instant, u64)>,
}

/// A process the guard took in (a machine, or agent work Tessera started),
/// with its job.
struct Adopted {
    pid: u32,
    /// Who asked (the machine itself for a machine).
    parent: u32,
    process: Owned,
    job: Owned,
    session: u32,
    /// Tessera's machine itself (when it ends, keys and buttons it may have
    /// held are let go in its session).
    machine: bool,
    /// The guard's number when it was taken in: a Stop numbered above it
    /// ends it.
    taken_at: u64,
}

/// The most processes held at once; more are refused (and refused agent
/// work isn't started).
const MAX_ADOPTED: usize = 512;

/// Answers at once (each ends within seconds); more wait their turn.
const MAX_ACTIVE: usize = 16;

/// The moment machines get to stop by themselves (letting go of keys and
/// buttons) before the guard ends them.
const GRACE: Duration = Duration::from_secs(2);

fn guard() -> &'static Guard {
    static G: OnceLock<Guard> = OnceLock::new();
    G.get_or_init(|| {
        let saved = read_saved();
        let pending = pending_stop();
        let (secret, renumbered) = signing_key(saved.as_ref(), &pending);
        let _ = BOUND.set(secret.as_ref().map(|(_, public)| public.clone()));
        if let Err(e) = &pending {
            tracing::warn!("the guard's stand-in Stop file can't be read ({e}): stopped");
        }
        let (state, changed) = recover(saved, pending, bound().as_deref(), renumbered);
        if state.stopped && changed {
            tracing::warn!("Tessera Guard starts stopped (Stop {})", state.stops);
        }
        Guard {
            kept: Mutex::new(Kept {
                state,
                // Merged from a stand-in file, or numbered anew: saved to the
                // registry (naming its key, the file then removed) by the
                // enforcer.
                // Every start: saved (to the disk itself) before anything is
                // signed. What was read may be a value Windows hadn't
                // written out yet (a flush that failed before a restart).
                unsaved: true,
                pending_left: false,
                undurable: true,
                // Machines that start now while stopped wait stopped; only a
                // new Stop ends them.
                ending: None,
            }),
            adopted: Mutex::new(Vec::new()),
            secret,
        }
    })
}

fn kept() -> std::sync::MutexGuard<'static, Kept> {
    guard().kept.lock().unwrap_or_else(|e| e.into_inner())
}

/// The public key the guard's numbers belong to (set once, at its start;
/// `None` outside the service, or when it signs nothing).
static BOUND: OnceLock<Option<String>> = OnceLock::new();

fn bound() -> Option<String> {
    BOUND.get().cloned().flatten()
}

/// The Stop's record as kept in the registry: one value, written whole.
fn read_saved() -> Option<Saved> {
    machine_text(STATE_KEY, "State").and_then(|text| serde_json::from_str(&text).ok())
}

/// The signing key for the record `saved`, and whether the numbers start
/// again under a new key. A key is kept only with the record that names it;
/// otherwise (the record lost or unreadable, written while the guard signed
/// nothing, or naming another key) it is deleted and a new one made, which
/// no server has pinned: they refuse this computer's machine until it is
/// added again. If it can't be deleted, the guard signs nothing.
fn signing_key(
    saved: Option<&Saved>,
    pending: &io::Result<Option<Saved>>,
) -> (Option<(Vec<u8>, String)>, bool) {
    match read_secret() {
        Ok(Some(k)) if super::keep_key(saved, pending, &k.1) => (Some(k), false),
        // None yet (a new install): nothing was ever signed.
        Ok(None) => (make_secret(), false),
        found => {
            if let Err(e) = &found {
                tracing::warn!("Tessera Guard's signing key can't be read: {e}");
            }
            match forget_secret() {
                Ok(()) => {
                    tracing::warn!(
                        "Tessera Guard's Stop record doesn't belong to its signing key: it starts stopped with a new key (remove this computer's machine in Settings → Devices and add it again)"
                    );
                    (make_secret(), true)
                }
                Err(e) => {
                    no_key(format!("the old one couldn't be removed: {e}"));
                    (None, true)
                }
            }
        }
    }
}

/// Delete the signing key (a new one is made when next needed): numbers
/// start again only with a key no server has pinned. Administrators may
/// delete it, never read it.
fn forget_secret() -> io::Result<()> {
    use windows_sys::Win32::System::Registry::{KEY_WOW64_64KEY, RegDeleteKeyExW};
    let secret = wide(super::SECRET_KEY);
    // SAFETY: a NUL-terminated key name.
    let r = unsafe { RegDeleteKeyExW(HKEY_LOCAL_MACHINE, secret.as_ptr(), KEY_WOW64_64KEY, 0) };
    if r != NO_ERROR && r != ERROR_FILE_NOT_FOUND {
        return Err(os_error(r));
    }
    Ok(())
}

/// Log lines noted under the Stop's lock, written by the enforcer once it
/// is let go (writing a log can wait; the lock must not).
static LATER: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn say_later(line: String) {
    let mut later = LATER.lock().unwrap_or_else(|e| e.into_inner());
    // Bounded: a few lines say it.
    if later.len() < 32 {
        later.push(line);
    }
}

/// Write what was noted under the Stop's lock (never called under it).
fn say_now() {
    let lines = std::mem::take(&mut *LATER.lock().unwrap_or_else(|e| e.into_inner()));
    for line in lines {
        tracing::warn!("{line}");
    }
}

/// Where a Stop goes when the registry won't take it: a file in the guard's
/// own folder (only SYSTEM and administrators may write there).
fn pending_file() -> io::Result<PathBuf> {
    Ok(guard_dir()?.join("stop-pending.json"))
}

/// The registry has the Stop as it is (saved just now): a stand-in beside
/// the program, newer or torn, would only stop again at the next start. What
/// can't be removed is tried again each second and said once (if it stays
/// and can't be read, the next start stops again: never the other way).
fn retire_pending(k: &mut Kept) {
    let gone = match pending_file().and_then(std::fs::remove_file) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    };
    match gone {
        Ok(()) => k.pending_left = false,
        Err(e) => {
            if !k.pending_left {
                say_later(format!(
                    "the guard's stand-in Stop file couldn't be removed ({e}); tried again each second"
                ));
            }
            k.pending_left = true;
        }
    }
}

/// The Stop kept in [`pending_file`], if any; an error when it can't be
/// read (which stops).
fn pending_stop() -> io::Result<Option<Saved>> {
    match std::fs::read_to_string(pending_file()?) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Save the Stop, naming the key its numbers belong to, to the disk itself
/// (the registry otherwise writes it out later: a power cut could lose it).
fn save_state(s: State) -> io::Result<()> {
    let text = serde_json::to_string(&Saved {
        state: s,
        key: bound(),
    })
    .map_err(io::Error::other)?;
    set_machine_text(STATE_KEY, "State", &text)?;
    let key = wide(STATE_KEY);
    let mut k: HKEY = std::ptr::null_mut();
    // SAFETY: a NUL-terminated key name; the key is closed before returning.
    unsafe {
        let r = RegOpenKeyExW(HKEY_LOCAL_MACHINE, key.as_ptr(), 0, KEY_QUERY_VALUE, &mut k);
        if r != NO_ERROR {
            return Err(os_error(r));
        }
        let r = RegFlushKey(k);
        RegCloseKey(k);
        if r != NO_ERROR {
            return Err(os_error(r));
        }
    }
    Ok(())
}

/// Write the text value `name` of HKEY_LOCAL_MACHINE\`key` (one value,
/// written whole).
fn set_machine_text(key: &str, name: &str, text: &str) -> io::Result<()> {
    let (key, name, data) = (wide(key), wide(name), wide(text));
    // SAFETY: a NUL-terminated string of the size given; one value, written
    // as a whole.
    let r = unsafe {
        RegSetKeyValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            name.as_ptr(),
            REG_SZ,
            data.as_ptr().cast(),
            (data.len() * 2) as u32,
        )
    };
    if r != NO_ERROR {
        return Err(os_error(r));
    }
    Ok(())
}

/// Stop (from the pipe or the semaphore): counted, saved (or tried again),
/// and the machines ended after [`GRACE`] by the enforcer.
fn stop_now(at_least: u32) -> Option<String> {
    let said = {
        let mut k = kept();
        stop_in(&mut k, at_least);
        persist(&mut k)
    };
    // Said outside the lock (writing a log can wait).
    if let Some(why) = &said {
        tracing::warn!("{why}");
    }
    said
}

/// A Stop in `k`: counted and numbered anew, and what was taken in before
/// it ended at its deadline. The earliest Stop not yet enforced sets the
/// moment (more Stops never put the ending off); the newest one's number
/// says what it ends.
fn stop_in(k: &mut Kept, at_least: u32) {
    super::stop_ending(&mut k.state, &mut k.ending, at_least);
}

/// Save the Stop as `k` holds it: in the registry (to the disk itself),
/// else in the stand-in file; in neither, it holds in memory and the guard
/// signs nothing until one takes it (tried again each second). What to tell
/// the caller when it isn't in the registry.
fn persist(k: &mut Kept) -> Option<String> {
    let saved = save_state(k.state);
    k.unsaved = saved.is_err();
    let Err(why) = saved else {
        k.undurable = false;
        retire_pending(k);
        return None;
    };
    // Kept beside the program instead, so a restart can't lose it.
    match keep_beside(k.state) {
        Ok(()) => {
            k.undurable = false;
            Some(format!(
                "the Stop holds and is kept in the guard's folder (the registry refused it: {why}; tried again each second)"
            ))
        }
        Err(e) => {
            k.undurable = true;
            Some(format!(
                "the Stop holds, but isn't saved yet (the registry: {why}; the guard's folder: {e}; tried again each second, and the guard signs nothing until it is saved)"
            ))
        }
    }
}

/// Write the Stop to the stand-in file, to the disk itself (a write the
/// system reports done is there after a power cut).
fn keep_beside(state: State) -> io::Result<()> {
    use std::io::Write as _;
    let text = serde_json::to_string(&Saved {
        state,
        key: bound(),
    })
    .map_err(io::Error::other)?;
    let mut file = std::fs::File::create(pending_file()?)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

static STATUS: OnceLock<usize> = OnceLock::new();
static STOP_EVENT: OnceLock<usize> = OnceLock::new();

fn report(state: u32, accepts: u32, wait_ms: u32) {
    let Some(handle) = STATUS.get() else {
        return;
    };
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: accepts,
        dwWin32ExitCode: NO_ERROR,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: wait_ms,
    };
    // SAFETY: the handle Windows gave this service.
    unsafe {
        SetServiceStatus(*handle as SERVICE_STATUS_HANDLE, &status);
    }
}

unsafe extern "system" fn control(
    code: u32,
    _kind: u32,
    _data: *mut core::ffi::c_void,
    _context: *mut core::ffi::c_void,
) -> u32 {
    match code {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            report(SERVICE_STOP_PENDING, 0, 5000);
            if let Some(e) = STOP_EVENT.get() {
                // SAFETY: the service's own event.
                unsafe {
                    SetEvent(*e as HANDLE);
                }
            }
            NO_ERROR
        }
        SERVICE_CONTROL_INTERROGATE => NO_ERROR,
        _ => ERROR_CALL_NOT_IMPLEMENTED,
    }
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut windows_sys::core::PWSTR) {
    let name = wide(SERVICE_NAME);
    // SAFETY: a NUL-terminated name and a handler that lives for ever.
    let handle =
        unsafe { RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(control), std::ptr::null()) };
    if handle.is_null() {
        return;
    }
    let _ = STATUS.set(handle as usize);
    report(SERVICE_START_PENDING, 0, 5000);
    // SAFETY: a manual-reset event, kept for the service's life.
    let stop = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if stop.is_null() {
        report(SERVICE_STOPPED, 0, 0);
        return;
    }
    let _ = STOP_EVENT.set(stop as usize);
    // Read the Stop before answering anyone; its signing key made once.
    let _ = guard();
    if secret().is_none() {
        tracing::error!(
            "Tessera Guard has no signing key: servers refuse actions on this computer's machine"
        );
    }
    let semaphore = match stop_semaphore() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Tessera Guard couldn't make its Stop semaphore: {e}");
            report(SERVICE_STOPPED, 0, 0);
            return;
        }
    };
    let stop_at = stop as usize;
    let enforcer = std::thread::spawn(move || enforce(semaphore, stop_at as HANDLE));
    report(
        SERVICE_RUNNING,
        SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN,
        0,
    );
    if let Err(e) = serve(stop) {
        tracing::error!("Tessera Guard stopped answering: {e}");
        // SAFETY: the service's own event.
        unsafe {
            SetEvent(stop);
        }
    }
    let _ = enforcer.join();
    // The machines it holds end with it (their jobs close): never left
    // running without the guard.
    report(SERVICE_STOPPED, 0, 0);
}

/// The semaphore anyone signed in here may release to stop (see
/// [`super::STOP_SEMAPHORE`]).
fn stop_semaphore() -> io::Result<Owned> {
    use std::hash::{BuildHasher, Hasher};
    let sd = Sd::new(super::STOP_SEMAPHORE_SDDL)?;
    let sa = sd.attributes();
    // A name nobody can know before it exists (anyone may make objects
    // under Global\, so a fixed name could be taken first); never one
    // that is already there. Published where only SYSTEM and administrators
    // write.
    for _ in 0..8 {
        let id = std::hash::RandomState::new().build_hasher().finish();
        let text = format!("{}-{id:016x}", super::STOP_SEMAPHORE);
        let name = wide(&text);
        // SAFETY: valid descriptor and name; the handle is owned.
        let h = unsafe { CreateSemaphoreW(&sa, 0, 1000, name.as_ptr()) };
        // SAFETY: plain call, right after the one it reports on.
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let Some(h) = Owned::new(h) else {
            continue;
        };
        if existed {
            continue;
        }
        set_machine_text(STATE_KEY, "StopName", &text)?;
        return Ok(h);
    }
    Err(io::Error::other("no unused name for the Stop semaphore"))
}

/// Each moment: Stops released on the semaphore (all at once count as
/// one), a Stop not saved yet, and machines to end once a Stop's moment
/// has passed.
fn enforce(semaphore: Owned, stop: HANDLE) {
    let handles = [stop, semaphore.0];
    let mut retried: Option<Instant> = None;
    loop {
        // SAFETY: two valid handles.
        let r = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, 250) };
        if r == WAIT_OBJECT_0 {
            return;
        }
        if r == WAIT_OBJECT_0 + 1 {
            // What is waiting now counts as one Stop (a bounded look: more
            // releases meanwhile are the next moment's).
            for _ in 0..1000 {
                // SAFETY: as above.
                if unsafe { WaitForSingleObject(semaphore.0, 0) } != WAIT_OBJECT_0 {
                    break;
                }
            }
            stop_now(0);
        }
        // The deadline first (saving may take a while): due whether or not
        // resumed since, what ran when the user stopped ends.
        let end = {
            let mut k = kept();
            let due = k
                .ending
                .filter(|(t, _)| t.elapsed() >= GRACE)
                .map(|(_, number)| number);
            if due.is_some() {
                k.ending = None;
            }
            due
        };
        if let Some(number) = end {
            end_all(number);
        }
        // Saving again, once a second at most, and not while an ending is
        // pending (a slow save could hold the lock past its deadline).
        if retried.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
            retried = Some(Instant::now());
            let mut k = kept();
            if k.ending.is_some() {
                // Next time.
            } else if k.unsaved {
                k.unsaved = save_state(k.state).is_err();
                if !k.unsaved {
                    // In the registry now: the stand-in goes.
                    k.undurable = false;
                    retire_pending(&mut k);
                } else if k.undurable && keep_beside(k.state).is_ok() {
                    // Not in the registry yet, but kept: it may be signed.
                    k.undurable = false;
                }
            } else if k.pending_left {
                retire_pending(&mut k);
            }
        }
        say_now();
        prune();
    }
}

/// Stop through the semaphore (anyone signed in here): whether it was
/// released (or already full, which is a Stop waiting all the same).
pub fn signal_stop() -> bool {
    // Its name as the guard published it (only SYSTEM and administrators
    // write there).
    let Some(text) =
        machine_text(STATE_KEY, "StopName").filter(|t| t.starts_with(super::STOP_SEMAPHORE))
    else {
        return false;
    };
    let name = wide(&text);
    // SAFETY: plain calls; the handle is owned.
    unsafe {
        let Some(s) = Owned::new(OpenSemaphoreW(SEMAPHORE_MODIFY_STATE, 0, name.as_ptr())) else {
            return false;
        };
        ReleaseSemaphore(s.0, 1, std::ptr::null_mut()) != 0
            || GetLastError() == ERROR_TOO_MANY_POSTS
    }
}

/// Run as the Windows service (`tessera guard service`); returns when
/// Windows stops it.
pub fn run_service() -> io::Result<()> {
    let mut name = wide(SERVICE_NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_mut_ptr(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: std::ptr::null_mut(),
            lpServiceProc: None,
        },
    ];
    // SAFETY: a table ending in an empty entry; it lives until this returns.
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// A new instance of the pipe (`first`: its very first, which fails when
/// another program holds the name).
fn new_instance(sd: &Sd, first: bool) -> io::Result<Owned> {
    let sa = sd.attributes();
    let name = wide(PIPE_NAME);
    let mode = PIPE_ACCESS_DUPLEX
        | FILE_FLAG_OVERLAPPED
        | if first {
            FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            0
        };
    // SAFETY: a NUL-terminated name and a valid descriptor.
    let pipe = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            MAX_MESSAGE as u32,
            MAX_MESSAGE as u32,
            0,
            &sa,
        )
    };
    Owned::new(pipe).ok_or_else(io::Error::last_os_error)
}

/// Answer on the pipe until `stop` is set. One instance always listens
/// (the next is made before a connected one is handed over), so the name
/// is never free for another program; users may not make instances.
fn serve(stop: HANDLE) -> io::Result<()> {
    let sd = Sd::new(PIPE_SDDL)?;
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut listening = new_instance(&sd, true)
        .map_err(|e| io::Error::other(format!("another program holds the guard's pipe ({e})")))?;
    loop {
        if !wait_for_client(&listening, stop)? {
            return Ok(());
        }
        // Its turn when one ends (each ends within seconds).
        while active.load(std::sync::atomic::Ordering::SeqCst) >= MAX_ACTIVE {
            // SAFETY: the service's own event.
            if unsafe { WaitForSingleObject(stop, 50) } == WAIT_OBJECT_0 {
                return Ok(());
            }
        }
        let next = loop {
            match new_instance(&sd, false) {
                Ok(p) => break p,
                Err(_) => {
                    // SAFETY: as above.
                    if unsafe { WaitForSingleObject(stop, 100) } == WAIT_OBJECT_0 {
                        return Ok(());
                    }
                }
            }
        };
        let connected = std::mem::replace(&mut listening, next);
        active.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let active = active.clone();
        std::thread::spawn(move || {
            answer(&connected);
            // SAFETY: the connected pipe this thread owns.
            unsafe {
                DisconnectNamedPipe(connected.0);
            }
            active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        });
    }
}

/// A client connected (`true`), or the service is stopping (`false`).
fn wait_for_client(pipe: &Owned, stop: HANDLE) -> io::Result<bool> {
    // SAFETY: an event and OVERLAPPED that outlive the operation (it is
    // waited for or cancelled and waited for below).
    unsafe {
        let event = Owned::new(CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()))
            .ok_or_else(io::Error::last_os_error)?;
        let mut ov: OVERLAPPED = std::mem::zeroed();
        ov.hEvent = event.0;
        if ConnectNamedPipe(pipe.0, &mut ov) == 0 {
            match GetLastError() {
                ERROR_PIPE_CONNECTED => return Ok(true),
                ERROR_IO_PENDING => {}
                e => return Err(os_error(e)),
            }
        } else {
            return Ok(true);
        }
        let handles = [event.0, stop];
        let r = WaitForMultipleObjects(2, handles.as_ptr(), 0, u32::MAX);
        if r == WAIT_OBJECT_0 {
            let mut n = 0u32;
            return Ok(GetOverlappedResult(pipe.0, &ov, &mut n, 0) != 0);
        }
        CancelIoEx(pipe.0, &ov);
        let mut n = 0u32;
        GetOverlappedResult(pipe.0, &ov, &mut n, 1);
        Ok(false)
    }
}

/// Who is on the other end, as Windows says: the token's facts, its
/// process (held from now on, so its id can't come to name another), its
/// session and the user signed in to that session.
struct Caller {
    client: Client,
    pid: u32,
    session: u32,
    sid: Vec<u8>,
    process: Option<Owned>,
    session_user: Option<Vec<u8>>,
}

fn identify(pipe: &Owned) -> Caller {
    let (mut pid, mut session) = (0u32, 0u32);
    // SAFETY: a connected pipe; the thread goes back to its own identity at
    // once, before anything else is done; tokens and the process are owned.
    unsafe {
        GetNamedPipeClientProcessId(pipe.0, &mut pid);
        GetNamedPipeClientSessionId(pipe.0, &mut session);
        let process = Owned::new(OpenProcess(
            PROCESS_SET_QUOTA
                | PROCESS_TERMINATE
                | PROCESS_QUERY_LIMITED_INFORMATION
                | PROCESS_SYNCHRONIZE,
            0,
            pid,
        ));
        let session_user = {
            let mut t: HANDLE = std::ptr::null_mut();
            (WTSQueryUserToken(session, &mut t) != 0)
                .then(|| Owned::new(t))
                .flatten()
                .and_then(|t| token_facts(t.0))
                .map(|f| f.sid)
        };
        let unknown = |process| Caller {
            client: Client::default(),
            pid,
            session,
            sid: Vec::new(),
            process,
            session_user: None,
        };
        if ImpersonateNamedPipeClient(pipe.0) == 0 {
            return unknown(process);
        }
        let mut token: HANDLE = std::ptr::null_mut();
        let opened = OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) != 0;
        if RevertToSelf() == 0 {
            // Can't be itself again: this thread does nothing more.
            std::process::abort();
        }
        let Some(token) = opened.then(|| Owned::new(token)).flatten() else {
            return unknown(process);
        };
        match token_facts(token.0) {
            Some(f) => Caller {
                client: Client {
                    known: true,
                    elevated: f.elevated,
                    admin: f.admin,
                    integrity: f.integrity,
                },
                pid,
                session,
                sid: f.sid,
                process,
                session_user,
            },
            None => unknown(process),
        }
    }
}

/// Wait (a moment at most) for the client to close its end once it has the
/// answer, so it is never cut off; a client that doesn't is left.
fn wait_closed(pipe: &Owned, deadline: Instant) {
    let mut byte = [0u8; 1];
    // SAFETY: `byte` outlives the operation (waited for in `overlapped`).
    let _ = overlapped(pipe.0, deadline, |ov| unsafe {
        ReadFile(pipe.0, byte.as_mut_ptr(), 1, std::ptr::null_mut(), ov)
    });
}

fn answer(pipe: &Owned) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let reply = match read_line(pipe.0, deadline) {
        Ok(line) => match serde_json::from_slice::<Request>(&line) {
            Ok(request) => {
                let caller = identify(pipe);
                match super::authorize(&request, &caller.client) {
                    Ok(()) => act(request, caller),
                    Err(why) => refused(why.into()),
                }
            }
            Err(_) => refused("that isn't a request Tessera Guard knows".into()),
        },
        Err(_) => return,
    };
    if let Ok(mut text) = serde_json::to_vec(&reply) {
        text.push(b'\n');
        if write_all(pipe.0, &text, Instant::now() + Duration::from_secs(3)).is_ok() {
            wait_closed(pipe, Instant::now() + Duration::from_secs(2));
        }
    }
}

fn reply(error: Option<String>) -> Reply {
    let s = kept().state;
    Reply {
        ok: error.is_none(),
        stopped: s.stopped,
        stops: s.stops,
        generation: s.generation,
        key: secret().map(|(_, k)| k.clone()),
        signature: None,
        error,
    }
}

/// The guard's signing key (PKCS#8) and its public part (hex): made once,
/// kept in a registry key only SYSTEM may read. `None` when it can't be.
fn secret() -> Option<&'static (Vec<u8>, String)> {
    guard().secret.as_ref()
}

/// Set the signing key's access list to [`super::SECRET_SDDL`].
fn renew_secret_acl(key: &[u16]) -> io::Result<()> {
    let sd = Sd::new(super::SECRET_SDDL)?;
    let mut k: HKEY = std::ptr::null_mut();
    // SAFETY: a NUL-terminated key name and a valid descriptor; the key is
    // closed before returning.
    unsafe {
        let r = RegOpenKeyExW(HKEY_LOCAL_MACHINE, key.as_ptr(), 0, WRITE_DAC, &mut k);
        if r != NO_ERROR {
            return Err(io::Error::from_raw_os_error(r as i32));
        }
        let r = RegSetKeySecurity(
            k,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            sd.0,
        );
        RegCloseKey(k);
        if r != NO_ERROR {
            return Err(io::Error::from_raw_os_error(r as i32));
        }
    }
    Ok(())
}

/// The signing key kept from before, if any (an error: there, but not
/// readable or not a key).
fn read_secret() -> io::Result<Option<(Vec<u8>, String)>> {
    use windows_sys::Win32::System::Registry::RRF_RT_REG_BINARY;
    let (key, name) = (wide(super::SECRET_KEY), wide("Key"));
    let mut buf = vec![0u8; 256];
    let mut bytes = buf.len() as u32;
    // SAFETY: `buf` holds `bytes` bytes.
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_BINARY,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if r == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if r != NO_ERROR {
        return Err(os_error(r));
    }
    buf.truncate(bytes as usize);
    let public = tessera_secure::sign::public_of(&buf)
        .ok_or_else(|| io::Error::other("it isn't a signing key"))?;
    // Kept from before: its access list as it must be now (SYSTEM alone
    // reads it; Administrators may delete it). The key is used either way;
    // a failure is said (uninstall may then need SYSTEM to remove it).
    if let Err(why) = renew_secret_acl(&key) {
        tracing::warn!("the signing key's access list couldn't be renewed: {why}");
    }
    Ok(Some((buf, super::hex(&public))))
}

/// A new signing key, kept where only SYSTEM may read it; `None` (said in
/// the log, with why) when it can't be.
fn make_secret() -> Option<(Vec<u8>, String)> {
    match new_secret() {
        Ok(k) => Some(k),
        Err(e) => {
            no_key(format!("a new one couldn't be made: {e}"));
            None
        }
    }
}

/// Why the guard has no signing key (said in its refusals, so the user
/// learns it without the log).
static NO_KEY: OnceLock<String> = OnceLock::new();

fn no_key(why: String) {
    tracing::error!(
        "Tessera Guard has no signing key ({why}): it signs nothing until it starts again"
    );
    let _ = NO_KEY.set(why);
}

fn new_secret() -> io::Result<(Vec<u8>, String)> {
    use windows_sys::Win32::System::Registry::REG_BINARY;
    let (key, name) = (wide(super::SECRET_KEY), wide("Key"));
    let (private, public) = tessera_secure::sign::generate()
        .ok_or_else(|| io::Error::other("no key could be generated"))?;
    let sd = Sd::new(super::SECRET_SDDL)?;
    let sa = sd.attributes();
    let mut k: HKEY = std::ptr::null_mut();
    // SAFETY: valid strings and descriptor; the key is closed below; the
    // data is `private`'s bytes.
    unsafe {
        let r = RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_ALL_ACCESS,
            &sa,
            &mut k,
            std::ptr::null_mut(),
        );
        if r != NO_ERROR {
            return Err(os_error(r));
        }
        let r = RegSetKeySecurity(
            k,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            sd.0,
        );
        RegCloseKey(k);
        if r != NO_ERROR {
            return Err(os_error(r));
        }
        let r = RegSetKeyValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            name.as_ptr(),
            REG_BINARY,
            private.as_ptr().cast(),
            private.len() as u32,
        );
        if r != NO_ERROR {
            return Err(os_error(r));
        }
    }
    Ok((private, super::hex(&public)))
}

fn refused(why: String) -> Reply {
    reply(Some(why))
}

fn act(request: Request, caller: Caller) -> Reply {
    let error = match request {
        Request::Status => None,
        Request::Stop { at_least } => stop_now(at_least),
        Request::Resume { generation, key } => {
            // Only the user's own approval: an administrator's account asks
            // to "Always notify" (else a program could approve itself).
            // The session's user unknown: the stronger rule (as for an
            // administrator's own account).
            let same_user = caller
                .session_user
                .as_deref()
                .is_none_or(|u| u == &caller.sid[..]);
            match super::uac_ok(uac(), same_user) {
                Err(why) => Some(why),
                // Approved for the numbers of another key (they started
                // again since): it names nothing here now.
                Ok(()) if key.as_deref() != secret().map(|(_, public)| public.as_str()) => Some(
                    "Tessera Guard started again with a new key since this was asked: press Resume again"
                        .into(),
                ),
                Ok(()) => {
                    let mut k = kept();
                    let before = k.state;
                    match k.state.resume(generation) {
                        // Nothing to resume: nothing to save either.
                        Ok(()) if !before.stopped => {
                            k.state = before;
                            None
                        }
                        Ok(()) => match save_state(k.state) {
                            Ok(()) => {
                                k.unsaved = false;
                                k.undurable = false;
                                retire_pending(&mut k);
                                None
                            }
                            Err(e) => {
                                // Not resumed unless saved. The registry may
                                // hold the Resume all the same (written, not
                                // on the disk yet): a Stop numbered above it
                                // holds instead, kept like any Stop.
                                let attempted = k.state.generation;
                                k.state = before;
                                k.state.generation = k.state.generation.max(attempted);
                                stop_in(&mut k, 0);
                                let kept_how = persist(&mut k)
                                    .map(|w| format!(" ({w})"))
                                    .unwrap_or_default();
                                Some(format!(
                                    "couldn't save the Resume ({e}): still stopped; press Resume again{kept_how}"
                                ))
                            }
                        },
                        Err(why) => Some(why.into()),
                    }
                }
            }
        }
        // Without its signing key the guard can't vouch for anything to a
        // server: nothing is taken in (so nothing runs there).
        Request::Adopt | Request::AdoptChild { .. } if secret().is_none() => Some(format!(
            "Tessera Guard has no signing key ({}), so it takes nothing in; once that is fixed, restart the computer (or the Tessera Guard service)",
            NO_KEY.get().map_or(
                "its log, guard.log in its folder under Program Files, may say why",
                String::as_str
            )
        )),
        Request::Adopt => adopt(caller)
            .err()
            .map(|e| format!("couldn't take Tessera's machine in: {e}")),
        Request::AdoptChild { pid } => {
            // Decided under the Stop's lock: no new agent program while
            // stopped (one taken in just before a Stop is ended with the
            // rest when it is enforced).
            let k = kept();
            if k.state.stopped {
                Some("Tessera is stopped on this computer, so no agent program starts".into())
            } else {
                adopt_child(pid, &caller, k.state.generation)
                    .err()
                    .map(|e| format!("couldn't take that agent program in: {e}"))
            }
        }
        // Signed together with the very Stop it says (read once).
        Request::Attest { nonce } => {
            let Some((private, public)) = secret() else {
                return refused("Tessera Guard has no signing key".into());
            };
            // Read once, the lock let go before any answer is made (an
            // answer reads the Stop itself).
            let (undurable, s) = {
                let k = kept();
                (k.undurable, k.state)
            };
            if undurable {
                return refused(
                    "Tessera Guard's Stop isn't saved yet, so it signs nothing (it tries again each second)"
                        .into(),
                );
            }
            let Some(sig) = tessera_secure::sign::sign(private, &super::attest_message(&nonce, s))
            else {
                return refused("Tessera Guard couldn't sign".into());
            };
            return Reply {
                ok: true,
                stopped: s.stopped,
                stops: s.stops,
                generation: s.generation,
                key: Some(public.clone()),
                signature: Some(super::hex(&sig)),
                error: None,
            };
        }
    };
    reply(error)
}

/// Take the caller's own process in (the one held since it connected):
/// Tessera's machine.
fn adopt(caller: Caller) -> io::Result<()> {
    let process = caller
        .process
        .as_ref()
        .ok_or_else(|| io::Error::other("its process couldn't be opened"))?;
    // SAFETY: duplicating a handle this owns, for the record.
    let process = unsafe { duplicate(process.0) }?;
    // Numbered and taken in under the Stop's lock (as agent programs are):
    // no Stop's ending can pass between the two.
    {
        let k = kept();
        adopt_process(caller.pid, process, &caller, true, k.state.generation)?;
    }
    tracing::info!("Tessera Guard took in machine process {}", caller.pid);
    Ok(())
}

/// Take in agent work the caller (Tessera's machine or server) just
/// started, before it runs: a process of the caller's user and session at
/// the user's normal level (nothing the caller couldn't end itself).
fn adopt_child(pid: u32, caller: &Caller, at: u64) -> io::Result<()> {
    // Only a program the caller itself started (its parent), and only so
    // many per caller (one caller can't fill the guard).
    if parent_of(pid) != Some(caller.pid) {
        return Err(io::Error::other("it isn't a program the caller started"));
    }
    let mine = guard()
        .adopted
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|a| a.parent == caller.pid)
        .count();
    if mine >= MAX_PER_CALLER {
        return Err(io::Error::other(
            "its caller has as many programs taken in as it may",
        ));
    }
    // SAFETY: plain call; the handle is owned.
    let process = Owned::new(unsafe {
        OpenProcess(
            PROCESS_SET_QUOTA
                | PROCESS_TERMINATE
                | PROCESS_QUERY_LIMITED_INFORMATION
                | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    })
    .ok_or_else(io::Error::last_os_error)?;
    adopt_process(pid, process, caller, false, at)
}

/// The most agent programs one caller may have taken in at once.
const MAX_PER_CALLER: usize = 128;

/// The process that started `pid` (from a snapshot of the system's
/// processes).
fn parent_of(pid: u32) -> Option<u32> {
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    // SAFETY: a snapshot walked with a correctly sized entry; closed by
    // `Owned`.
    unsafe {
        let snap = Owned::new(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0))?;
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snap.0, &mut e) != 0;
        while more {
            if e.th32ProcessID == pid {
                return Some(e.th32ParentProcessID);
            }
            more = Process32NextW(snap.0, &mut e) != 0;
        }
        None
    }
}

/// A handle of this process's own to `h`'s process.
unsafe fn duplicate(h: HANDLE) -> io::Result<Owned> {
    use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle};
    let mut out: HANDLE = std::ptr::null_mut();
    // SAFETY: valid handles of this process.
    let ok = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            h,
            GetCurrentProcess(),
            &mut out,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Owned::new(out).ok_or_else(io::Error::last_os_error)
}

/// Take `process` (id `pid`) in: still running, the caller's user and
/// session, at the user's normal level (not raised). A job nothing can
/// leave, ended on a Stop and when the process ends. Asking again for the
/// same process changes nothing.
fn adopt_process(
    pid: u32,
    process: Owned,
    caller: &Caller,
    machine: bool,
    taken_at: u64,
) -> io::Result<()> {
    // SAFETY: plain calls on handles this owns; buffers sized for each call.
    unsafe {
        if WaitForSingleObject(process.0, 0) != WAIT_TIMEOUT {
            return Err(io::Error::other("it ended before it was taken in"));
        }
        let mut its_session = u32::MAX;
        if ProcessIdToSessionId(pid, &mut its_session) == 0 || its_session != caller.session {
            return Err(io::Error::other("it isn't in the caller's session"));
        }
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(process.0, TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = Owned(token);
        let facts = token_facts(token.0).ok_or_else(|| io::Error::other("its user is unknown"))?;
        if facts.sid.is_empty()
            || caller.sid.is_empty()
            || EqualSid(facts.sid.as_ptr() as PSID, caller.sid.as_ptr() as PSID) == 0
        {
            return Err(io::Error::other("it isn't the caller's user's program"));
        }
        if facts.elevated || facts.integrity.is_none_or(|i| i > super::MEDIUM) {
            return Err(io::Error::other(
                "only programs at the user's normal level are taken in",
            ));
        }
        let mut held = guard().adopted.lock().unwrap_or_else(|e| e.into_inner());
        // The same process (its id can't be another's while it runs).
        if held
            .iter()
            .any(|a| a.pid == pid && WaitForSingleObject(a.process.0, 0) == WAIT_TIMEOUT)
        {
            return Ok(());
        }
        if held.len() >= MAX_ADOPTED {
            return Err(io::Error::other(
                "the guard holds as many programs as it may",
            ));
        }
        let job = Owned::new(CreateJobObjectW(std::ptr::null(), std::ptr::null()))
            .ok_or_else(io::Error::last_os_error)?;
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0
            || AssignProcessToJobObject(job.0, process.0) == 0
        {
            return Err(io::Error::last_os_error());
        }
        held.push(Arc::new(Adopted {
            pid,
            parent: caller.pid,
            process,
            job,
            session: caller.session,
            machine,
            taken_at,
        }));
        Ok(())
    }
}

/// Processes that ended: what they started ends too (their job), and a
/// machine's keys and buttons are let go in its session. Each moment, from
/// the enforcer.
fn prune() {
    let gone: Vec<Arc<Adopted>> = {
        let mut held = guard().adopted.lock().unwrap_or_else(|e| e.into_inner());
        let (gone, kept): (Vec<_>, Vec<_>) = held.drain(..).partition(|a| {
            // SAFETY: a process handle this guard holds.
            unsafe { WaitForSingleObject(a.process.0, 0) != WAIT_TIMEOUT }
        });
        *held = kept;
        gone
    };
    let mut sessions = Vec::new();
    for a in gone {
        // SAFETY: a job this guard holds.
        unsafe {
            TerminateJobObject(a.job.0, 1);
        }
        if a.machine && !sessions.contains(&a.session) {
            sessions.push(a.session);
        }
    }
    for s in sessions {
        release_input_in(s);
    }
}

/// Let go of keys and buttons in session `session` (the machine there was
/// ended, maybe holding some): a small program of the guard's own, started
/// there as its user (the user's normal level), on the visible desktop.
fn release_input_in(session: u32) {
    use windows_sys::Win32::System::Threading::{
        CREATE_NO_WINDOW, CreateProcessAsUserW, PROCESS_INFORMATION, STARTUPINFOW,
    };
    // Once per session within a moment (a Stop and the machine's end both
    // ask): never a second one running late into what comes next.
    {
        static LAST: Mutex<Vec<(u32, Instant)>> = Mutex::new(Vec::new());
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        last.retain(|(_, t)| t.elapsed() < Duration::from_secs(5));
        if last.iter().any(|(s, _)| *s == session) {
            return;
        }
        last.push((session, Instant::now()));
    }
    let Ok(exe) = std::env::current_exe() else {
        tracing::warn!("couldn't let go of input in session {session}: no program path");
        return;
    };
    let Some(job) = helper_job() else {
        tracing::warn!("couldn't let go of input in session {session}: no job for its helper");
        return;
    };
    let mut line = wide(format!("\"{}\" guard release-input", exe.display()));
    let mut desktop = wide("winsta0\\default");
    // SAFETY: a primary token from WTSQueryUserToken (closed by `Owned`),
    // zeroed structs of the sizes Windows expects, NUL-terminated strings
    // that outlive the call; the new process's handles are closed.
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if WTSQueryUserToken(session, &mut token) == 0 {
            tracing::warn!(
                "couldn't let go of input in session {session}: {}",
                io::Error::last_os_error()
            );
            return;
        }
        let Some(token) = Owned::new(token) else {
            return;
        };
        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        si.lpDesktop = desktop.as_mut_ptr();
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        if CreateProcessAsUserW(
            token.0,
            std::ptr::null(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_NO_WINDOW | windows_sys::Win32::System::Threading::CREATE_SUSPENDED,
            std::ptr::null(),
            std::ptr::null(),
            &si,
            &mut pi,
        ) != 0
        {
            let thread = Owned(pi.hThread);
            let process = Owned(pi.hProcess);
            // In the guard's own job before it runs: it can't outlive the
            // guard (the job ends with the service).
            if AssignProcessToJobObject(job, process.0) == 0 {
                let why = io::Error::last_os_error();
                end_helper(&process, session);
                tracing::warn!(
                    "couldn't let go of input in session {session}: its helper couldn't be put in the guard's job ({why})"
                );
                return;
            }
            if windows_sys::Win32::System::Threading::ResumeThread(thread.0) == u32::MAX {
                let why = io::Error::last_os_error();
                end_helper(&process, session);
                tracing::warn!(
                    "couldn't let go of input in session {session}: its helper couldn't start ({why})"
                );
                return;
            }
            drop(thread);
            // Watched: it ends within seconds, or is ended (and said).
            std::thread::spawn(move || {
                // The whole handle moves here (it closes when this ends).
                let process = process;
                if WaitForSingleObject(process.0, 5000) != WAIT_OBJECT_0 {
                    tracing::warn!("letting go of input in session {session} didn't finish");
                    end_helper(&process, session);
                    return;
                }
                let mut code = 0u32;
                if GetExitCodeProcess(process.0, &mut code) == 0 {
                    tracing::warn!(
                        "letting go of input in session {session}: its result can't be read ({})",
                        io::Error::last_os_error()
                    );
                } else if code != 0 {
                    tracing::warn!(
                        "letting go of input in session {session} failed ({code}): Windows refused some of it, or the desktop couldn't be seen"
                    );
                }
            });
        } else {
            tracing::warn!(
                "couldn't let go of input in session {session}: {}",
                io::Error::last_os_error()
            );
        }
    }
}

/// End an input-release helper and wait for its end (said when it can't be
/// ended or doesn't end; its job still ends it with the guard).
fn end_helper(process: &Owned, session: u32) {
    // SAFETY: a process handle the caller owns.
    unsafe {
        if TerminateProcess(process.0, 1) == 0 {
            tracing::warn!(
                "the input-release helper in session {session} couldn't be ended: {}",
                io::Error::last_os_error()
            );
        } else if WaitForSingleObject(process.0, 5000) != WAIT_OBJECT_0 {
            tracing::warn!("the input-release helper in session {session} didn't end when told");
        }
    }
}

/// The job the guard's helpers run in: kill-on-close, held for the
/// service's life (they end with it).
fn helper_job() -> Option<HANDLE> {
    static JOB: OnceLock<Option<usize>> = OnceLock::new();
    JOB.get_or_init(|| {
        // SAFETY: an unnamed job, configured below; its handle is kept for
        // the service's life on purpose.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                CloseHandle(job);
                return None;
            }
            Some(job as usize)
        }
    })
    .map(|j| j as HANDLE)
}

/// The user stopped (the Stop numbered `number`): every program the guard
/// took in before it ends, machines with everything they started.
fn end_all(number: u64) {
    let all: Vec<Arc<Adopted>> = guard()
        .adopted
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|a| a.taken_at < number)
        .cloned()
        .collect();
    let mut sessions = Vec::new();
    for a in all {
        // SAFETY: a job this guard holds.
        unsafe {
            TerminateJobObject(a.job.0, 1);
        }
        if a.machine {
            tracing::info!("Tessera Guard ended machine process {} (stopped)", a.pid);
            if !sessions.contains(&a.session) {
                sessions.push(a.session);
            }
        }
    }
    // Keys and buttons let go at once, not at the machine's next start.
    for s in sessions {
        release_input_in(s);
    }
}

// ---- installing ----------------------------------------------------------------

/// The guard's folder under Program Files (from the system's own setting,
/// not this program's environment).
pub fn guard_dir() -> io::Result<PathBuf> {
    let base = machine_text(
        r"SOFTWARE\Microsoft\Windows\CurrentVersion",
        "ProgramFilesDir",
    )
    .ok_or_else(|| io::Error::other("Windows didn't say where Program Files is"))?;
    Ok(PathBuf::from(base).join(DIR_NAME))
}

fn wait_service(svc: &Sc, want: u32, limit: Duration) -> bool {
    let until = Instant::now() + limit;
    loop {
        // SAFETY: a valid service handle; the buffer fits the class.
        let state = unsafe {
            let mut status: SERVICE_STATUS_PROCESS = std::mem::zeroed();
            let mut needed = 0u32;
            (QueryServiceStatusEx(
                svc.0,
                SC_STATUS_PROCESS_INFO,
                (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
                std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
                &mut needed,
            ) != 0)
                .then_some(status.dwCurrentState)
        };
        if state == Some(want) {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Stop the service and wait (15 s) until Windows says it stopped: an
/// error when it doesn't (nothing of it is changed or removed then).
fn stop_service(svc: &Sc) -> io::Result<()> {
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        let mut status: SERVICE_STATUS = SERVICE_STATUS {
            dwServiceType: 0,
            dwCurrentState: 0,
            dwControlsAccepted: 0,
            dwWin32ExitCode: 0,
            dwServiceSpecificExitCode: 0,
            dwCheckPoint: 0,
            dwWaitHint: 0,
        };
        // SAFETY: a valid service handle.
        let asked = unsafe { ControlService(svc.0, SERVICE_CONTROL_STOP, &mut status) } != 0;
        // SAFETY: plain call.
        let why = if asked {
            NO_ERROR
        } else {
            unsafe { GetLastError() }
        };
        if why == ERROR_SERVICE_NOT_ACTIVE {
            return Ok(());
        }
        // Still starting (or already stopping): it takes no Stop yet; asked
        // again in a moment, within the same time.
        if why == ERROR_SERVICE_CANNOT_ACCEPT_CTRL && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(200));
            continue;
        }
        if wait_service(
            svc,
            SERVICE_STOPPED,
            until.saturating_duration_since(Instant::now()),
        ) {
            return Ok(());
        }
        return Err(io::Error::other(
            "the Tessera Guard service didn't stop within 15 seconds, so this was stopped before its program, settings or Stop were changed: see `tessera guard status`, and try again (or restart the computer first)",
        ));
    }
}

/// Install (or update) the guard: run raised. Copies this program to the
/// guard's protected folder, makes its registry key (with `seed` as the
/// Stop when it has none yet), and sets up and starts the service.
pub fn install(seed: State) -> io::Result<PathBuf> {
    if !elevated() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "installing Tessera Guard takes an administrator's approval",
        ));
    }
    // This program's file, held from now until copied: nothing can change,
    // move or replace it meanwhile (no sharing but reading).
    let here = std::env::current_exe()?;
    let source = open_pinned(&here)?;
    let dir = guard_dir()?;
    let folder = Sd::new(DIR_SDDL)?;
    let dir_w = wide(&dir);
    let sa = folder.attributes();
    not_a_link(&dir)?;
    // SAFETY: a NUL-terminated path and a valid descriptor.
    if unsafe { CreateDirectoryW(dir_w.as_ptr(), &sa) } == 0 {
        // SAFETY: plain call.
        let e = unsafe { GetLastError() };
        if e != ERROR_ALREADY_EXISTS {
            return Err(os_error(e));
        }
    }
    not_a_link(&dir)?;
    // Whatever it had before: only administrators may change it.
    folder.apply_to(&dir)?;
    let exe = dir.join(EXE_NAME);
    let name = wide(SERVICE_NAME);
    // SAFETY: plain calls; handles closed by `Sc`.
    let scm = Sc::new(unsafe {
        OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_ALL_ACCESS)
    })?;
    // SAFETY: as above.
    let existing = unsafe { OpenServiceW(scm.0, name.as_ptr(), SERVICE_ALL_ACCESS) };
    let existing = (!existing.is_null()).then_some(Sc(existing));
    if let Some(svc) = &existing {
        stop_service(svc)?;
    }
    not_a_link(&exe)?;
    let mut copied = Err(io::Error::other("not tried"));
    for _ in 0..20 {
        copied = copy_pinned(&source, &exe);
        if copied.is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    copied.map_err(|e| {
        io::Error::other(format!("couldn't copy Tessera to {}: {e}", exe.display()))
    })?;
    folder.apply_to(&exe)?;
    make_key(seed)?;
    let line = wide(format!("\"{}\" guard service", exe.display()));
    let display = wide(SERVICE_DISPLAY);
    let svc = match existing {
        Some(svc) => {
            // SAFETY: a valid handle; NULL leaves a setting as it is.
            let ok = unsafe {
                ChangeServiceConfigW(
                    svc.0,
                    SERVICE_NO_CHANGE,
                    SERVICE_AUTO_START,
                    SERVICE_NO_CHANGE,
                    line.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    display.as_ptr(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            svc
        }
        // SAFETY: valid strings; NULL account: LocalSystem.
        None => Sc::new(unsafe {
            CreateServiceW(
                scm.0,
                name.as_ptr(),
                display.as_ptr(),
                SERVICE_ALL_ACCESS,
                SERVICE_WIN32_OWN_PROCESS,
                SERVICE_AUTO_START,
                SERVICE_ERROR_NORMAL,
                line.as_ptr(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
            )
        })?,
    };
    let mut about = wide(
        "Keeps your Stop for Tessera's agents where your programs can't undo it without your approval, and ends what Tessera's machine started when you stop it.",
    );
    let description = SERVICE_DESCRIPTIONW {
        lpDescription: about.as_mut_ptr(),
    };
    let mut actions = [
        SC_ACTION {
            Type: SC_ACTION_RESTART,
            Delay: 2000,
        },
        SC_ACTION {
            Type: SC_ACTION_RESTART,
            Delay: 5000,
        },
        SC_ACTION {
            Type: SC_ACTION_RESTART,
            Delay: 30000,
        },
    ];
    let failure = SERVICE_FAILURE_ACTIONSW {
        dwResetPeriod: 86_400,
        lpRebootMsg: std::ptr::null_mut(),
        lpCommand: std::ptr::null_mut(),
        cActions: actions.len() as u32,
        lpsaActions: actions.as_mut_ptr(),
    };
    // SAFETY: valid structs that outlive the calls.
    unsafe {
        ChangeServiceConfig2W(
            svc.0,
            SERVICE_CONFIG_DESCRIPTION,
            (&description as *const SERVICE_DESCRIPTIONW).cast(),
        );
        ChangeServiceConfig2W(
            svc.0,
            SERVICE_CONFIG_FAILURE_ACTIONS,
            (&failure as *const SERVICE_FAILURE_ACTIONSW).cast(),
        );
        if StartServiceW(svc.0, 0, std::ptr::null()) == 0
            && GetLastError() != ERROR_SERVICE_ALREADY_RUNNING
        {
            return Err(io::Error::last_os_error());
        }
    }
    if !wait_service(&svc, SERVICE_RUNNING, Duration::from_secs(15)) {
        return Err(io::Error::other(
            "Tessera Guard was installed but didn't start",
        ));
    }
    Ok(exe)
}

/// `path` opened for reading only, shared only for reading: while held, no
/// program can write, rename or delete it.
fn open_pinned(path: &Path) -> io::Result<Owned> {
    let name = wide(path);
    // SAFETY: a NUL-terminated path; the handle is owned.
    Owned::new(unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_SEQUENTIAL_SCAN,
            std::ptr::null_mut(),
        )
    })
    .ok_or_else(io::Error::last_os_error)
}

/// Refuse a link (or other reparse point) where the guard's folder or
/// program go.
fn not_a_link(path: &Path) -> io::Result<()> {
    let name = wide(path);
    // SAFETY: a NUL-terminated path.
    let attrs = unsafe { GetFileAttributesW(name.as_ptr()) };
    if attrs != INVALID_FILE_ATTRIBUTES && attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::other(format!(
            "{} is a link, so Tessera Guard isn't put there",
            path.display()
        )));
    }
    Ok(())
}

/// Write the bytes of `source` (held, see [`open_pinned`]) to `to`, made
/// anew with the guard folder's access list.
fn copy_pinned(source: &Owned, to: &Path) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{FILE_BEGIN, SetFilePointerEx};
    let file = Sd::new(DIR_SDDL)?;
    let sa = file.attributes();
    let name = wide(to);
    // SAFETY: valid handles, buffers of the sizes given; the new file is
    // owned and closed here.
    unsafe {
        if SetFilePointerEx(source.0, 0, std::ptr::null_mut(), FILE_BEGIN) == 0 {
            return Err(io::Error::last_os_error());
        }
        let out = Owned::new(CreateFileW(
            name.as_ptr(),
            GENERIC_WRITE,
            0,
            &sa,
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        ))
        .ok_or_else(io::Error::last_os_error)?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let mut got = 0u32;
            if ReadFile(
                source.0,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut got,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            if got == 0 {
                break;
            }
            let mut done = 0usize;
            while done < got as usize {
                let mut wrote = 0u32;
                if WriteFile(
                    out.0,
                    buf[done..].as_ptr(),
                    (got as usize - done) as u32,
                    &mut wrote,
                    std::ptr::null_mut(),
                ) == 0
                    || wrote == 0
                {
                    return Err(io::Error::last_os_error());
                }
                done += wrote as usize;
            }
        }
        if FlushFileBuffers(out.0) == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// The registry key, only administrators (and the service) may change;
/// `seed` written when it has no Stop yet.
fn make_key(seed: State) -> io::Result<()> {
    let sd = Sd::new(KEY_SDDL)?;
    let sa = sd.attributes();
    let path = wide(STATE_KEY);
    let mut key: HKEY = std::ptr::null_mut();
    let mut disposition = 0u32;
    // SAFETY: valid strings and descriptor; the key is closed below.
    unsafe {
        let r = RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            path.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_ALL_ACCESS,
            &sa,
            &mut key,
            &mut disposition,
        );
        if r != NO_ERROR {
            return Err(os_error(r));
        }
        // It may have been there already: its access list as it must be.
        let r = RegSetKeySecurity(
            key,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            sd.0,
        );
        RegCloseKey(key);
        if r != NO_ERROR {
            return Err(os_error(r));
        }
    }
    // A new key: the Stop this computer kept until now. A key that was
    // there already keeps its Stop as it is; with none readable in it, it
    // stays stopped (see `recover`), to be resumed with approval.
    // Its numbers start again: so does the signing key (one left from
    // before would sign numbers it signed already).
    const REG_CREATED_NEW_KEY: u32 = 1;
    if disposition == REG_CREATED_NEW_KEY {
        forget_secret()?;
        save_state(seed)?;
    }
    Ok(())
}

/// Remove the guard (run raised): its service, its key and its folder.
/// The machines it held end with it.
pub fn uninstall() -> io::Result<()> {
    if !elevated() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "removing Tessera Guard takes an administrator's approval",
        ));
    }
    let name = wide(SERVICE_NAME);
    // SAFETY: plain calls; handles closed by `Sc`.
    unsafe {
        let scm = Sc::new(OpenSCManagerW(
            std::ptr::null(),
            std::ptr::null(),
            SC_MANAGER_ALL_ACCESS,
        ))?;
        let svc = OpenServiceW(scm.0, name.as_ptr(), SERVICE_ALL_ACCESS);
        if !svc.is_null() {
            let svc = Sc(svc);
            stop_service(&svc)?;
            if DeleteService(svc.0) == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        let key = wide(STATE_KEY);
        let r = RegDeleteTreeW(HKEY_LOCAL_MACHINE, key.as_ptr());
        if r != NO_ERROR && r != ERROR_FILE_NOT_FOUND {
            return Err(os_error(r));
        }
        forget_secret()?;
    }
    let dir = guard_dir()?;
    // Its own files (never anything else that may be there): the stand-in
    // Stop and the logs.
    for name in ["stop-pending.json", "guard.log", "guard.old.log"] {
        match std::fs::remove_file(dir.join(name)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                return Err(io::Error::other(format!(
                    "the service is gone, but {} couldn't be removed: {e}",
                    dir.join(name).display()
                )));
            }
            _ => {}
        }
    }
    let exe = wide(dir.join(EXE_NAME));
    for _ in 0..20 {
        // SAFETY: NUL-terminated paths.
        unsafe {
            if DeleteFileW(exe.as_ptr()) != 0 || GetLastError() == ERROR_FILE_NOT_FOUND {
                if RemoveDirectoryW(wide(&dir).as_ptr()) == 0
                    && GetLastError() != ERROR_FILE_NOT_FOUND
                {
                    return Err(io::Error::other(format!(
                        "the service is gone, but {} couldn't be removed: {}",
                        dir.display(),
                        io::Error::last_os_error()
                    )));
                }
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(io::Error::other(format!(
        "the service is gone, but {} couldn't be deleted yet: delete it after a restart",
        dir.display()
    )))
}
