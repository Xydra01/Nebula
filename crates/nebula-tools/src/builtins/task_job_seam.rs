//! The per-task Job Object spawn seam: the `CURRENT_TASK_JOB` task-local and
//! [`spawn_in_task_job`], the Task-scoped successor to the per-process `spawn_in_job` from issue
//! #27 (GitHub issue #30, design "the per-task context and spawn seam").
//!
//! # Why this lives in `nebula-tools`, not `nebula-sandbox`
//!
//! The audited `unsafe` Win32 Job Object FFI lives in [`nebula_sandbox::job`], and the per-task
//! [`TaskJob`](nebula_sandbox::task_job::TaskJob) wrapper in `nebula_sandbox::task_job`. But the
//! *seam* `shell.run` and `git.*` call — and the `CURRENT_TASK_JOB` task-local the executor sets
//! around a task's work — belong here, beside the built-ins that use them and alongside the
//! `CURRENT_WORKTREE` task-local (issue #29), for the same reason: `nebula-tools` depends on
//! `nebula-sandbox`, never the reverse, so the task-scoped glue sits on the `nebula-tools` side of
//! the dependency edge. The executor (issue #32) sets **both** task-locals in one scope:
//!
//! ```ignore
//! let job = Arc::new(TaskJob::new(task_id, limits)?);          // nebula-sandbox
//! CURRENT_WORKTREE
//!     .scope(Some(root), CURRENT_TASK_JOB.scope(Some(job), async {
//!         run_task_tool_calls().await  // shell.run/git.* spawn into `job`, confined to `root`
//!     }))
//!     .await;
//! ```
//!
//! # The spawn flow
//!
//! [`spawn_in_task_job`] reads `CURRENT_TASK_JOB`. With a job in scope it spawns the child
//! **suspended** ([`nebula_sandbox::job::spawn_suspended`]), **assigns** it to the task's job, and
//! only then **resumes** it — so the child cannot run, and therefore cannot spawn a descendant,
//! before it is a job member (Requirement 4). With no job in scope it fails closed with
//! [`JobError::NoTaskInScope`] and spawns nothing (Requirement 10.4), mirroring the worktree
//! provider's rejected-sentinel philosophy.

#[cfg(windows)]
use nebula_sandbox::task_job::JobError;
#[cfg(windows)]
use std::sync::Arc;

#[cfg(windows)]
use nebula_sandbox::task_job::TaskJob;

tokio::task_local! {
    /// The [`TaskJob`](nebula_sandbox::task_job::TaskJob) of the task currently executing, or
    /// `None` when no task is in scope.
    ///
    /// The executor (issue #32) sets this around all of a task's tool-calling work via
    /// `CURRENT_TASK_JOB.scope(Some(job), fut)`, alongside the `CURRENT_WORKTREE` task-local from
    /// issue #29. Each `tokio` task carries its own copy, so concurrent tasks spawn into their own
    /// jobs through the single shared seam with no shared mutable "current job" to race on.
    #[cfg(windows)]
    pub static CURRENT_TASK_JOB: Option<Arc<TaskJob>>;
}

/// A task child kept inside its task's [`TaskJob`](nebula_sandbox::task_job::TaskJob).
///
/// Mirrors issue #27's per-process `JobChild`, but the job is the per-task job rather than a fresh
/// per-process one: holding this guard does not keep the job alive beyond the executor's owning
/// `Arc<TaskJob>`, and the child dies when the task's job is closed (cancel or daemon death). The
/// `wait_with_timeout` API matches the one `shell.run` / `git.*` already use, so their call sites
/// migrate with no behavioural change.
#[cfg(windows)]
pub struct JobChild {
    child: tokio::process::Child,
    // Keeps the task's job referenced for the child's lifetime; dropped with the `JobChild`.
    _job: Arc<TaskJob>,
}

