//! Phase 1 developer CLI. Subcommands:
//!
//! * `devices`                                   list devices
//! * `storages`                                  list storages on first device
//! * `ls   <storage_id> [parent_id]`             list a folder
//! * `pull <storage_id> <file_id> <dest>`        download
//! * `push <storage_id> <parent_id> <src>`       upload
//!
//! Designed to be the smallest possible reproduction harness for failure
//! scenarios: every subcommand maps 1:1 to a `mtp-session` API call so
//! Phase 2 tests can replay them.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use mtp_session::{Device, MtpError, Session, PARENT_ROOT};
use orchestrator::{ConflictPolicy, Event, JobKind, JobSpec, JobState, Orchestrator};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        print_usage(&args[0]);
        return ExitCode::from(1);
    }

    let session = Session::open();
    let cmd = args[1].as_str();
    let rest = &args[2..];

    let result = match cmd {
        "devices" => cmd_devices(&session),
        "storages" => cmd_storages(&session),
        "ls" => cmd_ls(&session, rest),
        "pull" => cmd_pull(&session, rest),
        "push" => cmd_push(&session, rest),
        "verify" => cmd_verify(&session),
        "verify-q" => cmd_verify_q(&session),
        "adb" => return cmd_adb(rest),
        "help" | "--help" | "-h" => {
            print_usage(&args[0]);
            return ExitCode::SUCCESS;
        }
        other => {
            eprintln!("unknown subcommand: {other}");
            print_usage(&args[0]);
            return ExitCode::from(1);
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            if e.is_likely_permission_issue() {
                eprintln!("hint: tap 'Allow' on the phone, unlock it, and quit Android File Transfer / Image Capture if running.");
            }
            ExitCode::from(2)
        }
    }
}

fn print_usage(prog: &str) {
    eprintln!(
        "usage:\n  \
         {prog} devices\n  \
         {prog} storages\n  \
         {prog} ls   <storage_id> [parent_id]\n  \
         {prog} pull <storage_id> <file_id> <dest_path>\n  \
         {prog} push <storage_id> <parent_id> <src_path>\n  \
         {prog} verify   (single-process read+write probe via mtp-session)\n  \
         {prog} verify-q (single-process probe via Phase 2 orchestrator: progress + cancel + conflict)\n  \
         {prog} adb where                          (resolve adb executable and source)\n  \
         {prog} adb devices                        (list adb devices with classified state)\n  \
         {prog} adb probe [serial]                 (Phase 1 capability probe: discovery + state + shell echo)\n  \
         {prog} adb shell <serial> -- <cmd...>     (run an adb shell command, capture stdout/stderr)\n  \
         {prog} adb caps <serial>                  (Phase 2 per-device capability: tar/find/stat probe)\n  \
         {prog} adb manifest <serial> <root>       (Phase 2 manifest probe: find ... stat under <root>)\n  \
         {prog} adb tar-upload <serial> <src> <dest>  (Phase 2 streaming upload: tar | adb shell tar -x)\n  \
         {prog} adb smoke <serial>                 (Phase 3 active tar -x smoke check)\n  \
         {prog} adb plan <serial> <src> <dest>     (Phase 3 conflict planner dry-run: report only)\n  \
         {prog} adb verify-q <serial> <src> <dest> (Phase 3 orchestrator end-to-end: AdbTarUpload job)"
    );
}

fn first_device(session: &Session) -> Result<Device, MtpError> {
    let mut devs = session.list_devices()?;
    if devs.is_empty() {
        return Err(MtpError::NoDevice);
    }
    Ok(devs.remove(0))
}

fn cmd_devices(session: &Session) -> Result<(), MtpError> {
    let devs = session.list_devices()?;
    if devs.is_empty() {
        println!("(no devices)");
        return Ok(());
    }
    for (i, d) in devs.iter().enumerate() {
        let info = d.info();
        println!(
            "[{i}] {} {} — serial {} (friendly: {})",
            info.manufacturer.as_deref().unwrap_or("?"),
            info.model.as_deref().unwrap_or("?"),
            info.serial.as_deref().unwrap_or("?"),
            info.friendly_name.as_deref().unwrap_or(""),
        );
        let cap = d.capabilities();
        println!(
            "    caps: list={} dl={} ul={} rename={} delete={} mkdir={} progress={} cancel={}",
            cap.can_list,
            cap.can_download,
            cap.can_upload,
            cap.can_rename,
            cap.can_delete,
            cap.can_create_folder,
            cap.supports_progress_callback,
            cap.supports_cancel,
        );
    }
    Ok(())
}

