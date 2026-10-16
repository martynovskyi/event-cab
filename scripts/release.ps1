#Requires -Version 7
<#
.SYNOPSIS
    Cuts a release: bumps Cargo.toml, runs the CI checks, commits, tags vX.Y.Z and pushes.
    The pushed tag triggers .github/workflows/release.yml (GHCR image + GitHub release).

.EXAMPLE
    ./scripts/release.ps1 0.2.0
    ./scripts/release.ps1 0.2.0 -SkipChecks   # don't run fmt/clippy/test locally
    ./scripts/release.ps1 0.2.0 -NoPush       # commit and tag locally only
#>
param(
    [Parameter(Mandatory)]
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$Version,
    [switch]$SkipChecks,
    [switch]$NoPush,
    [switch]$Yes
)

$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)

$Tag = "v$Version"

function Invoke-Native([scriptblock]$Cmd) {
    & $Cmd
    if ($LASTEXITCODE -ne 0) { throw "'$Cmd' failed with exit code $LASTEXITCODE" }
}

# --- preconditions ---
$branch = git rev-parse --abbrev-ref HEAD
if ($branch -ne 'main') { throw "Releases are cut from main (current branch: $branch)" }
if (git status --porcelain) { throw 'Working tree is not clean; commit or stash first' }

Invoke-Native { git fetch --quiet --tags origin main }
$behind = git rev-list --count HEAD..origin/main
if ([int]$behind -gt 0) { throw "main is $behind commit(s) behind origin/main; pull first" }
if (git tag --list $Tag) { throw "Tag $Tag already exists" }

$cargoToml = Get-Content Cargo.toml -Raw
$versionLine = [regex]'(?m)^version\s*=\s*"([^"]+)"'
$current = $versionLine.Match($cargoToml).Groups[1].Value
if ([version]$Version -le [version]$current) {
    throw "New version $Version must be greater than the current $current"
}

# --- bump ---
Write-Host "Releasing $current -> $Version"
$updated = $versionLine.Replace($cargoToml, "version = `"$Version`"", 1)
[IO.File]::WriteAllText((Resolve-Path Cargo.toml), $updated)
Invoke-Native { cargo update --workspace --quiet }   # sync the package version into Cargo.lock

try {
    if (-not $SkipChecks) {
        & "$PSScriptRoot/dev.ps1" check
    }
} catch {
    git checkout -- Cargo.toml Cargo.lock
    throw
}

Invoke-Native { git add Cargo.toml Cargo.lock }
Invoke-Native { git commit --quiet -m "chore: release $Tag" }
Invoke-Native { git tag -a $Tag -m "Release $Tag" }
Write-Host "Created commit and tag $Tag." -ForegroundColor Green

if ($NoPush) {
    Write-Host "Not pushed. When ready: git push --atomic origin main $Tag"
    return
}

if (-not $Yes) {
    $answer = Read-Host "Push main and $Tag to origin? [y/N]"
    if ($answer -notmatch '^(y|yes)$') {
        Write-Host "Not pushed. When ready: git push --atomic origin main $Tag"
        return
    }
}
Invoke-Native { git push --atomic origin main $Tag }

$repo = (git remote get-url origin) -replace '^.*github\.com[:/]', '' -replace '\.git$', ''
Write-Host "Pushed. Follow the release at https://github.com/$repo/actions" -ForegroundColor Green
