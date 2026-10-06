//! Starting tool-server processes. The host goes through [`ToolLauncher`] so tests can
//! substitute an in-process fake.
//!
//! Mirrors `nebula_model::launcher`: each real process is placed in a Windows Job Object with
//! `KILL_ON_JOB_CLOSE`, so a crashed daemon never leaks tool servers. Unlike the model
//! launcher, stdin is piped as well as stdout, because MCP speaks JSON-RPC over the child's
//! standard streams.

use std::path::PathBuf;
use std::process::Stdio;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::Command;

use crate::ToolError;

/// Everything needed to start one tool server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchSpec {
    /// Manifest name, used only for error messages.
    pub server: String,
    /// Executable.
    pub program: PathBuf,
    /// Arguments.
    pub args: Vec<String>,
    /// Extra environment variables.
    pub env: Vec<(String, String)>,
}

/// The child's piped standard streams, split for concurrent read and write.
pub struct ChildIo {
    /// Write JSON-RPC requests here (the child's stdin).
    pub stdin: Box<dyn AsyncWrite + Send + Unpin>,
    /// Read JSON-RPC responses from here (the child's stdout).
    pub stdout: Box<dyn AsyncRead + Send + Unpin>,
}

/// Starts tool-server processes.
#[async_trait]
pub trait ToolLauncher: Send + Sync {
    /// Starts a server and returns a handle plus its piped stdio. Returning does not mean the
    /// server has finished its MCP handshake.
    async fn launch(&self, spec: LaunchSpec)
    -> Result<(Box<dyn ChildProcess>, ChildIo), ToolError>;
}

/// A running (or exited) tool-server process.
#[async_trait]
pub trait ChildProcess: Send {
    /// OS process id, if there is one.
    fn pid(&self) -> Option<u32>;
    /// `Some(description)` once the process has exited.
    fn try_exit(&mut self) -> Option<String>;
    /// Kills the process and waits for it to exit.
    async fn stop(&mut self);
}

/// Launches real processes, each in a Windows Job Object that kills it if Nebula dies.
#[derive(Clone, Copy, Debug, Default)]
pub struct StdioLauncher;

#[async_trait]
impl ToolLauncher for StdioLauncher {
    #[tracing::instrument(level = "debug", skip_all, fields(server = %spec.server, program = %spec.program.display()))]
    async fn launch(
        &self,
        spec: LaunchSpec,
    ) -> Result<(Box<dyn ChildProcess>, ChildIo), ToolError> {
        let launch_err = |detail: String| ToolError::Launch {
            server: spec.server.clone(),
            detail,
        };
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args)
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| launch_err(format!("{}: {e}", spec.program.display())))?;
        #[cfg(windows)]
        let job = {
            let job = job::Job::new().map_err(|e| launch_err(format!("job object: {e}")))?;
            if let Some(h) = child.raw_handle() {
                job.assign(h)
                    .map_err(|e| launch_err(format!("assign job object: {e}")))?;
            }
            job
        };
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| launch_err("child stdin was not piped".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| launch_err("child stdout was not piped".to_owned()))?;
        let io = ChildIo {
            stdin: Box::new(stdin),
            stdout: Box::new(stdout),
        };
        let proc = Process {
            pid: child.id(),
            child,
            #[cfg(windows)]
            _job: job,
        };
        Ok((Box::new(proc), io))
    }
}

struct Process {
    pid: Option<u32>,
    child: tokio::process::Child,
    // Dropped after `child`: closing the job kills anything still running in it.
    #[cfg(windows)]
    _job: job::Job,
}

#[async_trait]
impl ChildProcess for Process {
    fn pid(&self) -> Option<u32> {
        self.pid
    }

    fn try_exit(&mut self) -> Option<String> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status.to_string()),
            Ok(None) => None,
            Err(e) => Some(format!("wait failed: {e}")),
        }
    }

    async fn stop(&mut self) {
        let _ = self.child.start_kill();
        if tokio::time::timeout(std::time::Duration::from_secs(10), self.child.wait())
            .await
            .is_err()
        {
            tracing::error!(event = "tool.stop_timeout", pid = self.pid);
        }
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod job {
    //! A Job Object with `KILL_ON_JOB_CLOSE`: when the last handle closes (including when
    //! Nebula crashes), Windows terminates every process in it. Identical in intent to the
    //! model launcher's job module; kept separate so each crate owns its own Win32 surface.

    use std::io;
    use std::os::windows::io::RawHandle;

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    use windows::core::PCWSTR;

    pub(super) struct Job(HANDLE);

    // SAFETY: a job handle is a kernel handle usable from any thread.
    unsafe impl Send for Job {}

    fn win_err(e: &windows::core::Error) -> io::Error {
        io::Error::from_raw_os_error(e.code().0)
    }

    impl Job {
        pub(super) fn new() -> io::Result<Self> {
            // SAFETY: no security attributes and no name; the returned handle is owned here.
            let handle =
                unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(|e| win_err(&e))?;
            let job = Self(handle);
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                .map_err(io::Error::other)?;
            // SAFETY: `info` is a valid JOBOBJECT_EXTENDED_LIMIT_INFORMATION of `size` bytes
            // that outlives the call.
            unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    (&raw const info).cast(),
                    size,
                )
            }
            .map_err(|e| win_err(&e))?;
            Ok(job)
        }

        pub(super) fn assign(&self, process: RawHandle) -> io::Result<()> {
            // SAFETY: both handles are valid for the duration of the call; the process handle
            // is borrowed from a live `Child`.
            unsafe { AssignProcessToJobObject(self.0, HANDLE(process)) }.map_err(|e| win_err(&e))
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: the handle was created by `new` and is closed exactly once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}
