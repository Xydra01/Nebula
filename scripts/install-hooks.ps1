# Points git at the repo's tracked hooks in .githooks/ (gitleaks pre-commit and pre-push).
$ErrorActionPreference = 'Stop'
$root = (git rev-parse --show-toplevel).Trim()
if (-not (Get-Command gitleaks -ErrorAction SilentlyContinue)) {
    throw 'gitleaks is not on PATH. Install it with: winget install Gitleaks.Gitleaks'
}
git -C $root config core.hooksPath .githooks
Write-Host "Hooks installed: core.hooksPath=.githooks ($(gitleaks version))"
