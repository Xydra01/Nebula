# Design: GitHub Flow — Nebula-dev-bot Branches and PRs

## Architecture

The GitHub flow is housed in `crates/nebula-daemon/src/github/`, structured into modular components:

```
crates/nebula-daemon/src/github/
├── mod.rs          # Entry points: GithubService, handle_task_completion
├── safety.rs       # Branch name validation, force-push refusal, refspec sanitization
├── push.rs         # Git push execution with child-process extraheader auth
├── api.rs          # GitHub REST API client (find PR, create PR, update PR)
└── format.rs       # PR markdown body & trailer formatting
```

### 1. Safety & Validation (`safety.rs`)

```rust
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PushSafetyError {
    #[error("refusing to push protected branch '{0}'")]
    ProtectedBranch(String),
    #[error("refusing to push base branch '{0}'")]
    BaseBranch(String),
    #[error("force-push is strictly refused")]
    ForcePushRefused,
    #[error("branch '{branch}' does not start with required prefix '{expected_prefix}'")]
    InvalidBranchPrefix {
        branch: String,
        expected_prefix: String,
    },
    #[error("invalid branch ref syntax: '{0}'")]
    InvalidRefSyntax(String),
}
```

Validation rules:
- Branch cannot be empty.
- Branch cannot equal `main`, `master`, or configured `base_branch`.
- Branch cannot start with `+`.
- Branch must start with `branch_prefix` (e.g., `nebula/`).
- Branch ref syntax must not contain illegal git characters (`..`, `~`, `^`, `:`, `?`, `*`, `[`, `\`, control chars, space).

Refspec is strictly built as:
`refs/heads/<branch>:refs/heads/<branch>`
No `--force` or `-f` flags are ever passed to `git push`.

### 2. Push Engine with Isolated Authentication (`push.rs`)

To prevent the secret token from ever leaking into process tables, command line arguments, git remotes, or disk config, authentication is provided via `git`'s `GIT_CONFIG_*` environment variables scoped exclusively to the child process:

```rust
let basic_auth = base64_encode(format!("x-access-token:{token}"));
let mut cmd = tokio::process::Command::new("git");
cmd.current_dir(repo_root)
    .arg("push")
    .arg(&remote_url)
    .arg(&format!("refs/heads/{branch}:refs/heads/{branch}"))
    .env("GIT_CONFIG_COUNT", "1")
    .env("GIT_CONFIG_KEY_0", "http.https://github.com/.extraheader")
    .env("GIT_CONFIG_VALUE_0", format!("AUTHORIZATION: basic {basic_auth}"))
    .kill_on_drop(true);
```

### 3. GitHub REST API Client (`api.rs`)

Interacts with GitHub REST API (`https://api.github.com` or local mock endpoint in tests):
- `POST /repos/{owner}/{repo}/pulls`
  - Body: `{ "title": title, "body": body, "head": head, "base": base }`
  - Headers:
    - `Authorization: Bearer <token>`
    - `User-Agent: Nebula-dev-bot`
    - `X-GitHub-Api-Version: 2022-11-28`
- `GET /repos/{owner}/{repo}/pulls?head={owner}:{head}&state=open`
  - Used to detect existing PRs for follow-up commits.

### 4. PR Formatting (`format.rs`)

Formats PR markdown:
- Title: Concise summary of task goal.
- Body:
  - Task summary and final status.
  - Trace ID (`trace_id`).
  - Verification & test results.
  - Co-author attribution trailers (`Co-authored-by: Nebula-dev-bot <bot@nebula>`).

### 5. Configuration (`[github]`)

```toml
[github]
enabled = false
repo = "Xydra01/Nebula"
base_branch = "main"
branch_prefix = "nebula/"
token_target = "nebula/github_bot_token"
bot_username = "Nebula-dev-bot"
```
