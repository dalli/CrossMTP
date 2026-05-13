//! CrossMTP Session Layer.
//!
//! Safe Rust wrapper around `libmtp` for the macOS MVP. Phase 1 scope:
//!
//! * one-shot library init
//! * device enumeration via raw devices
//! * storage listing per device
//! * folder listing (one level) via `LIBMTP_Get_Files_And_Folders`
//! * file download via `LIBMTP_Get_File_To_File`
//! * file upload via `LIBMTP_Send_File_From_File`
//! * normalised error model
//! * platform capability struct
//!
//! Out of scope for Phase 1: progress callbacks (Phase 2 orchestrator owns
//! that), cancellation, recursive walks, rename/delete, multi-device
//! coordination.

#![allow(unsafe_op_in_unsafe_fn)]

mod ffi;

pub mod capability;
pub mod error;

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::Path;
use std::ptr;
use std::sync::OnceLock;

pub use capability::Capabilities;
pub use error::{MtpError, Result};

/// Top-level entry point. `Session::open()` performs the one-shot
/// `LIBMTP_Init` and returns a handle that can enumerate devices.
///
/// Construction is idempotent: the underlying init runs at most once per
/// process, even if `Session` is dropped and re-created.
#[derive(Debug)]
pub struct Session {
    _private: (),
}

static INIT: OnceLock<()> = OnceLock::new();

impl Session {
    pub fn open() -> Self {
        INIT.get_or_init(|| {
            // SAFETY: documented as safe to call once at process start.
            unsafe {
                ffi::LIBMTP_Init();
                ffi::LIBMTP_Set_Debug(0);
            }
        });
        Self { _private: () }
    }

    /// Enumerate all currently attached MTP devices.
    ///
    /// Each returned `Device` owns its libmtp handle and releases it on drop.
    pub fn list_devices(&self) -> Result<Vec<Device>> {
        let mut raw: *mut ffi::LIBMTP_raw_device_t = ptr::null_mut();
        let mut numdevs: c_int = 0;

        // SAFETY: out-pointers are valid; libmtp allocates via malloc.
        let err = unsafe { ffi::LIBMTP_Detect_Raw_Devices(&mut raw, &mut numdevs) };

        match err {
            ffi::LIBMTP_error_number_t_LIBMTP_ERROR_NONE => {}
            ffi::LIBMTP_error_number_t_LIBMTP_ERROR_NO_DEVICE_ATTACHED => {
                if !raw.is_null() {
                    unsafe { ffi::free(raw.cast()) };
                }
                return Ok(Vec::new());
            }
            other => {
                if !raw.is_null() {
                    unsafe { ffi::free(raw.cast()) };
                }
                return Err(MtpError::from_libmtp(other as i32));
            }
        }

        if numdevs <= 0 || raw.is_null() {
            if !raw.is_null() {
                unsafe { ffi::free(raw.cast()) };
            }
            return Ok(Vec::new());
        }

        let mut devices = Vec::with_capacity(numdevs as usize);
        // SAFETY: `raw` points at a contiguous array of `numdevs` raw_device_t structs
        // allocated by libmtp. We index it with normal pointer arithmetic.
        for i in 0..(numdevs as isize) {
            let raw_dev_ptr = unsafe { raw.offset(i) };
            // Open uncached so libmtp re-reads the device tree each time we ask.
            let dev = unsafe { ffi::LIBMTP_Open_Raw_Device_Uncached(raw_dev_ptr) };
            if dev.is_null() {
                // Skip devices we can't open (other process holds the USB
                // interface, permissions denied, etc.). The orchestrator
                // layer will surface a friendlier error.
                continue;
            }
            devices.push(Device {
                handle: dev,
                capabilities: Capabilities::macos_libmtp_default(),
            });
        }

        // SAFETY: libmtp allocated `raw` via malloc; we own the buffer.
        unsafe { ffi::free(raw.cast()) };

        Ok(devices)
    }
}

/// One opened MTP device. Drops release the libmtp handle.
pub struct Device {
    handle: *mut ffi::LIBMTP_mtpdevice_t,
    capabilities: Capabilities,
}

// `LIBMTP_mtpdevice_t` is documented as not thread-safe. We mark `Device` as
// `Send` so it can be moved across threads (e.g. into a transfer worker) but
// deliberately do NOT implement `Sync`: only one thread may use a given
// device handle at a time. The orchestrator layer enforces this with a
// single active worker.
unsafe impl Send for Device {}

