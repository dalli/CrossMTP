//! `adb devices -l` parsing and device state classification.
//!
//! Goals (plan.md §4.2):
//!   * distinguish `unauthorized`, `offline`, `no permissions`, missing
//!   * surface stable identifier — **serial only** (Phase 0 retro: do
//!     not cache `transport_id`, it gets reassigned on reconnect).
//!   * keep `transport_id` for *display* but never for routing.

use crate::error::{AdbError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceState {
    /// Ready for shell / push / pull.
    Device,
    /// User has not yet accepted the USB debugging prompt.
    Unauthorized,
    /// Listed but not responding (cable jiggle, transient ADB hiccup).
    Offline,
    /// Linux udev-style permission failure. Kept as a separate variant
    /// to match plan.md §4.2.
    NoPermissions,
    /// adb reported a state we don't yet recognise. The raw token is
    /// preserved so it shows up in logs and bug reports.
    Other(String),
}

impl DeviceState {
    pub fn from_token(token: &str) -> Self {
        match token {
            "device" => DeviceState::Device,
            "unauthorized" => DeviceState::Unauthorized,
            "offline" => DeviceState::Offline,
            "no" => DeviceState::NoPermissions, // "no permissions" gets split — fixed up in parser
            other => DeviceState::Other(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdbDevice {
    /// **Stable** device identifier. Use this and only this for routing
    /// decisions (Phase 0 retro §2.3).
    pub serial: String,
    pub state: DeviceState,
    /// Volatile — reassigned on reconnect. Display-only.
    pub transport_id: Option<u32>,
    /// `model:foo` from `-l`, kept for friendly names in the UI.
    pub model: Option<String>,
    /// `product:foo` token, kept for the same reason.
    pub product: Option<String>,
}

impl AdbDevice {
    pub fn is_ready(&self) -> bool {
        matches!(self.state, DeviceState::Device)
    }

    /// Map a non-ready device state to the corresponding `AdbError`.
    /// Returns `Ok(())` when the device is ready.
    pub fn require_ready(&self) -> Result<()> {
        match &self.state {
            DeviceState::Device => Ok(()),
            DeviceState::Unauthorized => Err(AdbError::Unauthorized {
                serial: self.serial.clone(),
            }),
            DeviceState::Offline => Err(AdbError::Offline {
                serial: self.serial.clone(),
            }),
            DeviceState::NoPermissions => Err(AdbError::NoPermissions {
                serial: self.serial.clone(),
            }),
            DeviceState::Other(s) => Err(AdbError::CommandFailed {
                code: -1,
                stderr: format!("device {} in state {s}", self.serial),
            }),
        }
    }
}

/// Parse the textual output of `adb devices -l`.
///
/// Example shape:
/// ```text
/// List of devices attached
/// 24117RK2CG             device transport_id:3 product:zorn model:24117RK2CG
/// EMULATOR-5554          unauthorized transport_id:7
/// abc123                 no permissions; see [http://...]
/// ```
pub fn parse_devices_output(stdout: &str) -> Result<Vec<AdbDevice>> {
    let mut out = Vec::new();
    let mut saw_header = false;
    for raw in stdout.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("List of devices") {
            saw_header = true;
            continue;
        }
        // Adb sometimes prints daemon startup chatter on stdout before
        // the header ("* daemon not running ..."). Skip those.
        if line.starts_with('*') {
            continue;
        }
        let device = parse_one_line(line)?;
        out.push(device);
    }
    let _ = saw_header; // not all adb versions print the header on -l
    Ok(out)
}

fn parse_one_line(line: &str) -> Result<AdbDevice> {
    // Whitespace-separated. First token = serial. Second = state token
    // (with the special case of "no permissions" — two tokens).
    let mut iter = line.split_whitespace();
    let serial = iter
        .next()
        .ok_or_else(|| AdbError::ParseError(line.to_string()))?
        .to_string();
    let state_token = iter
        .next()
        .ok_or_else(|| AdbError::ParseError(line.to_string()))?;
    let rest: Vec<&str> = iter.collect();

    let state = if state_token == "no" {
        // "no permissions; see <url>" — consume the trailing tokens.
        DeviceState::NoPermissions
    } else {
        DeviceState::from_token(state_token)
    };

    let mut transport_id = None;
    let mut model = None;
    let mut product = None;
    for token in rest {
        if let Some(v) = token.strip_prefix("transport_id:") {
            transport_id = v.parse::<u32>().ok();
        } else if let Some(v) = token.strip_prefix("model:") {
            model = Some(v.to_string());
        } else if let Some(v) = token.strip_prefix("product:") {
            product = Some(v.to_string());
        }
        // other "-l" tokens (device:, usb:) ignored on purpose.
    }

    Ok(AdbDevice {
        serial,
        state,
        transport_id,
        model,
        product,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ready_device_with_transport_and_model() {
        let out = "List of devices attached\n\
                   24117RK2CG             device transport_id:3 product:zorn model:24117RK2CG device:zorn\n";
        let parsed = parse_devices_output(out).unwrap();
        assert_eq!(parsed.len(), 1);
        let d = &parsed[0];
        assert_eq!(d.serial, "24117RK2CG");
        assert_eq!(d.state, DeviceState::Device);
        assert_eq!(d.transport_id, Some(3));
        assert_eq!(d.model.as_deref(), Some("24117RK2CG"));
        assert_eq!(d.product.as_deref(), Some("zorn"));
        assert!(d.is_ready());
    }

    #[test]
    fn parses_unauthorized() {
        let out = "List of devices attached\nABCDEF unauthorized transport_id:9\n";
        let parsed = parse_devices_output(out).unwrap();
        assert_eq!(parsed[0].state, DeviceState::Unauthorized);
        let err = parsed[0].require_ready().unwrap_err();
        assert!(matches!(err, AdbError::Unauthorized { .. }));
        assert!(err.is_likely_user_action_required());
    }

    #[test]
    fn parses_offline() {
        let out = "List of devices attached\nFOO offline\n";
        let parsed = parse_devices_output(out).unwrap();
        assert_eq!(parsed[0].state, DeviceState::Offline);
        assert!(matches!(
            parsed[0].require_ready(),
            Err(AdbError::Offline { .. })
        ));
    }

    #[test]
    fn parses_no_permissions() {
        let out = "List of devices attached\nBAR no permissions; see [http://example]\n";
        let parsed = parse_devices_output(out).unwrap();
        assert_eq!(parsed[0].state, DeviceState::NoPermissions);
        assert!(matches!(
            parsed[0].require_ready(),
            Err(AdbError::NoPermissions { .. })
        ));
    }

    #[test]
    fn skips_daemon_chatter_and_empty_lines() {
        let out = "* daemon not running; starting now at tcp:5037 *\n\
                   * daemon started successfully *\n\
                   List of devices attached\n\
                   \n\
                   SERIAL device transport_id:1\n";
        let parsed = parse_devices_output(out).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].serial, "SERIAL");
    }

    #[test]
    fn empty_listing_yields_empty_vec() {
        let parsed = parse_devices_output("List of devices attached\n").unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn unknown_state_preserves_raw_token() {
        let out = "List of devices attached\nXX recovery\n";
        let parsed = parse_devices_output(out).unwrap();
        assert_eq!(parsed[0].state, DeviceState::Other("recovery".into()));
    }
}
