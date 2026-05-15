import {
  Dispatch,
  PointerEvent as ReactPointerEvent,
  SetStateAction,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ask } from "@tauri-apps/plugin-dialog";
import {
  ConflictPolicy,
  DeviceSnapshot,
  Entry,
  JobView,
  QueueGroupView,
  Storage,
  TransferEvent,
  LocalEntry,
  AdbStatusWire,
  AdbPlanReport,
} from "./types";
import { AdbPanel } from "./components/AdbPanel";
import { Banner } from "./components/Banner";
import { Browser } from "./components/Browser";
import { LocalBrowser } from "./components/LocalBrowser";
import { QueuePanel } from "./components/QueuePanel";

interface BreadcrumbNode {
  id: number; // PARENT_ROOT (0xFFFFFFFF) for the storage root
  name: string;
}

const PARENT_ROOT = 0xffffffff;

type InternalDrag =
  | { type: "local"; item: LocalEntry; label: string; x: number; y: number }
  | { type: "mtp"; item: Entry; label: string; x: number; y: number };

export function App() {
  const { t } = useTranslation();
  const [snapshot, setSnapshot] = useState<DeviceSnapshot | null>(null);
  const [loading, setLoading] = useState(false);
  const [activeStorage, setActiveStorage] = useState<Storage | null>(null);
  const [breadcrumb, setBreadcrumb] = useState<BreadcrumbNode[]>([]);
  const [entries, setEntries] = useState<Entry[]>([]);
  const [browserError, setBrowserError] = useState<string | null>(null);
  const [jobs, setJobs] = useState<Map<number, JobView>>(new Map());
  const [jobGroups, setJobGroups] = useState<Map<number, QueueGroupView>>(new Map());
  const [conflictPolicy, setConflictPolicy] = useState<ConflictPolicy>("rename");
  
  // Local Browser States
  const [localPath, setLocalPath] = useState<string>("");
  const [localEntries, setLocalEntries] = useState<LocalEntry[]>([]);
  const [localError, setLocalError] = useState<string | null>(null);

  const [envHints, setEnvHints] = useState<string[]>([]);

  // Keep the latest jobs accessible in event handler without re-subscribing.
  const jobsRef = useRef(jobs);
  jobsRef.current = jobs;
  const activeStorageRef = useRef(activeStorage);
  activeStorageRef.current = activeStorage;
  const breadcrumbRef = useRef(breadcrumb);
  breadcrumbRef.current = breadcrumb;
  const localPathRef = useRef(localPath);
  localPathRef.current = localPath;

  // Stable ref to the latest uploadFiles closure so the once-only effect
  // can read it without resubscribing.
  const uploadFilesRef = useRef<(paths: string[]) => void>(() => {});
  const [internalDrag, setInternalDrag] = useState<InternalDrag | null>(null);
  const internalDragRef = useRef<InternalDrag | null>(null);
  internalDragRef.current = internalDrag;
  const [leftWidth, setLeftWidth] = useState(50);
  const [queueHeight, setQueueHeight] = useState(190);
  const mainRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const localPaneRef = useRef<HTMLDivElement>(null);
  const devicePaneRef = useRef<HTMLDivElement>(null);

  const startHorizontalResize = useCallback((event: ReactPointerEvent<HTMLDivElement>) => {
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    const startX = event.clientX;
    const startWidth = leftWidth;

    const onPointerMove = (moveEvent: PointerEvent) => {
      if (!containerRef.current) return;
      const containerWidth = containerRef.current.clientWidth;
      const deltaX = moveEvent.clientX - startX;
      const deltaPct = (deltaX / containerWidth) * 100;
      setLeftWidth(clamp(startWidth + deltaPct, 20, 80));
    };

    const onPointerUp = () => {
      document.removeEventListener("pointermove", onPointerMove);
      document.removeEventListener("pointerup", onPointerUp);
    };

    document.addEventListener("pointermove", onPointerMove);
    document.addEventListener("pointerup", onPointerUp);
  }, [leftWidth]);

  const startVerticalResize = useCallback((event: ReactPointerEvent<HTMLDivElement>) => {
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    const startY = event.clientY;
    const startHeight = queueHeight;

    const onPointerMove = (moveEvent: PointerEvent) => {
      if (!mainRef.current) return;
      const mainHeight = mainRef.current.clientHeight;
      const deltaY = startY - moveEvent.clientY;
      const maxQueueHeight = Math.max(150, mainHeight - 240);
      setQueueHeight(clamp(startHeight + deltaY, 120, maxQueueHeight));
    };

    const onPointerUp = () => {
      document.removeEventListener("pointermove", onPointerMove);
      document.removeEventListener("pointerup", onPointerUp);
    };

    document.addEventListener("pointermove", onPointerMove);
    document.addEventListener("pointerup", onPointerUp);
  }, [queueHeight]);

  const refresh = useCallback(async (force = false) => {
    setLoading(true);
    setBrowserError(null);
    try {
      const snap = await invoke<DeviceSnapshot>("device_snapshot", { force });
      setSnapshot(snap);

      if (snap.devices.length > 0) {
        const isPaused = await invoke<boolean>("get_queue_state");
        if (isPaused) {
          const shouldResume = await ask(
            t("app.resume.prompt"),
            { title: t("app.resume.title"), kind: "info" }
          );
          if (shouldResume) {
            await invoke("resume_queue");
          } else {
            await invoke("clear_queue");
            setJobs(new Map());
            setJobGroups(new Map());
          }
        }
      }

      const firstStorage = snap.storages[0] ?? null;
      setActiveStorage(firstStorage);
      if (firstStorage) {
        setBreadcrumb([{ id: PARENT_ROOT, name: firstStorage.description ?? "Root" }]);
        await loadEntries(firstStorage.id, PARENT_ROOT);
      } else {
        setBreadcrumb([]);
        setEntries([]);
      }
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setSnapshot({ devices: [], storages: [], error: message, permissionHint: true });
      setEntries([]);
      setBreadcrumb([]);
    } finally {
      setLoading(false);
    }
  }, []);

  const loadLocalEntries = useCallback(async (path: string) => {
    try {
      const list = await invoke<LocalEntry[]>("list_local_entries", { path });
      setLocalEntries(list);
      setLocalPath(path);
      setLocalError(null);
    } catch (err) {
      setLocalEntries([]);
      setLocalError(err instanceof Error ? err.message : String(err));
    }
  }, []);

  const enterLocalFolder = useCallback((entry: LocalEntry) => {
    if (!entry.isDir) return;
    loadLocalEntries(entry.path);
  }, [loadLocalEntries]);

  const goToLocalCrumb = useCallback((path: string) => {
    if (path) loadLocalEntries(path);
  }, [loadLocalEntries]);

  const loadEntries = useCallback(async (storageId: number, parentId: number) => {
    try {
      const list = await invoke<Entry[]>("list_entries", { storageId, parentId });
      const sorted = [...list].sort((a, b) => {
        if (a.kind !== b.kind) return a.kind === "folder" ? -1 : 1;
        return a.name.localeCompare(b.name);
      });
      setEntries(sorted);
      setBrowserError(null);
    } catch (err) {
      setEntries([]);
      setBrowserError(err instanceof Error ? err.message : String(err));
    }
  }, []);

  const refreshAfterTransfer = useCallback(
    (job: JobView | undefined) => {
      if (!job) return;
      window.setTimeout(() => {
        if (job.kind.kind === "upload") {
          const storage = activeStorageRef.current;
          const crumb = breadcrumbRef.current;
          const parent = crumb[crumb.length - 1]?.id ?? PARENT_ROOT;
          if (storage) loadEntries(storage.id, parent);
        } else {
          const path = localPathRef.current;
          if (path) loadLocalEntries(path);
        }
      }, 250);
    },
    [loadEntries, loadLocalEntries],
  );

  // Initial mount: subscribe to transfer events + native drag-drop.
  useEffect(() => {
    const unlistenTransfer = listen<TransferEvent>("transfer-event", (e) => {
      if (e.payload.type === "queuePaused") {
        setBrowserError(`${t("app.transfer.paused")}${e.payload.reason}`);
        return;
      }

      if (
        e.payload.type === "stateChanged" &&
        ["completed", "failed", "cancelled", "skipped"].includes(e.payload.state.tag)
      ) {
        refreshAfterTransfer(jobsRef.current.get(e.payload.id));
      }
      setJobs((prev) => applyEvent(prev, e.payload));
    });
    // Tauri 2 emits this when files are dropped onto the window.
    const unlistenDrop = listen<{ paths: string[]; position?: { x: number; y: number } }>("tauri://drag-drop", (e) => {
      if (!e.payload?.paths?.length) return;
      if (!e.payload.position || pointInElement(e.payload.position, devicePaneRef.current)) {
        uploadFilesRef.current(e.payload.paths);
      }
    });

    // Initialize Local Path
    invoke<string>("default_dest_dir")
      .then((dir) => {
        setLocalPath(dir);
        loadLocalEntries(dir);
      })
      .catch(() => {});

    invoke<{ libmtpLoaded: boolean; hints: string[] }>("environment_check")
      .then((r) => setEnvHints(r.hints))
      .catch(() => {});
    refresh();

    const unlistenLang = listen<string>("language-changed", (e) => {
      import("i18next").then((i18next) => i18next.default.changeLanguage(e.payload));
    });

    return () => {
      unlistenTransfer.then((fn) => fn());
      unlistenDrop.then((fn) => fn());
      unlistenLang.then((fn) => fn());
    };
  }, [loadLocalEntries, refresh, refreshAfterTransfer]);

  const enterFolder = useCallback(
    async (entry: Entry) => {
      if (entry.kind !== "folder" || !activeStorage) return;
      setBreadcrumb((b) => [...b, { id: entry.itemId, name: entry.name }]);
      await loadEntries(activeStorage.id, entry.itemId);
    },
    [activeStorage, loadEntries],
  );

  const goToCrumb = useCallback(
    async (idx: number) => {
      if (!activeStorage) return;
      const next = breadcrumb.slice(0, idx + 1);
      setBreadcrumb(next);
      const target = next[next.length - 1];
      await loadEntries(activeStorage.id, target.id);
    },
    [activeStorage, breadcrumb, loadEntries],
  );

  const downloadLocalFile = useCallback(
    async (entryData: string) => {
      if (!localPath || !activeStorage) {
        setBrowserError(t("app.error.local_missing"));
        return;
      }
      try {
        const entry: Entry = JSON.parse(entryData);

        const ids = await invoke<number[]>("enqueue_download", {
          storageId: activeStorage.id,
          fileId: entry.itemId,
          name: entry.name,
          isDir: entry.kind === "folder",
          destDir: localPath,
          expectedSize: entry.size,
          modifiedSecs: entry.modifiedSecs ?? null,
          conflict: conflictPolicy,
        });
        rememberGroup(ids, entry.name, entry.kind === "folder", setJobGroups);
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        alert(`${t("app.error.download")}${msg}`);
        setBrowserError(msg);
      }
      setTimeout(() => {
        loadLocalEntries(localPath);
      }, 1500);
    },
    [activeStorage, localPath, conflictPolicy, loadLocalEntries],
  );

  const downloadEntry = useCallback((entry: Entry) => {
    downloadLocalFile(JSON.stringify(entry));
  }, [downloadLocalFile]);

  // Internal drag from LocalBrowser → Browser. Funnel through the same
  // `uploadFiles` path so ADB auto-routing applies here too. (Without
  // this the user would only get fast path on Finder drag-drop.)
  const uploadLocalEntry = useCallback((entry: LocalEntry) => {
    uploadFilesRef.current([entry.path]);
  }, []);

  const startLocalDrag = useCallback((entry: LocalEntry, point: { x: number; y: number }) => {
    setInternalDrag({ type: "local", item: entry, label: entry.name, ...point });
  }, []);

  const startMtpDrag = useCallback((entry: Entry, point: { x: number; y: number }) => {
    setInternalDrag({ type: "mtp", item: entry, label: entry.name, ...point });
  }, []);

  useEffect(() => {
    if (!internalDrag) return;

    const onPointerMove = (event: PointerEvent) => {
      setInternalDrag((current) => current && { ...current, x: event.clientX, y: event.clientY });
    };

    const onPointerUp = (event: PointerEvent) => {
      const current = internalDragRef.current;
      setInternalDrag(null);
      if (!current) return;

      const point = { x: event.clientX, y: event.clientY };
      if (current.type === "local" && pointInElement(point, devicePaneRef.current)) {
        uploadLocalEntry(current.item);
      }
      if (current.type === "mtp" && pointInElement(point, localPaneRef.current)) {
        downloadEntry(current.item);
      }
    };

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setInternalDrag(null);
    };

    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", onPointerUp, { once: true });
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerup", onPointerUp);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [internalDrag, downloadEntry, uploadLocalEntry]);

  const uploadFiles = useCallback(
    async (paths: string[]) => {
      if (!activeStorage) {
        setBrowserError(t("app.error.no_storage"));
        return;
      }
      const parent = breadcrumb[breadcrumb.length - 1]?.id ?? PARENT_ROOT;
      const failures: string[] = [];

      // Phase A.0 — auto-routing: directories on internal storage may
      // qualify for the ADB tar fast path. Files always stay on MTP.
      const adbEligible: string[] = [];
      const mtpForced: string[] = [];
      for (const p of paths) {
        try {
          const st = await invoke<{ isDir: boolean; exists: boolean }>(
            "local_stat",
            { path: p },
          );
          if (st.exists && st.isDir) adbEligible.push(p);
          else mtpForced.push(p);
        } catch {
          mtpForced.push(p);
        }
      }

      const adbHandled = new Set<string>();
      if (adbEligible.length > 0) {
        try {
          const route = await routeFoldersViaAdb(
            adbEligible,
            activeStorage,
            breadcrumb,
            setJobGroups,
            t,
          );
          for (const p of route.handled) adbHandled.add(p);
          for (const msg of route.errors) failures.push(msg);
        } catch (e) {
          // Never let an ADB-routing throw kill the React tree —
          // surface it on the banner and fall back to MTP for everything.
          failures.push(
            `${t("app.error.adb_routing")}${e instanceof Error ? e.message : String(e)}`,
          );
        }
      }

      const mtpPaths = [
        ...mtpForced,
        ...adbEligible.filter((p) => !adbHandled.has(p)),
      ];
      for (const path of mtpPaths) {
        try {
          const ids = await invoke<number[]>("enqueue_upload", {
            storageId: activeStorage.id,
            parentId: parent,
            source: path,
            conflict: conflictPolicy,
          });
          rememberGroup(ids, basename(path), ids.length > 1, setJobGroups);
        } catch (err) {
          failures.push(err instanceof Error ? err.message : String(err));
        }
      }
      if (failures.length > 0) {
        setBrowserError(failures.join("\n"));
      } else {
        setBrowserError(null);
      }
      setTimeout(() => {
        if (activeStorage) loadEntries(activeStorage.id, parent);
      }, 1500);
    },
    [activeStorage, breadcrumb, conflictPolicy, loadEntries],
  );

  uploadFilesRef.current = uploadFiles;

  const cancelJob = useCallback((id: number) => {
    invoke("cancel_job", { jobId: id }).catch(() => {});
  }, []);

  const sortedJobs = useMemo(() => {
    return [...jobs.values()].sort((a, b) => b.startedAt - a.startedAt);
  }, [jobs]);

  return (
    <>
      <Banner
        snapshot={snapshot}
        loading={loading}
        onRefresh={() => refresh(true)}
        conflictPolicy={conflictPolicy}
        onConflictChange={setConflictPolicy}
        envHints={envHints}
      />
      <AdbPanel />
      <div
        className="main"
        ref={mainRef}
        style={{ gridTemplateRows: `minmax(220px, 1fr) 8px minmax(120px, ${queueHeight}px)` }}
      >
        <div
          className="panes"
          ref={containerRef}
          style={{ gridTemplateColumns: `minmax(260px, ${leftWidth}%) 8px minmax(260px, 1fr)` }}
        >
          <div className="pane" ref={localPaneRef}>
            <LocalBrowser
              currentPath={localPath}
              entries={localEntries}
              error={localError}
              onEnter={enterLocalFolder}
              onCrumb={goToLocalCrumb}
              onDragItem={startLocalDrag}
            />
          </div>
          <div
            aria-label={t("app.aria.horizontal")}
            aria-orientation="vertical"
            className="resizer horizontal"
            onPointerDown={startHorizontalResize}
            role="separator"
          />
          <div className="pane" ref={devicePaneRef}>
            <Browser
              breadcrumb={breadcrumb}
              entries={entries}
              error={browserError}
              onEnter={enterFolder}
              onCrumb={goToCrumb}
              onDragItem={startMtpDrag}
            />
          </div>
        </div>
        <div
          aria-label={t("app.aria.vertical")}
          aria-orientation="horizontal"
          className="resizer vertical"
          onPointerDown={startVerticalResize}
          role="separator"
        />
        {internalDrag && (
          <div className="drag-ghost" style={{ left: internalDrag.x + 12, top: internalDrag.y + 12 }}>
            <span>{internalDrag.type === "local" ? "↑" : "↓"}</span>
            {internalDrag.label}
          </div>
        )}
        <QueuePanel jobs={sortedJobs} groups={jobGroups} onCancel={cancelJob} />
      </div>
    </>
  );
}

function rememberGroup(
  ids: number[],
  label: string,
  shouldGroup: boolean,
  setJobGroups: Dispatch<SetStateAction<Map<number, QueueGroupView>>>,
) {
  if (!shouldGroup || ids.length <= 1) return;
  const group: QueueGroupView = {
    id: `${Date.now()}-${ids[0]}`,
    label,
    totalFiles: ids.length,
  };
  setJobGroups((prev) => {
    const next = new Map(prev);
    for (const id of ids) {
      next.set(id, group);
    }
    return next;
  });
}

// Phase A.0 — auto-routing helper. Tries to push directory uploads
// through the ADB tar fast path when:
//   • exactly one ADB device is in `device` state and tar-capable
//   • we can confirm an internal-storage absolute root (/sdcard)
//   • the active MTP storage looks like internal storage (single storage
//     OR description contains internal/shared/phone). SD-card mapping is
//     out of scope (plan.md §2.1).
//   • dest path under /sdcard can be derived from the breadcrumb.
// Anything that fails any check is silently returned to the MTP path
// — this helper never throws on capability gaps.
async function routeFoldersViaAdb(
  folderPaths: string[],
  storage: Storage,
  breadcrumb: BreadcrumbNode[],
  setJobGroups: Dispatch<SetStateAction<Map<number, QueueGroupView>>>,
  t: any,
): Promise<{ handled: string[]; errors: string[] }> {
  const handled: string[] = [];
  const errors: string[] = [];

  const raw = storage.description ?? "";
  const desc = raw.toLowerCase();
  // SD-card descriptions usually contain "sd"/"card"/"카드"/"외장" so we
  // gate them out explicitly to avoid false-positive on dual-storage
  // devices where internal storage doesn't ship a Latin label.
  const looksSdCard =
    desc.includes("sd") ||
    desc.includes("card") ||
    raw.includes("카드") ||
    raw.includes("외장");
  const looksInternal =
    !looksSdCard &&
    (desc.includes("internal") ||
      desc.includes("shared") ||
      desc.includes("phone") ||
      raw.includes("내부") ||
      raw.includes("공유") ||
      raw.includes("저장") ||
      desc === "" ||
      desc === "root");
  if (!looksInternal) return { handled, errors };

  let status: AdbStatusWire;
  try {
    status = await invoke<AdbStatusWire>("adb_status");
  } catch {
    return { handled, errors };
  }
  if (!status.adbAvailable) return { handled, errors };
  const ready = status.devices.filter(
    (d) => d.state === "device" && d.canTarUpload && d.tarExtractSmokeOk,
  );
  if (ready.length !== 1) return { handled, errors };
  const dev = ready[0];

  let root: string | null = null;
  try {
    root = await invoke<string | null>("adb_internal_storage_root", {
      serial: dev.serial,
    });
  } catch {
    return { handled, errors };
  }
  if (!root) return { handled, errors };

  // breadcrumb[0] is the storage root label, not a real folder name on
  // /sdcard. Skip it; everything after is a real folder name.
  const rel = breadcrumb.slice(1).map((b) => b.name).join("/");
  const destBase = rel ? `${root}/${rel}` : root;

  for (const folder of folderPaths) {
    const folderName = basename(folder);
    const dest = `${destBase}/${folderName}`;
    let report: AdbPlanReport;
    try {
      report = await invoke<AdbPlanReport>("adb_plan_upload", {
        serial: dev.serial,
        source: folder,
        destPath: dest,
      });
    } catch (e) {
      // Plan failure → don't claim this folder; let MTP handle it.
      errors.push(
        t("adb.prep_failed", { folderName, error: e instanceof Error ? e.message : String(e) }),
      );
      continue;
    }

    const totalConflicts = report.skippedSame.length + report.renamed.length;
    if (totalConflicts > 0) {
      const msg = t("adb.confirm.msg", { folderName, clean: report.clean.length, skipped: report.skippedSame.length, renamed: report.renamed.length });
      const proceed = await ask(msg, { title: t("adb.confirm.title"), kind: "info" });
      if (!proceed) continue; // → falls through to MTP
    }

    try {
      const id = await invoke<number>("enqueue_adb_tar_upload", {
        planToken: report.planToken,
      });
      rememberGroup([id], `${folderName} (ADB)`, true, setJobGroups);
      handled.push(folder);
    } catch (e) {
      errors.push(
        t("adb.start_failed", { folderName, error: e instanceof Error ? e.message : String(e) }),
      );
    }
  }
  return { handled, errors };
}

function basename(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

function pointInElement(point: { x: number; y: number }, element: HTMLElement | null): boolean {
  if (!element) return false;
  const rect = element.getBoundingClientRect();
  return point.x >= rect.left && point.x <= rect.right && point.y >= rect.top && point.y <= rect.bottom;
}

function applyEvent(prev: Map<number, JobView>, ev: TransferEvent): Map<number, JobView> {
  const next = new Map(prev);
  switch (ev.type) {
    case "enqueued": {
      next.set(ev.id, {
        id: ev.id,
        kind: ev.kind,
        state: { tag: "queued" },
        sent: 0,
        total: 0,
        startedAt: Date.now(),
      });
      break;
    }
    case "stateChanged": {
      const job = next.get(ev.id);
      if (job) {
        next.set(ev.id, { ...job, state: ev.state });
      }
      break;
    }
    case "progress": {
      const job = next.get(ev.id);
      if (job) {
        next.set(ev.id, { ...job, sent: ev.sent, total: ev.total });
      }
      break;
    }
    case "bulkProgress": {
      const job = next.get(ev.id);
      if (job) {
        next.set(ev.id, {
          ...job,
          currentFile: ev.currentFile,
          filesDone: ev.filesDone,
          totalFiles: ev.totalFiles,
        });
      }
      break;
    }
    case "queuePaused":
    case "workerStopped":
      break;
  }
  return next;
}
