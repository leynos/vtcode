use std::time::Duration;

/// Errors returned by the WebMCP bridge and its runtime adapters.
#[derive(Debug, thiserror::Error)]
pub enum WebmcpError {
    /// The request or configuration is malformed.
    #[error("invalid WebMCP request: {0}")]
    InvalidRequest(String),
    /// The browser origin is not explicitly allowed.
    #[error("browser origin is not allowed: {0}")]
    OriginRejected(String),
    /// The pairing code is unknown or has expired.
    #[error("pairing code is expired or unknown")]
    PairingExpired,
    /// A one-time pairing code was already consumed.
    #[error("pairing code has already been used")]
    PairingUsed,
    /// The session token has been revoked or is not recognized.
    #[error("WebMCP session is not authorized")]
    Unauthorized,
    /// A request exceeded a configured transport limit.
    #[error("WebMCP request exceeds the configured limit")]
    LimitExceeded,
    /// The requested path is outside the workspace boundary.
    #[error("workspace path is not allowed: {0}")]
    PathRejected(String),
    /// The browser based its edit on a stale file.
    #[error("workspace file changed since the browser read it: {path}")]
    Conflict {
        /// Relative path of the conflicting file.
        path: String,
        /// Digest supplied by the browser.
        expected: String,
        /// Digest observed by the adapter.
        actual: String,
    },
    /// A proposal identifier was not found.
    #[error("WebMCP proposal was not found")]
    ProposalNotFound,
    /// A mutation was requested without terminal authorization.
    #[error("runtime approval is required before applying a WebMCP proposal")]
    ApprovalRequired,
    /// The selected adapter does not implement the requested operation.
    #[error("WebMCP operation is unavailable: {0}")]
    Unsupported(String),
    /// A revert identifier was not found or is no longer current.
    #[error("the requested WebMCP change cannot be reverted")]
    ChangeNotFound,
    /// A multi-file mutation could not be fully rolled back.
    #[error("the WebMCP change could not be rolled back completely; operator review is required")]
    PartialApply,
    /// The event hub cannot replay the requested range.
    #[error("WebMCP event sequence gap: requested after {requested}, oldest is {oldest}")]
    SequenceGap {
        /// Last sequence known by the reconnecting client.
        requested: u64,
        /// Oldest sequence retained by the hub.
        oldest: u64,
    },
    /// A subscriber could not keep up with lifecycle events.
    #[error("WebMCP client is too slow to receive lifecycle events")]
    SlowClient,
    /// An adapter operation exceeded its deadline.
    #[error("WebMCP operation timed out after {0:?}")]
    Timeout(Duration),
    /// An underlying filesystem or process operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// A JSON protocol payload failed to decode.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// An adapter returned a contextual failure.
    #[error("WebMCP runtime adapter failed: {0}")]
    Adapter(String),
}

/// Result alias used throughout the bridge.
pub type Result<T> = std::result::Result<T, WebmcpError>;
