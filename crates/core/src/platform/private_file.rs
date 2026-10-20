//! Windows credential storage. Never inherit a readable parent's DACL.
//! Set the protected DACL at creation, then read it back before writing bytes.

use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SE_FILE_OBJECT, SetSecurityInfo,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl, GetSecurityDescriptorDacl,
    GetTokenInformation, PROTECTED_DACL_SECURITY_INFORMATION, SE_DACL_PROTECTED,
    SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_WRITE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING, READ_CONTROL, WRITE_DAC,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

struct Local(*mut std::ffi::c_void);
impl Drop for Local {
    fn drop(&mut self) {
        // SAFETY: these buffers were allocated by the security conversion APIs.
        unsafe {
            LocalFree(self.0);
        }
    }
}

fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut value: Vec<u16> = path.as_os_str().encode_wide().collect();
    if value.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains a NUL",
        ));
    }
    value.push(0);
    Ok(value)
}

fn descriptor(directory: bool) -> io::Result<Local> {
    // SAFETY: token information is kept in aligned storage until its SID has
    // been converted. All API-owned buffers and the token handle are released.
    unsafe {
        let mut token = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        let result = (|| {
            let mut len = 0;
            GetTokenInformation(token, TokenUser, null_mut(), 0, &mut len);
            let mut buf = vec![0usize; (len as usize).div_ceil(size_of::<usize>())];
            if GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) == 0 {
                return Err(io::Error::last_os_error());
            }
            let user = &*buf.as_ptr().cast::<TOKEN_USER>();
            let mut sid = null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut sid) == 0 {
                return Err(io::Error::last_os_error());
            }
            let _sid = Local(sid.cast());
            let mut n = 0;
            while *sid.add(n) != 0 {
                n += 1;
            }
            let sid = String::from_utf16_lossy(std::slice::from_raw_parts(sid, n));
            let flags = if directory { "OICI" } else { "" };
            let sddl = format!("D:P(A;{flags};FA;;;{sid})(A;{flags};FA;;;SY)(A;{flags};FA;;;BA)");
            parse_descriptor(&sddl)
        })();
        CloseHandle(token);
        result
    }
}

fn parse_descriptor(sddl: &str) -> io::Result<Local> {
    let sddl: Vec<u16> = sddl.encode_utf16().chain([0]).collect();
    let mut sd = null_mut();
    // SAFETY: SDDL is NUL terminated. The allocated descriptor is owned below.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut sd, null_mut())
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(Local(sd))
}

fn dacl(sd: &Local) -> io::Result<*mut ACL> {
    let mut present = 0;
    let mut defaulted = 0;
    let mut acl = null_mut();
    // SAFETY: sd is a valid security descriptor for the duration of this call.
    if unsafe { GetSecurityDescriptorDacl(sd.0, &mut present, &mut acl, &mut defaulted) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if present == 0 || acl.is_null() {
        return Err(io::Error::other("private storage has no DACL"));
    }
    Ok(acl)
}

fn verify(file: &File, expected: &Local) -> io::Result<()> {
    let mut sd = null_mut();
    // SAFETY: the file handle is live; GetSecurityInfo allocates the descriptor.
    let error = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            &mut sd,
        )
    };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    let actual = Local(sd);
    let (want, got) = (dacl(expected)?, dacl(&actual)?);
    let mut control = 0;
    let mut revision = 0;
    // SAFETY: both ACLs belong to live descriptors. AclSize bounds their bytes.
    let matches = unsafe {
        GetSecurityDescriptorControl(actual.0, &mut control, &mut revision) != 0
            && control & SE_DACL_PROTECTED != 0
            && (*want).AclSize == (*got).AclSize
            && std::slice::from_raw_parts(want.cast::<u8>(), (*want).AclSize as usize)
                == std::slice::from_raw_parts(got.cast::<u8>(), (*got).AclSize as usize)
    };
    if !matches {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "could not verify private storage permissions on this filesystem",
        ));
    }
    Ok(())
}

/// A new, empty file, verified private before the caller can write any bytes.
pub fn create_new(path: &Path) -> io::Result<File> {
    let sd = descriptor(false)?;
    let sa = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    let path = wide(path)?;
    // SAFETY: path and descriptor live through creation; the handle is owned below.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            FILE_GENERIC_WRITE | READ_CONTROL,
            0,
            &sa,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: this is the newly created, uniquely owned file handle.
    let file = unsafe { File::from_raw_handle(handle) };
    verify(&file, &sd)?;
    Ok(file)
}

/// Create missing private directories with their DACL already set, and protect
/// the final app-owned directory even if it existed under a readable parent.
pub fn create_dir_all(path: &Path) -> io::Result<()> {
    let sd = descriptor(true)?;
    let sa = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    let mut missing = Vec::new();
    let mut next = path;
    while !next.as_os_str().is_empty() && !next.try_exists()? {
        missing.push(next.to_path_buf());
        next = next.parent().unwrap_or(Path::new("."));
    }
    for dir in missing.into_iter().rev() {
        let wide = wide(&dir)?;
        // SAFETY: path and descriptor are valid until creation returns.
        if unsafe { CreateDirectoryW(wide.as_ptr(), &sa) } == 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error);
            }
        }
        protect_dir(&dir, &sd)?;
    }
    protect_dir(path, &sd)
}

fn protect_dir(path: &Path, sd: &Local) -> io::Result<()> {
    let path = wide(path)?;
    // SAFETY: path is NUL terminated, the returned handle is owned below.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            READ_CONTROL | WRITE_DAC,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the handle is uniquely owned by this File.
    let file = unsafe { File::from_raw_handle(handle) };
    let metadata = file.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private directory is not a plain directory",
        ));
    }
    // SAFETY: the handle and ACL remain valid throughout the call.
    let error = unsafe {
        SetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            dacl(sd)?,
            null(),
        )
    };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    verify(&file, sd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn credentials_and_staged_bytes_have_a_protected_exact_dacl() {
        let parent = tempfile::tempdir().unwrap();
        let readable = parse_descriptor("D:P(A;OICI;FA;;;WD)").unwrap();
        protect_dir(parent.path(), &readable).unwrap();
        let dir = parent.path().join("private/nested");
        create_dir_all(&dir).unwrap();
        // Opening the folder and verifying again covers an existing directory.
        protect_dir(&dir, &descriptor(true).unwrap()).unwrap();
        let mut file = create_new(&dir.join("auth.tmp")).unwrap();
        assert_eq!(file.metadata().unwrap().len(), 0);
        verify(&file, &descriptor(false).unwrap()).unwrap();
        file.write_all(b"fixture credential").unwrap();
        assert!(verify(&file, &readable).is_err());
        assert!(create_new(&dir.join("auth.tmp")).is_err());
    }

    #[test]
    fn private_storage_rejects_nuls() {
        use std::os::windows::ffi::OsStringExt;
        let path = std::path::PathBuf::from(std::ffi::OsString::from_wide(&[97, 0, 98]));
        assert_eq!(
            create_new(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
