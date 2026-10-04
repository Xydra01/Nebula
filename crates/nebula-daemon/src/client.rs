//! A client for the daemon's pipe, used by the CLI and the tests.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use nebula_proto::{Event, Message, Method, ProtoError, Request, RequestId, TraceId};
use serde::de::DeserializeOwned;
use tokio::io::{AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
use tokio_util::codec::{FramedRead, LinesCodec};

/// `ERROR_PIPE_BUSY`: every instance is connected; retry.
const ERROR_PIPE_BUSY: i32 = 231;
/// Longest accepted line from the daemon.
const MAX_LINE: usize = 64 * 1024 * 1024;

/// Client failures.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The pipe doesn't exist: the daemon isn't running.
    #[error("the Nebula daemon is not running (no pipe {0})")]
    NotRunning(String),
    /// Pipe I/O failed.
    #[error("pipe: {0}")]
    Io(#[from] std::io::Error),
    /// A bad message, or an error response from the daemon.
    #[error(transparent)]
    Proto(#[from] ProtoError),
    /// The daemon closed the connection.
    #[error("the daemon closed the connection")]
    Closed,
}

impl ClientError {
    /// The daemon's error code, for error responses.
    #[must_use]
    pub fn rpc_code(&self) -> Option<i64> {
        match self {
            Self::Proto(ProtoError::Rpc(e)) => Some(e.code),
            _ => None,
        }
    }
}

/// One connection. Events that arrive while waiting for a response are queued for
/// [`Client::next_event`].
pub struct Client {
    lines: FramedRead<ReadHalf<NamedPipeClient>, LinesCodec>,
    writer: WriteHalf<NamedPipeClient>,
    next_id: u64,
    events: VecDeque<Event>,
}

impl Client {
    /// Connects to `path`, retrying while the pipe is busy for up to `timeout`.
    ///
    /// # Errors
    /// [`ClientError::NotRunning`] if the pipe doesn't exist; I/O errors otherwise.
    pub async fn connect(path: &str, timeout: Duration) -> Result<Self, ClientError> {
        let deadline = Instant::now() + timeout;
        let pipe = loop {
            match ClientOptions::new().open(path) {
                Ok(p) => break p,
                Err(e)
                    if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(ClientError::NotRunning(path.to_owned()));
                }
                Err(e) => return Err(e.into()),
            }
        };
        let (rd, writer) = tokio::io::split(pipe);
        Ok(Self {
            lines: FramedRead::new(rd, LinesCodec::new_with_max_length(MAX_LINE)),
            writer,
            next_id: 1,
            events: VecDeque::new(),
        })
    }

    /// Writes one raw line (tests use this for malformed input).
    ///
    /// # Errors
    /// Pipe errors.
    pub async fn send_line(&mut self, line: &str) -> Result<(), ClientError> {
        self.writer.write_all(line.as_bytes()).await?;
        if !line.ends_with('\n') {
            self.writer.write_all(b"\n").await?;
        }
        Ok(())
    }

    /// Reads the next message of any kind.
    ///
    /// # Errors
    /// [`ClientError::Closed`] at end of stream, or I/O and decode errors.
    pub async fn read_message(&mut self) -> Result<Message, ClientError> {
        match self.lines.next().await {
            None => Err(ClientError::Closed),
            Some(Err(e)) => Err(std::io::Error::other(e).into()),
            Some(Ok(line)) => Ok(Message::decode(&line)?),
        }
    }

    /// Calls `method` and decodes its result; events received meanwhile are queued.
    ///
    /// # Errors
    /// [`ProtoError::Rpc`] (inside [`ClientError::Proto`]) for an error response.
    pub async fn call<T: DeserializeOwned>(&mut self, method: Method) -> Result<T, ClientError> {
        self.call_traced(method, None).await
    }

    /// [`Client::call`] with a trace ID.
    ///
    /// # Errors
    /// See [`Client::call`].
    pub async fn call_traced<T: DeserializeOwned>(
        &mut self,
        method: Method,
        trace_id: Option<TraceId>,
    ) -> Result<T, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let mut req = Request::new(id, method);
        req.trace_id = trace_id;
        self.send_line(&Message::Request(req).encode()?).await?;
        loop {
            match self.read_message().await? {
                Message::Response(r) if r.id == RequestId::Num(id) => return Ok(r.into_result()?),
                Message::Notification(n) => self.events.push_back(n.event),
                Message::Response(_) | Message::Request(_) => {}
            }
        }
    }

    /// The next event, queued or read.
    ///
    /// # Errors
    /// [`ClientError::Closed`] at end of stream.
    pub async fn next_event(&mut self) -> Result<Event, ClientError> {
        if let Some(e) = self.events.pop_front() {
            return Ok(e);
        }
        loop {
            if let Message::Notification(n) = self.read_message().await? {
                return Ok(n.event);
            }
        }
    }
}