fn cmd_storages(session: &Session) -> Result<(), MtpError> {
    let dev = first_device(session)?;
    let storages = dev.list_storages()?;
    if storages.is_empty() {
        println!("(no storages — phone may be locked or MTP not granted)");
        return Ok(());
    }
    for s in storages {
        println!(
            "storage 0x{:08x}  {}  free={} max={}",
            s.id,
            s.description.as_deref().unwrap_or(""),
            human_bytes(s.free_bytes),
            human_bytes(s.max_bytes),
        );
    }
    Ok(())
}

fn cmd_ls(session: &Session, args: &[String]) -> Result<(), MtpError> {
    if args.is_empty() {
        return Err(MtpError::InvalidArgument(
            "ls needs <storage_id> [parent_id]",
        ));
    }
    let storage_id = parse_u32(&args[0])?;
    let parent_id = if args.len() >= 2 {
        parse_u32(&args[1])?
    } else {
        PARENT_ROOT
    };
    let dev = first_device(session)?;
    let entries = dev.list_entries(storage_id, parent_id)?;
    if entries.is_empty() {
        println!("(empty)");
        return Ok(());
    }
    for e in entries {
        let kind = match e.kind {
            mtp_session::EntryKind::Folder => "DIR ",
            mtp_session::EntryKind::File => "FILE",
        };
        println!(
            "{kind} id={:>10}  size={:>12}  {}",
            e.item_id,
            human_bytes(e.size),
            e.name,
        );
    }
    Ok(())
}

fn cmd_pull(session: &Session, args: &[String]) -> Result<(), MtpError> {
    if args.len() < 3 {
        return Err(MtpError::InvalidArgument(
            "pull needs <storage_id> <file_id> <dest>",
        ));
    }
    let _storage_id = parse_u32(&args[0])?;
    let file_id = parse_u32(&args[1])?;
    let dest = PathBuf::from(&args[2]);
    let dev = first_device(session)?;
    println!("downloading file id {file_id} → {}", dest.display());
    dev.download_file(file_id, &dest)?;
    let bytes = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    println!("done. {} bytes written.", human_bytes(bytes));
    Ok(())
}

fn cmd_push(session: &Session, args: &[String]) -> Result<(), MtpError> {
    if args.len() < 3 {
        return Err(MtpError::InvalidArgument(
            "push needs <storage_id> <parent_id> <src>",
        ));
    }
    let storage_id = parse_u32(&args[0])?;
    let parent_id = parse_u32(&args[1])?;
    let src = PathBuf::from(&args[2]);
    let name = src
        .file_name()
        .ok_or(MtpError::InvalidArgument("source path has no filename"))?
        .to_string_lossy()
        .into_owned();
    let dev = first_device(session)?;
    println!(
        "uploading {} → storage 0x{:08x} parent {parent_id} as {name}",
        src.display(),
        storage_id,
    );
    let new_id = dev.upload_file(&src, storage_id, parent_id, &name)?;
    println!("done. new item id = {new_id}");
    Ok(())
}