impl std::fmt::Debug for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Device")
            .field("manufacturer", &self.manufacturer())
            .field("model", &self.model())
            .field("serial", &self.serial())
            .finish()
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: we own the handle.
            unsafe { ffi::LIBMTP_Release_Device(self.handle) };
            self.handle = ptr::null_mut();
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub friendly_name: Option<String>,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Storage {
    pub id: u32,
    pub description: Option<String>,
    pub free_bytes: u64,
    pub max_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Folder,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub item_id: u32,
    pub parent_id: u32,
    pub storage_id: u32,
    pub name: String,
    pub size: u64,
    /// Modification time as Unix epoch seconds when the device reports it.
    pub modified_secs: Option<u64>,
    pub kind: EntryKind,
}

/// Sentinel parent id meaning "list the root of the given storage".
pub const PARENT_ROOT: u32 = 0xFFFFFFFF;

impl Device {
    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    pub fn info(&self) -> DeviceInfo {
        DeviceInfo {
            friendly_name: self.friendly_name(),
            manufacturer: self.manufacturer(),
            model: self.model(),
            serial: self.serial(),
        }
    }

    pub fn friendly_name(&self) -> Option<String> {
        unsafe { take_owned_cstring(ffi::LIBMTP_Get_Friendlyname(self.handle)) }
    }
    pub fn manufacturer(&self) -> Option<String> {
        unsafe { take_owned_cstring(ffi::LIBMTP_Get_Manufacturername(self.handle)) }
    }
    pub fn model(&self) -> Option<String> {
        unsafe { take_owned_cstring(ffi::LIBMTP_Get_Modelname(self.handle)) }
    }
    pub fn serial(&self) -> Option<String> {
        unsafe { take_owned_cstring(ffi::LIBMTP_Get_Serialnumber(self.handle)) }
    }

    /// Fetch the device storage list. libmtp populates `device->storage` as
    /// a linked list which we copy into safe Rust values.
    pub fn list_storages(&self) -> Result<Vec<Storage>> {
        // sortby = 0 (LIBMTP_STORAGE_SORTBY_NOTSORTED)
        let rc = unsafe { ffi::LIBMTP_Get_Storage(self.handle, 0) };
        if rc != 0 {
            // Any error here is "we couldn't get storage" — usually means
            // the user hasn't tapped "Allow" on the device yet.
            return Err(MtpError::StorageUnavailable);
        }

        let mut out = Vec::new();
        // SAFETY: the device handle is valid and `storage` is a libmtp-owned
        // linked list whose nodes live as long as the device handle.
        let mut node = unsafe { (*self.handle).storage };
        while !node.is_null() {
            let s = unsafe { &*node };
            let description = unsafe {
                if s.StorageDescription.is_null() {
                    None
                } else {
                    Some(
                        CStr::from_ptr(s.StorageDescription)
                            .to_string_lossy()
                            .into_owned(),
                    )
                }
            };
            out.push(Storage {
                id: s.id,
                description,
                free_bytes: s.FreeSpaceInBytes,
                max_bytes: s.MaxCapacity,
            });
            node = unsafe { (*node).next };
        }
        Ok(out)
    }

