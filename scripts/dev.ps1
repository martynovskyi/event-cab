#Requires -Version 7
<#
.SYNOPSIS
    Local development helper: runs the service, tests and CI checks against a disposable Postgres container.

.EXAMPLE
    ./scripts/dev.ps1            # start PG (if needed) and run the service on :3001 / :3002
    ./scripts/dev.ps1 test       # cargo test against the local PG
    ./scripts/dev.ps1 check      # same checks as CI: fmt, clippy, test
    ./scripts/dev.ps1 openapi    # regenerate openapi/*.json
    ./scripts/dev.ps1 psql       # psql shell into the dev database
    ./scripts/dev.ps1 stop       # remove the PG container (data is discarded)

    Any env var already set (ADMIN_TOKEN, RUST_LOG, ALLOWED_HOSTS, ...) overrides the dev defaults.
#>
param(
    [ValidateSet('run', 'test', 'check', 'openapi', 'db', 'psql', 'stop')]
    [string]$Command = 'run',
    [int]$PgPort = 55432
)

$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)

$Container = 'event-cab-pg'
$PgImage = 'postgres:18.4-alpine3.23'
$ServerUrl = "postgres://postgres:postgres@127.0.0.1:$PgPort"

# Runs a native command and fails the script on a non-zero exit code.
function Invoke-Native([scriptblock]$Cmd) {
    & $Cmd
    if ($LASTEXITCODE -ne 0) { throw "'$Cmd' failed with exit code $LASTEXITCODE" }
}

function Start-Pg {
    $state = docker ps -a --filter "name=^$Container$" --format '{{.State}}'
    if (-not $state) {
        Write-Host "Starting $PgImage as '$Container' on port $PgPort..."
        Invoke-Native { docker run -d --name $Container -e POSTGRES_PASSWORD=postgres -p "${PgPort}:5432" $PgImage } | Out-Null
    } elseif ($state -ne 'running') {
        Invoke-Native { docker start $Container } | Out-Null
    }
    for ($i = 0; $i -lt 30; $i++) {
        docker exec $Container pg_isready -U postgres -q 2>$null
        if ($LASTEXITCODE -eq 0) { return }
        Start-Sleep -Seconds 1
    }
    throw "Postgres in '$Container' did not become ready"
}

function Initialize-DevDb {
    $exists = docker exec $Container psql -U postgres -tAc "SELECT 1 FROM pg_database WHERE datname = 'events'"
    if ($exists -ne '1') {
        Invoke-Native { docker exec $Container psql -U postgres -qc 'CREATE DATABASE events' }
    }
}

function Set-DefaultEnv([string]$Name, [string]$Value) {
    if (-not [Environment]::GetEnvironmentVariable($Name)) {
        [Environment]::SetEnvironmentVariable($Name, $Value)
    }
}

switch ($Command) {
    'run' {
        Start-Pg
        Initialize-DevDb
        Set-DefaultEnv DATABASE_URL "$ServerUrl/events"
        Set-DefaultEnv ADMIN_TOKEN 'dev-admin-token-0123456789abcdef'
        Set-DefaultEnv IP_HASH_SALT 'dev-salt'
        Set-DefaultEnv ALLOWED_HOSTS 'localhost'
        Set-DefaultEnv RUST_LOG 'info,events=debug'
        Write-Host "Public: http://localhost:3001  Admin: http://localhost:3002 (Bearer $env:ADMIN_TOKEN)"
        Invoke-Native { cargo run -- serve }
    }
    'test' {
        Start-Pg
        Set-DefaultEnv DATABASE_URL "$ServerUrl/postgres"
        Invoke-Native { cargo test --locked --all-targets }
    }
    'check' {
        Invoke-Native { cargo fmt --all --check }
        Invoke-Native { cargo clippy --all-targets --locked -- -D warnings }
        Start-Pg
        Set-DefaultEnv DATABASE_URL "$ServerUrl/postgres"
        Invoke-Native { cargo test --locked --all-targets }
        Write-Host 'All checks passed.' -ForegroundColor Green
    }
    'openapi' {
        Invoke-Native { cargo run --quiet -- openapi openapi }
    }
    'db' {
        Start-Pg
        Initialize-DevDb
        Write-Host "Postgres ready: $ServerUrl/events"
    }
    'psql' {
        Start-Pg
        Initialize-DevDb
        docker exec -it $Container psql -U postgres -d events
    }
    'stop' {
        docker rm -f $Container 2>$null | Out-Null
        Write-Host "Removed '$Container'."
    }
}
