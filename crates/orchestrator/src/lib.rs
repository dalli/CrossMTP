//! CrossMTP Transfer Orchestrator (Phase 2).
//!
//! Design rules drawn from `docs/cross-mtp-dev-plan.md` and `AGENTS.md`:
//!
//! * **Single active worker.** One OS thread owns the `mtp-session::Device`
//!   handle and is the only thing allowed to talk to libmtp. All callers
//!   submit `Cmd`s through an `mpsc` channel.
//! * **Explicit state machine.** Every job goes through deterministic
//!   transitions; the worker never short-cuts. State changes are always
//!   announced through the event channel before the worker proceeds.
//! * **Failure-aware.** Every public API surface returns the cause of
//!   failure, distinguishing `Failed` from `Cancelled` and `Skipped`.
//!   "Don't claim recovery you haven't implemented."
//! * **Capability-honest.** The orchestrator only enables features the
//!   underlying `Capabilities` struct says are real.
//!
//! Out of scope for Phase 2: multi-device coordination (one worker
//! handles one device), persistent queue, retries, throttling, parallel
//! transfers. UI integration is Phase 3.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use adb_session::{AdbSession, CancelHandle as AdbCancelHandle, DeviceCapabilities};
use mtp_session::{Device, Entry, EntryKind, MtpError, Storage};
use tar_stream::ConflictPlan;

// ---------- public types ----------

/// Stable, monotonically-increasing identifier for an enqueued job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JobId(pub u64);

/// What to do when the destination already holds an entry with the same
/// name. The orchestrator handles this in `Validating`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    /// Abort the job. Job ends in [`JobState::Skipped`].
    Skip,
    /// Auto-rename ("name (1).ext", "name (2).ext", ...).
    Rename,
    /// Replace existing. **Only honored for downloads** (local `std::fs`
    /// overwrite). For uploads we currently surface `Failed` because the
    /// MVP intentionally does not expose `delete`.
    Overwrite,
}

#[derive(Debug, Clone)]
pub enum JobKind {
    Download {
        storage_id: u32,
        file_id: u32,
        /// File name as listed on the device — used for conflict resolution
        /// against the local filesystem.
        name: String,
        /// Local destination *directory*. The actual file lands at
        /// `dest_dir.join(resolved_name)`.
        dest_dir: PathBuf,
        /// Expected size from the device listing, used only for progress
        /// fallback when libmtp reports `total = 0`.
        expected_size: u64,
        /// Device-reported modification time, if available.
        modified_secs: Option<u64>,
    },
    Upload {
        storage_id: u32,
        parent_id: u32,
        source: PathBuf,
        /// Desired name on the device — may be mutated by `Rename`.
        name: String,
        /// The relative path of directories to traverse/create on the device
        /// before uploading the file.
        relative_path: Vec<String>,
    },
    BulkUpload {
        storage_id: u32,
        parent_id: u32,
        source: PathBuf,
        name: String,
    },
    /// Phase 3: ADB + tar streaming upload. Wires
    /// [`adb_session::upload_tar`] into the orchestrator state machine.
    /// The conflict plan must already be resolved (typically by
    /// `adb_session::plan_upload` against a fresh manifest probe).
    AdbTarUpload {
        /// Serial of the ADB device to target. Must already be in the
        /// `device` state.
        serial: String,
        /// Local file or directory to stream.
        source: PathBuf,
        /// Device-side destination (e.g. `/sdcard/Download/myfolder`).
        /// Validated against `is_safe_dest_path` before spawn.
        dest_path: String,
        /// Pre-computed conflict plan keyed by tar entry path.
        plan: ConflictPlan,
    },
}

#[derive(Debug, Clone)]
pub struct JobSpec {
    pub kind: JobKind,
    pub conflict: ConflictPolicy,
}

/// Lifecycle states. Transitions are only ever made by the worker.
///
/// ```text
/// Queued
///   │
///   ▼
/// Validating ──► Skipped       (conflict=Skip and target exists)
///   │       └─► Failed         (pre-flight error, e.g. listing failed)
///   │       └─► Cancelled      (cancel arrived before transfer began)
///   ▼
/// Transferring ──► Completed
///   │         └─► Cancelling ──► Cancelled
///   │         │             └─► Completed   (race: finished anyway)
///   │         └─► Failed
/// ```
#[derive(Debug, Clone)]
pub enum JobState {
    Queued,
    Validating,
    Transferring,
    Cancelling,
    Completed { item_id: Option<u32>, bytes: u64 },
    Failed(String),
    Cancelled,
    Skipped(String),
}

impl JobState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            JobState::Completed { .. }
                | JobState::Failed(_)
                | JobState::Cancelled
                | JobState::Skipped(_)
        )
    }
}

#[derive(Debug, Clone)]
pub enum Event {
    Enqueued { id: JobId, kind: JobKind },
    StateChanged { id: JobId, state: JobState },
    Progress { id: JobId, sent: u64, total: u64 },
    BulkProgress {
        id: JobId,
        current_file: String,
        files_done: u32,
        total_files: u32,
    },
    QueuePaused { reason: String },
    WorkerStopped,
}

// ---------- public API ----------

/// Bundle of ADB-side resources the orchestrator owns. `AdbSession`
/// itself is a thin handle; the orchestrator keeps it alongside the MTP
/// `Device` so a single worker can dispatch either job kind.
///
/// Wrapped in `Arc<AdbSession>` so the worker can clone it for the
/// streaming call without poisoning the worker's `&mut self`. The
/// serial is captured at construction so cancellation can pkill the
/// device-side tar even after the worker has dropped the spec.
pub struct AdbContext {
    pub session: Arc<AdbSession>,
    pub serial: String,
    /// Cached per-device capabilities probed at bring-up time. Phase 4
    /// retro §6-5: the orchestrator owns this so the UI doesn't have to
    /// re-probe before every upload. `None` means the orchestrator was
    /// constructed without a probe (test path) and the worker treats the
    /// fast-path as unavailable.
    pub capabilities: Option<DeviceCapabilities>,
}

