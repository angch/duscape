# The installer's PATH edit: `add` or `remove` one folder in the user's PATH.
#   path.ps1 add|remove <folder>
# In PowerShell, not NSIS: NSIS's strings stop at 1024 characters, and a longer PATH read and
# written back through them would come back cut short. The value is read unexpanded and
# written back as the kind it was, so a `%USERPROFILE%\...` entry stays one (.NET's
# `[Environment]::SetEnvironmentVariable` would expand them all). The installer tells the
# shell of the change (WM_SETTINGCHANGE) itself.
param(
    [Parameter(Mandatory)] [ValidateSet('add', 'remove')] [string]$Action,
    [Parameter(Mandatory)] [string]$Folder
)
$ErrorActionPreference = 'Stop'
$key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
try {
    $exists = $key.GetValueNames() -contains 'Path'
    $value = if ($exists) {
        $key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    } else { '' }
    $kind = if ($exists) { $key.GetValueKind('Path') } else { [Microsoft.Win32.RegistryValueKind]::ExpandString }
    $folder = $Folder.TrimEnd('\')
    # Every entry but this folder's (compared without case or a trailing backslash), empty
    # entries dropped.
    $parts = @($value -split ';' | Where-Object { $_ -and ($_.TrimEnd('\') -ne $folder) })
    if ($Action -eq 'add') { $parts += $folder }
    if ($parts.Count -gt 0) {
        $key.SetValue('Path', ($parts -join ';'), $kind)
    } elseif ($exists) {
        $key.DeleteValue('Path')
    }
} finally {
    $key.Close()
}