#[cfg(windows)]
impl JobChild {
    /// The child's OS process id, if it has one.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Wait for the child to exit, collecting its captured stdout/stderr, but give up after
    /// `timeout`.
    ///
    /// Returns `Ok(Some(output))` if the child exits in time, `Ok(None)` if `timeout` elapses first
    /// (the caller drops the `JobChild`; the child is terminated when the task's job closes), or
    /// `Err` if waiting on the child fails. Identical in shape to issue #27's `JobChild`.
    pub async fn wait_with_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> std::io::Result<Option<std::process::Output>> {
        let stdout = self.child.stdout.take();
        let stderr = self.child.stderr.take();
        let wait = async {
            let status = self.child.wait().await?;
            let mut out = Vec::new();
            let mut err = Vec::new();
            if let Some(mut s) = stdout {
                use tokio::io::AsyncReadExt as _;
                s.read_to_end(&mut out).await?;
            }
            if let Some(mut s) = stderr {
                use tokio::io::AsyncReadExt as _;
                s.read_to_end(&mut err).await?;
            }
            Ok(std::process::Output {
                status,
                stdout: out,
                stderr: err,
            })
        };
        match tokio::time::timeout(timeout, wait).await {
            Ok(result) => result.map(Some),
            Err(_elapsed) => Ok(None),
        }
    }
}

/// Spawn `cmd` into the current task's [`TaskJob`](nebula_sandbox::task_job::TaskJob):
/// suspended spawn → assign → resume (Requirement 4).
///
/// Reads the `CURRENT_TASK_JOB` task-local the executor scoped around the task's work. With a job
/// in scope, the child is spawned suspended, assigned to the job while still suspended, and only
/// then resumed, so no descendant can escape the job. With no job in scope, nothing is spawned and
/// [`JobError::NoTaskInScope`] is returned (Requirement 10.4). The caller configures `cmd` (program,
/// args, env, stdio, `current_dir`); the seam adds `CREATE_SUSPENDED | CREATE_NO_WINDOW` and
/// `kill_on_drop(true)` via [`nebula_sandbox::job::spawn_suspended`].
///
/// # Errors
/// [`JobError::NoTaskInScope`] if no task is in scope (nothing is spawned); [`JobError::Spawn`] if
/// the process cannot start; [`JobError::Assign`] if assignment fails — the still-suspended child is
/// terminated before it can run, leaving no descendant (Requirement 4.5, 4.6);
/// [`JobError::ExitedBeforeAssign`] if the child exits before assignment is confirmed
/// (Requirement 2.7).
#[cfg(windows)]
pub fn spawn_in_task_job(cmd: &mut tokio::process::Command) -> Result<JobChild, JobError> {
    // Read the current task's job; clone the `Arc` only to hold it for the child's lifetime.
    let job = CURRENT_TASK_JOB
        .try_with(Clone::clone)
        .ok()
        .flatten()
        .ok_or(JobError::NoTaskInScope)?;

    // Spawn suspended so the child cannot run — and cannot spawn a descendant — before it joins the
    // job (Requirement 4.1).
    let (child, resume) =
        nebula_sandbox::job::spawn_suspended(cmd).map_err(|e| JobError::Spawn(e.to_string()))?;

    // Assign while still suspended. On failure the child is killed before it ever runs: dropping
    // `resume` without calling `resume()` leaves the thread suspended, and dropping `child`
    // (`kill_on_drop`) terminates it, so no descendant is left behind (Requirement 4.5, 4.6).
    if let Err(e) = job.assign(child_raw_handle(&child)?) {
        drop(resume);
        drop(child);
        return Err(e);
    }

    // Assigned: release the child to run (Requirement 4.3). Only reached on the `Ok` assign path.
    resume.resume().map_err(|e| JobError::Assign {
        task_id: job.task_id().to_owned(),
        detail: format!("resume failed after assignment: {e}"),
    })?;

    Ok(JobChild { child, _job: job })
}

/// The child's raw process handle, or [`JobError::ExitedBeforeAssign`] if it has already exited
/// (so the seam never reports a successful spawn for a child it could not assign, Requirement 2.7).
#[cfg(windows)]
fn child_raw_handle(
    child: &tokio::process::Child,
) -> Result<std::os::windows::io::RawHandle, JobError> {
    child.raw_handle().ok_or(JobError::ExitedBeforeAssign)
}

