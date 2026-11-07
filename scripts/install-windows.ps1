# Install sennya for the current user: %LOCALAPPDATA%\Programs\sennya, the user PATH and a
# Start menu shortcut. Run it next to sennya.exe and sennya.ico:
#   powershell -ExecutionPolicy Bypass -File install-windows.ps1
param([string]$From = $PSScriptRoot)
$ErrorActionPreference = 'Stop'

$dir = Join-Path $env:LOCALAPPDATA 'Programs\sennya'
$exe = Join-Path $dir 'sennya.exe'
New-Item -ItemType Directory -Force $dir | Out-Null

# A running window or daemon keeps its exe locked, but a locked exe can still be renamed.
Get-ChildItem $dir -Filter '*.old' | Remove-Item -Force -ErrorAction SilentlyContinue
if (Test-Path $exe) {
    Rename-Item $exe ("sennya.{0}.old" -f (Get-Date -Format 'yyyyMMddHHmmss'))
}
Copy-Item (Join-Path $From 'sennya.exe') $exe
Copy-Item (Join-Path $From 'sennya.ico') (Join-Path $dir 'sennya.ico') -Force

$path = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not (($path -split ';') -contains $dir)) {
    $new = if ($path) { "$path;$dir" } else { $dir }
    [Environment]::SetEnvironmentVariable('Path', $new, 'User')
}

$shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut(
    (Join-Path ([Environment]::GetFolderPath('Programs')) 'Sennya.lnk'))
$shortcut.TargetPath = $exe
$shortcut.WorkingDirectory = $env:USERPROFILE
$shortcut.IconLocation = Join-Path $dir 'sennya.ico'
# Minimized: the console window sennya closes at start never shows.
$shortcut.WindowStyle = 7
$shortcut.Description = 'sennya terminal'
$shortcut.Save()

& $exe install-skill | Out-Null
Write-Output "Installed $exe, PATH and the Start menu shortcut"
