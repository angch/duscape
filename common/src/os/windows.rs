#[cfg(not(test))]
pub fn is_user_admin() -> bool {
    #[repr(C)]
    #[allow(non_snake_case)]
    struct SID_IDENTIFIER_AUTHORITY {
        Value: [u8; 6],
    }

    const SECURITY_NT_AUTHORITY: SID_IDENTIFIER_AUTHORITY = SID_IDENTIFIER_AUTHORITY {
        Value: [0, 0, 0, 0, 0, 5],
    };
    const SECURITY_BUILTIN_DOMAIN_RID: u32 = 0x0000_0020;
    const DOMAIN_ALIAS_RID_ADMINS: u32 = 0x0000_0220;

    #[link(name = "advapi32")]
    #[allow(non_snake_case)]
    unsafe extern "system" {
        fn AllocateAndInitializeSid(
            pIdentifierAuthority: *const SID_IDENTIFIER_AUTHORITY,
            nSubAuthorityCount: u8,
            nSubAuthority0: u32,
            nSubAuthority1: u32,
            nSubAuthority2: u32,
            nSubAuthority3: u32,
            nSubAuthority4: u32,
            nSubAuthority5: u32,
            nSubAuthority6: u32,
            nSubAuthority7: u32,
            pSid: *mut *mut core::ffi::c_void,
        ) -> i32;

        fn CheckTokenMembership(
            TokenHandle: *mut core::ffi::c_void,
            SidToCheck: *mut core::ffi::c_void,
            IsMember: *mut i32,
        ) -> i32;

        fn FreeSid(pSid: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
    }

    let mut admin_sid: *mut core::ffi::c_void = core::ptr::null_mut();
    let auth = SECURITY_NT_AUTHORITY;
    let alloc_success = unsafe {
        AllocateAndInitializeSid(
            &auth,
            2,
            SECURITY_BUILTIN_DOMAIN_RID,
            DOMAIN_ALIAS_RID_ADMINS,
            0,
            0,
            0,
            0,
            0,
            0,
            &mut admin_sid,
        )
    };

    if alloc_success == 0 {
        return false;
    }

    let mut is_member: i32 = 0;
    let check_success =
        unsafe { CheckTokenMembership(core::ptr::null_mut(), admin_sid, &mut is_member) };

    unsafe {
        FreeSid(admin_sid);
    }

    check_success != 0 && is_member != 0
}

#[cfg(test)]
pub fn is_user_admin() -> bool {
    false
}

pub fn size_on_disk_fast(metadata: &::std::fs::Metadata) -> u64 {
    metadata.len()
}

#[repr(C)]
#[derive(Default)]
#[allow(non_snake_case)]
struct BY_HANDLE_FILE_INFORMATION {
    dwFileAttributes: u32,
    ftCreationTime: [u32; 2],
    ftLastAccessTime: [u32; 2],
    ftLastWriteTime: [u32; 2],
    dwVolumeSerialNumber: u32,
    nFileSizeHigh: u32,
    nFileSizeLow: u32,
    nNumberOfLinks: u32,
    nFileIndexHigh: u32,
    nFileIndexLow: u32,
}

fn query_file_info(path: &::std::path::Path) -> Option<BY_HANDLE_FILE_INFORMATION> {
    use ::std::os::windows::ffi::OsStrExt;

    const FILE_SHARE_READ: u32 = 1;
    const FILE_SHARE_WRITE: u32 = 2;
    const FILE_SHARE_DELETE: u32 = 4;
    const OPEN_EXISTING: u32 = 3;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const INVALID_HANDLE_VALUE: *mut core::ffi::c_void = -1isize as *mut core::ffi::c_void;

    #[link(name = "kernel32")]
    #[allow(non_snake_case)]
    unsafe extern "system" {
        fn CreateFileW(
            lpFileName: *const u16,
            dwDesiredAccess: u32,
            dwShareMode: u32,
            lpSecurityAttributes: *mut core::ffi::c_void,
            dwCreationDisposition: u32,
            dwFlagsAndAttributes: u32,
            hTemplateFile: *mut core::ffi::c_void,
        ) -> *mut core::ffi::c_void;

        fn GetFileInformationByHandle(
            hFile: *mut core::ffi::c_void,
            lpFileInformation: *mut BY_HANDLE_FILE_INFORMATION,
        ) -> i32;

        fn CloseHandle(hObject: *mut core::ffi::c_void) -> i32;
    }

    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);

    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            core::ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            core::ptr::null_mut(),
        )
    };

    if handle == INVALID_HANDLE_VALUE {
        return None;
    }

    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
    unsafe {
        CloseHandle(handle);
    }

    if ok != 0 { Some(info) } else { None }
}

