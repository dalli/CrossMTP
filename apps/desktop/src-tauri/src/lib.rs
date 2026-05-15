//! CrossMTP desktop Tauri shell.
//!
//! Bridges the React frontend to the `mtp-session` + `orchestrator` crates.
//!
//! Threading model:
//! * The orchestrator owns its own worker thread and the libmtp `Device`.
//! * A second background thread ("event pump") owns the orchestrator's
//!   single Receiver and forwards every event to the Tauri app via
//!   `app.emit("transfer-event", payload)`.
//! * Tauri commands run on the Tauri runtime thread pool; they only ever
//!   touch the orchestrator handle (which is `Send + Sync`-safe through
//!   internal channels) and the lazily-built device snapshot cache.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use adb_session::{
    plan_upload, probe_manifest, AdbSession, DeviceState, LocalFile, PlanRequest, UploadPolicy,
};
use orchestrator::{
    AdbContext, ConflictPolicy, Event as OrchEvent, JobId, JobKind, JobSpec, JobState,
    Orchestrator,
};
use serde::{Deserialize, Serialize};
use tar_stream::ConflictPlan;
use tauri::{AppHandle, Emitter, Manager, State};

// ---------- wire types (camelCase to match the React side) ----------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceInfo {
    index: usize,
    friendly_name: Option<String>,
    manufacturer: Option<String>,
    model: Option<String>,
    serial: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Storage {
    id: u32,
    description: Option<String>,
    free_bytes: u64,
    max_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    item_id: u32,
    parent_id: u32,
    storage_id: u32,
    name: String,
    size: u64,
    modified_secs: Option<u64>,
    kind: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalEntry {
    path: String,
    name: String,
    size: u64,
    is_dir: bool,
    modified: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceSnapshot {
    devices: Vec<DeviceInfo>,
    storages: Vec<Storage>,
    error: Option<String>,
    permission_hint: bool,
}

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
enum WireEvent {
    Enqueued { id: u64, kind: WireKind },
    StateChanged { id: u64, state: WireState },
    Progress { id: u64, sent: u64, total: u64 },
    BulkProgress {
        id: u64,
        current_file: String,
        files_done: u32,
        total_files: u32,
    },
    QueuePaused { reason: String },
    WorkerStopped,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
enum WireKind {
    Download {
        storage_id: u32,
        file_id: u32,
        name: String,
        dest_dir: String,
        expected_size: u64,
    },
    Upload {
        storage_id: u32,
        parent_id: u32,
        source: String,
        name: String,
    },
    BulkUpload {
        storage_id: u32,
        parent_id: u32,
        source: String,
        name: String,
    },
    AdbTarUpload {
        serial: String,
        source: String,
        dest_path: String,
    },
}

impl From<JobKind> for WireKind {
    fn from(k: JobKind) -> Self {
        match k {
            JobKind::Download {
                storage_id,
                file_id,
                name,
                dest_dir,
                expected_size,
                modified_secs: _,
            } => WireKind::Download {
                storage_id,
                file_id,
                name,
                dest_dir: dest_dir.to_string_lossy().into_owned(),
                expected_size,
            },
            JobKind::Upload {
                storage_id,
                parent_id,
                source,
                name,
                relative_path: _,
            } => WireKind::Upload {
                storage_id,
                parent_id,
                source: source.to_string_lossy().into_owned(),
                name,
            },
            JobKind::BulkUpload {
                storage_id,
                parent_id,
                source,
                name,
            } => WireKind::BulkUpload {
                storage_id,
                parent_id,
                source: source.to_string_lossy().into_owned(),
                name,
            },
            JobKind::AdbTarUpload {
                serial,
                source,
                dest_path,
                plan: _,
            } => WireKind::AdbTarUpload {
                serial,
                source: source.to_string_lossy().into_owned(),
                dest_path,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WireState {
    tag: &'static str,
    bytes: Option<u64>,
    item_id: Option<u32>,
    reason: Option<String>,
}

impl From<JobState> for WireState {
    fn from(s: JobState) -> Self {
        match s {
            JobState::Queued => Self {
                tag: "queued",
                bytes: None,
                item_id: None,
                reason: None,
            },
            JobState::Validating => Self {
                tag: "validating",
                bytes: None,
                item_id: None,
                reason: None,
            },
            JobState::Transferring => Self {
                tag: "transferring",
                bytes: None,
                item_id: None,
                reason: None,
            },
            JobState::Cancelling => Self {
                tag: "cancelling",
                bytes: None,
                item_id: None,
                reason: None,
            },
            JobState::Completed { item_id, bytes } => Self {
                tag: "completed",
                bytes: Some(bytes),
                item_id,
                reason: None,
            },
            JobState::Failed(reason) => Self {
                tag: "failed",
                bytes: None,
                item_id: None,
                reason: Some(reason),
            },
            JobState::Cancelled => Self {
                tag: "cancelled",
                bytes: None,
                item_id: None,
                reason: None,
            },
            JobState::Skipped(reason) => Self {
                tag: "skipped",
                bytes: None,
                item_id: None,
                reason: Some(reason),
            },
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum WireConflict {
    Skip,
    Rename,
    Overwrite,
}

impl From<WireConflict> for ConflictPolicy {
    fn from(c: WireConflict) -> Self {
        match c {
            WireConflict::Skip => ConflictPolicy::Skip,
            WireConflict::Rename => ConflictPolicy::Rename,
            WireConflict::Overwrite => ConflictPolicy::Overwrite,
        }
    }
}

// ---------- shared state ----------

/// Lazily-built bridge: holds the orchestrator + a clone of the
/// `mtp-session::Session` we use for read-only listing calls.
///
/// The orchestrator owns the `Device`, so listing calls (which also need
/// a `Device`) cannot use the same handle. Phase 3 takes the pragmatic
/// shortcut of routing listing through the orchestrator's worker too —
/// see [`AppState::list_entries`]. This keeps a single owner.
struct AppState {
    orchestrator: Arc<Orchestrator>,
    inner: Mutex<Option<DeviceBridge>>,
    last_snapshot: Mutex<Option<DeviceSnapshot>>,
    /// Phase 4: shared ADB session built lazily by `adb_status`. The
    /// orchestrator does NOT own an `AdbContext` directly yet — the
    /// current orchestrator is single-device-typed (MTP). The Tauri
    /// shell holds the ADB session and feeds tar-upload jobs through a
    /// dedicated ADB orchestrator spawned per opt-in serial.
    adb_session: Mutex<Option<Arc<AdbSession>>>,
    /// Per-serial orchestrator handle for ADB uploads. Keyed by serial
    /// so multi-device coordination stays out of scope (plan.md §2.2).
    adb_orchestrators: Mutex<HashMap<String, Arc<Orchestrator>>>,
    /// Plan registry: `adb_plan_upload` returns an opaque token; the
    /// matching `ConflictPlan` stays here so the wire never carries the
    /// full HashMap. Phase 3 retro §4-5.
    adb_plans: Mutex<HashMap<u64, AdbPlanEntry>>,
    next_plan_id: std::sync::atomic::AtomicU64,
    /// Captured at setup so per-serial ADB orchestrator pumps can emit
    /// `transfer-event` without having to thread an `AppHandle` through
    /// every call site.
    app_handle: Mutex<Option<AppHandle>>,
}

#[derive(Debug, Clone)]
struct AdbPlanEntry {
    serial: String,
    source: PathBuf,
    dest_path: String,
    plan: ConflictPlan,
}

#[allow(dead_code)] // info/storages are kept for future reuse; UI currently re-fetches.
struct DeviceBridge {
    orchestrator: Arc<Orchestrator>,
    /// Cached device info captured at open time so the UI can render
    /// without requiring a round-trip to libmtp on every refresh.
    info: DeviceInfo,
    storages: Vec<Storage>,
}

// AppState is now constructed in `run()` inside `.setup()`

// ---------- commands ----------

/// Quick environment check the UI can show to the user when nothing
/// else works. Returns a short report describing what we can / can't see.
#[tauri::command]
fn environment_check() -> EnvReport {
    // We can't dlopen-test libmtp — at this point in execution it must
    // already be loaded, so reaching here is itself proof of presence.
    // Instead we report what we know and leave dynamic-link failure
    // recovery to the install docs.
    let mut hints = Vec::<String>::new();

    // macOS daemons that race the USB interface
    if cfg!(target_os = "macos") {
        let suspect = [
            "icdd",
            "Android File Transfer",
            "Android File Transfer Agent",
        ];
        let mut found = Vec::new();
        if let Ok(out) = std::process::Command::new("pgrep")
            .arg("-l")
            .arg("-f")
            .arg("icdd|Android File Transfer")
            .output()
        {
            if !out.stdout.is_empty() {
                for s in suspect {
                    if String::from_utf8_lossy(&out.stdout).contains(s) {
                        found.push(s.to_string());
                    }
                }
            }
        }
        if !found.is_empty() {
            hints.push(format!(
                "macOS USB 데몬이 실행 중입니다: {}. CrossMTP가 폰을 잡으려면 이 프로세스들을 종료해야 합니다. 터미널: killall \"Android File Transfer\" \"Android File Transfer Agent\" icdd",
                found.join(", ")
            ));
        }
    }

    EnvReport {
        libmtp_loaded: true, // we're running, therefore yes
        hints,
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct EnvReport {
    libmtp_loaded: bool,
    hints: Vec<String>,
}

#[tauri::command]
fn device_snapshot(
    force: Option<bool>,
    state: State<'_, AppState>,
    _app: AppHandle,
) -> Result<DeviceSnapshot, String> {
    let force = force.unwrap_or(false);
    // Reuse the existing bridge unless the caller explicitly asked for a
    // fresh open. This matters because the React side calls
    // `device_snapshot` once on mount and immediately follows up with
    // `list_entries` — opening the device twice on macOS races the
    // system USB daemons (Phase 3 retro #1).
    if !force {
        let guard = state.inner.lock().unwrap();
        if let Some(bridge) = guard.as_ref() {
            // Re-fetch storages through the worker so free/used numbers
            // stay accurate, but keep the same orchestrator + device.
            let storages_res = bridge.orchestrator.list_storages();
            match storages_res {
                Ok(storages_raw) => {
                    let storages: Vec<Storage> = storages_raw
                        .into_iter()
                        .map(|s| Storage {
                            id: s.id,
                            description: s.description,
                            free_bytes: s.free_bytes,
                            max_bytes: s.max_bytes,
                        })
                        .collect();
                    let snap = DeviceSnapshot {
                        devices: vec![bridge.info.clone()],
                        storages,
                        error: None,
                        permission_hint: false,
                    };
                    *state.last_snapshot.lock().unwrap() = Some(snap.clone());
                    return Ok(snap);
                }
                Err(_) => {
                    // Bridge appears stale (device went away?). Drop it
                    // and fall through to the rebuild path below.
                }
            }
        }
        drop(guard);
    }

    drop_bridge(&state);

    let session = mtp_session::Session::open();
    let mut devices_raw = match session.list_devices() {
        Ok(d) => d,
        Err(e) => {
            let snap = DeviceSnapshot {
                devices: vec![],
                storages: vec![],
                error: Some(format!("{e}")),
                permission_hint: e.is_likely_permission_issue(),
            };
            *state.last_snapshot.lock().unwrap() = Some(snap.clone());
            return Ok(snap);
        }
    };
    if devices_raw.is_empty() {
        let snap = DeviceSnapshot {
            devices: vec![],
            storages: vec![],
            error: None,
            permission_hint: true,
        };
        *state.last_snapshot.lock().unwrap() = Some(snap.clone());
        return Ok(snap);
    }

    let dev = devices_raw.remove(0);
    drop(devices_raw); // release any extra opens immediately

    let info_raw = dev.info();
    let info = DeviceInfo {
        index: 0,
        friendly_name: info_raw.friendly_name,
        manufacturer: info_raw.manufacturer,
        model: info_raw.model,
        serial: info_raw.serial,
    };

    let storages_raw = match dev.list_storages() {
        Ok(s) => s,
        Err(e) => {
            let snap = DeviceSnapshot {
                devices: vec![info.clone()],
                storages: vec![],
                error: Some(format!("{e}")),
                permission_hint: e.is_likely_permission_issue(),
            };
            *state.last_snapshot.lock().unwrap() = Some(snap.clone());
            return Ok(snap);
        }
    };
    let storages: Vec<Storage> = storages_raw
        .into_iter()
        .map(|s| Storage {
            id: s.id,
            description: s.description,
            free_bytes: s.free_bytes,
            max_bytes: s.max_bytes,
        })
        .collect();

    // Update the existing orchestrator instead of replacing it.
    if let Err(e) = state.orchestrator.update_device(dev) {
        let snap = DeviceSnapshot {
            devices: vec![info.clone()],
            storages: vec![],
            error: Some(format!("Failed to update device: {e}")),
            permission_hint: e.is_likely_permission_issue(),
        };
        *state.last_snapshot.lock().unwrap() = Some(snap.clone());
        return Ok(snap);
    }

    let bridge = DeviceBridge {
        orchestrator: state.orchestrator.clone(),
        info: info.clone(),
        storages: storages.clone(),
    };
    *state.inner.lock().unwrap() = Some(bridge);

    let snap = DeviceSnapshot {
        devices: vec![info],
        storages,
        error: None,
        permission_hint: false,
    };
    *state.last_snapshot.lock().unwrap() = Some(snap.clone());
    Ok(snap)
}

fn drop_bridge(state: &State<'_, AppState>) {
    let mut guard = state.inner.lock().unwrap();
    if let Some(bridge) = guard.take() {
        // Try to extract the orchestrator and shut it down so the worker
        // thread releases the device handle before we re-open.
        if let Ok(orch) = Arc::try_unwrap(bridge.orchestrator) {
            orch.shutdown();
        }
        // If shutdown() couldn't run (other Arcs alive) we still drop the
        // bridge here; Orchestrator::Drop will fire shutdown on its own.
    }
}

/// Listing routes through the orchestrator's worker so it reuses the
/// same `Device` handle the worker already owns. This is the Phase 4 #1
/// fix — opening the device twice on macOS races the system USB daemons
/// and trips `LIBMTP PANIC: Unable to initialize device`.
///
/// Side effect: while a transfer is running on the worker, listing
/// requests queue behind it. That's intentional for Phase 3/4 — single
/// worker, no parallel device access.
#[tauri::command]
fn list_entries(
    storage_id: u32,
    parent_id: u32,
    state: State<'_, AppState>,
) -> Result<Vec<Entry>, String> {
    let guard = state.inner.lock().unwrap();
    let bridge = guard.as_ref().ok_or("기기가 연결되지 않았습니다.")?;
    let raw = bridge
        .orchestrator
        .list_entries(storage_id, parent_id)
        .map_err(|e| e.to_string())?;
    Ok(raw
        .into_iter()
        .map(|e| Entry {
            item_id: e.item_id,
            parent_id: e.parent_id,
            storage_id: e.storage_id,
            name: e.name,
            size: e.size,
            modified_secs: e.modified_secs,
            kind: match e.kind {
                mtp_session::EntryKind::File => "file",
                mtp_session::EntryKind::Folder => "folder",
            },
        })
        .collect())
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
fn enqueue_download(
    storage_id: u32,
    file_id: u32,
    name: String,
    is_dir: bool,
    dest_dir: String,
    expected_size: u64,
    modified_secs: Option<u64>,
    conflict: WireConflict,
    state: State<'_, AppState>,
) -> Result<Vec<u64>, String> {
    let guard = state.inner.lock().unwrap();
    let bridge = guard.as_ref().ok_or("기기가 연결되지 않았습니다.")?;
    let conflict_policy: ConflictPolicy = conflict.into();
    let dest_path = PathBuf::from(dest_dir);

    if is_dir {
        let mut ids = Vec::new();
        let target_dir = dest_path.join(&name);
        if !target_dir.exists() {
            std::fs::create_dir_all(&target_dir)
                .map_err(|e| format!("mkdir {}: {e}", target_dir.display()))?;
        }
        download_dir_recursive(
            bridge,
            storage_id,
            file_id,
            &target_dir,
            conflict_policy,
            &mut ids,
        )?;
        if ids.is_empty() {
            return Err(format!("'{}' 안에 다운로드할 파일이 없습니다.", name));
        }
        return Ok(ids);
    }

    let id = bridge.orchestrator.enqueue(JobSpec {
        kind: JobKind::Download {
            storage_id,
            file_id,
            name,
            dest_dir: dest_path,
            expected_size,
            modified_secs,
        },
        conflict: conflict_policy,
    });
    Ok(vec![id.0])
}

fn download_dir_recursive(
    bridge: &DeviceBridge,
    storage_id: u32,
    dir_id: u32,
    local_base: &std::path::Path,
    conflict: ConflictPolicy,
    out_ids: &mut Vec<u64>,
) -> Result<(), String> {
    let entries = bridge
        .orchestrator
        .list_entries(storage_id, dir_id)
        .map_err(|e| format!("폴더 목록을 가져올 수 없습니다: {e}"))?;

    for entry in entries {
        if entry.kind == mtp_session::EntryKind::Folder {
            let next_local = local_base.join(&entry.name);
            if !next_local.exists() {
                std::fs::create_dir_all(&next_local)
                    .map_err(|e| format!("mkdir {}: {e}", next_local.display()))?;
            }
            download_dir_recursive(
                bridge,
                storage_id,
                entry.item_id,
                &next_local,
                conflict,
                out_ids,
            )?;
        } else {
            let id = bridge.orchestrator.enqueue(JobSpec {
                kind: JobKind::Download {
                    storage_id,
                    file_id: entry.item_id,
                    name: entry.name.clone(),
                    dest_dir: local_base.to_path_buf(),
                    expected_size: entry.size,
                    modified_secs: entry.modified_secs,
                },
                conflict,
            });
            out_ids.push(id.0);
        }
    }
    Ok(())
}

#[tauri::command]
async fn enqueue_upload(
    storage_id: u32,
    parent_id: u32,
    source: String,
    conflict: WireConflict,
    state: State<'_, AppState>,
) -> Result<Vec<u64>, String> {
    let orch = state.orchestrator.clone();
    let source = PathBuf::from(source);
    let conflict: ConflictPolicy = conflict.into();

    let name = source
        .file_name()
        .ok_or("경로에 파일명이 없습니다.")?
        .to_string_lossy()
        .into_owned();

    let id = orch.enqueue(JobSpec {
        kind: JobKind::BulkUpload {
            storage_id,
            parent_id,
            source,
            name,
        },
        conflict,
    });
    Ok(vec![id.0])
}

fn metadata_modified_secs(metadata: &std::fs::Metadata) -> Option<u64> {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
}

fn timestamps_match(left: Option<u64>, right: Option<u64>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.abs_diff(right) <= 2,
        _ => true,
    }
}

#[tauri::command]
fn cancel_job(job_id: u64, state: State<'_, AppState>) -> Result<(), String> {
    let guard = state.inner.lock().unwrap();
    let bridge = guard.as_ref().ok_or("기기가 연결되지 않았습니다.")?;
    bridge.orchestrator.cancel(JobId(job_id));
    Ok(())
}

#[tauri::command]
fn default_dest_dir() -> String {
    if let Some(downloads) = dirs_downloads() {
        return downloads.to_string_lossy().into_owned();
    }
    std::env::temp_dir().to_string_lossy().into_owned()
}

#[tauri::command]
async fn pick_dest_dir(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    // Tauri's dialog plugin uses an OS-thread callback. The earlier
    // version blocked the calling tauri command thread on a std mpsc
    // recv, which can deadlock the runtime if the dialog callback
    // happens on the same thread. The fix: hand the dialog an OS
    // thread-safe Sender, then `await` on a tokio task that performs
    // the blocking recv off the runtime via spawn_blocking.
    let (tx, rx) = std::sync::mpsc::channel();
    app.dialog().file().pick_folder(move |folder| {
        let _ = tx.send(folder);
    });
    let folder = tokio::task::spawn_blocking(move || rx.recv())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|_| "dialog channel closed".to_string())?;
    Ok(folder.map(|p| p.to_string()))
}

fn dirs_downloads() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home).join("Downloads");
        if p.exists() {
            return Some(p);
        }
    }
    None
}

#[tauri::command]
fn resume_queue(state: State<'_, AppState>) {
    state.orchestrator.resume_queue();
}

#[tauri::command]
fn clear_queue(state: State<'_, AppState>) {
    state.orchestrator.clear_queue();
}

#[tauri::command]
fn get_queue_state(state: State<'_, AppState>) -> bool {
    state.orchestrator.get_queue_state()
}

#[tauri::command]
fn list_local_entries(path: String) -> Result<Vec<LocalEntry>, String> {
    let mut entries = Vec::new();
    let read_dir = std::fs::read_dir(&path).map_err(|e| e.to_string())?;

    for entry in read_dir {
        let entry = entry.map_err(|e| e.to_string())?;
        let metadata = entry.metadata().map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path().to_string_lossy().into_owned();

        let modified = metadata
            .modified()
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::from_secs(0))
            .as_secs();

        entries.push(LocalEntry {
            path,
            name,
            size: metadata.len(),
            is_dir: metadata.is_dir(),
            modified,
        });
    }

    // Sort directories first, then by name
    entries.sort_by(|a, b| {
        if a.is_dir != b.is_dir {
            return b.is_dir.cmp(&a.is_dir);
        }
        a.name.cmp(&b.name)
    });

    Ok(entries)
}

#[tauri::command]
fn get_local_roots() -> Result<Vec<LocalEntry>, String> {
    let mut roots = Vec::new();

    // Add Home directory
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(LocalEntry {
            path: PathBuf::from(home).to_string_lossy().into_owned(),
            name: "Home".to_string(),
            size: 0,
            is_dir: true,
            modified: 0,
        });
    }

    #[cfg(unix)]
    {
        roots.push(LocalEntry {
            path: "/".to_string(),
            name: "Root (/)".to_string(),
            size: 0,
            is_dir: true,
            modified: 0,
        });
    }

    Ok(roots)
}

// ---------- ADB (Phase 4) ----------
//
// The ADB fast path is opt-in: the UI calls `adb_status` to discover the
// adb binary + connected devices, then `adb_plan_upload` to compute a
// conflict report for a given source/dest, and finally
// `enqueue_adb_tar_upload` with the plan token. Each step is honest
// about whether it's a real fast path or a "probe failed → fall back to
// MTP" outcome (plan.md §2.1).

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdbDeviceWire {
    serial: String,
    state: String,
    model: Option<String>,
    can_tar_upload: bool,
    tar_extract_smoke_ok: bool,
    has_tar: bool,
    has_find: bool,
    has_stat: bool,
    tar_impl: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdbStatusWire {
    adb_available: bool,
    adb_path: Option<String>,
    adb_source: Option<String>,
    error: Option<String>,
    devices: Vec<AdbDeviceWire>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdbPlanReportWire {
    plan_token: u64,
    clean: Vec<String>,
    skipped_same: Vec<String>,
    renamed: Vec<RenamedPairWire>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RenamedPairWire {
    original: String,
    new_name: String,
}

/// Discover the `adb` binary, list devices, and probe per-device
/// capabilities. Cheap enough to call every time the user opens the ADB
/// panel; UI is responsible for not polling in a loop.
#[tauri::command]
fn adb_status(state: State<'_, AppState>) -> AdbStatusWire {
    // `AdbSession::open()` runs discovery internally; cache the resulting
    // session so subsequent plan + upload calls reuse it. Reusing avoids
    // re-discovering on every plan call.
    let session = {
        let mut guard = state.adb_session.lock().unwrap();
        if let Some(s) = guard.as_ref() {
            Arc::clone(s)
        } else {
            match AdbSession::open() {
                Ok(s) => {
                    let s = Arc::new(s);
                    *guard = Some(Arc::clone(&s));
                    s
                }
                Err(e) => {
                    return AdbStatusWire {
                        adb_available: false,
                        adb_path: None,
                        adb_source: None,
                        error: Some(format!("{e}")),
                        devices: vec![],
                    };
                }
            }
        }
    };
    let loc = session.location().clone();

    let listed = match session.list_devices() {
        Ok(d) => d,
        Err(e) => {
            return AdbStatusWire {
                adb_available: true,
                adb_path: Some(loc.path.to_string_lossy().into_owned()),
                adb_source: Some(format!("{:?}", loc.source)),
                error: Some(format!("{e}")),
                devices: vec![],
            };
        }
    };

    let mut devices = Vec::new();
    for d in listed {
        if !matches!(d.state, DeviceState::Device) {
            devices.push(AdbDeviceWire {
                serial: d.serial.clone(),
                state: format!("{:?}", d.state).to_ascii_lowercase(),
                model: d.model.clone(),
                can_tar_upload: false,
                tar_extract_smoke_ok: false,
                has_tar: false,
                has_find: false,
                has_stat: false,
                tar_impl: None,
            });
            continue;
        }
        // Authorised device — probe per-device capabilities + smoke check.
        let mut caps = adb_session::probe_device(&session, &d.serial).unwrap_or_else(|_| {
            adb_session::DeviceCapabilities {
                has_tar: false,
                has_find: false,
                has_stat: false,
                tar_impl: None,
                tar_extract_smoke_ok: false,
            }
        });
        caps.tar_extract_smoke_ok =
            adb_session::smoke_check_extract(&session, &d.serial).unwrap_or(false);
        devices.push(AdbDeviceWire {
            serial: d.serial.clone(),
            state: "device".into(),
            model: d.model.clone(),
            can_tar_upload: caps.can_tar_upload(),
            tar_extract_smoke_ok: caps.tar_extract_smoke_ok,
            has_tar: caps.has_tar,
            has_find: caps.has_find,
            has_stat: caps.has_stat,
            tar_impl: caps.tar_impl.clone(),
        });
    }

    AdbStatusWire {
        adb_available: true,
        adb_path: Some(loc.path.to_string_lossy().into_owned()),
        adb_source: Some(format!("{:?}", loc.source)),
        error: None,
        devices,
    }
}

/// Run a manifest probe + `plan_upload` for the local `source` against
/// the device-side `dest_path`. Returns a `PlanReport` plus an opaque
/// `plan_token` the UI hands back to `enqueue_adb_tar_upload`. The full
/// `ConflictPlan` (HashMap-backed) stays server-side; only counts and
/// per-entry summaries cross the IPC boundary. Phase 3 retro §4-5.
#[tauri::command]
fn adb_plan_upload(
    serial: String,
    source: String,
    dest_path: String,
    state: State<'_, AppState>,
) -> Result<AdbPlanReportWire, String> {
    let session = state
        .adb_session
        .lock()
        .unwrap()
        .as_ref()
        .map(Arc::clone)
        .ok_or_else(|| "ADB 세션이 아직 초기화되지 않았습니다. 먼저 ADB 상태를 새로고침해주세요.".to_string())?;

    let source_path = PathBuf::from(&source);
    if !source_path.exists() {
        return Err(format!("로컬 경로를 찾을 수 없습니다: {source}"));
    }

    // Collect local files (relative paths + size + mtime). We reuse
    // tar-stream's walk for the path normalisation and mtime semantics.
    let local_root = source_path.clone();
    let walked = tar_stream::walk(&local_root).map_err(|e| format!("로컬 스캔 실패: {e}"))?;
    let mut locals = Vec::new();
    for e in walked {
        if matches!(e.kind, tar_stream::EntryKind::File) {
            locals.push(LocalFile {
                rel_path: e.relative.join("/"),
                size: e.size,
                mtime_secs: e.mtime_secs,
            });
        }
    }

    let remote = probe_manifest(&session, &serial, &dest_path)
        .map_err(|e| format!("기기 매니페스트 조회 실패: {e}"))?;

    let policy = UploadPolicy::plan_defaults();
    let (plan, report) = plan_upload(&PlanRequest {
        locals: &locals,
        remote: &remote,
        policy: &policy,
    })
    .map_err(|e| format!("planner: {e}"))?;

    let token = state
        .next_plan_id
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    state.adb_plans.lock().unwrap().insert(
        token,
        AdbPlanEntry {
            serial: serial.clone(),
            source: source_path.clone(),
            dest_path: dest_path.clone(),
            plan,
        },
    );

    Ok(AdbPlanReportWire {
        plan_token: token,
        clean: report.clean,
        skipped_same: report.skipped_same,
        renamed: report
            .renamed
            .into_iter()
            .map(|(original, new_name)| RenamedPairWire { original, new_name })
            .collect(),
    })
}

/// Enqueue an ADB tar-upload job. `plan_token` must come from a prior
/// `adb_plan_upload` call in this process — tokens are not durable
/// across restarts. Returns the orchestrator job id.
#[tauri::command]
fn enqueue_adb_tar_upload(
    plan_token: u64,
    state: State<'_, AppState>,
) -> Result<u64, String> {
    let entry = state
        .adb_plans
        .lock()
        .unwrap()
        .remove(&plan_token)
        .ok_or_else(|| "유효하지 않은 plan_token (만료되었거나 이미 사용됨)".to_string())?;

    let session = state
        .adb_session
        .lock()
        .unwrap()
        .as_ref()
        .map(Arc::clone)
        .ok_or_else(|| "ADB 세션이 초기화되지 않았습니다.".to_string())?;

    // One orchestrator per serial. The orchestrator owns the AdbContext
    // exclusively, so we look up an existing handle or spin a new one
    // bound to this device. MTP-only orchestrator stays the
    // app-default; ADB jobs only ever run on the per-serial
    // orchestrator.
    let mut map = state.adb_orchestrators.lock().unwrap();
    let orch = if let Some(o) = map.get(&entry.serial) {
        Arc::clone(o)
    } else {
        let adb_ctx = AdbContext::probe(session.clone(), entry.serial.clone())
            .map_err(|e| format!("ADB 디바이스 프로브 실패: {e}"))?;
        let (orch, events) = Orchestrator::start_with_adb(None, Some(adb_ctx));
        let orch = Arc::new(orch);
        let app_clone_orch = orch.clone();
        // Pump events through the same Tauri channel — but we need
        // access to AppHandle. Defer pump setup to the worker thread we
        // already manage in `run()`. To keep this PR contained, we
        // launch a per-serial pump now using the orchestrator we just
        // started. The pump runs until the orchestrator drops.
        spawn_adb_event_pump(events, &app_handle_for_state(&state));
        map.insert(entry.serial.clone(), Arc::clone(&orch));
        let _ = app_clone_orch; // keep alive
        orch
    };

    let id = orch.enqueue(JobSpec {
        kind: JobKind::AdbTarUpload {
            serial: entry.serial,
            source: entry.source,
            dest_path: entry.dest_path,
            plan: entry.plan,
        },
        conflict: ConflictPolicy::Skip,
    });
    Ok(id.0)
}

/// Lightweight stat for a local path. Used by the UI to decide whether
/// a dropped path is a directory (→ candidate for ADB tar fast path) or
/// a single file (→ stays on MTP). Errors only on missing/permission.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalStatWire {
    is_dir: bool,
    exists: bool,
}

#[tauri::command]
fn local_stat(path: String) -> Result<LocalStatWire, String> {
    match std::fs::metadata(&path) {
        Ok(md) => Ok(LocalStatWire {
            is_dir: md.is_dir(),
            exists: true,
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(LocalStatWire {
            is_dir: false,
            exists: false,
        }),
        Err(e) => Err(format!("{e}")),
    }
}

/// Verify that `/sdcard` (= internal shared storage on virtually every
/// modern Android device) is reachable & writable on the given serial.
/// Returns the absolute device path if usable, otherwise `Ok(None)` so
/// the UI can silently fall back to MTP. Errors only on adb-session
/// plumbing failures.
///
/// We do not currently attempt to map MTP storage descriptions to SD-card
/// or OTG paths — that's deferred (plan.md §2.1, §4.1). The single
/// "internal" root covers ~all phones in the stated MVP scope.
#[tauri::command]
fn adb_internal_storage_root(
    serial: String,
    state: State<'_, AppState>,
) -> Result<Option<String>, String> {
    let session = state
        .adb_session
        .lock()
        .unwrap()
        .as_ref()
        .map(Arc::clone)
        .ok_or_else(|| "ADB 세션이 초기화되지 않았습니다.".to_string())?;
    // `ls -d /sdcard` is enough: exit 0 + no stderr means the path is
    // present and the shell user can stat it. We don't probe writability
    // here — the smoke check on the AdbContext already covers tar -x on
    // /sdcard (see `smoke_check_extract`).
    let out = session
        .shell(&serial, &["ls", "-d", "/sdcard"])
        .map_err(|e| format!("adb shell 실패: {e}"))?;
    if out.exit_code == 0 && out.stdout.contains("/sdcard") {
        Ok(Some("/sdcard".to_string()))
    } else {
        Ok(None)
    }
}

/// Cancel an ADB job by id and serial — we don't track which serial owns
/// which id on the JS side, so the JS layer passes the serial that
/// minted the plan.
#[tauri::command]
fn adb_cancel_job(
    serial: String,
    job_id: u64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let map = state.adb_orchestrators.lock().unwrap();
    let orch = map
        .get(&serial)
        .ok_or_else(|| format!("ADB orchestrator for {serial} not running"))?;
    orch.cancel(JobId(job_id));
    Ok(())
}

/// AppHandle reach-around for the lazy per-serial orchestrator pump.
/// Stored on `AppState` at setup time.
fn app_handle_for_state(state: &State<'_, AppState>) -> AppHandle {
    state
        .app_handle
        .lock()
        .unwrap()
        .clone()
        .expect("app handle set at setup")
}

fn spawn_adb_event_pump(events: std::sync::mpsc::Receiver<OrchEvent>, app: &AppHandle) {
    let app = app.clone();
    thread::Builder::new()
        .name("crossmtp-adb-event-pump".into())
        .spawn(move || {
            for event in events.iter() {
                let wire = match event {
                    OrchEvent::Enqueued { id, kind } => WireEvent::Enqueued {
                        id: id.0,
                        kind: kind.into(),
                    },
                    OrchEvent::StateChanged { id, state } => WireEvent::StateChanged {
                        id: id.0,
                        state: state.into(),
                    },
                    OrchEvent::Progress { id, sent, total } => WireEvent::Progress {
                        id: id.0,
                        sent,
                        total,
                    },
                    OrchEvent::BulkProgress {
                        id,
                        current_file,
                        files_done,
                        total_files,
                    } => WireEvent::BulkProgress {
                        id: id.0,
                        current_file,
                        files_done,
                        total_files,
                    },
                    OrchEvent::QueuePaused { reason } => WireEvent::QueuePaused { reason },
                    OrchEvent::WorkerStopped => WireEvent::WorkerStopped,
                };
                if let Err(e) = app.emit("transfer-event", &wire) {
                    eprintln!("[crossmtp-adb] event emit failed: {e}");
                }
            }
        })
        .expect("failed to spawn adb event pump");
}

// ---------- entry point ----------

static APP_STATE: std::sync::OnceLock<()> = std::sync::OnceLock::new();

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    APP_STATE.get_or_init(|| {});
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            device_snapshot,
            list_entries,
            enqueue_download,
            enqueue_upload,
            cancel_job,
            default_dest_dir,
            pick_dest_dir,
            environment_check,
            resume_queue,
            clear_queue,
            get_queue_state,
            list_local_entries,
            get_local_roots,
            adb_status,
            adb_plan_upload,
            enqueue_adb_tar_upload,
            adb_cancel_job,
            adb_internal_storage_root,
            local_stat,
        ])
        .setup(|app| {
            // Friendly default window title with version.
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_title("CrossMTP (alpha)");
            }

            // --- Menu Setup ---
            if let Ok(menu) = tauri::menu::Menu::default(app.handle()) {
                if let (Ok(en_item), Ok(ko_item)) = (
                    tauri::menu::MenuItem::with_id(app, "lang_en", "English", true, None::<&str>),
                    tauri::menu::MenuItem::with_id(app, "lang_ko", "한국어", true, None::<&str>)
                ) {
                    if let Ok(lang_menu) = tauri::menu::Submenu::with_id_and_items(app, "language", "Language", true, &[&en_item, &ko_item]) {
                        let _ = menu.append(&lang_menu);
                        let _ = app.set_menu(menu);
                    }
                }
            }

            app.on_menu_event(move |app_handle, event| {
                if event.id().as_ref() == "lang_en" {
                    let _ = app_handle.emit("language-changed", "en");
                } else if event.id().as_ref() == "lang_ko" {
                    let _ = app_handle.emit("language-changed", "ko");
                }
            });
            // ------------------

            let (orch, events) = Orchestrator::start(None);
            let orch = Arc::new(orch);
            let orch_for_state = orch.clone();

            let app_clone = app.handle().clone();
            thread::Builder::new()
                .name("crossmtp-event-pump".into())
                .spawn(move || {
                    // Per-job throttle for high-frequency progress events.
                    // libmtp's progress callback can fire dozens of times per
                    // file; on a 10k-file bulk upload that floods the webview
                    // IPC and freezes the UI even though the worker thread is
                    // still happily transferring. Cap progress emits to ~10/s
                    // per job. State changes and other one-shot events are
                    // never throttled.
                    use std::collections::HashMap;
                    use std::time::{Duration, Instant};
                    const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(100);
                    let mut last_progress: HashMap<u64, Instant> = HashMap::new();
                    let mut last_bulk_progress: HashMap<u64, Instant> = HashMap::new();

                    for event in events.iter() {
                        let wire = match event {
                            OrchEvent::Enqueued { id, kind } => WireEvent::Enqueued {
                                id: id.0,
                                kind: kind.into(),
                            },
                            OrchEvent::StateChanged { id, state } => {
                                // Terminal state — drop throttle bookkeeping
                                // so a future job with the same id (won't
                                // happen with monotonic ids, but cheap) or
                                // restart starts fresh.
                                last_progress.remove(&id.0);
                                last_bulk_progress.remove(&id.0);
                                WireEvent::StateChanged {
                                    id: id.0,
                                    state: state.into(),
                                }
                            }
                            OrchEvent::Progress { id, sent, total } => {
                                let now = Instant::now();
                                let allow = match last_progress.get(&id.0) {
                                    Some(prev) if now.duration_since(*prev) < PROGRESS_MIN_INTERVAL
                                        && sent < total =>
                                    {
                                        false
                                    }
                                    _ => true,
                                };
                                if !allow {
                                    continue;
                                }
                                last_progress.insert(id.0, now);
                                WireEvent::Progress {
                                    id: id.0,
                                    sent,
                                    total,
                                }
                            }
                            OrchEvent::BulkProgress {
                                id,
                                current_file,
                                files_done,
                                total_files,
                            } => {
                                let now = Instant::now();
                                let is_final = files_done >= total_files;
                                let allow = is_final
                                    || match last_bulk_progress.get(&id.0) {
                                        Some(prev) => {
                                            now.duration_since(*prev) >= PROGRESS_MIN_INTERVAL
                                        }
                                        None => true,
                                    };
                                if !allow {
                                    continue;
                                }
                                last_bulk_progress.insert(id.0, now);
                                WireEvent::BulkProgress {
                                    id: id.0,
                                    current_file,
                                    files_done,
                                    total_files,
                                }
                            }
                            OrchEvent::QueuePaused { reason } => WireEvent::QueuePaused { reason },
                            OrchEvent::WorkerStopped => WireEvent::WorkerStopped,
                        };
                        if let Err(e) = app_clone.emit("transfer-event", &wire) {
                            eprintln!("[crossmtp] event emit failed: {e}");
                        }
                    }
                })
                .expect("failed to spawn event pump");

            app.manage(AppState {
                orchestrator: orch_for_state,
                inner: Mutex::new(None),
                last_snapshot: Mutex::new(None),
                adb_session: Mutex::new(None),
                adb_orchestrators: Mutex::new(HashMap::new()),
                adb_plans: Mutex::new(HashMap::new()),
                next_plan_id: std::sync::atomic::AtomicU64::new(1),
                app_handle: Mutex::new(Some(app.handle().clone())),
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