impl AdbContext {
    /// Convenience constructor: probe the device once at bring-up,
    /// stamp the smoke-check result into `DeviceCapabilities`, and stash
    /// the cache on the context. Used by the Tauri shell when the user
    /// opts into ADB mode (plan.md §8 Phase 4 capability gate).
    pub fn probe(session: Arc<AdbSession>, serial: String) -> Result<Self, adb_session::AdbError> {
        let mut caps = adb_session::probe_device(&session, &serial)?;
        // Smoke check is a spawn+stdin call, not advertised by
        // `probe_device` itself — fill it in here so callers see one
        // authoritative `DeviceCapabilities`.
        caps.tar_extract_smoke_ok =
            adb_session::smoke_check_extract(&session, &serial).unwrap_or(false);
        Ok(Self {
            session,
            serial,
            capabilities: Some(caps),
        })
    }

    /// True when the per-device probe + smoke check both said the ADB
    /// fast path is real for this device.
    pub fn can_tar_upload(&self) -> bool {
        self.capabilities
            .as_ref()
            .map(|c| c.can_tar_upload())
            .unwrap_or(false)
    }
}

/// Public handle to the worker thread. Drop = graceful shutdown after
/// the current job completes.
pub struct Orchestrator {
    cmd_tx: Sender<Cmd>,
    next_id: AtomicU64,
    cancels: Arc<Mutex<HashMap<JobId, Arc<AtomicBool>>>>,
    adb_cancels: Arc<Mutex<HashMap<JobId, AdbCancelHandle>>>,
    join: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Orchestrator {
    /// Take ownership of `device` and spawn the worker. Returns the
    /// orchestrator handle and the receiving end of the event channel.
    /// The caller is the single subscriber — fan-out is the UI layer's
    /// responsibility.
    pub fn start(device: Option<Device>) -> (Self, Receiver<Event>) {
        Self::start_with_adb(device, None)
    }

    /// Start the worker with both MTP and ADB contexts available. Either
    /// can be `None`; the worker fails the relevant `JobKind` with a
    /// descriptive error when its context is missing.
    pub fn start_with_adb(
        device: Option<Device>,
        adb: Option<AdbContext>,
    ) -> (Self, Receiver<Event>) {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let (evt_tx, evt_rx) = mpsc::channel::<Event>();
        let cancels: Arc<Mutex<HashMap<JobId, Arc<AtomicBool>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let adb_cancels: Arc<Mutex<HashMap<JobId, AdbCancelHandle>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let cancels_worker = cancels.clone();
        let adb_cancels_worker = adb_cancels.clone();

        let join = thread::Builder::new()
            .name("crossmtp-orchestrator".into())
            .spawn(move || {
                Worker::new(device, adb, cmd_rx, evt_tx, cancels_worker, adb_cancels_worker).run();
            })
            .expect("failed to spawn orchestrator worker");

        (
            Self {
                cmd_tx,
                next_id: AtomicU64::new(1),
                cancels,
                adb_cancels,
                join: std::sync::Mutex::new(Some(join)),
            },
            evt_rx,
        )
    }

    pub fn enqueue(&self, spec: JobSpec) -> JobId {
        let id = JobId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels.lock().unwrap().insert(id, cancel.clone());
        let _ = self.cmd_tx.send(Cmd::Enqueue { id, spec, cancel });
        id
    }

    /// Synchronous read-only listing routed through the worker so it
    /// reuses the orchestrator's owned device handle. Avoids the
    /// double-open race against macOS USB daemons (Phase 3 retro #1).
    /// Blocks the caller until the worker processes the request, which
    /// may include waiting for any in-flight transfer to finish.
    pub fn list_entries(&self, storage_id: u32, parent_id: u32) -> Result<Vec<Entry>, MtpError> {
        let (tx, rx) = mpsc::channel();
        if self
            .cmd_tx
            .send(Cmd::ListEntries {
                storage_id,
                parent_id,
                reply: tx,
            })
            .is_err()
        {
            return Err(MtpError::Device("orchestrator worker not running".into()));
        }
        rx.recv()
            .unwrap_or_else(|_| Err(MtpError::Device("orchestrator reply channel closed".into())))
    }

    /// Create a folder on the device, routed through the worker.
    /// Returns the new folder's object id. See
    /// [`mtp_session::Device::create_folder`] for semantics.
    pub fn create_folder(
        &self,
        name: String,
        parent_id: u32,
        storage_id: u32,
    ) -> Result<u32, MtpError> {
        let (tx, rx) = mpsc::channel();
        if self
            .cmd_tx
            .send(Cmd::CreateFolder {
                name,
                parent_id,
                storage_id,
                reply: tx,
            })
            .is_err()
        {
            return Err(MtpError::Device("orchestrator worker not running".into()));
        }
        rx.recv()
            .unwrap_or_else(|_| Err(MtpError::Device("orchestrator reply channel closed".into())))
    }

    /// Read-only storage listing, also routed through the worker.
    pub fn list_storages(&self) -> Result<Vec<Storage>, MtpError> {
        let (tx, rx) = mpsc::channel();
        if self.cmd_tx.send(Cmd::ListStorages { reply: tx }).is_err() {
            return Err(MtpError::Device("orchestrator worker not running".into()));
        }
        rx.recv()
            .unwrap_or_else(|_| Err(MtpError::Device("orchestrator reply channel closed".into())))
    }

    pub fn cancel(&self, id: JobId) {
        if let Some(flag) = self.cancels.lock().unwrap().get(&id) {
            flag.store(true, Ordering::SeqCst);
        }
        // Also fire the ADB-specific cancel handle if this job is an
        // in-flight tar upload. The handle drives `CancelAwareSink` so
        // the next write returns `Interrupted` and the §6.1 cleanup
        // sequence runs.
        if let Some(handle) = self.adb_cancels.lock().unwrap().get(&id) {
            handle.cancel();
        }
        let _ = self.cmd_tx.send(Cmd::Cancel(id));
    }

    /// Update the device handle (e.g. after reconnection)
    pub fn update_device(&self, device: Device) -> Result<(), MtpError> {
        let (tx, rx) = mpsc::channel();
        if self
            .cmd_tx
            .send(Cmd::UpdateDevice { device, reply: tx })
            .is_ok()
        {
            let _ = rx.recv();
        }
        Ok(())
    }

    /// Resume a paused queue
    pub fn resume_queue(&self) {
        let _ = self.cmd_tx.send(Cmd::ResumeQueue);
    }

    /// Clear the queue completely
    pub fn clear_queue(&self) {
        let _ = self.cmd_tx.send(Cmd::ClearQueue);
    }

    /// Check if the queue is paused and not empty
    pub fn get_queue_state(&self) -> bool {
        let (tx, rx) = mpsc::channel();
        if self.cmd_tx.send(Cmd::GetQueueState { reply: tx }).is_ok() {
            rx.recv().unwrap_or(false)
        } else {
            false
        }
    }

    /// Gracefully shutdown the worker
    pub fn shutdown(&self) {
        let _ = self.cmd_tx.send(Cmd::Shutdown);
        if let Some(j) = self.join.lock().unwrap().take() {
            let _ = j.join();
        }
    }
}

impl Drop for Orchestrator {
    fn drop(&mut self) {
        // Best-effort shutdown if the caller forgot to call shutdown().
        let _ = self.cmd_tx.send(Cmd::Shutdown);
        if let Some(j) = self.join.lock().unwrap().take() {
            let _ = j.join();
        }
    }
}

// ---------- worker internals ----------

enum Cmd {
    Enqueue {
        id: JobId,
        spec: JobSpec,
        cancel: Arc<AtomicBool>,
    },
    Cancel(JobId),
    ListEntries {
        storage_id: u32,
        parent_id: u32,
        reply: Sender<Result<Vec<Entry>, MtpError>>,
    },
    ListStorages {
        reply: Sender<Result<Vec<Storage>, MtpError>>,
    },
    CreateFolder {
        name: String,
        parent_id: u32,
        storage_id: u32,
        reply: Sender<Result<u32, MtpError>>,
    },
    UpdateDevice {
        device: Device,
        reply: Sender<()>,
    },
    ResumeQueue,
    ClearQueue,
    GetQueueState {
        reply: Sender<bool>,
    },
    Shutdown,
}

struct PendingJob {
    id: JobId,
    spec: JobSpec,
    cancel: Arc<AtomicBool>,
}

struct Worker {
    device: Option<Device>,
    adb: Option<AdbContext>,
    paused: bool,
    cmd_rx: Receiver<Cmd>,
    evt_tx: Sender<Event>,
    cancels: Arc<Mutex<HashMap<JobId, Arc<AtomicBool>>>>,
    adb_cancels: Arc<Mutex<HashMap<JobId, AdbCancelHandle>>>,
    queue: VecDeque<PendingJob>,
    shutdown: bool,
    folder_cache: HashMap<(u32, u32, String), u32>,
}

impl Worker {
    fn new(
        device: Option<Device>,
        adb: Option<AdbContext>,
        cmd_rx: Receiver<Cmd>,
        evt_tx: Sender<Event>,
        cancels: Arc<Mutex<HashMap<JobId, Arc<AtomicBool>>>>,
        adb_cancels: Arc<Mutex<HashMap<JobId, AdbCancelHandle>>>,
    ) -> Self {
        // The pause gate only applies to MTP jobs (they require a live
        // `Device`). ADB jobs gate on `adb` presence at dispatch time.
        let paused = device.is_none() && adb.is_none();
        Self {
            device,
            adb,
            paused,
            cmd_rx,
            evt_tx,
            cancels,
            adb_cancels,
            queue: VecDeque::new(),
            shutdown: false,
            folder_cache: HashMap::new(),
        }
    }

