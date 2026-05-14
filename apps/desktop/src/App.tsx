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
            "이전에 중단된 전송이 있습니다. 이어서 전송하시겠습니까?\n\n[예: 이어서 전송] [아니오: 대기열 지우고 새로 시작]",
            { title: "이어받기", kind: "info" }
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
        setBrowserError(`전송이 중단되었습니다: ${e.payload.reason}`);
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
    return () => {
      unlistenTransfer.then((fn) => fn());
      unlistenDrop.then((fn) => fn());
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
        setBrowserError("로컬 대상 폴더가 없거나 기기가 연결되지 않았습니다.");
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
        alert("다운로드 오류: " + msg);
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

  const uploadMTPFile = useCallback(async (entryData: string) => {
    if (!activeStorage) return;
    const parent = breadcrumb[breadcrumb.length - 1]?.id ?? PARENT_ROOT;
    try {
      const entry: LocalEntry = JSON.parse(entryData);

      const ids = await invoke<number[]>("enqueue_upload", {
        storageId: activeStorage.id,
        parentId: parent,
        source: entry.path,
        conflict: conflictPolicy,
      });
      rememberGroup(ids, entry.name, entry.isDir, setJobGroups);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      alert("업로드 오류: " + msg);
      setBrowserError(msg);
    }
    setTimeout(() => {
      if (activeStorage) loadEntries(activeStorage.id, parent);
    }, 1500);
  }, [activeStorage, breadcrumb, conflictPolicy, loadEntries]);

  const uploadLocalEntry = useCallback((entry: LocalEntry) => {
    uploadMTPFile(JSON.stringify(entry));
  }, [uploadMTPFile]);

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
        setBrowserError("업로드할 기기 저장소가 연결되지 않았습니다.");
        return;
      }
      const parent = breadcrumb[breadcrumb.length - 1]?.id ?? PARENT_ROOT;
      const failures: string[] = [];
      for (const path of paths) {
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
            aria-label="PC와 Android 패널 너비 조절"
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
          aria-label="파일 패널과 전송 큐 높이 조절"
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
