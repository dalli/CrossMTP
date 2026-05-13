//! Phase 0 prototype.
//!
//! Goal: prove that a Rust binary can link against system libmtp on macOS,
//! initialise the library, and enumerate connected MTP devices.
//!
//! This deliberately uses minimal hand-written FFI rather than bindgen so
//! Phase 0 stays cheap to audit. Phase 1 will replace it with a proper
//! Session crate (likely bindgen-generated).

use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_void};
use std::process::ExitCode;
use std::ptr;

type LibmtpDevice = c_void;
type LibmtpRawDevice = c_void;

#[repr(C)]
#[allow(dead_code, non_camel_case_types)]
enum LibmtpError {
    None = 0,
    General = 1,
    PtpLayer = 2,
    UsbLayer = 3,
    MemoryAllocation = 4,
    NoDeviceAttached = 5,
    StorageFull = 6,
    Connecting = 7,
    Cancelled = 8,
}

#[link(name = "mtp")]
extern "C" {
    fn LIBMTP_Init();
    fn LIBMTP_Set_Debug(level: c_int);

    fn LIBMTP_Detect_Raw_Devices(devices: *mut *mut LibmtpRawDevice, numdevs: *mut c_int) -> c_int;

    fn LIBMTP_Open_Raw_Device_Uncached(raw: *mut LibmtpRawDevice) -> *mut LibmtpDevice;
    fn LIBMTP_Release_Device(device: *mut LibmtpDevice);

    fn LIBMTP_Get_Friendlyname(device: *mut LibmtpDevice) -> *mut c_char;
    fn LIBMTP_Get_Manufacturername(device: *mut LibmtpDevice) -> *mut c_char;
    fn LIBMTP_Get_Modelname(device: *mut LibmtpDevice) -> *mut c_char;
    fn LIBMTP_Get_Serialnumber(device: *mut LibmtpDevice) -> *mut c_char;

    fn free(ptr: *mut c_void);
}

/// Raw libmtp_raw_device_t struct size on this platform.
///
/// We never inspect fields — we only need the size so we can index into the
/// array libmtp returns. The size is hard to compute portably, so we let
/// libmtp itself allocate and treat each entry as an opaque chunk; index
/// math uses `LIBMTP_Detect_Raw_Devices`'s contract that entries are
/// contiguous and laid out as a C array of the struct.
///
/// We sidestep the whole issue by only opening the *first* device returned.
/// Phase 1 will use bindgen and walk the array properly.
fn take_owned_cstring(ptr: *mut c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: libmtp returns a malloc()'d, NUL-terminated string we own.
    unsafe {
        let s = CStr::from_ptr(ptr).to_string_lossy().into_owned();
        free(ptr.cast());
        Some(s)
    }
}

fn main() -> ExitCode {
    println!("crossmtp Phase 0 probe — libmtp linkage check\n");

    // SAFETY: LIBMTP_Init is documented as safe to call once at process start.
    unsafe {
        LIBMTP_Init();
        LIBMTP_Set_Debug(0);
    }
    println!("[ok] LIBMTP_Init returned");

    let mut raw_devs: *mut LibmtpRawDevice = ptr::null_mut();
    let mut numdevs: c_int = 0;

    // SAFETY: out-pointers are valid; libmtp owns the allocation it writes into raw_devs.
    let err = unsafe { LIBMTP_Detect_Raw_Devices(&mut raw_devs, &mut numdevs) };

    match err {
        x if x == LibmtpError::None as c_int => {
            println!("[ok] LIBMTP_Detect_Raw_Devices: {numdevs} device(s)");
        }
        x if x == LibmtpError::NoDeviceAttached as c_int => {
            println!("[info] no MTP device attached");
            println!("\nPhase 0 result: libmtp links and runs. Device enumeration");
            println!("is reachable but unverified against a real Android device in");
            println!("this environment. Connect a device and re-run to validate.");
            return ExitCode::SUCCESS;
        }
        other => {
            eprintln!("[err] LIBMTP_Detect_Raw_Devices failed: code {other}");
            return ExitCode::from(2);
        }
    }

    if numdevs <= 0 || raw_devs.is_null() {
        println!("[info] device list empty");
        if !raw_devs.is_null() {
            // SAFETY: libmtp allocated this with malloc.
            unsafe { free(raw_devs.cast()) };
        }
        return ExitCode::SUCCESS;
    }

    // We only inspect the first device. Phase 1 will iterate properly via bindgen.
    // SAFETY: raw_devs points to a valid raw_device returned by libmtp.
    let device = unsafe { LIBMTP_Open_Raw_Device_Uncached(raw_devs) };
    if device.is_null() {
        eprintln!("[err] LIBMTP_Open_Raw_Device_Uncached returned null");
        // SAFETY: libmtp allocated this with malloc.
        unsafe { free(raw_devs.cast()) };
        return ExitCode::from(3);
    }
    println!("[ok] opened first raw device");

    // SAFETY: device is a valid open mtpdevice. Each getter returns malloc'd memory we free.
    let friendly = unsafe { take_owned_cstring(LIBMTP_Get_Friendlyname(device)) };
    let manufacturer = unsafe { take_owned_cstring(LIBMTP_Get_Manufacturername(device)) };
    let model = unsafe { take_owned_cstring(LIBMTP_Get_Modelname(device)) };
    let serial = unsafe { take_owned_cstring(LIBMTP_Get_Serialnumber(device)) };

    println!(
        "  friendly:     {}",
        friendly.as_deref().unwrap_or("(unknown)")
    );
    println!(
        "  manufacturer: {}",
        manufacturer.as_deref().unwrap_or("(unknown)")
    );
    println!(
        "  model:        {}",
        model.as_deref().unwrap_or("(unknown)")
    );
    println!(
        "  serial:       {}",
        serial.as_deref().unwrap_or("(unknown)")
    );

    // SAFETY: device is owned by us at this point.
    unsafe {
        LIBMTP_Release_Device(device);
        free(raw_devs.cast());
    }

    println!("\nPhase 0 result: libmtp linkage and basic device handshake OK.");
    ExitCode::SUCCESS
}
