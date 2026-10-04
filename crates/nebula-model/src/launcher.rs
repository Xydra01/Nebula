//! Starting llama-server processes. The supervisor goes through [`Launcher`] so tests can
//! substitute an in-process fake.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};

use crate::ModelError;

/// Everything needed to start one server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchSpec {
    /// Executable.
    pub program: PathBuf,
    /// Arguments.
    pub args: Vec<String>,
    /// Extra environment variables (includes `LLAMA_API_KEY`).
    pub env: Vec<(String, String)>,
    /// Port the server listens on at `127.0.0.1`.
    pub port: u16,
}

/// Starts server processes.
#[async_trait]
pub trait Launcher: Send + Sync {
    /// Starts a server. Returning does not mean it is healthy yet.
    async fn launch(&self, spec: LaunchSpec) -> Result<Box<dyn ServerProcess>, ModelError>;
}

/// A running (or exited) server process.
#[async_trait]
pub trait ServerProcess: Send {
    /// OS process id, if there is one.
    fn pid(&self) -> Option<u32>;
    /// `Some(description)` once the process has exited.
    fn try_exit(&mut self) -> Option<String>;
    /// Kills the process and waits for it to exit, so its VRAM is free on return.
    async fn stop(&mut self);
    /// The last lines of stdout/stderr.
    fn recent_output(&self) -> Vec<String>;
}

/// Launches real processes, each in a Windows Job Object that kills it if Nebula dies.
#[derive(Clone, Debug)]
pub struct ProcessLauncher {
    /// Lines of output kept per process.
    pub output_lines: usize,
}

impl Default for ProcessLauncher {
    fn default() -> Self {
        Self { output_lines: 400 }
    }
}

type Lines = Arc<Mutex<VecDeque<String>>>;

fn spawn_reader(stream: impl AsyncRead + Unpin + Send + 'static, lines: Lines, cap: usize) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stream);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf).trim_end().to_owned();
                    let mut l = lines.lock().unwrap_or_else(PoisonError::into_inner);
                    if l.len() >= cap {
                        l.pop_front();
                    }
                    l.push_back(line);
                }
            }
        }
    });
}

#[async_trait]
impl Launcher for ProcessLauncher {
    #[tracing::instrument(level = "debug", skip_all, fields(program = %spec.program.display(), port = spec.port))]
    async fn launch(&self, spec: LaunchSpec) -> Result<Box<dyn ServerProcess>, ModelError> {
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args)
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| ModelError::Launch(format!("{}: {e}", spec.program.display())))?;
        #[cfg(windows)]
        let job = {
            let job =
                job::Job::new().map_err(|e| ModelError::Launch(format!("job object: {e}")))?;
            if let Some(h) = child.raw_handle() {
                job.assign(h)
                    .map_err(|e| ModelError::Launch(format!("assign job object: {e}")))?;
            }
            job
        };
        let lines: Lines = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(out) = child.stdout.take() {
            spawn_reader(out, Arc::clone(&lines), self.output_lines);
        }
        if let Some(err) = child.stderr.take() {
            spawn_reader(err, Arc::clone(&lines), self.output_lines);
        }
        Ok(Box::new(Process {
            pid: child.id(),
            child,
            lines,
            #[cfg(windows)]
            _job: job,
        }))
    }
}

struct Process {
    pid: Option<u32>,
    child: Child,
    lines: Lines,
    // Dropped after `child`: closing the job kills anything still running in it.
    #[cfg(windows)]
    _job: job::Job,
}

#[async_trait]
impl ServerProcess for Process {
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
        if tokio::time::timeout(Duration::from_secs(30), self.child.wait())
            .await
            .is_err()
        {
            tracing::error!(event = "model.stop_timeout", pid = self.pid);
        }
    }

    fn recent_output(&self) -> Vec<String> {
        let l = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        l.iter().cloned().collect()
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod job {
    //! A Job Object with `KILL_ON_JOB_CLOSE`: when the last handle closes (including when
    //! Nebula crashes), Windows terminates every process in it.

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
