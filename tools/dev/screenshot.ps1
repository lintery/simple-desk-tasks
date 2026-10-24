# Renders docs/screenshot.png from tools/dev/showcase.html (made-up data on a
# neutral wallpaper) with headless Edge:  pwsh -File tools/dev/screenshot.ps1
# Headless Edge leaves a sixth widget frame unpainted, so the calendar is shot
# on its own (same wallpaper) and pasted into the picture of the others.
param([int]$Port = 8790)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$root = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$edge = @("${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe", "$env:ProgramFiles\Microsoft\Edge\Application\msedge.exe") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $edge) { throw 'Microsoft Edge not found.' }
$tmp = Join-Path ([IO.Path]::GetTempPath()) "sdt-shot-$PID"
New-Item -ItemType Directory -Force $tmp | Out-Null
$server = Start-Process python -ArgumentList '-m', 'http.server', $Port, '--bind', '127.0.0.1', '--directory', "`"$root`"" -PassThru -WindowStyle Hidden
try {
    $base = "http://127.0.0.1:$Port/tools/dev/showcase.html"
    for ($i = 0; $i -lt 50; $i++) {
        try { Invoke-WebRequest -UseBasicParsing $base -TimeoutSec 1 | Out-Null; break } catch { Start-Sleep -Milliseconds 200 }
    }
    $shot = {
        param($query, $out)
        & $edge --headless=new --disable-gpu --hide-scrollbars --no-first-run --user-data-dir="$tmp\profile" `
            --force-device-scale-factor=2 --window-size=1560,900 --virtual-time-budget=8000 --screenshot="$out" "$base$query" 2>&1 | Out-Null
        if (-not (Test-Path $out)) { throw "Edge didn't write $out" }
    }
    & $shot '?skip=calendar' "$tmp\desk.png"
    & $shot '?only=calendar' "$tmp\calendar.png"
    $desk = [System.Drawing.Bitmap]::FromFile("$tmp\desk.png")
    $cal = [System.Drawing.Bitmap]::FromFile("$tmp\calendar.png")
    try {
        $g = [System.Drawing.Graphics]::FromImage($desk)
        # The calendar's frame: 1150,60 340×780 CSS px at 2× (plus a margin of wallpaper).
        $rect = [System.Drawing.Rectangle]::new(2280, 100, 720, 1600)
        $g.DrawImage($cal, $rect, $rect, [System.Drawing.GraphicsUnit]::Pixel)
        $g.Dispose()
        $out = Join-Path $root 'docs\screenshot.png'
        $desk.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
        Write-Host "Saved $out ($($desk.Width)×$($desk.Height))"
    } finally {
        $desk.Dispose()
        $cal.Dispose()
    }
} finally {
    Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
