//! JSON-RPC 2.0 envelopes and newline-delimited framing.

use serde::de::{self, DeserializeOwned};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::events::Event;
use crate::ids::TraceId;
use crate::methods::Method;

/// Version of this protocol. Bump on any incompatible change to a message.
///
/// v2 added `tools.list` and `tools.call` (the MCP tool host).
pub const PROTO_VERSION: u32 = 2;

/// Error codes. The negative 32xxx range is JSON-RPC's; -32000 to -32099 are Nebula's.
pub mod error_code {
    /// Invalid JSON.
    pub const PARSE_ERROR: i64 = -32700;
    /// Not a valid request object.
    pub const INVALID_REQUEST: i64 = -32600;
    /// Unknown method.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Bad parameters.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Daemon-side failure.
    pub const INTERNAL_ERROR: i64 = -32603;
    /// The peer speaks a different `proto_version`.
    pub const VERSION_MISMATCH: i64 = -32000;
    /// The model server is not ready (starting, restarting, failed or unloaded).
    pub const MODEL_UNAVAILABLE: i64 = -32001;
    /// The request conflicts with work in progress.
    pub const BUSY: i64 = -32002;
    /// The operation was cancelled.
    pub const CANCELLED: i64 = -32003;
    /// The daemon is shutting down.
    pub const SHUTTING_DOWN: i64 = -32004;
    /// Unknown profile or chat ID.
    pub const NOT_FOUND: i64 = -32005;
}

/// The literal `"2.0"` that every JSON-RPC message carries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JsonRpcVersion;

impl Serialize for JsonRpcVersion {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("2.0")
    }
}

impl<'de> Deserialize<'de> for JsonRpcVersion {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = String::deserialize(d)?;
        if v == "2.0" {
            Ok(Self)
        } else {
            Err(de::Error::invalid_value(
                de::Unexpected::Str(&v),
                &"\"2.0\"",
            ))
        }
    }
}

/// A request ID: a number or a string, echoed back in the response.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    /// Numeric ID (what Nebula's own clients send).
    Num(u64),
    /// String ID.
    Str(String),
}

impl From<u64> for RequestId {
    fn from(n: u64) -> Self {
        Self::Num(n)
    }
}

/// A client call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// Always `"2.0"`.
    pub jsonrpc: JsonRpcVersion,
    /// Echoed in the response.
    pub id: RequestId,
    /// Sender's protocol version.
    pub proto_version: u32,
    /// Method and parameters.
    #[serde(flatten)]
    pub call: Method,
    /// Trace to attach the work to; the daemon starts a new one if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<TraceId>,
}

impl Request {
    /// A request at the current protocol version, with no trace.
    pub fn new(id: impl Into<RequestId>, call: Method) -> Self {
        Self {
            jsonrpc: JsonRpcVersion,
            id: id.into(),
            proto_version: PROTO_VERSION,
            call,
            trace_id: None,
        }
    }
}

/// A JSON-RPC error object.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcError {
    /// One of [`error_code`].
    pub code: i64,
    /// Human-readable message.
    pub message: String,
    /// Extra detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    /// An error without `data`.
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

/// Either the method's result or an error.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// Success; the method's result type, as JSON.
    Result(Value),
    /// Failure.
    Error(RpcError),
}

/// The daemon's answer to one [`Request`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// Always `"2.0"`.
    pub jsonrpc: JsonRpcVersion,
    /// The request's ID.
    pub id: RequestId,
    /// Sender's protocol version.
    pub proto_version: u32,
    /// Result or error.
    #[serde(flatten)]
    pub outcome: Outcome,
}

impl Response {
    /// A success response carrying `result`.
    ///
    /// # Errors
    /// If `result` cannot be serialized to JSON.
    pub fn ok<T: Serialize>(id: RequestId, result: &T) -> Result<Self, ProtoError> {
        Ok(Self {
            jsonrpc: JsonRpcVersion,
            id,
            proto_version: PROTO_VERSION,
            outcome: Outcome::Result(serde_json::to_value(result)?),
        })
    }

    /// An error response.
    #[must_use]
    pub fn err(id: RequestId, error: RpcError) -> Self {
        Self {
            jsonrpc: JsonRpcVersion,
            id,
            proto_version: PROTO_VERSION,
            outcome: Outcome::Error(error),
        }
    }

    /// The typed result, or the daemon's error.
    ///
    /// # Errors
    /// [`ProtoError::Rpc`] for an error response; [`ProtoError::Json`] if the result doesn't
    /// match `T`.
    pub fn into_result<T: DeserializeOwned>(self) -> Result<T, ProtoError> {
        match self.outcome {
            Outcome::Result(v) => Ok(serde_json::from_value(v)?),
            Outcome::Error(e) => Err(ProtoError::Rpc(e)),
        }
    }
}

/// A daemon event; no reply is expected.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    /// Always `"2.0"`.
    pub jsonrpc: JsonRpcVersion,
    /// Sender's protocol version.
    pub proto_version: u32,
    /// Event name and payload.
    #[serde(flatten)]
    pub event: Event,
}