/// Bytes in use on the volume whose root is `path`, or `None` when `path` is not a volume root — a
/// folder's size cannot be compared with its volume's.
///
/// This is the figure Explorer and WizTree report as used. It counts what no directory walk can
/// see: NTFS's own metadata files, shadow copies, and folders the scan was refused.
pub fn volume_used(path: &::std::path::Path) -> Option<u64> {
    use ::std::os::windows::ffi::{OsStrExt, OsStringExt};

    #[link(name = "kernel32")]
    #[allow(non_snake_case)]
    unsafe extern "system" {
        fn GetVolumePathNameW(
            lpszFileName: *const u16,
            lpszVolumePathName: *mut u16,
            cchBufferLength: u32,
        ) -> i32;
        fn GetDiskFreeSpaceExW(
            lpDirectoryName: *const u16,
            lpFreeBytesAvailableToCaller: *mut u64,
            lpTotalNumberOfBytes: *mut u64,
            lpTotalNumberOfFreeBytes: *mut u64,
        ) -> i32;
    }

    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    let mut root = [0u16; 1024];
    // SAFETY: `wide` is NUL-terminated and `root` is as long as the length passed.
    if unsafe { GetVolumePathNameW(wide.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
        return None;
    }
    let len = root.iter().position(|&c| c == 0).unwrap_or(root.len());
    let root_path = ::std::path::PathBuf::from(::std::ffi::OsString::from_wide(&root[..len]));
    let same = |a: &::std::path::Path| a.canonicalize().ok();
    if same(&root_path)? != same(path)? {
        return None;
    }

    let (mut available, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: `root` is NUL-terminated at `len`, and the three outputs are live `u64`s.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            root.as_ptr(),
            &raw mut available,
            &raw mut total,
            &raw mut free,
        )
    };
    (ok != 0).then(|| total.saturating_sub(free))
}

/// Turn on `SeBackupPrivilege`, if this process holds it, and report whether it did.
///
/// An elevated administrator holds it, but disabled. Enabled, it lets `CreateFileW` with
/// `FILE_FLAG_BACKUP_SEMANTICS` — which the walker already passes — open any directory for reading
/// whatever its ACL says: `System Volume Information`, other users' profiles, `WindowsApps`. It
/// grants reading only; deleting still needs ordinary permission. Unelevated, the privilege is not
/// held and this changes nothing.
pub fn enable_backup_privilege() -> bool {
    type Handle = *mut core::ffi::c_void;
    const TOKEN_ADJUST_PRIVILEGES: u32 = 0x0020;
    const TOKEN_QUERY: u32 = 0x0008;
    const SE_PRIVILEGE_ENABLED: u32 = 0x0002;
    const ERROR_NOT_ALL_ASSIGNED: u32 = 1300;

    #[repr(C)]
    #[derive(Default)]
    struct Luid {
        low: u32,
        high: i32,
    }
    #[repr(C)]
    struct TokenPrivileges {
        count: u32,
        luid: Luid,
        attributes: u32,
    }

    #[link(name = "advapi32")]
    #[allow(non_snake_case)]
    unsafe extern "system" {
        fn OpenProcessToken(
            ProcessHandle: Handle,
            DesiredAccess: u32,
            TokenHandle: *mut Handle,
        ) -> i32;
        fn LookupPrivilegeValueW(
            lpSystemName: *const u16,
            lpName: *const u16,
            lpLuid: *mut Luid,
        ) -> i32;
        fn AdjustTokenPrivileges(
            TokenHandle: Handle,
            DisableAllPrivileges: i32,
            NewState: *const TokenPrivileges,
            BufferLength: u32,
            PreviousState: *mut core::ffi::c_void,
            ReturnLength: *mut u32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    #[allow(non_snake_case)]
    unsafe extern "system" {
        fn GetCurrentProcess() -> Handle;
        fn CloseHandle(hObject: Handle) -> i32;
        fn GetLastError() -> u32;
    }

    let name: Vec<u16> = "SeBackupPrivilege ".encode_utf16().collect();
    let mut luid = Luid::default();
    // SAFETY: `name` is NUL-terminated and `luid` is a live `LUID`.
    if unsafe { LookupPrivilegeValueW(core::ptr::null(), name.as_ptr(), &raw mut luid) } == 0 {
        return false;
    }
    let mut token: Handle = core::ptr::null_mut();
    // SAFETY: the pseudo-handle from `GetCurrentProcess` needs no closing; `token` is written on
    // success and closed below.
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &raw mut token,
        )
    } == 0
    {
        return false;
    }
    let privileges = TokenPrivileges {
        count: 1,
        luid,
        attributes: SE_PRIVILEGE_ENABLED,
    };
    // SAFETY: `privileges` is a `TOKEN_PRIVILEGES` with room for the one entry it declares.
    let adjusted = unsafe {
        AdjustTokenPrivileges(
            token,
            0,
            &raw const privileges,
            0,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        )
    };
    // Success with `ERROR_NOT_ALL_ASSIGNED` is how the call says the privilege was not held.
    // SAFETY: no preconditions; read before anything else can overwrite it.
    let enabled = adjusted != 0 && unsafe { GetLastError() } != ERROR_NOT_ALL_ASSIGNED;
    // SAFETY: `token` was opened above and is closed exactly once.
    unsafe {
        CloseHandle(token);
    }
    enabled
}

pub fn volume_id(path: &::std::path::Path) -> Option<u64> {
    query_file_info(path).map(|info| u64::from(info.dwVolumeSerialNumber))
}

pub fn link_count(path: &::std::path::Path) -> u64 {
    query_file_info(path)
        .map(|info| u64::from(info.nNumberOfLinks))
        .unwrap_or(1)
}