    /// List the immediate children of `parent_id` on `storage_id`.
    /// Use [`PARENT_ROOT`] as `parent_id` to list a storage root.
    pub fn list_entries(&self, storage_id: u32, parent_id: u32) -> Result<Vec<Entry>> {
        // SAFETY: device handle is valid.
        let head = unsafe { ffi::LIBMTP_Get_Files_And_Folders(self.handle, storage_id, parent_id) };
        if head.is_null() {
            // Could be empty folder OR an error. libmtp doesn't distinguish
            // cleanly; fall back to checking the device error stack.
            if let Some(err) = self.take_error() {
                return Err(err);
            }
            return Ok(Vec::new());
        }

        let mut out = Vec::new();
        let mut node = head;
        while !node.is_null() {
            let f = unsafe { &*node };
            let name = unsafe {
                if f.filename.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(f.filename).to_string_lossy().into_owned()
                }
            };
            let kind = if f.filetype == ffi::LIBMTP_filetype_t_LIBMTP_FILETYPE_FOLDER {
                EntryKind::Folder
            } else {
                EntryKind::File
            };
            out.push(Entry {
                item_id: f.item_id,
                parent_id: f.parent_id,
                storage_id: f.storage_id,
                name,
                size: f.filesize,
                modified_secs: (f.modificationdate > 0).then_some(f.modificationdate as u64),
                kind,
            });
            let next = unsafe { (*node).next };
            unsafe { ffi::LIBMTP_destroy_file_t(node) };
            node = next;
        }
        Ok(out)
    }

    /// Download `file_id` from the device into a local path.
    pub fn download_file(&self, file_id: u32, dest: &Path) -> Result<()> {
        let dest_c = path_to_cstring(dest)?;
        // SAFETY: device handle valid; dest_c lives across the call.
        let rc = unsafe {
            ffi::LIBMTP_Get_File_To_File(self.handle, file_id, dest_c.as_ptr(), None, ptr::null())
        };
        if rc != 0 {
            return Err(self.take_error().unwrap_or(MtpError::TransferFailed));
        }
        Ok(())
    }

    /// Download with a progress + cancellation callback. The closure is
    /// called by libmtp on its own transfer thread. Return `true` from the
    /// closure to abort the transfer; libmtp will then stop and the call
    /// will surface as `MtpError::Cancelled` (best effort — libmtp does not
    /// always set its error code for cancellation, so we infer it).
    pub fn download_file_with_progress(
        &self,
        file_id: u32,
        dest: &Path,
        modified_secs: Option<u64>,
        mut progress: impl FnMut(u64, u64) -> bool,
    ) -> Result<()> {
        let dest_c = path_to_cstring(dest)?;
        let mut cb: &mut dyn FnMut(u64, u64) -> bool = &mut progress;
        let data = &mut cb as *mut _ as *mut c_void;
        // SAFETY: trampoline_thunk only dereferences `data` while libmtp is
        // running this call; the trampoline lives on this stack frame.
        let rc = unsafe {
            ffi::LIBMTP_Get_File_To_File(
                self.handle,
                file_id,
                dest_c.as_ptr(),
                Some(progress_thunk),
                data,
            )
        };
        if rc != 0 {
            // If the user returned `true` from the callback at any point
            // during the transfer, treat the failure as cancelled. libmtp
            // doesn't tell us directly so we read it back from a side
            // channel set by the trampoline.
            if CANCEL_OBSERVED.with(|c| c.replace(false)) {
                return Err(MtpError::Cancelled);
            }
            return Err(self.take_error().unwrap_or(MtpError::TransferFailed));
        }
        // Reset side channel even on success path.
        CANCEL_OBSERVED.with(|c| c.replace(false));

        if let Some(secs) = modified_secs {
            if let Ok(file) = std::fs::File::open(dest) {
                let mtime = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
                let _ = file.set_times(std::fs::FileTimes::new().set_modified(mtime));
            }
        }

        Ok(())
    }

    /// Upload a local file to a device folder. The file is created as
    /// `name` under `(storage_id, parent_id)`.
    pub fn upload_file(
        &self,
        source: &Path,
        storage_id: u32,
        parent_id: u32,
        name: &str,
    ) -> Result<u32> {
        self.upload_file_inner(source, storage_id, parent_id, name, None)
    }

    /// Upload with a progress + cancellation callback. See
    /// [`Device::download_file_with_progress`] for callback semantics.
    pub fn upload_file_with_progress(
        &self,
        source: &Path,
        storage_id: u32,
        parent_id: u32,
        name: &str,
        mut progress: impl FnMut(u64, u64) -> bool,
    ) -> Result<u32> {
        let mut cb: &mut dyn FnMut(u64, u64) -> bool = &mut progress;
        self.upload_file_inner(source, storage_id, parent_id, name, Some(&mut cb))
    }

    fn upload_file_inner(
        &self,
        source: &Path,
        storage_id: u32,
        parent_id: u32,
        name: &str,
        callback: Option<&mut &mut dyn FnMut(u64, u64) -> bool>,
    ) -> Result<u32> {
        let src_c = path_to_cstring(source)?;
        let name_c =
            CString::new(name).map_err(|_| MtpError::InvalidArgument("filename has NUL byte"))?;
        let metadata = std::fs::metadata(source).map_err(MtpError::Io)?;
        let size = metadata.len();
        let modified_secs = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs())
            .unwrap_or(0);

        // libmtp owns the file_t we pass in; it mutates `item_id` to the new id.
        let mut file = ffi::LIBMTP_file_t {
            filename: name_c.as_ptr() as *mut c_char,
            filesize: size,
            modificationdate: modified_secs as _,
            filetype: ffi::LIBMTP_filetype_t_LIBMTP_FILETYPE_UNKNOWN,
            parent_id,
            storage_id,
            item_id: 0,
            next: ptr::null_mut(),
        };

        let (cb_fn, cb_data): (ffi::LIBMTP_progressfunc_t, *const c_void) = match callback {
            Some(cb_ref) => (Some(progress_thunk), cb_ref as *mut _ as *const c_void),
            None => (None, ptr::null()),
        };

        // SAFETY: pointers live across the call; libmtp will not retain them.
        let rc = unsafe {
            ffi::LIBMTP_Send_File_From_File(self.handle, src_c.as_ptr(), &mut file, cb_fn, cb_data)
        };
        if rc != 0 {
            if CANCEL_OBSERVED.with(|c| c.replace(false)) {
                return Err(MtpError::Cancelled);
            }
            return Err(self.take_error().unwrap_or(MtpError::TransferFailed));
        }
        CANCEL_OBSERVED.with(|c| c.replace(false));
        Ok(file.item_id)
    }

    /// Create a folder under `(storage_id, parent_id)`. Returns the
    /// new folder's object id. libmtp may sanitise illegal characters
    /// in `name` in-place; we don't surface the sanitised name here —
    /// callers that care should follow up with `list_entries` to
    /// re-discover it.
    pub fn create_folder(&self, name: &str, parent_id: u32, storage_id: u32) -> Result<u32> {
        let name_c = CString::new(name)
            .map_err(|_| MtpError::InvalidArgument("folder name has NUL byte"))?;
        // libmtp takes a mutable `char *` because it may sanitise the
        // string in place. We hand it owned memory via `into_raw` then
        // reclaim on the way out so Drop frees it.
        let raw = name_c.into_raw();
        // SAFETY: `raw` is a valid NUL-terminated C string we own; libmtp
        // keeps no reference past the call.
        let new_id = unsafe { ffi::LIBMTP_Create_Folder(self.handle, raw, parent_id, storage_id) };
        // Reclaim ownership so the CString's Drop runs.
        let _reclaim = unsafe { CString::from_raw(raw) };
        if new_id == 0 {
            return Err(self
                .take_error()
                .unwrap_or_else(|| MtpError::Device("create_folder returned 0".into())));
        }
        Ok(new_id)
    }

    /// Drain the libmtp error stack for this device into a normalised
    /// `MtpError`. Returns `None` if the stack is empty.
    fn take_error(&self) -> Option<MtpError> {
        let mut text: Vec<String> = Vec::new();
        // SAFETY: device handle valid.
        unsafe {
            let mut node = ffi::LIBMTP_Get_Errorstack(self.handle);
            while !node.is_null() {
                let e = &*node;
                if !e.error_text.is_null() {
                    text.push(CStr::from_ptr(e.error_text).to_string_lossy().into_owned());
                }
                node = (*node).next;
            }
            ffi::LIBMTP_Clear_Errorstack(self.handle);
        }
        if text.is_empty() {
            None
        } else {
            let joined = text.join(" | ");
            Some(classify_device_text(joined))
        }
    }
}