impl Notification {
    /// A notification at the current protocol version.
    #[must_use]
    pub const fn new(event: Event) -> Self {
        Self {
            jsonrpc: JsonRpcVersion,
            proto_version: PROTO_VERSION,
            event,
        }
    }
}

/// Errors from framing, decoding and typed results.
#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    /// Malformed JSON or a payload that doesn't match its type.
    #[error("invalid message: {0}")]
    Json(#[from] serde_json::Error),
    /// Valid JSON, but not a JSON-RPC request, response or notification.
    #[error("not a JSON-RPC message: {0}")]
    Shape(&'static str),
    /// A request for a method this daemon doesn't have.
    #[error("unknown method {0:?}")]
    UnknownMethod(String),
    /// The peer speaks another protocol version.
    #[error("protocol version mismatch: peer {peer}, ours {ours}")]
    VersionMismatch {
        /// Peer's version.
        peer: u32,
        /// [`PROTO_VERSION`].
        ours: u32,
    },
    /// An error response from the daemon.
    #[error("daemon error {}: {}", .0.code, .0.message)]
    Rpc(RpcError),
}

impl ProtoError {
    /// The JSON-RPC error to send back for a message that failed to decode.
    #[must_use]
    pub fn to_rpc_error(&self) -> RpcError {
        match self {
            Self::Json(e) if e.is_syntax() || e.is_eof() => {
                RpcError::new(error_code::PARSE_ERROR, self.to_string())
            }
            Self::Json(_) => RpcError::new(error_code::INVALID_PARAMS, self.to_string()),
            Self::Shape(_) => RpcError::new(error_code::INVALID_REQUEST, self.to_string()),
            Self::UnknownMethod(_) => RpcError::new(error_code::METHOD_NOT_FOUND, self.to_string()),
            Self::VersionMismatch { .. } => {
                RpcError::new(error_code::VERSION_MISMATCH, self.to_string())
            }
            Self::Rpc(e) => e.clone(),
        }
    }
}

/// Any message on the pipe.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    /// Client call.
    Request(Request),
    /// Answer to a call.
    Response(Response),
    /// Event.
    Notification(Notification),
}

impl Message {
    /// Serializes to one line of JSON, terminated by `\n`. JSON string escaping guarantees the
    /// line contains no other newline.
    ///
    /// # Errors
    /// If a payload cannot be serialized.
    pub fn encode(&self) -> Result<String, ProtoError> {
        let mut line = match self {
            Self::Request(r) => serde_json::to_string(r)?,
            Self::Response(r) => serde_json::to_string(r)?,
            Self::Notification(n) => serde_json::to_string(n)?,
        };
        line.push('\n');
        Ok(line)
    }

    /// Parses one line (with or without the trailing newline).
    ///
    /// The kind is decided by the members present: `id` + `method` is a request, `id` +
    /// `result`/`error` a response, `method` alone a notification. The protocol version is
    /// checked before the payload, so a newer peer gets [`ProtoError::VersionMismatch`] rather
    /// than a confusing parameter error.
    ///
    /// # Errors
    /// [`ProtoError`] describing why the line is not a valid message.
    pub fn decode(line: &str) -> Result<Self, ProtoError> {
        let mut value: Value = serde_json::from_str(line.trim_end_matches(['\n', '\r']))?;
        let obj = value
            .as_object_mut()
            .ok_or(ProtoError::Shape("expected a JSON object"))?;
        if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(ProtoError::Shape("missing \"jsonrpc\": \"2.0\""));
        }
        let peer = obj
            .get("proto_version")
            .and_then(Value::as_u64)
            .ok_or(ProtoError::Shape("missing or invalid proto_version"))?;
        if peer != u64::from(PROTO_VERSION) {
            return Err(ProtoError::VersionMismatch {
                peer: u32::try_from(peer).unwrap_or(u32::MAX),
                ours: PROTO_VERSION,
            });
        }
        let kind = {
            let has = |k: &str| obj.contains_key(k);
            if has("result") && has("error") {
                return Err(ProtoError::Shape("both result and error"));
            }
            (has("id"), has("method"), has("result") || has("error"))
        };
        if kind.1 {
            // JSON-RPC allows omitting `params`; every parameterless method takes `{}`.
            obj.entry("params")
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
        }
        match kind {
            (true, true, false) => {
                let method = obj
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !Method::NAMES.contains(&method) {
                    return Err(ProtoError::UnknownMethod(method.to_owned()));
                }
                Ok(Self::Request(serde_json::from_value(value)?))
            }
            (true, false, true) => Ok(Self::Response(serde_json::from_value(value)?)),
            (false, true, false) => Ok(Self::Notification(serde_json::from_value(value)?)),
            _ => Err(ProtoError::Shape(
                "ambiguous members (id/method/result/error)",
            )),
        }
    }
}
