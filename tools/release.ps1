# Builds, signs and publishes a release that installed copies update to.
#   pwsh -File tools/release.ps1            # asks before publishing
#   pwsh -File tools/release.ps1 -DryRun    # build and sign only
# The version comes from src-tauri/Cargo.toml; bump it there first.
# Signing key: %USERPROFILE%\.simple-desk-widgets\updater.key (made once with
#   tools/signer keygen; its public half is src-tauri/updater.pub).
param([switch]$DryRun)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$version = (Select-String -Path src-tauri\Cargo.toml -Pattern '^version = "([^"]+)"').Matches[0].Groups[1].Value
$tag = "v$version"
$key = Join-Path $env:USERPROFILE '.simple-desk-widgets\updater.key'
if (-not (Test-Path $key)) { throw "Signing key not found: $key" }
if (git status --porcelain) { throw 'Commit or stash your changes first: the release is built from HEAD.' }
if (git tag --list $tag) { throw "Tag $tag already exists; bump the version in src-tauri/Cargo.toml." }
if (-not $DryRun -and (gh release view $tag 2>$null)) { throw "Release $tag already exists on GitHub." }
if (-not $DryRun) {
    git fetch --quiet
    if (-not (git branch -r --contains HEAD)) { throw 'Push HEAD first: the release tag is made on GitHub from this commit.' }
}

Write-Host "Building $tag..."
$env:CARGO_TARGET_DIR = Join-Path $env:LOCALAPPDATA 'simple-desk-widgets-build'
cargo build --release --locked --manifest-path src-tauri/Cargo.toml
if ($LASTEXITCODE) { throw 'Build failed.' }
$dist = Join-Path $root 'dist'
New-Item -ItemType Directory -Force $dist | Out-Null
# The same exe under the old name too: copies installed before the rename
# (DeskTasks.exe) look for that one.
$names = 'SimpleDeskWidgets.exe', 'DeskTasks.exe'
foreach ($name in $names) {
    Copy-Item (Join-Path $env:CARGO_TARGET_DIR 'release\simple-desk-widgets.exe') (Join-Path $dist $name) -Force
}

Write-Host 'Signing...'
$env:CARGO_TARGET_DIR = Join-Path $env:LOCALAPPDATA 'simple-desk-widgets-signer'
cargo build --release --manifest-path tools/signer/Cargo.toml
if ($LASTEXITCODE) { throw 'Signer build failed.' }
foreach ($name in $names) {
    & (Join-Path $env:CARGO_TARGET_DIR 'release\simple-desk-widgets-signer.exe') sign $key (Join-Path $dist $name) $version
    if ($LASTEXITCODE) { throw 'Signing failed.' }
}

$exe = Get-Item (Join-Path $dist $names[0])
Write-Host ("Ready: {0} ({1:N1} MB) + signature, and the same as {2}" -f $exe.FullName, ($exe.Length / 1MB), $names[1])
if ($DryRun) { return }

$answer = Read-Host "Publish $tag on GitHub (public, installed copies will update to it)? [y/N]"
if ($answer -notmatch '^(y|yes|д|да)$') { Write-Host 'Not published.'; return }
$assets = $names | ForEach-Object { (Join-Path $dist $_), (Join-Path $dist "$_.minisig") }
gh release create $tag @assets --title $tag --generate-notes --target (git rev-parse HEAD)
if ($LASTEXITCODE) { throw 'Publishing failed.' }
git fetch --tags --quiet
Write-Host "Published $tag."