/// Single-process end-to-end probe. Designed for the macOS reality where
/// each new process re-races system daemons for the USB interface.
fn cmd_verify(session: &Session) -> Result<(), MtpError> {
    println!("=== verify: device list ===");
    let mut devs = session.list_devices()?;
    if devs.is_empty() {
        return Err(MtpError::NoDevice);
    }
    for (i, d) in devs.iter().enumerate() {
        let info = d.info();
        println!(
            "[{i}] {} {} — serial {}",
            info.manufacturer.as_deref().unwrap_or("?"),
            info.model.as_deref().unwrap_or("?"),
            info.serial.as_deref().unwrap_or("?"),
        );
    }
    let dev = devs.remove(0);
    drop(devs); // release any extra handles before doing real work
    println!("\n=== verify: storages on first device ===");
    let storages = dev.list_storages()?;
    if storages.is_empty() {
        println!("(no storages — phone may be locked or MTP not granted)");
        return Ok(());
    }
    for s in &storages {
        println!(
            "storage 0x{:08x}  {}  free={} max={}",
            s.id,
            s.description.as_deref().unwrap_or(""),
            human_bytes(s.free_bytes),
            human_bytes(s.max_bytes),
        );
    }
    let primary = &storages[0];

    println!(
        "\n=== verify: list root of storage 0x{:08x} ===",
        primary.id
    );
    let entries = dev.list_entries(primary.id, PARENT_ROOT)?;
    let mut shown = 0;
    let mut first_file: Option<mtp_session::Entry> = None;
    let mut first_folder: Option<mtp_session::Entry> = None;
    for e in entries {
        if shown < 15 {
            let kind = match e.kind {
                mtp_session::EntryKind::Folder => "DIR ",
                mtp_session::EntryKind::File => "FILE",
            };
            println!(
                "{kind} id={:>10}  size={:>12}  {}",
                e.item_id,
                human_bytes(e.size),
                e.name,
            );
            shown += 1;
        }
        if first_file.is_none() && e.kind == mtp_session::EntryKind::File {
            first_file = Some(e.clone());
        }
        if first_folder.is_none() && e.kind == mtp_session::EntryKind::Folder {
            first_folder = Some(e.clone());
        }
    }

    if let Some(folder) = &first_folder {
        println!(
            "\n=== verify: list inside folder '{}' (id {}) ===",
            folder.name, folder.item_id
        );
        let inner = dev.list_entries(primary.id, folder.item_id)?;
        for (i, e) in inner.iter().take(10).enumerate() {
            let kind = match e.kind {
                mtp_session::EntryKind::Folder => "DIR ",
                mtp_session::EntryKind::File => "FILE",
            };
            println!(
                "  [{i}] {kind} id={} size={} {}",
                e.item_id,
                human_bytes(e.size),
                e.name
            );
            if first_file.is_none() && e.kind == mtp_session::EntryKind::File {
                first_file = Some(e.clone());
            }
        }
    }

    if let Some(file) = first_file {
        let dest = std::env::temp_dir().join(format!("crossmtp-verify-{}.bin", file.item_id));
        println!(
            "\n=== verify: download id {} ('{}', {}) → {} ===",
            file.item_id,
            file.name,
            human_bytes(file.size),
            dest.display()
        );
        match dev.download_file(file.item_id, &dest) {
            Ok(()) => {
                let got = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
                println!("download OK: {} on disk", human_bytes(got));
                if got != file.size {
                    println!("WARN: size mismatch (expected {}, got {})", file.size, got);
                }

                println!("\n=== verify: round-trip upload (push the file we just pulled) ===");
                let upload_name = format!("crossmtp-verify-{}.bin", file.item_id);
                match dev.upload_file(&dest, primary.id, PARENT_ROOT, &upload_name) {
                    Ok(new_id) => println!("upload OK: new item id {new_id}"),
                    Err(e) => {
                        println!("upload FAILED: {e}");
                        if e.is_likely_permission_issue() {
                            println!("hint: tap Allow on phone");
                        }
                    }
                }
            }
            Err(e) => println!("download FAILED: {e}"),
        }
    } else {
        println!("\n(no files at root or in first folder; skipping pull/push)");
    }

    println!("\n=== verify: done ===");
    Ok(())
}

