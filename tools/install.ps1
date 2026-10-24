# Builds Desk Tasks and installs it to %LOCALAPPDATA%\Programs\DeskTasks.
# The app registers its own autostart on first run from there.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
# Short target dir: the default one under a deep project path exceeds MAX_PATH.
$env:CARGO_TARGET_DIR = Join-Path $env:LOCALAPPDATA 'desk-tasks-build'

Push-Location (Join-Path $root 'src-tauri')
try { cargo build --release; if ($LASTEXITCODE) { throw "cargo build failed" } }
finally { Pop-Location }

$dest = Join-Path $env:LOCALAPPDATA 'Programs\DeskTasks'
New-Item -ItemType Directory -Force $dest | Out-Null
Get-Process desk-tasks, DeskTasks -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500
Copy-Item (Join-Path $env:CARGO_TARGET_DIR 'release\desk-tasks.exe') (Join-Path $dest 'DeskTasks.exe') -Force
# A hand install ends any update on trial (it would otherwise roll this build back).
'DeskTasks.trial', 'DeskTasks.old.exe', 'DeskTasks.bad.exe', 'DeskTasks.new.exe' |
    ForEach-Object { Remove-Item (Join-Path $dest $_) -Force -ErrorAction SilentlyContinue }

# WScript.Shell can't save to a non-ASCII path, so save under an ASCII name and rename.
$programs = [Environment]::GetFolderPath('Programs')
$tmpLnk = Join-Path $programs 'DeskTasks.lnk'
$lnk = Join-Path $programs 'Задачи.lnk'
$sh = New-Object -ComObject WScript.Shell
$s = $sh.CreateShortcut($tmpLnk)
$s.TargetPath = Join-Path $dest 'DeskTasks.exe'
$s.WorkingDirectory = $dest
$s.Description = 'Виджеты задач на рабочем столе'
$s.Save()
Move-Item -LiteralPath $tmpLnk -Destination $lnk -Force

Start-Process (Join-Path $dest 'DeskTasks.exe')
"Installed to $dest"
