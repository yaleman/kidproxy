use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorKind {
    Config,
    FrontendTls,
    BackendTls,
    Dns,
    Connect,
    Timeout,
    UpstreamProtocol,
    ClientProtocol,
    SqliteWrite,
    Internal,
}

impl ErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::FrontendTls => "tls_frontend",
            Self::BackendTls => "tls_backend",
            Self::Dns => "dns",
            Self::Connect => "connect",
            Self::Timeout => "timeout",
            Self::UpstreamProtocol => "upstream_protocol",
            Self::ClientProtocol => "client_protocol",
            Self::SqliteWrite => "sqlite_write",
            Self::Internal => "internal",
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProxyResult {
    Inflight,
    Success,
    ClientRejected,
    UpstreamError,
    StreamError,
}

impl ProxyResult {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inflight => "inflight",
            Self::Success => "success",
            Self::ClientRejected => "client_rejected",
            Self::UpstreamError => "upstream_error",
            Self::StreamError => "stream_error",
        }
    }
}

impl fmt::Display for ProxyResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorInfo {
    pub kind: ErrorKind,
    pub text: String,
}

impl ErrorInfo {
    pub fn new(kind: ErrorKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
        }
    }

    pub fn from_display(kind: ErrorKind, error: impl fmt::Display) -> Self {
        Self::new(kind, error.to_string())
    }
}