/// End-to-end orchestrator probe. Picks the first device, the first
/// storage, walks one folder to find a downloadable file, then exercises:
///
/// 1. download with progress events
/// 2. upload (round-trip) with progress events
/// 3. upload again with `Rename` policy → expect new filename, not error
/// 4. upload again with `Skip`   policy → expect Skipped state
/// 5. cancel: enqueue a download and immediately cancel before run
///
/// Designed to expose orchestrator state transitions, not to test edge
/// cases of libmtp itself (Phase 4 will do that with a checklist).
fn cmd_verify_q(session: &Session) -> Result<(), MtpError> {
    println!("=== verify-q: device + storage discovery ===");
    let mut devs = session.list_devices()?;
    if devs.is_empty() {
        return Err(MtpError::NoDevice);
    }
    let dev = devs.remove(0);
    drop(devs);
    {
        let info = dev.info();
        println!(
            "device: {} {} (serial {})",
            info.manufacturer.as_deref().unwrap_or("?"),
            info.model.as_deref().unwrap_or("?"),
            info.serial.as_deref().unwrap_or("?"),
        );
        let cap = dev.capabilities();
        println!(
            "caps: progress={} cancel={} (post-Phase-2)",
            cap.supports_progress_callback, cap.supports_cancel
        );
    }

    let storages = dev.list_storages()?;
    let primary = storages
        .first()
        .ok_or(MtpError::StorageUnavailable)?
        .clone();
    println!(
        "storage 0x{:08x} {} (free {})",
        primary.id,
        primary.description.as_deref().unwrap_or(""),
        human_bytes(primary.free_bytes)
    );

    // Find a small file: walk root, then first folder that has a file.
    let root = dev.list_entries(primary.id, PARENT_ROOT)?;
    let mut downloadable: Option<mtp_session::Entry> = root
        .iter()
        .find(|e| e.kind == mtp_session::EntryKind::File && e.size > 0 && e.size < 1_000_000)
        .cloned();
    if downloadable.is_none() {
        for folder in root
            .iter()
            .filter(|e| e.kind == mtp_session::EntryKind::Folder)
        {
            let inner = dev.list_entries(primary.id, folder.item_id)?;
            if let Some(f) = inner.into_iter().find(|e| {
                e.kind == mtp_session::EntryKind::File && e.size > 0 && e.size < 5_000_000
            }) {
                downloadable = Some(f);
                break;
            }
        }
    }
    let target = downloadable.ok_or_else(|| {
        MtpError::Device("no small downloadable file found in first 2 levels".into())
    })?;
    println!(
        "picked file: id={} '{}' ({})",
        target.item_id,
        target.name,
        human_bytes(target.size)
    );

    let storage_id = primary.id;
    let dest_dir = std::env::temp_dir().join("crossmtp-verify-q");
    let _ = std::fs::remove_dir_all(&dest_dir);
    std::fs::create_dir_all(&dest_dir).map_err(MtpError::Io)?;

    // Hand the device to the orchestrator. From here on we never touch
    // the device handle directly — that's the whole point of the layer.
    let (orch, events) = Orchestrator::start(Some(dev));

    // --- 1. download with progress ---
    println!("\n--- 1) download via orchestrator ---");
    let download_id = orch.enqueue(JobSpec {
        kind: JobKind::Download {
            storage_id,
            file_id: target.item_id,
            name: target.name.clone(),
            dest_dir: dest_dir.clone(),
            expected_size: target.size,
            modified_secs: target.modified_secs,
        },
        conflict: ConflictPolicy::Overwrite,
    });
    let result = wait_terminal(&events, download_id, Duration::from_secs(120));
    if !matches!(result, Some(JobState::Completed { .. })) {
        println!("download did not complete cleanly: {result:?}");
        orch.shutdown();
        return Err(MtpError::TransferFailed);
    }
    let local_path = dest_dir.join(&target.name);
    let on_disk = std::fs::metadata(&local_path).map(|m| m.len()).unwrap_or(0);
    println!("on-disk size: {}", human_bytes(on_disk));

    // --- 2. upload (round-trip) ---
    println!("\n--- 2) upload (round-trip) ---");
    let upload_name = format!("crossmtp-verify-q-{}.bin", target.item_id);
    let _ = orch.enqueue(JobSpec {
        kind: JobKind::Upload {
            storage_id,
            parent_id: PARENT_ROOT,
            source: local_path.clone(),
            name: upload_name.clone(),
            relative_path: vec![],
        },
        conflict: ConflictPolicy::Skip,
    });
    let upload_state =
        drain_until_terminal_with_label(&events, "upload-1", Duration::from_secs(120));
    println!("upload-1 outcome: {upload_state:?}");

    // --- 3. upload again with Rename ---
    println!("\n--- 3) upload again (Rename) — should produce a new filename ---");
    let _ = orch.enqueue(JobSpec {
        kind: JobKind::Upload {
            storage_id,
            parent_id: PARENT_ROOT,
            source: local_path.clone(),
            name: upload_name.clone(),
            relative_path: vec![],
        },
        conflict: ConflictPolicy::Rename,
    });
    let rename_state =
        drain_until_terminal_with_label(&events, "upload-2", Duration::from_secs(120));
    println!("upload-2 outcome: {rename_state:?}");

    // --- 4. upload again with Skip ---
    println!("\n--- 4) upload again (Skip) — should hit Skipped ---");
    let _ = orch.enqueue(JobSpec {
        kind: JobKind::Upload {
            storage_id,
            parent_id: PARENT_ROOT,
            source: local_path.clone(),
            name: upload_name.clone(),
            relative_path: vec![],
        },
        conflict: ConflictPolicy::Skip,
    });
    let skip_state = drain_until_terminal_with_label(&events, "upload-3", Duration::from_secs(60));
    println!("upload-3 outcome: {skip_state:?}");

    // --- 5. cancel: enqueue + immediate cancel ---
    println!("\n--- 5) cancel-before-run ---");
    let to_cancel = orch.enqueue(JobSpec {
        kind: JobKind::Download {
            storage_id,
            file_id: target.item_id,
            name: format!("cancel-{}", target.name),
            dest_dir: dest_dir.clone(),
            expected_size: target.size,
            modified_secs: target.modified_secs,
        },
        conflict: ConflictPolicy::Overwrite,
    });
    orch.cancel(to_cancel);
    let cancel_state = wait_terminal(&events, to_cancel, Duration::from_secs(30));
    println!("cancel outcome: {cancel_state:?}");

    orch.shutdown();
    println!("\n=== verify-q: done ===");
    Ok(())
}

