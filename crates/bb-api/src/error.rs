use std::{backtrace::Backtrace, error, fmt};

#[derive(Debug)]
enum Kind {
    Invalid,
    Transport,
    Http(u16),
    Decode,
    Io,
}

/// A configuration, transport, or Bitbucket API failure.
#[derive(Debug)]
pub struct Error {
    kind: Kind,
    message: String,
    source: Option<Box<dyn error::Error + Send + Sync>>,
    backtrace: Backtrace,
}

impl Error {
    fn new(kind: Kind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
            backtrace: Backtrace::capture(),
        }
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new(Kind::Invalid, message)
    }

    pub(crate) fn encode(source: serde_json::Error) -> Self {
        let mut error = Self::invalid("cannot encode JSON request");
        error.source = Some(Box::new(source));
        error
    }

    pub(crate) fn http(status: u16, message: impl Into<String>) -> Self {
        Self::new(Kind::Http(status), message)
    }

    /// Returns the HTTP status when the server rejected the request.
    pub fn status(&self) -> Option<u16> {
        if let Kind::Http(status) = self.kind {
            Some(status)
        } else {
            None
        }
    }

    /// Reports missing or invalid authentication.
    pub fn is_authentication(&self) -> bool {
        self.status() == Some(401)
    }
    /// Reports insufficient permissions.
    pub fn is_forbidden(&self) -> bool {
        self.status() == Some(403)
    }
    /// Reports an unavailable resource.
    pub fn is_not_found(&self) -> bool {
        self.status() == Some(404)
    }
    /// Reports an exhausted rate limit.
    pub fn is_rate_limited(&self) -> bool {
        self.status() == Some(429)
    }
    /// Reports invalid client input.
    pub fn is_invalid_input(&self) -> bool {
        matches!(self.kind, Kind::Invalid)
    }
    /// Reports transport failures, whose outcome may be uncertain for writes.
    pub fn is_transport(&self) -> bool {
        matches!(self.kind, Kind::Transport)
    }
    /// Returns a concise diagnostic without a backtrace or source chain.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if let Some(source) = &self.source {
            write!(f, ": {source}")?;
        }
        if self.backtrace.status() == std::backtrace::BacktraceStatus::Captured {
            write!(f, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl error::Error for Error {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| &**source as &(dyn error::Error + 'static))
    }
}

impl From<reqwest::Error> for Error {
    fn from(source: reqwest::Error) -> Self {
        let message = if source.is_timeout() {
            "request timed out"
        } else {
            "HTTP transport failed"
        };
        let mut error = Self::new(Kind::Transport, message);
        error.source = Some(Box::new(source.without_url()));
        error
    }
}

impl From<serde_json::Error> for Error {
    fn from(source: serde_json::Error) -> Self {
        let mut error = Self::new(Kind::Decode, "invalid JSON response");
        error.source = Some(Box::new(source));
        error
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        let mut error = Self::new(Kind::Io, "I/O operation failed");
        error.source = Some(Box::new(source));
        error
    }
}

impl From<url::ParseError> for Error {
    fn from(source: url::ParseError) -> Self {
        let mut error = Self::invalid("invalid URL");
        error.source = Some(Box::new(source));
        error
    }
}

impl From<std::num::ParseIntError> for Error {
    fn from(source: std::num::ParseIntError) -> Self {
        let mut error = Self::invalid("identifier must be a positive integer");
        error.source = Some(Box::new(source));
        error
    }
}

impl From<reqwest::header::InvalidHeaderName> for Error {
    fn from(source: reqwest::header::InvalidHeaderName) -> Self {
        let mut error = Self::invalid("invalid header name");
        error.source = Some(Box::new(source));
        error
    }
}

impl From<reqwest::header::InvalidHeaderValue> for Error {
    fn from(source: reqwest::header::InvalidHeaderValue) -> Self {
        let mut error = Self::invalid("invalid header value");
        error.source = Some(Box::new(source));
        error
    }
}
