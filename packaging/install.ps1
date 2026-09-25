# Install a disktree release on Windows: the program and a Start menu
# shortcut, for the current user and without administrator rights.
#
#   powershell -ExecutionPolicy Bypass -File install.ps1
#   powershell -ExecutionPolicy Bypass -File install.ps1 -Uninstall
#
# The Windows counterpart of install.sh. The program goes where per-user
# installers put theirs, %LOCALAPPDATA%\Programs, and that directory is
# added to the user's PATH so `disktree C:\some\dir` works in a terminal.
param(
    [string]$Prefix = (Join-Path $env:LOCALAPPDATA 'Programs\disktree'),
    [switch]$Uninstall
)
$ErrorActionPreference = 'Stop'

$here = $PSScriptRoot
$startMenu = [Environment]::GetFolderPath('Programs')
$shortcut = Join-Path $startMenu 'disktree.lnk'
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$entries = @($userPath -split ';' | Where-Object { $_ })

if ($Uninstall) {
    Remove-Item -Force -ErrorAction SilentlyContinue `
        (Join-Path $Prefix 'disktree.exe'), $shortcut
    Remove-Item -Force -ErrorAction SilentlyContinue $Prefix
    $kept = $entries | Where-Object { $_ -ne $Prefix }
    [Environment]::SetEnvironmentVariable('Path', ($kept -join ';'), 'User')
    Write-Host 'removed'
    return
}

$version = (Get-Content (Join-Path $here 'VERSION') -ErrorAction SilentlyContinue)
New-Item -ItemType Directory -Force $Prefix | Out-Null
Copy-Item -Force (Join-Path $here 'disktree.exe') $Prefix
$exe = Join-Path $Prefix 'disktree.exe'

# The shortcut opens on the home directory, like running it with no path.
$shell = New-Object -ComObject WScript.Shell
$link = $shell.CreateShortcut($shortcut)
$link.TargetPath = $exe
$link.WorkingDirectory = $env:USERPROFILE
$link.Description = 'See what is using your disk, and free it up'
$link.Save()

if ($entries -notcontains $Prefix) {
    [Environment]::SetEnvironmentVariable(
        'Path', (($entries + $Prefix) -join ';'), 'User')
    $note = "`nnote: $Prefix was added to PATH; open a new terminal to use it"
}

Write-Host "installed disktree $version`:"
Write-Host "  $exe"
Write-Host "  $shortcut"
if ($note) { Write-Host $note }