fn wait_terminal(
    rx: &std::sync::mpsc::Receiver<Event>,
    id: orchestrator::JobId,
    timeout: Duration,
) -> Option<JobState> {
    let deadline = Instant::now() + timeout;
    let mut last_progress_print = Instant::now();
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining.min(Duration::from_millis(500))) {
            Ok(Event::StateChanged { id: ev_id, state }) if ev_id == id => {
                println!("  [{}] state -> {:?}", id.0, state);
                if state.is_terminal() {
                    return Some(state);
                }
            }
            Ok(Event::Progress {
                id: ev_id,
                sent,
                total,
            }) if ev_id == id => {
                if last_progress_print.elapsed() > Duration::from_millis(150) {
                    let pct = if total > 0 {
                        (sent as f64 / total as f64) * 100.0
                    } else {
                        0.0
                    };
                    println!("  [{}] progress {} / {} ({:.1}%)", id.0, sent, total, pct);
                    last_progress_print = Instant::now();
                }
            }
            Ok(_) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return None,
        }
    }
    None
}

/// Like wait_terminal but doesn't know the job id ahead of time; just
/// waits for the next Enqueued + watches it.
fn drain_until_terminal_with_label(
    rx: &std::sync::mpsc::Receiver<Event>,
    label: &str,
    timeout: Duration,
) -> Option<JobState> {
    let deadline = Instant::now() + timeout;
    let mut watching: Option<orchestrator::JobId> = None;
    let mut last_print = Instant::now();
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining.min(Duration::from_millis(500))) {
            Ok(Event::Enqueued { id, .. }) if watching.is_none() => {
                watching = Some(id);
                println!("  [{label}={}] enqueued", id.0);
            }
            Ok(Event::StateChanged { id, state }) if Some(id) == watching => {
                println!("  [{label}={}] state -> {:?}", id.0, state);
                if state.is_terminal() {
                    return Some(state);
                }
            }
            Ok(Event::Progress { id, sent, total }) if Some(id) == watching => {
                if last_print.elapsed() > Duration::from_millis(150) {
                    let pct = if total > 0 {
                        (sent as f64 / total as f64) * 100.0
                    } else {
                        0.0
                    };
                    println!(
                        "  [{label}={}] progress {} / {} ({:.1}%)",
                        id.0, sent, total, pct
                    );
                    last_print = Instant::now();
                }
            }
            Ok(_) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return None,
        }
    }
    None
}

