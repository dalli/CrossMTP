//! Error model for the tar stream builder.

use std::io;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, TarError>;

#[derive(Debug, Error)]
pub enum TarError {
    /// Source path doesn't exist or isn't accessible.
    #[error("source path not found or unreadable: {path}")]
    SourceNotFound { path: String },

    /// A path element resolved outside the source root, or contained
    /// `..` after normalisation. We refuse to emit such entries so a
    /// crafted symlink can't write outside `<dest>` on the device.
    #[error("path traversal blocked: {path}")]
    PathTraversal { path: String },

    /// A tar entry path component is empty, contains NUL, or is too long
    /// for USTAR (100 bytes name + 155 bytes prefix).
    #[error("invalid tar entry name: {reason}")]
    InvalidEntryName { reason: String },

    /// Special file type (socket, fifo, block/char device) — we don't
    /// emit these into the tar; the orchestrator decides whether to skip
    /// or fail. The builder skips by default and records the reason in
    /// the progress channel.
    #[error("unsupported entry kind for {path}: {kind}")]
    UnsupportedEntry { path: String, kind: String },

    /// Reading or stating a local file failed mid-stream. Wraps the
    /// underlying io::Error so callers can match on it.
    #[error("io error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: io::Error,
    },

    /// Generic IO without an associated path (e.g. writing to the sink).
    #[error("io error: {0}")]
    Sink(#[from] io::Error),
}
