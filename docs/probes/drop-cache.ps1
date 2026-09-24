# Empty Windows's file cache, for a cold-cache measurement: what `echo 3 > /proc/sys/vm/drop_caches`
# does on Linux. Needs an elevated shell. Used by bench-matrix.ps1 before each cold run.
#
# Cached file data and metadata — NTFS's MFT and directory index blocks among them — live in the
# system file cache's working set and, once trimmed from it, on the standby list, from where a
# read is a page fault rather than a disk read. So: flush every volume's write cache, trim the
# system cache's working set to nothing (`SetSystemFileCacheSize`, which needs
# SeIncreaseQuotaPrivilege), write the modified page list out and purge the standby lists
# (`NtSetSystemInformation(SystemMemoryListInformation)`, which needs
# SeProfileSingleProcessPrivilege) — the calls behind Sysinternals RAMMap's "Empty" menu.
# Processes' own working sets are left alone: they hold code and heap, not the filesystem's
# metadata, and trimming them would make everything on the machine slow rather than the scan cold.
$ErrorActionPreference = "Stop"

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class DropCache {
    [DllImport("ntdll.dll")]
    public static extern int NtSetSystemInformation(int infoClass, ref int info, int length);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool SetSystemFileCacheSize(IntPtr minimum, IntPtr maximum, int flags);
    [DllImport("kernel32.dll")]
    public static extern IntPtr GetCurrentProcess();
    [DllImport("advapi32.dll", SetLastError = true)]
    public static extern bool OpenProcessToken(IntPtr process, int access, out IntPtr token);
    [DllImport("advapi32.dll", SetLastError = true)]
    public static extern bool LookupPrivilegeValue(string system, string name, out long luid);
    [StructLayout(LayoutKind.Sequential, Pack = 1)]
    public struct TokenPrivileges { public int Count; public long Luid; public int Attributes; }
    [DllImport("advapi32.dll", SetLastError = true)]
    public static extern bool AdjustTokenPrivileges(IntPtr token, bool disableAll, ref TokenPrivileges state, int length, IntPtr previous, IntPtr returned);

    const int TOKEN_ADJUST_PRIVILEGES = 0x20, TOKEN_QUERY = 0x8, SE_PRIVILEGE_ENABLED = 0x2;
    public static bool Enable(string privilege) {
        IntPtr token;
        if (!OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, out token)) return false;
        TokenPrivileges state = new TokenPrivileges();
        state.Count = 1; state.Attributes = SE_PRIVILEGE_ENABLED;
        if (!LookupPrivilegeValue(null, privilege, out state.Luid)) return false;
        if (!AdjustTokenPrivileges(token, false, ref state, 0, IntPtr.Zero, IntPtr.Zero)) return false;
        return Marshal.GetLastWin32Error() == 0;
    }
    const int SystemMemoryListInformation = 80;
    public static int MemoryList(int command) {
        return NtSetSystemInformation(SystemMemoryListInformation, ref command, 4);
    }
}
"@

foreach ($privilege in "SeIncreaseQuotaPrivilege", "SeProfileSingleProcessPrivilege") {
    if (-not [DropCache]::Enable($privilege)) {
        Write-Error "cannot enable ${privilege}: run from an elevated shell"
        exit 1
    }
}
Get-Volume | Where-Object DriveLetter | ForEach-Object { Write-VolumeCache -DriveLetter $_.DriveLetter }
if (-not [DropCache]::SetSystemFileCacheSize([IntPtr]::new(-1), [IntPtr]::new(-1), 0)) {
    Write-Error "SetSystemFileCacheSize failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
    exit 1
}
# MemoryFlushModifiedList = 3, MemoryPurgeStandbyList = 4, MemoryPurgeLowPriorityStandbyList = 5.
foreach ($command in 3, 4, 5) {
    $status = [DropCache]::MemoryList($command)
    if ($status -ne 0) {
        Write-Error ("NtSetSystemInformation({0}) failed: NTSTATUS 0x{1:X8}" -f $command, $status)
        exit 1
    }
}