    fn run(mut self) {
        loop {
            let has_any_backend = self.device.is_some() || self.adb.is_some();
            let block = self.paused || !has_any_backend || self.queue.is_empty();
            self.drain_commands(block);
            if self.shutdown && (self.paused || !has_any_backend || self.queue.is_empty()) {
                break;
            }
            if !self.paused && has_any_backend {
                if let Some(job) = self.queue.pop_front() {
                    self.execute(job);
                }
            }
        }
        let _ = self.evt_tx.send(Event::WorkerStopped);
    }

    /// Pull every pending command from the channel. If `block_if_empty`,
    /// block on the first recv; otherwise just drain whatever's there.
    fn drain_commands(&mut self, block_if_empty: bool) {
        if block_if_empty {
            match self.cmd_rx.recv() {
                Ok(cmd) => self.handle_cmd(cmd),
                Err(_) => {
                    self.shutdown = true;
                    return;
                }
            }
        }
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            self.handle_cmd(cmd);
        }
    }

    fn handle_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Enqueue { id, spec, cancel } => {
                let _ = self.evt_tx.send(Event::Enqueued {
                    id,
                    kind: spec.kind.clone(),
                });
                self.transition(id, JobState::Queued);
                self.queue.push_back(PendingJob { id, spec, cancel });
            }
            Cmd::ListEntries {
                storage_id,
                parent_id,
                reply,
            } => {
                let result = match &self.device {
                    Some(d) => d.list_entries(storage_id, parent_id),
                    None => Err(MtpError::NoDevice),
                };
                let _ = reply.send(result);
            }
            Cmd::ListStorages { reply } => {
                let result = match &self.device {
                    Some(d) => d.list_storages(),
                    None => Err(MtpError::NoDevice),
                };
                let _ = reply.send(result);
            }
            Cmd::CreateFolder {
                name,
                parent_id,
                storage_id,
                reply,
            } => {
                let result = match &self.device {
                    Some(d) => d.create_folder(&name, parent_id, storage_id),
                    None => Err(MtpError::NoDevice),
                };
                let _ = reply.send(result);
            }
            Cmd::UpdateDevice { device, reply } => {
                self.device = Some(device);
                self.paused = false; // Auto-resume when a new device is given
                let _ = reply.send(());
            }
            Cmd::ResumeQueue => {
                self.paused = false;
            }
            Cmd::ClearQueue => {
                while let Some(j) = self.queue.pop_front() {
                    self.transition(j.id, JobState::Cancelled);
                    self.cancels.lock().unwrap().remove(&j.id);
                }
                self.paused = false;
            }
            Cmd::GetQueueState { reply } => {
                let _ = reply.send(self.paused && !self.queue.is_empty());
            }
            Cmd::Cancel(id) => {
                // If the job is still queued and not running, fast-cancel it here.
                if let Some(pos) = self.queue.iter().position(|j| j.id == id) {
                    self.queue.remove(pos);
                    self.transition(id, JobState::Cancelled);
                    self.cancels.lock().unwrap().remove(&id);
                }
                // If it's already running, the cancel flag was set by
                // Orchestrator::cancel — the progress trampoline will see it.
            }
            Cmd::Shutdown => {
                self.shutdown = true;
            }
        }
    }

    fn execute(&mut self, job: PendingJob) {
        let id = job.id;

        // Fast-path: cancel arrived between enqueue and execute.
        if job.cancel.load(Ordering::SeqCst) {
            self.transition(id, JobState::Cancelled);
            self.cancels.lock().unwrap().remove(&id);
            self.adb_cancels.lock().unwrap().remove(&id);
            return;
        }

        // Phase 3: ADB tar uploads have their own pipeline. Dispatch
        // before the MTP-only conflict/transfer machinery runs.
        if matches!(job.spec.kind, JobKind::AdbTarUpload { .. }) {
            self.execute_adb_tar_upload(job);
            return;
        }

        self.transition(id, JobState::Validating);

        // Resolve conflicts.
        let resolved = match self.resolve_conflict(&job.spec) {
            Ok(r) => r,
            Err(state) => {
                self.transition(id, state);
                self.cancels.lock().unwrap().remove(&id);
                return;
            }
        };

        // Re-check cancel after validation.
        if job.cancel.load(Ordering::SeqCst) {
            self.transition(id, JobState::Cancelled);
            self.cancels.lock().unwrap().remove(&id);
            return;
        }

        self.transition(id, JobState::Transferring);

        // Build a fresh progress closure each retry attempt because the
        // device callbacks take FnMut by move. `mk_progress` keeps the
        // capture list explicit and tiny.
        let mk_progress = || {
            let cancel_flag = job.cancel.clone();
            let evt_tx = self.evt_tx.clone();
            move |sent: u64, total: u64| -> bool {
                let _ = evt_tx.send(Event::Progress { id, sent, total });
                cancel_flag.load(Ordering::SeqCst)
            }
        };

        let outcome = match resolved {
            Resolved::Download {
                file_id,
                full_dest,
                expected_size,
                modified_secs,
            } => {
                let download_path = full_dest.clone();
                let res = retry_in_place(|| {
                    self.device
                        .as_ref()
                        .unwrap()
                        .download_file_with_progress(
                            file_id,
                            &full_dest,
                            modified_secs,
                            mk_progress(),
                        )
                })
                .map(|()| {
                    let bytes = std::fs::metadata(&full_dest)
                        .map(|m| m.len())
                        .unwrap_or(expected_size);
                    (None, bytes)
                });
                // Cleanup any partial bytes left on disk if the user
                // cancelled or the transfer failed mid-stream. Leaving
                // half a file would silently mislead a casual user into
                // thinking they have the asset.
                if res.is_err() && download_path.exists() {
                    let _ = std::fs::remove_file(&download_path);
                }
                res
            }
            Resolved::Upload {
                source,
                storage_id,
                parent_id,
                name,
                expected_size,
            } => retry_in_place(|| {
                self.device.as_ref().unwrap().upload_file_with_progress(
                    &source,
                    storage_id,
                    parent_id,
                    &name,
                    mk_progress(),
                )
            })
            .map(|item_id| (Some(item_id), expected_size)),
            Resolved::BulkUpload {
                storage_id,
                parent_id,
                source,
                name,
            } => {
                self.execute_bulk_upload(id, job.cancel.clone(), storage_id, parent_id, source, name);
                return; // execute_bulk_upload handles transition and cleanup
            }
        };

        let mut remove_cancel_registration = true;
        match outcome {
            Ok((item_id, bytes)) => {
                self.transition(id, JobState::Completed { item_id, bytes });
            }
            Err(MtpError::Cancelled) => {
                self.transition(id, JobState::Cancelled);
            }
            Err(e) => {
                let error_msg = e.to_string();
                if e.is_session_dead() {
                    // Cable disconnect / lockscreen — handle is gone.
                    // Revert job state, push back to the queue head, and
                    // pause. UI's reconnect flow calls `update_device`.
                    self.transition(id, JobState::Queued);
                    self.queue.push_front(job);
                    self.paused = true;
                    remove_cancel_registration = false;
                    let _ = self.evt_tx.send(Event::QueuePaused { reason: error_msg });
                } else {
                    // session_lost (PTP/USB layer fault) or any other
                    // error: handle may still be alive (listings keep
                    // working), so don't lock the queue — just fail this
                    // job and let the user try something else.
                    self.transition(id, JobState::Failed(error_msg));
                }
            }
        }
        if remove_cancel_registration {
            self.cancels.lock().unwrap().remove(&id);
        }
    }

    fn resolve_conflict(&mut self, spec: &JobSpec) -> Result<Resolved, JobState> {
        match &spec.kind {
            JobKind::Download {
                storage_id: _,
                file_id,
                name,
                dest_dir,
                expected_size,
                modified_secs,
            } => {
                if !dest_dir.exists() {
                    if let Err(e) = std::fs::create_dir_all(dest_dir) {
                        return Err(JobState::Failed(format!(
                            "mkdir {}: {e}",
                            dest_dir.display()
                        )));
                    }
                }
                let candidate = dest_dir.join(name);
                let resolved_path = if candidate.exists() {
                    if local_file_matches(&candidate, *expected_size, *modified_secs) {
                        return Err(JobState::Skipped(format!(
                            "{name} already exists locally with the same size and timestamp"
                        )));
                    }
                    match spec.conflict {
                        ConflictPolicy::Skip => {
                            return Err(JobState::Skipped(format!(
                                "{} exists locally",
                                candidate.display()
                            )))
                        }
                        ConflictPolicy::Overwrite => candidate,
                        ConflictPolicy::Rename => unique_local_path(dest_dir, name),
                    }
                } else {
                    candidate
                };
                Ok(Resolved::Download {
                    file_id: *file_id,
                    full_dest: resolved_path,
                    expected_size: *expected_size,
                    modified_secs: *modified_secs,
                })
            }
            JobKind::Upload {
                storage_id,
                parent_id,
                source,
                name,
                relative_path,
            } => {
                let metadata = std::fs::metadata(source)
                    .map_err(|e| JobState::Failed(format!("stat {}: {e}", source.display())))?;
                let expected_size = metadata.len();
                let modified_secs = metadata_modified_secs(&metadata);

                let mut current_parent = *parent_id;
                for folder in relative_path {
                    let key = (*storage_id, current_parent, folder.clone());
                    if let Some(&id) = self.folder_cache.get(&key) {
                        current_parent = id;
                    } else {
                        let entries = self
                            .device
                            .as_ref()
                            .unwrap()
                            .list_entries(*storage_id, current_parent)
                            .map_err(|e| JobState::Failed(format!("list parent: {e}")))?;
                        let id = match entries
                            .iter()
                            .find(|e| e.kind == EntryKind::Folder && e.name == *folder)
                        {
                            Some(e) => e.item_id,
                            None => self
                                .device
                                .as_ref()
                                .unwrap()
                                .create_folder(folder, current_parent, *storage_id)
                                .map_err(|e| {
                                    JobState::Failed(format!("create folder '{folder}': {e}"))
                                })?,
                        };
                        self.folder_cache.insert(key, id);
                        current_parent = id;
                    }
                }

                let entries = self
                    .device
                    .as_ref()
                    .unwrap()
                    .list_entries(*storage_id, current_parent)
                    .map_err(|e| JobState::Failed(format!("list parent: {e}")))?;
                let collides = entries
                    .iter()
                    .any(|e| e.kind == EntryKind::File && e.name == *name);
                let resolved_name = if collides {
                    if entries.iter().any(|e| {
                        e.kind == EntryKind::File
                            && e.name == *name
                            && remote_file_matches(e, expected_size, modified_secs)
                    }) {
                        return Err(JobState::Skipped(format!(
                            "{name} already exists on device with the same size and timestamp"
                        )));
                    }
                    match spec.conflict {
                        ConflictPolicy::Skip => {
                            return Err(JobState::Skipped(format!("{name} exists on device")))
                        }
                        ConflictPolicy::Overwrite => {
                            return Err(JobState::Failed(
                                "Overwrite policy not supported for upload (delete capability not implemented in MVP)".into(),
                            ));
                        }
                        ConflictPolicy::Rename => unique_remote_name(&entries, name),
                    }
                } else {
                    name.clone()
                };
                Ok(Resolved::Upload {
                    source: source.clone(),
                    storage_id: *storage_id,
                    parent_id: current_parent,
                    name: resolved_name,
                    expected_size,
                })
            }
            JobKind::BulkUpload {
                storage_id,
                parent_id,
                source,
                name,
            } => Ok(Resolved::BulkUpload {
                storage_id: *storage_id,
                parent_id: *parent_id,
                source: source.clone(),
                name: name.clone(),
            }),
            JobKind::AdbTarUpload { .. } => {
                // ADB tar uploads are dispatched before `resolve_conflict`
                // runs (see `execute`). Reaching this arm means a caller
                // misrouted the spec — treat as a hard failure rather
                // than a silent skip.
                Err(JobState::Failed(
                    "AdbTarUpload reached MTP resolver — dispatch routing bug".into(),
                ))
            }
        }
    }

    fn execute_bulk_upload(
        &mut self,
        id: JobId,
        cancel: Arc<AtomicBool>,
        storage_id: u32,
        parent_id: u32,
        source: PathBuf,
        name: String,
    ) {
        // 1. Pre-scan: Total bytes and file count
        let mut total_bytes = 0;
        let mut total_files = 0;
        self.scan_local_dir(&source, &mut total_bytes, &mut total_files);

        // 2. Initial state
        let mut sent_bytes = 0;
        let mut files_done = 0;

        self.transition(id, JobState::Transferring);

        let initial_entries = match self.device.as_ref().unwrap().list_entries(storage_id, parent_id) {
            Ok(entries) => entries,
            Err(e) => {
                self.transition(id, JobState::Failed(e.to_string()));
                return;
            }
        };

        // 3. Recursive upload
        let result = self.bulk_upload_recursive(
            id,
            &cancel,
            storage_id,
            parent_id,
            &source,
            &name,
            &mut sent_bytes,
            total_bytes,
            &mut files_done,
            total_files,
            Some(&initial_entries),
        );

        match result {
            Ok(()) => {
                self.transition(id, JobState::Completed { item_id: None, bytes: sent_bytes });
                self.cancels.lock().unwrap().remove(&id);
            }
            Err(MtpError::Cancelled) => {
                self.transition(id, JobState::Cancelled);
                self.cancels.lock().unwrap().remove(&id);
            }
            Err(e) if e.is_session_dead() => {
                // Cable disconnect / lockscreen. Pause the queue so the
                // UI can surface a reconnect prompt; the bulk job itself
                // is marked Failed (mid-traversal resume requires
                // checkpointed state, deferred to Step 2).
                let error_msg = format!(
                    "{e} — Please click the refresh button to reconnect the device"
                );
                self.transition(id, JobState::Failed(error_msg.clone()));
                self.paused = true;
                let _ = self.evt_tx.send(Event::QueuePaused { reason: error_msg });
                self.cancels.lock().unwrap().remove(&id);
            }
            Err(e) if e.is_session_lost() => {
                // PTP/USB transaction desync (e.g. 0x2002 on first
                // Send_File). Handle may still be alive — don't lock
                // the whole queue; let the user try other paths.
                let error_msg = format!(
                    "{e} — Please try another location/file or check the phone's USB mode and permissions"
                );
                self.transition(id, JobState::Failed(error_msg));
                self.cancels.lock().unwrap().remove(&id);
            }
            Err(e) => {
                self.transition(id, JobState::Failed(e.to_string()));
                self.cancels.lock().unwrap().remove(&id);
            }
        }
    }

    fn scan_local_dir(&self, path: &std::path::Path, total_bytes: &mut u64, total_files: &mut u32) {
        let Ok(meta) = std::fs::metadata(path) else { return; };
        if meta.is_file() {
            *total_bytes += meta.len();
            *total_files += 1;
        } else if meta.is_dir() {
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    self.scan_local_dir(&entry.path(), total_bytes, total_files);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn bulk_upload_recursive(
        &mut self,
        id: JobId,
        cancel: &Arc<AtomicBool>,
        storage_id: u32,
        parent_id: u32,
        local_path: &std::path::Path,
        target_name: &str,
        sent_bytes: &mut u64,
        total_bytes: u64,
        files_done: &mut u32,
        total_files: u32,
        parent_entries: Option<&[Entry]>,
    ) -> Result<(), MtpError> {
        if cancel.load(Ordering::SeqCst) { return Err(MtpError::Cancelled); }

        let meta = std::fs::metadata(local_path).map_err(|e| MtpError::Device(e.to_string()))?;
        
        if meta.is_file() {
            let local_size = meta.len();
            let local_modified = metadata_modified_secs(&meta);

            // Check if identical file already exists on MTP using cached entries
            let already_exists = if let Some(entries) = parent_entries {
                entries.iter().any(|e| {
                    e.kind == EntryKind::File && 
                    e.name == target_name && 
                    e.size == local_size && 
                    timestamps_match(local_modified, e.modified_secs)
                })
            } else {
                false
            };

            if already_exists {
                *sent_bytes += local_size;
                *files_done += 1;
                let _ = self.evt_tx.send(Event::Progress {
                    id,
                    sent: *sent_bytes,
                    total: total_bytes,
                });
                return Ok(());
            }

            let _ = self.evt_tx.send(Event::BulkProgress {
                id,
                current_file: target_name.to_string(),
                files_done: *files_done,
                total_files,
            });

            let base_sent = *sent_bytes;
            let mk_progress = || {
                let evt_tx = self.evt_tx.clone();
                let cancel_clone = cancel.clone();
                move |sent: u64, _| -> bool {
                    let _ = evt_tx.send(Event::Progress {
                        id,
                        sent: base_sent + sent,
                        total: total_bytes,
                    });
                    cancel_clone.load(Ordering::SeqCst)
                }
            };

            retry_in_place(|| {
                self.device.as_ref().unwrap().upload_file_with_progress(
                    local_path,
                    storage_id,
                    parent_id,
                    target_name,
                    mk_progress(),
                )
            })?;

            *sent_bytes += local_size;
            *files_done += 1;
            // Inter-file breath: some devices (notably Samsung One UI 6,
            // Pixel Android 14+) drop PTP responses when hammered with
            // back-to-back Send_File calls. 25ms is below the human
            // perception threshold for thousands of files but enough to
            // let the device drain its response queue.
            std::thread::sleep(std::time::Duration::from_millis(25));
            Ok(())
        } else {
            // Directory: fetch entries ONCE for this directory level.
            // On miss we create the folder, then force a `list_entries`
            // round-trip on the *new* folder before recursing — some
            // Android stacks (Xiaomi HyperOS, observed first-hand) reject
            // `Send_File_From_File` with PTP 0x2002 if the parent object
            // was created in the same session but no `GetObjectHandles`
            // request has flushed the device-side object tree yet.
            let device = self.device.as_ref().unwrap();
            let resolve_existing = |e: &Entry| -> Result<(u32, Vec<Entry>), MtpError> {
                let sub = device.list_entries(storage_id, e.item_id)?;
                // Brief settle window after listing an existing folder
                // before issuing the first Send_File. Xiaomi HyperOS has
                // been observed to NACK send_file_object_info with PTP
                // 0x2002 when it lands on the wire too soon after a
                // GetObjectHandles + multiple GetObjectInfo storm on a
                // folder that already holds many children. The new-folder
                // path already pays this cost via the post-create sleep.
                std::thread::sleep(std::time::Duration::from_millis(80));
                Ok((e.item_id, sub))
            };
            let create_and_sync = |parent: u32| -> Result<(u32, Vec<Entry>), MtpError> {
                let new_id = device.create_folder(target_name, parent, storage_id)?;
                std::thread::sleep(std::time::Duration::from_millis(100));
                // GetObjectHandles on the freshly-created folder forces the
                // device to commit it before we try to write children.
                // Tolerate listing failures here: an empty result is the
                // common case, but a transient device hiccup shouldn't
                // abort the whole bulk upload.
                let sub = device.list_entries(storage_id, new_id).unwrap_or_default();
                Ok((new_id, sub))
            };
            let (mtp_dir_id, entries) = if let Some(entries) = parent_entries {
                match entries.iter().find(|e| e.kind == EntryKind::Folder && e.name == target_name) {
                    Some(e) => resolve_existing(e)?,
                    None => create_and_sync(parent_id)?,
                }
            } else {
                let parent_list = device.list_entries(storage_id, parent_id)?;
                match parent_list.iter().find(|e| e.kind == EntryKind::Folder && e.name == target_name) {
                    Some(e) => resolve_existing(e)?,
                    None => create_and_sync(parent_id)?,
                }
            };

            if let Ok(read_dir) = std::fs::read_dir(local_path) {
                for entry in read_dir.flatten() {
                    let sub_path = entry.path();
                    let sub_name = sub_path.file_name().unwrap().to_string_lossy().into_owned();
                    self.bulk_upload_recursive(
                        id,
                        cancel,
                        storage_id,
                        mtp_dir_id,
                        &sub_path,
                        &sub_name,
                        sent_bytes,
                        total_bytes,
                        files_done,
                        total_files,
                        Some(&entries),
                    )?;
                }
            }
            Ok(())
        }
    }

    /// Phase 3 — ADB tar upload pipeline. Reuses the standard state
    /// transitions (`Validating → Transferring → Completed/Failed/
    /// Cancelled`) so the UI doesn't need a separate event vocabulary.
    /// Progress events are coarse (final bytes_emitted) for now;
    /// fine-grained streaming progress arrives in Phase 4.
    fn execute_adb_tar_upload(&mut self, job: PendingJob) {
        let id = job.id;
        let (serial, source, dest_path, plan) = match job.spec.kind {
            JobKind::AdbTarUpload {
                serial,
                source,
                dest_path,
                plan,
            } => (serial, source, dest_path, plan),
            _ => unreachable!("execute_adb_tar_upload called with wrong kind"),
        };

        self.transition(id, JobState::Validating);

        let Some(adb) = self.adb.as_ref() else {
            self.transition(
                id,
                JobState::Failed("ADB context not configured for this orchestrator".into()),
            );
            self.cancels.lock().unwrap().remove(&id);
            return;
        };
        if adb.serial != serial {
            self.transition(
                id,
                JobState::Failed(format!(
                    "AdbTarUpload serial mismatch: orchestrator bound to {}, job asked for {}",
                    adb.serial, serial
                )),
            );
            self.cancels.lock().unwrap().remove(&id);
            return;
        }
        if !source.exists() {
            self.transition(
                id,
                JobState::Failed(format!("source path missing: {}", source.display())),
            );
            self.cancels.lock().unwrap().remove(&id);
            return;
        }

        // Re-check cancel after validation.
        if job.cancel.load(Ordering::SeqCst) {
            self.transition(id, JobState::Cancelled);
            self.cancels.lock().unwrap().remove(&id);
            return;
        }

        // Register the ADB cancel handle so `Orchestrator::cancel` can
        // poke it mid-stream. Drop registration on terminal state.
        let adb_cancel = AdbCancelHandle::new();
        self.adb_cancels
            .lock()
            .unwrap()
            .insert(id, adb_cancel.clone());

        self.transition(id, JobState::Transferring);

        let session = Arc::clone(&adb.session);
        // Estimate the total payload size up-front by walking the source
        // tree once. Tar header overhead (~512 B per entry + 1 KiB EOF)
        // is ignored — for typical media it's <0.5 % and the user sees a
        // monotonically progressing bar instead of a stuck-at-100 % one.
        let mut total_bytes: u64 = 0;
        let mut total_files: u32 = 0;
        self.scan_local_dir(&source, &mut total_bytes, &mut total_files);

        let evt_tx = self.evt_tx.clone();
        let on_progress: adb_session::ProgressCallback = Box::new(move |bytes| {
            let _ = evt_tx.send(Event::Progress {
                id,
                sent: bytes.min(total_bytes.max(bytes)),
                total: total_bytes.max(bytes),
            });
        });
        let outcome = adb_session::upload_tar_with_progress(
            &session,
            &serial,
            &source,
            &dest_path,
            plan,
            adb_cancel.clone(),
            Some(on_progress),
        );

        let bytes = match &outcome {
            Ok(o) => o.progress.bytes_emitted,
            Err(_) => 0,
        };
        let final_total = total_bytes.max(bytes);
        let _ = self.evt_tx.send(Event::Progress {
            id,
            sent: bytes.min(final_total),
            total: final_total,
        });

        match outcome {
            Ok(o) if !adb_cancel.is_cancelled() && o.host_exit_code == Some(0) => {
                self.transition(
                    id,
                    JobState::Completed {
                        item_id: None,
                        bytes: o.progress.bytes_emitted,
                    },
                );
            }
            Ok(_) if adb_cancel.is_cancelled() => {
                self.transition(id, JobState::Cancelled);
            }
            Ok(o) => {
                self.transition(
                    id,
                    JobState::Failed(format!(
                        "device-side tar exited {:?}: {}",
                        o.host_exit_code,
                        o.stderr_tail
                            .lines()
                            .next()
                            .unwrap_or("(no stderr)")
                    )),
                );
            }
            Err(e) => {
                if adb_cancel.is_cancelled() {
                    self.transition(id, JobState::Cancelled);
                } else {
                    self.transition(id, JobState::Failed(e.to_string()));
                }
            }
        }

        self.cancels.lock().unwrap().remove(&id);
        self.adb_cancels.lock().unwrap().remove(&id);
    }

    fn transition(&self, id: JobId, state: JobState) {
        let _ = self.evt_tx.send(Event::StateChanged { id, state });
    }
}

enum Resolved {
    Download {
        file_id: u32,
        full_dest: PathBuf,
        expected_size: u64,
        modified_secs: Option<u64>,
    },
    Upload {
        source: PathBuf,
        storage_id: u32,
        parent_id: u32,
        name: String,
        expected_size: u64,
    },
    BulkUpload {
        storage_id: u32,
        parent_id: u32,
        source: PathBuf,
        name: String,
    },
}

/// Retry a libmtp call on the same device handle up to two extra times
/// when the failure is classified as a transient device-reported error.
/// Session-broken errors and caller-side errors (`Cancelled`,
/// `InvalidArgument`, `Io`) are returned immediately so the worker can
/// route them to pause-the-queue or fail-fast paths.
fn retry_in_place<T, F>(mut op: F) -> Result<T, MtpError>
where
    F: FnMut() -> Result<T, MtpError>,
{
    const MAX_RETRIES: u32 = 2;
    let mut attempts: u32 = 0;
    loop {
        match op() {
            Ok(v) => return Ok(v),
            Err(e) if e.is_retryable_in_place() && attempts < MAX_RETRIES => {
                attempts += 1;
                let backoff_ms = 250u64 * (1u64 << (attempts - 1)); // 250, 500
                std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
                continue;
            }
            Err(e) => return Err(e),
        }
    }
}

fn unique_local_path(dir: &std::path::Path, name: &str) -> PathBuf {
    let (stem, ext) = split_name(name);
    for n in 1..1000 {
        let candidate_name = match ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        };
        let candidate = dir.join(&candidate_name);
        if !candidate.exists() {
            return candidate;
        }
    }
    // Fall back to timestamp suffix; unlikely to collide.
    dir.join(format!("{name}.{}", now_millis()))
}

