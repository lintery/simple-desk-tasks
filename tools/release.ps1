# Builds, signs and publishes a release that installed copies update to.
#   pwsh -File tools/release.ps1            # asks before publishing
#   pwsh -File tools/release.ps1 -DryRun    # build and sign only
# The version comes from src-tauri/Cargo.toml; bump it there first.
# Signing key: %USERPROFILE%\.simple-desk-tasks\updater.key (made once with
#   tools/signer keygen; its public half is src-tauri/updater.pub).
param([switch]$DryRun)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$version = (Select-String -Path src-tauri\Cargo.toml -Pattern '^version = "([^"]+)"').Matches[0].Groups[1].Value
$tag = "v$version"
$key = Join-Path $env:USERPROFILE '.simple-desk-tasks\updater.key'
if (-not (Test-Path $key)) { throw "Signing key not found: $key" }
if (git status --porcelain) { throw 'Commit or stash your changes first: the release is built from HEAD.' }
if (git tag --list $tag) { throw "Tag $tag already exists; bump the version in src-tauri/Cargo.toml." }
if (-not $DryRun -and (gh release view $tag 2>$null)) { throw "Release $tag already exists on GitHub." }
if (-not $DryRun) {
    git fetch --quiet
    if (-not (git branch -r --contains HEAD)) { throw 'Push HEAD first: the release tag is made on GitHub from this commit.' }
}

Write-Host "Building $tag..."
$env:CARGO_TARGET_DIR = Join-Path $env:LOCALAPPDATA 'desk-tasks-build'
cargo build --release --locked --manifest-path src-tauri/Cargo.toml
if ($LASTEXITCODE) { throw 'Build failed.' }
$dist = Join-Path $root 'dist'
New-Item -ItemType Directory -Force $dist | Out-Null
Copy-Item (Join-Path $env:CARGO_TARGET_DIR 'release\desk-tasks.exe') (Join-Path $dist 'DeskTasks.exe') -Force

Write-Host 'Signing...'
$env:CARGO_TARGET_DIR = Join-Path $env:LOCALAPPDATA 'desk-tasks-signer'
cargo build --release --manifest-path tools/signer/Cargo.toml
if ($LASTEXITCODE) { throw 'Signer build failed.' }
& (Join-Path $env:CARGO_TARGET_DIR 'release\desk-tasks-signer.exe') sign $key (Join-Path $dist 'DeskTasks.exe') $version
if ($LASTEXITCODE) { throw 'Signing failed.' }

$exe = Get-Item (Join-Path $dist 'DeskTasks.exe')
Write-Host ("Ready: {0} ({1:N1} MB) + signature" -f $exe.FullName, ($exe.Length / 1MB))
if ($DryRun) { return }

$answer = Read-Host "Publish $tag on GitHub (public, installed copies will update to it)? [y/N]"
if ($answer -notmatch '^(y|yes|д|да)$') { Write-Host 'Not published.'; return }
gh release create $tag (Join-Path $dist 'DeskTasks.exe') (Join-Path $dist 'DeskTasks.exe.minisig') --title $tag --generate-notes --target (git rev-parse HEAD)
if ($LASTEXITCODE) { throw 'Publishing failed.' }
git fetch --tags --quiet
Write-Host "Published $tag."
