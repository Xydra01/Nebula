# Requirements: GitHub Flow — Nebula-dev-bot Branches and PRs

## Introduction

This feature implements automated GitHub flow for Nebula (GitHub issue #36, design Section 9): pushing task branches and opening Pull Requests as `Nebula-dev-bot`.

In Phase 0, this was prototyped as a PowerShell script (`scripts/bot-push-pr.ps1`). In Phase 1, the daemon autonomously handles task execution via worktrees (issue #29) and executor loops (issue #32). Upon completing a task, Nebula retains the task branch and—when configured—pushes the branch and opens a Pull Request against `main` (or the configured base branch) with full attribution, trace ID, and test results.

## Scope

### In Scope
1. **Branch Push via Bot Token**:
   - Pushes the retained task branch to the remote repository.
   - Reads the bot token from Windows Credential Manager (`nebula/github_bot_token`).
   - Uses child-process isolated `GIT_CONFIG_*` environment variables so the token never appears in process arguments, remote URLs, or on-disk git config.
2. **Pull Request Creation & Updates**:
   - Opens a GitHub Pull Request with a title, markdown description, test results, and trace ID.
   - Includes co-author trailers (`Co-authored-by: Nebula-dev-bot <bot@nebula>`).
   - Supports follow-up commits: if a PR already exists for the task branch, updates the PR rather than opening a duplicate.
3. **Strict Safety Guardrails**:
   - Refuses in code any attempt to push to `main`, `master`, or base branch.
   - Refuses in code any force-pushes (`+` prefix, `--force`).
   - Refuses any branch outside the configured prefix (e.g. `nebula/`).
   - Refuses merges.
4. **Zero Token Leakage**:
   - Token is never placed in command-line arguments, git URLs, or configuration files.
   - Token is registered with the telemetry redactor and masked across all logs.
   - An automated test verifies that the token never appears in any log file.

### Out of Scope
- Automated merging (AGENTS.md hard rule 1: "Never push to main, force-push, or merge PRs. Work on a branch and open a PR. The owner reviews and merges everything").
- Interactive code review comments thread resolution.

## Acceptance Criteria

1. **Finished Task Produces PR**: A finished task produces a PR with a description, test results, and a trace ID.
2. **Push Restrictions Enforced in Code**: Attempts to push `main` or force-push are refused in code, not just by remote branch protection.
3. **No Token Leakage in Logs**: The token never appears in logs (verified by an automated test that greps log files).