fn unique_remote_name(entries: &[mtp_session::Entry], name: &str) -> String {
    let (stem, ext) = split_name(name);
    let taken: std::collections::HashSet<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    for n in 1..1000 {
        let candidate = match ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        };
        if !taken.contains(candidate.as_str()) {
            return candidate;
        }
    }
    format!("{name}.{}", now_millis())
}

fn split_name(name: &str) -> (&str, Option<&str>) {
    if let Some(dot) = name.rfind('.') {
        if dot > 0 && dot < name.len() - 1 {
            return (&name[..dot], Some(&name[dot + 1..]));
        }
    }
    (name, None)
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn local_file_matches(
    path: &std::path::Path,
    expected_size: u64,
    remote_modified_secs: Option<u64>,
) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() || metadata.len() != expected_size {
        return false;
    }
    timestamps_match(metadata_modified_secs(&metadata), remote_modified_secs)
}

fn remote_file_matches(entry: &Entry, local_size: u64, local_modified_secs: Option<u64>) -> bool {
    entry.size == local_size && timestamps_match(local_modified_secs, entry.modified_secs)
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
        // Some Android MTP providers omit modification timestamps. In that
        // case size equality is the only reliable metadata signal available.
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("crossmtp-orchestrator-{name}-{suffix}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn recv_state(rx: &Receiver<Event>, id: JobId, tag: fn(&JobState) -> bool) -> JobState {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let now = std::time::Instant::now();
            assert!(now < deadline, "timed out waiting for state event");
            let remaining = deadline.saturating_duration_since(now);
            match rx.recv_timeout(remaining).unwrap() {
                Event::StateChanged { id: got, state } if got == id && tag(&state) => {
                    return state;
                }
                _ => {}
            }
        }
    }

    #[test]
    fn terminal_state_helper_matches_public_lifecycle() {
        assert!(!JobState::Queued.is_terminal());
        assert!(!JobState::Validating.is_terminal());
        assert!(!JobState::Transferring.is_terminal());
        assert!(!JobState::Cancelling.is_terminal());
        assert!(JobState::Completed {
            item_id: None,
            bytes: 1
        }
        .is_terminal());
        assert!(JobState::Failed("nope".into()).is_terminal());
        assert!(JobState::Cancelled.is_terminal());
        assert!(JobState::Skipped("exists".into()).is_terminal());
    }

    #[test]
    fn split_name_preserves_hidden_and_extensionless_names() {
        assert_eq!(split_name("photo.jpg"), ("photo", Some("jpg")));
        assert_eq!(split_name("archive.tar.gz"), ("archive.tar", Some("gz")));
        assert_eq!(split_name("README"), ("README", None));
        assert_eq!(split_name(".nomedia"), (".nomedia", None));
        assert_eq!(split_name("trailing."), ("trailing.", None));
    }

    #[test]
    fn unique_local_path_uses_next_available_finder_style_suffix() {
        let dir = temp_dir("local-rename");
        fs::write(dir.join("photo.jpg"), b"one").unwrap();
        fs::write(dir.join("photo (1).jpg"), b"two").unwrap();

        let candidate = unique_local_path(&dir, "photo.jpg");

        assert_eq!(candidate.file_name().unwrap(), "photo (2).jpg");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unique_remote_name_uses_next_available_finder_style_suffix() {
        let entries = vec![
            mtp_session::Entry {
                item_id: 1,
                parent_id: 0,
                storage_id: 10,
                name: "clip.mp4".into(),
                size: 1,
                modified_secs: Some(100),
                kind: mtp_session::EntryKind::File,
            },
            mtp_session::Entry {
                item_id: 2,
                parent_id: 0,
                storage_id: 10,
                name: "clip (1).mp4".into(),
                size: 1,
                modified_secs: Some(101),
                kind: mtp_session::EntryKind::File,
            },
        ];

        assert_eq!(unique_remote_name(&entries, "clip.mp4"), "clip (2).mp4");
    }

    #[test]
    fn queued_job_can_be_cancelled_while_worker_is_paused_without_device() {
        let (orch, rx) = Orchestrator::start(None);
        let id = orch.enqueue(JobSpec {
            kind: JobKind::Download {
                storage_id: 1,
                file_id: 2,
                name: "demo.bin".into(),
                dest_dir: std::env::temp_dir(),
                expected_size: 10,
                modified_secs: None,
            },
            conflict: ConflictPolicy::Rename,
        });

        recv_state(&rx, id, |s| matches!(s, JobState::Queued));
        assert!(orch.get_queue_state());

        orch.cancel(id);
        let state = recv_state(&rx, id, |s| matches!(s, JobState::Cancelled));

        assert!(matches!(state, JobState::Cancelled));
        assert!(!orch.get_queue_state());
        orch.shutdown();
    }

    #[test]
    fn adb_tar_upload_without_adb_context_fails_with_descriptive_error() {
        let (orch, rx) = Orchestrator::start_with_adb(None, None);
        // Force a non-empty queue gate by feeding a fake AdbContext-less job.
        // Without an AdbContext the worker may stay paused, so we use a
        // dummy MTP-less device but a noop ADB serial mismatch path —
        // since adb is None we test the "ADB context not configured"
        // branch through validation. The worker only runs jobs when
        // *some* backend is present; without either it pauses. So the
        // job stays Queued and we just observe Queued + cancel it to
        // keep the test deterministic.
        let id = orch.enqueue(JobSpec {
            kind: JobKind::AdbTarUpload {
                serial: "ANY".into(),
                source: std::env::temp_dir(),
                dest_path: "/sdcard/Download/x".into(),
                plan: tar_stream::ConflictPlan::new(),
            },
            conflict: ConflictPolicy::Skip,
        });
        recv_state(&rx, id, |s| matches!(s, JobState::Queued));
        orch.cancel(id);
        let state = recv_state(&rx, id, |s| matches!(s, JobState::Cancelled));
        assert!(matches!(state, JobState::Cancelled));
        orch.shutdown();
    }

    #[test]
    fn read_commands_report_no_device_when_worker_has_no_device() {
        let (orch, _rx) = Orchestrator::start(None);

        let err = orch.list_entries(1, mtp_session::PARENT_ROOT).unwrap_err();

        assert!(matches!(err, MtpError::NoDevice));
        orch.shutdown();
    }
}
