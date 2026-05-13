//! Normalised error model for the Session Layer.
//!
//! The orchestrator and UI layers must be able to *distinguish* the
//! recoverable from the fatal cases — that's the whole point of the MVP.
//! libmtp's own enum is too coarse, so we collapse it into a small set of
//! variants and keep the original text where it's useful.

use std::io;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, MtpError>;

#[derive(Debug, Error)]
pub enum MtpError {
    /// Library returned `NoDeviceAttached`. Treat as "expected, just empty".
    #[error("no MTP device attached")]
    NoDevice,

    /// Could not open the device — usually because another process on
    /// macOS (Image Capture, Android File Transfer, Photos) is holding
    /// the USB interface, or the user has not yet accepted MTP on the
    /// phone.
    #[error("device cannot be opened (likely USB interface held by another process or MTP not authorised on the phone)")]
    DeviceLocked,

    /// Storage list is empty / unreadable. Most common cause is the
    /// phone is at the unlock screen or the user dismissed the MTP
    /// permission dialog.
    #[error("device storage unavailable (phone locked or MTP permission not granted)")]
    StorageUnavailable,

    /// libmtp connect / PTP error.
    #[error("MTP connection error")]
    Connection,

    /// PTP layer fault (LIBMTP_ERROR_PTP_LAYER, response code 0x2002, etc.).
    /// The device session is effectively dead — the only safe recovery is
    /// to release and reopen the libmtp device handle. Common on large
    /// directory uploads when the device PTP response queue drifts.
    #[error("device PTP session lost: {0}")]
    PtpLayer(String),

    /// USB layer fault (LIBMTP_ERROR_USB_LAYER). Cable jiggle / device
    /// reset. Recovery same as `PtpLayer`.
    #[error("device USB session lost: {0}")]
    UsbLayer(String),

    /// Transfer didn't complete and we could not extract a meaningful
    /// device-side error.
    #[error("transfer failed")]
    TransferFailed,

    /// Cancelled by caller. Reserved for Phase 2 orchestrator.
    #[error("cancelled")]
    Cancelled,

    /// Free-form error text from libmtp's per-device error stack.
    #[error("device reported: {0}")]
    Device(String),

    /// Caller misuse — bad path, NUL byte, etc.
    #[error("invalid argument: {0}")]
    InvalidArgument(&'static str),

    /// Local filesystem error during transfer staging.
    #[error("local IO error")]
    Io(#[from] io::Error),
}

impl MtpError {
    /// Map a raw `LIBMTP_error_number_t` value into our enum.
    pub(crate) fn from_libmtp(code: i32) -> Self {
        // Mirror libmtp.h order. We avoid pulling in the bindgen consts here
        // to keep this file dependency-free and easier to read.
        match code {
            0 => MtpError::Device("LIBMTP_ERROR_NONE returned in error path".into()),
            1 => MtpError::Device("general libmtp error".into()),
            2 => MtpError::PtpLayer("LIBMTP_ERROR_PTP_LAYER".into()),
            3 => MtpError::UsbLayer("LIBMTP_ERROR_USB_LAYER".into()),
            4 => MtpError::Device("memory allocation".into()),
            5 => MtpError::NoDevice,
            6 => MtpError::Device("storage full".into()),
            7 => MtpError::Connection,
            8 => MtpError::Cancelled,
            other => MtpError::Device(format!("unknown libmtp error {other}")),
        }
    }

    /// True if a UI is expected to surface a "tap Allow on your phone" hint.
    pub fn is_likely_permission_issue(&self) -> bool {
        matches!(
            self,
            MtpError::DeviceLocked | MtpError::StorageUnavailable | MtpError::Connection
        )
    }

    /// True when the libmtp/PTP session is no longer usable and the only
    /// safe recovery is reopening the device handle. Used by the orchestrator
    /// to stop retrying on a dead handle and pause the queue instead.
    pub fn is_session_broken(&self) -> bool {
        self.is_session_dead() || self.is_session_lost()
    }

    /// Handle-is-dead errors: the device is gone or refusing communication
    /// at the USB/PTP-init level. Listing/transfers cannot succeed until
    /// the user reconnects the cable or unlocks the phone. The orchestrator
    /// pauses the queue and waits for the UI's reconnect path to update
    /// the device handle.
    pub fn is_session_dead(&self) -> bool {
        matches!(
            self,
            MtpError::Connection | MtpError::NoDevice | MtpError::DeviceLocked
        )
    }

    /// Transaction-state-desync errors: handle is still open (listing can
    /// still work), but a specific PTP/USB operation was refused. Common
    /// when the device rejects a write (storage permission, scoped-storage,
    /// first `Send_File` with a stale parent id). Retrying the same job
    /// will likely fail the same way, but other operations may still
    /// succeed — so the orchestrator should fail the job without pausing
    /// the entire queue.
    pub fn is_session_lost(&self) -> bool {
        matches!(self, MtpError::PtpLayer(_) | MtpError::UsbLayer(_))
    }

    /// True when a retry on the same handle has a realistic chance of
    /// succeeding (transient device-reported errors, generic transfer
    /// failures). Session-broken errors and caller-side errors return false.
    pub fn is_retryable_in_place(&self) -> bool {
        matches!(
            self,
            MtpError::Device(_) | MtpError::TransferFailed | MtpError::StorageUnavailable
        )
    }
}