/// Dispatch the `adb <sub>` subcommands. Returns ExitCode directly so the
/// adb error model (`AdbError`) doesn't need to be cross-cast to `MtpError`.
fn cmd_adb(rest: &[String]) -> ExitCode {
    use adb_session::{AdbError, AdbSession};

    let (sub, rest) = match rest.split_first() {
        Some((s, r)) => (s.as_str(), r),
        None => {
            eprintln!(
                "usage: adb {{where|devices|probe|shell|caps|manifest|tar-upload}} [...]"
            );
            return ExitCode::from(1);
        }
    };

    fn cmd_adb_where() -> std::result::Result<(), AdbError> {
        let loc = adb_session::discover_adb()?;
        println!("adb path:   {}", loc.path.display());
        println!("adb source: {:?}", loc.source);
        Ok(())
    }

    fn cmd_adb_devices() -> std::result::Result<(), AdbError> {
        let session = AdbSession::open()?;
        let devs = session.list_devices()?;
        if devs.is_empty() {
            println!("(no devices)");
            return Ok(());
        }
        for d in &devs {
            println!(
                "{:<24} state={:<14} transport_id={} model={} product={}",
                d.serial,
                format!("{:?}", d.state),
                d.transport_id
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "-".into()),
                d.model.as_deref().unwrap_or("-"),
                d.product.as_deref().unwrap_or("-"),
            );
        }
        Ok(())
    }

    fn cmd_adb_probe(serial_arg: Option<&str>) -> std::result::Result<(), AdbError> {
        println!("=== adb probe ===");
        let session = AdbSession::open()?;
        let loc = session.location();
        println!("adb: {} ({:?})", loc.path.display(), loc.source);
        let cap = session.capabilities();
        println!(
            "caps: probe={} tar_upload={} shell={} child_tracking={}",
            cap.adb_availability_probe,
            cap.adb_tar_upload,
            cap.can_run_shell,
            cap.can_track_child_processes,
        );
        let device = match serial_arg {
            Some(s) => session.require_device(s)?,
            None => session.pick_ready_device()?,
        };
        println!(
            "device: serial={} state={:?} model={:?} product={:?}",
            device.serial, device.state, device.model, device.product,
        );
        // Phase 1 shell smoke check: device-side getprop is cheap and
        // proves the shell pipe works without writing anything.
        let out = session.shell(&device.serial, &["getprop", "ro.build.version.release"])?;
        println!(
            "shell exit={} ro.build.version.release={}",
            out.exit_code,
            out.stdout.trim()
        );
        Ok(())
    }

    fn cmd_adb_shell(rest: &[String]) -> std::result::Result<(), AdbError> {
        if rest.len() < 2 {
            eprintln!("usage: adb shell <serial> -- <cmd...>");
            return Err(AdbError::CommandFailed {
                code: -1,
                stderr: "missing args".into(),
            });
        }
        let serial = &rest[0];
        // Accept either `shell SER -- echo hi` or `shell SER echo hi`.
        let cmd_start = if rest[1] == "--" { 2 } else { 1 };
        if cmd_start >= rest.len() {
            eprintln!("usage: adb shell <serial> -- <cmd...>");
            return Err(AdbError::CommandFailed {
                code: -1,
                stderr: "missing command".into(),
            });
        }
        let cmd: Vec<&str> = rest[cmd_start..].iter().map(|s| s.as_str()).collect();
        let session = AdbSession::open()?;
        let out = session.shell(serial, &cmd)?;
        if !out.stdout.is_empty() {
            print!("{}", out.stdout);
        }
        if !out.stderr.is_empty() {
            eprint!("{}", out.stderr);
        }
        if out.exit_code != 0 {
            return Err(AdbError::CommandFailed {
                code: out.exit_code,
                stderr: out.stderr,
            });
        }
        Ok(())
    }

    fn cmd_adb_caps(serial: &str) -> std::result::Result<(), AdbError> {
        let session = AdbSession::open()?;
        let _ = session.require_device(serial)?;
        let caps = adb_session::probe_device(&session, serial)?;
        println!(
            "tar={} find={} stat={} smoke_ok={} tar_impl={:?}",
            caps.has_tar,
            caps.has_find,
            caps.has_stat,
            caps.tar_extract_smoke_ok,
            caps.tar_impl,
        );
        println!("can_tar_upload={}", caps.can_tar_upload());
        Ok(())
    }

    fn cmd_adb_manifest(serial: &str, root: &str) -> std::result::Result<(), AdbError> {
        let session = AdbSession::open()?;
        let _ = session.require_device(serial)?;
        let m = adb_session::probe_manifest(&session, serial, root)?;
        println!("manifest under {} ({} entries):", m.root, m.len());
        let mut keys: Vec<&String> = m.entries.keys().collect();
        keys.sort();
        for k in keys {
            let e = &m.entries[k];
            println!("  {:>10}  mtime={}  {}", e.size, e.mtime_secs, k);
        }
        Ok(())
    }

    fn cmd_adb_smoke(serial: &str) -> std::result::Result<(), AdbError> {
        let session = AdbSession::open()?;
        let _ = session.require_device(serial)?;
        let ok = adb_session::smoke_check_extract(&session, serial)?;
        println!("tar_extract_smoke_ok={ok}");
        Ok(())
    }

    fn cmd_adb_plan(
        serial: &str,
        src: &str,
        dest: &str,
    ) -> std::result::Result<(), AdbError> {
        use std::path::Path;
        let session = AdbSession::open()?;
        let _ = session.require_device(serial)?;
        let remote = adb_session::probe_manifest(&session, serial, dest)?;
        // Walk local src using tar-stream's traversal so the rel paths
        // match exactly what the streamer will emit.
        let entries = tar_stream::walk(Path::new(src)).map_err(|e| AdbError::CommandFailed {
            code: -1,
            stderr: format!("local walk failed: {e}"),
        })?;
        let mut locals: Vec<adb_session::LocalFile> = Vec::new();
        for e in entries {
            if !matches!(e.kind, tar_stream::EntryKind::File) {
                continue;
            }
            locals.push(adb_session::LocalFile {
                rel_path: e.relative.join("/"),
                size: e.size,
                mtime_secs: e.mtime_secs,
            });
        }
        let policy = adb_session::UploadPolicy::plan_defaults();
        let (_, report) = adb_session::plan_upload(&adb_session::PlanRequest {
            locals: &locals,
            remote: &remote,
            policy: &policy,
        })
        .map_err(|e| AdbError::CommandFailed {
            code: -1,
            stderr: format!("plan failed: {e}"),
        })?;
        println!(
            "locals={} remote={} clean={} skipped_same={} renamed={}",
            locals.len(),
            remote.len(),
            report.clean_count(),
            report.skipped_count(),
            report.renamed_count()
        );
        for r in &report.skipped_same {
            println!("  SKIP {r}");
        }
        for (orig, new) in &report.renamed {
            println!("  RENAME {orig} -> {new}");
        }
        Ok(())
    }

    fn cmd_adb_verify_q(
        serial: &str,
        src: &str,
        dest: &str,
    ) -> std::result::Result<(), AdbError> {
        use std::path::{Path, PathBuf};
        use std::sync::Arc;
        let session = Arc::new(AdbSession::open()?);
        let _ = session.require_device(serial)?;

        // Probe + plan up-front so the orchestrator job receives a
        // ready-to-stream ConflictPlan (plan.md §5 ADB requirement).
        let remote = adb_session::probe_manifest(&session, serial, dest)?;
        let entries = tar_stream::walk(Path::new(src)).map_err(|e| AdbError::CommandFailed {
            code: -1,
            stderr: format!("walk: {e}"),
        })?;
        let mut locals: Vec<adb_session::LocalFile> = Vec::new();
        for e in entries {
            if !matches!(e.kind, tar_stream::EntryKind::File) {
                continue;
            }
            locals.push(adb_session::LocalFile {
                rel_path: e.relative.join("/"),
                size: e.size,
                mtime_secs: e.mtime_secs,
            });
        }
        let policy = adb_session::UploadPolicy::plan_defaults();
        let (plan, report) = adb_session::plan_upload(&adb_session::PlanRequest {
            locals: &locals,
            remote: &remote,
            policy: &policy,
        })
        .map_err(|e| AdbError::CommandFailed {
            code: -1,
            stderr: format!("plan: {e}"),
        })?;
        println!(
            "plan: clean={} skipped={} renamed={}",
            report.clean_count(),
            report.skipped_count(),
            report.renamed_count()
        );

        let adb_ctx = orchestrator::AdbContext {
            session: session.clone(),
            serial: serial.to_string(),
        };
        let (orch, events) = orchestrator::Orchestrator::start_with_adb(None, Some(adb_ctx));

        let id = orch.enqueue(orchestrator::JobSpec {
            kind: orchestrator::JobKind::AdbTarUpload {
                serial: serial.to_string(),
                source: PathBuf::from(src),
                dest_path: dest.to_string(),
                plan,
            },
            conflict: orchestrator::ConflictPolicy::Skip,
        });
        println!("[{}] enqueued AdbTarUpload {} -> {}", id.0, src, dest);

        let deadline = Instant::now() + Duration::from_secs(600);
        while Instant::now() < deadline {
            let rem = deadline.saturating_duration_since(Instant::now());
            match events.recv_timeout(rem.min(Duration::from_millis(500))) {
                Ok(orchestrator::Event::StateChanged { id: ev, state }) if ev == id => {
                    println!("  [{}] state -> {:?}", id.0, state);
                    if state.is_terminal() {
                        orch.shutdown();
                        return match state {
                            orchestrator::JobState::Completed { .. } => Ok(()),
                            orchestrator::JobState::Skipped(_) => Ok(()),
                            orchestrator::JobState::Cancelled => Err(AdbError::CommandFailed {
                                code: -1,
                                stderr: "cancelled".into(),
                            }),
                            orchestrator::JobState::Failed(msg) => Err(AdbError::CommandFailed {
                                code: -1,
                                stderr: msg,
                            }),
                            _ => unreachable!(),
                        };
                    }
                }
                Ok(orchestrator::Event::Progress { id: ev, sent, total }) if ev == id => {
                    println!("  [{}] progress {} / {}", id.0, sent, total);
                }
                Ok(_) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => break,
            }
        }
        orch.shutdown();
        Err(AdbError::CommandFailed {
            code: -1,
            stderr: "verify-q timed out".into(),
        })
    }

    fn cmd_adb_tar_upload(
        serial: &str,
        src: &str,
        dest: &str,
    ) -> std::result::Result<(), AdbError> {
        let session = AdbSession::open()?;
        let _ = session.require_device(serial)?;
        let cancel = adb_session::CancelHandle::new();
        let outcome = adb_session::upload_tar(
            &session,
            serial,
            std::path::Path::new(src),
            dest,
            tar_stream::ConflictPlan::new(),
            cancel,
        )?;
        println!(
            "files_emitted={} files_skipped={} bytes={} host_exit={:?}",
            outcome.progress.files_emitted,
            outcome.progress.files_skipped,
            outcome.progress.bytes_emitted,
            outcome.host_exit_code,
        );
        if !outcome.stderr_tail.is_empty() {
            eprintln!("stderr: {}", outcome.stderr_tail);
        }
        Ok(())
    }

    let result: std::result::Result<(), AdbError> = match sub {
        "where" => cmd_adb_where(),
        "devices" => cmd_adb_devices(),
        "probe" => cmd_adb_probe(rest.first().map(|s| s.as_str())),
        "shell" => cmd_adb_shell(rest),
        "caps" => {
            if rest.is_empty() {
                eprintln!("usage: adb caps <serial>");
                return ExitCode::from(1);
            }
            cmd_adb_caps(&rest[0])
        }
        "manifest" => {
            if rest.len() < 2 {
                eprintln!("usage: adb manifest <serial> <root>");
                return ExitCode::from(1);
            }
            cmd_adb_manifest(&rest[0], &rest[1])
        }
        "tar-upload" => {
            if rest.len() < 3 {
                eprintln!("usage: adb tar-upload <serial> <src> <dest>");
                return ExitCode::from(1);
            }
            cmd_adb_tar_upload(&rest[0], &rest[1], &rest[2])
        }
        "smoke" => {
            if rest.is_empty() {
                eprintln!("usage: adb smoke <serial>");
                return ExitCode::from(1);
            }
            cmd_adb_smoke(&rest[0])
        }
        "plan" => {
            if rest.len() < 3 {
                eprintln!("usage: adb plan <serial> <src> <dest>");
                return ExitCode::from(1);
            }
            cmd_adb_plan(&rest[0], &rest[1], &rest[2])
        }
        "verify-q" => {
            if rest.len() < 3 {
                eprintln!("usage: adb verify-q <serial> <src> <dest>");
                return ExitCode::from(1);
            }
            cmd_adb_verify_q(&rest[0], &rest[1], &rest[2])
        }
        other => {
            eprintln!("unknown adb subcommand: {other}");
            return ExitCode::from(1);
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            if e.is_likely_user_action_required() {
                eprintln!("hint: check the USB debugging prompt on the phone, or replug the cable.");
            }
            if matches!(e, AdbError::AdbNotAvailable) {
                eprintln!(
                    "hint: install Android platform-tools, or set CROSSMTP_ADB=/path/to/adb."
                );
            }
            ExitCode::from(2)
        }
    }
}

fn parse_u32(s: &str) -> Result<u32, MtpError> {
    let s = s.trim();
    let parsed = if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16)
    } else {
        s.parse::<u32>()
    };
    parsed.map_err(|_| MtpError::InvalidArgument("expected u32 (decimal or 0x-hex)"))
}

fn human_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} {}", UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}