#[cfg(all(test, windows))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{CURRENT_TASK_JOB, spawn_in_task_job};
    use nebula_sandbox::task_job::{JobError, JobLimits, TaskJob};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::process::Command;

    /// Whether a PID is still a live process. Uses `tasklist` filtered to the PID; a running PID
    /// appears in the output, a dead one does not. Avoids extra Win32 FFI outside the audited
    /// `nebula-sandbox::job` module.
    async fn pid_is_alive(pid: u32) -> bool {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .await
            .expect("tasklist runs");
        let text = String::from_utf8_lossy(&out.stdout);
        text.contains(&format!("\"{pid}\""))
    }

    /// Feature: job-objects-for-tasks, Property 10 (seam half): with no task in scope the seam
    /// spawns nothing and returns `NoTaskInScope` (Requirement 10.4).
    #[tokio::test]
    async fn no_task_in_scope_spawns_nothing() {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", "echo", "hi"]);
        match spawn_in_task_job(&mut cmd) {
            Err(JobError::NoTaskInScope) => {}
            Err(other) => panic!("expected NoTaskInScope, got error {other:?}"),
            Ok(_) => {
                panic!("expected NoTaskInScope with no task in scope, but a child was spawned")
            }
        }
    }

    /// Feature: job-objects-for-tasks, Property 1: no survivors after a Task_Job closes.
    ///
    /// Validates: Requirements 3.1, 3.2, 5.1, 5.3, 6.3. A task child launches a grandchild that
    /// would outlive its parent (a long `ping`); both join the task's one job by inherited
    /// membership. Dropping the `Arc<TaskJob>` (what the executor does on cancel) closes the job,
    /// and the OS terminates the whole tree — no survivors — well within the 2s bound.
    #[tokio::test]
    async fn closing_the_task_job_leaves_no_survivors() {
        let job = Arc::new(TaskJob::new("task-no-survivors", JobLimits::default()).unwrap());

        // A parent cmd that starts a detached grandchild (`ping -n 60` ~ 59s) and then exits. The
        // grandchild would survive its parent, but it is a member of the task's job by inheritance,
        // so closing the job must kill it too.
        let grandchild_pid = CURRENT_TASK_JOB
            .scope(Some(Arc::clone(&job)), async {
                let mut cmd = Command::new("cmd");
                // `start /B` launches the grandchild without waiting; it keeps running after cmd exits.
                cmd.args(["/C", "start", "/B", "ping", "-n", "60", "127.0.0.1"]);
                let mut child = spawn_in_task_job(&mut cmd).expect("spawn into task job");
                // Let the parent run and launch the grandchild, then finish.
                let _ = child.wait_with_timeout(Duration::from_secs(5)).await;
                // Find the ping grandchild's PID (a member of the same job).
                find_ping_pid().await
            })
            .await;

        let Some(pid) = grandchild_pid else {
            // If the grandchild could not be observed (environment without `start`/`ping`), the
            // test cannot prove the property; fail loudly rather than pass vacuously.
            panic!("could not observe the ping grandchild to prove no-survivors");
        };

        assert!(
            pid_is_alive(pid).await,
            "the grandchild must be running before the job is closed",
        );

        // Cancel the task: drop the only owning handle. Kill-on-close terminates the whole tree.
        drop(job);

        // Assert no survivor within the 2s bound (poll a few times).
        let mut alive = true;
        for _ in 0..20 {
            if !pid_is_alive(pid).await {
                alive = false;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            !alive,
            "the grandchild (pid {pid}) must be terminated within 2s of closing the task job",
        );
    }

    /// Find a running `ping.exe` PID, if any. Best-effort helper for the no-survivors test.
    async fn find_ping_pid() -> Option<u32> {
        let out = Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq PING.EXE", "/NH", "/FO", "CSV"])
            .output()
            .await
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        // CSV: "PING.EXE","1234",... — the second field is the PID.
        for line in text.lines() {
            let fields: Vec<&str> = line.split("\",\"").collect();
            if let Some(pid_field) = fields.get(1) {
                let pid_str = pid_field.trim_matches('"');
                if let Ok(pid) = pid_str.parse::<u32>() {
                    return Some(pid);
                }
            }
        }
        None
    }
}