/// Classify a raw libmtp error-stack text into the most specific
/// `MtpError` variant we can identify. libmtp emits the PTP response
/// code (0x2002) and a "PTP Layer" / "USB Layer" prefix for the two
/// session-fatal cases — we route those into the dedicated variants so
/// the orchestrator can stop retrying on a dead handle.
fn classify_device_text(text: String) -> MtpError {
    let lower = text.to_ascii_lowercase();
    let looks_ptp = lower.contains("ptp layer")
        || lower.contains("ptp_layer")
        || lower.contains("0x2002")
        || lower.contains("ptp_rc_generalerror")
        || lower.contains("general error")
        || lower.contains("session not opened");
    let looks_usb = lower.contains("usb layer")
        || lower.contains("usb_layer")
        || lower.contains("libusb")
        || lower.contains("broken pipe");
    if looks_ptp {
        MtpError::PtpLayer(text)
    } else if looks_usb {
        MtpError::UsbLayer(text)
    } else {
        MtpError::Device(text)
    }
}

// Side channel used by the progress trampoline to tell the calling Rust
// frame "the user requested cancel". libmtp's progressfunc_t return value
// is the in-band signal but libmtp doesn't always set its own error code
// to LIBMTP_ERROR_CANCELLED on the way out, so we mirror it here on the
// thread that initiated the call.
thread_local! {
    static CANCEL_OBSERVED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

extern "C" fn progress_thunk(sent: u64, total: u64, data: *const c_void) -> c_int {
    if data.is_null() {
        return 0;
    }
    // SAFETY: caller built `data` from `&mut &mut dyn FnMut(...)`.
    let cb = unsafe { &mut *(data as *mut &mut dyn FnMut(u64, u64) -> bool) };
    if cb(sent, total) {
        CANCEL_OBSERVED.with(|c| c.set(true));
        1
    } else {
        0
    }
}

unsafe fn take_owned_cstring(ptr: *mut c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let s = CStr::from_ptr(ptr).to_string_lossy().into_owned();
    ffi::free(ptr.cast::<c_void>());
    Some(s)
}

fn path_to_cstring(path: &Path) -> Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| MtpError::InvalidArgument("path has NUL byte"))
}
