# Builds Simple Desk Widgets and installs it to
# %LOCALAPPDATA%\Programs\SimpleDeskWidgets. The app registers its own
# autostart on first run from there.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
# Short target dir: the default one under a deep project path exceeds MAX_PATH.
$env:CARGO_TARGET_DIR = Join-Path $env:LOCALAPPDATA 'simple-desk-widgets-build'

Push-Location (Join-Path $root 'src-tauri')
try { cargo build --release; if ($LASTEXITCODE) { throw "cargo build failed" } }
finally { Pop-Location }

$dest = Join-Path $env:LOCALAPPDATA 'Programs\SimpleDeskWidgets'
New-Item -ItemType Directory -Force $dest | Out-Null
# Also the copy from before the rename (Desk Tasks).
Get-Process simple-desk-widgets, SimpleDeskWidgets, desk-tasks, DeskTasks -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500
Copy-Item (Join-Path $env:CARGO_TARGET_DIR 'release\simple-desk-widgets.exe') (Join-Path $dest 'SimpleDeskWidgets.exe') -Force
# A hand install ends any update on trial (it would otherwise roll this build back).
'SimpleDeskWidgets.trial', 'SimpleDeskWidgets.old.exe', 'SimpleDeskWidgets.bad.exe', 'SimpleDeskWidgets.new.exe' |
    ForEach-Object { Remove-Item (Join-Path $dest $_) -Force -ErrorAction SilentlyContinue }

# WScript.Shell can't save to a non-ASCII path, so save under an ASCII name and rename.
$programs = [Environment]::GetFolderPath('Programs')
$tmpLnk = Join-Path $programs 'SimpleDeskWidgets.lnk'
$lnk = Join-Path $programs 'Simple Desk Widgets.lnk'
$sh = New-Object -ComObject WScript.Shell
$s = $sh.CreateShortcut($tmpLnk)
$s.TargetPath = Join-Path $dest 'SimpleDeskWidgets.exe'
$s.WorkingDirectory = $dest
$s.Description = 'Задачи, фокус, календарь и плеер на рабочем столе'
$s.Save()
Move-Item -LiteralPath $tmpLnk -Destination $lnk -Force

# The install from before the rename: its folder and shortcut (the app
# itself moves its data and autostart over on first start).
Remove-Item (Join-Path $env:LOCALAPPDATA 'Programs\DeskTasks') -Recurse -Force -ErrorAction SilentlyContinue
Remove-Item (Join-Path $programs 'Задачи.lnk'), (Join-Path $programs 'DeskTasks.lnk') -Force -ErrorAction SilentlyContinue

Start-Process (Join-Path $dest 'SimpleDeskWidgets.exe')
"Installed to $dest"
