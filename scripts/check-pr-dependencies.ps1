<#
.SYNOPSIS
    Automated PR Dependency and Protected Set Gate Checker.
.DESCRIPTION
    Inspects git diff against origin/main. If changes touch the Protected Set
    (rules.toml, config/default.toml, permission tiers), verifies that all
    prerequisite dependency issues (e.g. #74 before #71) are closed/merged on GitHub.
.PARAMETER BaseBranch
    The base branch to diff against (defaults to "main").
.PARAMETER SkipNetworkCheck
    If specified, skips remote GitHub API validation (useful for offline local development).
    Can also be enabled via environment variable $env:NEBULA_SKIP_DEPENDENCY_NETWORK_CHECK = "1".
#>
param(
    [string]$BaseBranch = "main",
    [switch]$SkipNetworkCheck = $false
)

$ErrorActionPreference = 'Stop'

# Check environment override
if ($env:NEBULA_SKIP_DEPENDENCY_NETWORK_CHECK -eq "1") {
    $SkipNetworkCheck = $true
}

Write-Host "Running Protected Set & Dependency Check against '$BaseBranch'..." -ForegroundColor Cyan

# 1. Identify changed files against base branch
$changedFiles = git diff --name-only "$BaseBranch...HEAD" 2>$null
if (-not $changedFiles) {
    $changedFiles = git diff --name-only HEAD 2>$null
}

if (-not $changedFiles) {
    Write-Host "No changed files detected. Dependency check passed." -ForegroundColor Green
    exit 0
}

Write-Host "Changed files in PR:" -ForegroundColor DarkGray
$changedFiles | ForEach-Object { Write-Host "  - $_" -ForegroundColor DarkGray }

# 2. Check if Protected Set permission files are touched
$protectedPatterns = @(
    "config/default.toml",
    "config/runtime.lock.toml",
    "rules.toml",
    "crates/nebula-sandbox/src/tier.rs",
    "crates/nebula-sandbox/src/classifier.rs",
    "crates/nebula-sandbox/src/approval.rs"
)

$touchesProtected = $false
foreach ($pattern in $protectedPatterns) {
    if ($changedFiles -match [regex]::Escape($pattern)) {
        $touchesProtected = $true
        Write-Host "ALERT: PR modifies Protected Set file: $pattern" -ForegroundColor Yellow
    }
}

if (-not $touchesProtected) {
    Write-Host "No Protected Set permission files modified. Check passed." -ForegroundColor Green
    exit 0
}

# 3. If offline override is set, warn and allow local push/commit
if ($SkipNetworkCheck) {
    Write-Host ""
    Write-Host "==========================================================================" -ForegroundColor Yellow
    Write-Host "OFFLINE MODE: -SkipNetworkCheck / NEBULA_SKIP_DEPENDENCY_NETWORK_CHECK active" -ForegroundColor Yellow
    Write-Host "Skipping remote GitHub API dependency verification for Protected Set changes." -ForegroundColor Yellow
    Write-Host "Ensure dependent issues (e.g. #74) are merged before opening or merging a PR." -ForegroundColor Yellow
    Write-Host "==========================================================================" -ForegroundColor Yellow
    exit 0
}

# 4. If Protected Set is touched, verify GitHub CLI is available
if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
    Write-Error "GitHub CLI (gh) is required to verify Protected Set issue dependencies. If working offline, pass -SkipNetworkCheck or set NEBULA_SKIP_DEPENDENCY_NETWORK_CHECK=1."
    exit 1
}

# 5. Check git commit messages and PR description for dependency relationships
$commits = git log "$BaseBranch...HEAD" --pretty=format:"%B" 2>$null
if (-not $commits) {
    $commits = git log -n 5 --pretty=format:"%B"
}

# Known dependency DAG: Issue #71 (ruleset whitelist) is blocked by Issue #74 (red-team property tests)
if ($commits -match "#71" -or $changedFiles -match "default.toml|rules.toml") {
    Write-Host "Verifying prerequisite dependency: Issue #74 (Security red-team tests for #71)..." -ForegroundColor Cyan
    
    $issue74Raw = gh issue view 74 --json state,title 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Could not connect to GitHub API ($issue74Raw). If working offline, re-run with -SkipNetworkCheck or set NEBULA_SKIP_DEPENDENCY_NETWORK_CHECK=1."
        exit 1
    }

    $issue74Json = $issue74Raw | ConvertFrom-Json
    if ($issue74Json.state -ne "CLOSED") {
        Write-Host ""
        Write-Host "==========================================================================" -ForegroundColor Red
        Write-Host "CI / PRE-PUSH GATE FAILURE: PR DEPENDENCY UNRESOLVED" -ForegroundColor Red
        Write-Host "==========================================================================" -ForegroundColor Red
        Write-Host "This PR modifies Protected Set permission rules (Issue #71)." -ForegroundColor Red
        Write-Host "Prerequisite Issue #74 ($($issue74Json.title)) is still OPEN." -ForegroundColor Red
        Write-Host "Protected Set Rule 9 requires security property tests (#74) to be merged" -ForegroundColor Red
        Write-Host "into main BEFORE permission rules (#71) can be introduced." -ForegroundColor Red
        Write-Host "==========================================================================" -ForegroundColor Red
        exit 1
    }

    Write-Host "Prerequisite Issue #74 is CLOSED (merged). Gate satisfied." -ForegroundColor Green
}

Write-Host "Protected Set & Dependency Check passed successfully." -ForegroundColor Green
exit 0
