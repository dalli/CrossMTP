// Wire types — must match the serde-renamed Rust structs in src-tauri/src/lib.rs.

export interface DeviceInfo {
  index: number;
  friendlyName: string | null;
  manufacturer: string | null;
  model: string | null;
  serial: string | null;
}

export interface Storage {
  id: number;
  description: string | null;
  freeBytes: number;
  maxBytes: number;
}

export type EntryKind = "file" | "folder";

export interface Entry {
  itemId: number;
  parentId: number;
  storageId: number;
  name: string;
  size: number;
  modifiedSecs?: number | null;
  kind: EntryKind;
}

export interface LocalEntry {
  path: string;
  name: string;
  size: number;
  isDir: boolean;
  modified: number;
}

export type JobStateTag =
  | "queued"
  | "validating"
  | "transferring"
  | "cancelling"
  | "completed"
  | "failed"
  | "cancelled"
  | "skipped";

export interface JobStateView {
  tag: JobStateTag;
  bytes?: number;
  itemId?: number | null;
  reason?: string;
}

export type ConflictPolicy = "skip" | "rename" | "overwrite";

export type JobKindView =
  | { kind: "download"; storageId: number; fileId: number; name: string; destDir: string; expectedSize: number }
  | { kind: "upload"; storageId: number; parentId: number; source: string; name: string };

export interface JobView {
  id: number;
  kind: JobKindView;
  state: JobStateView;
  sent: number;
  total: number;
  startedAt: number;
}

export interface QueueGroupView {
  id: string;
  label: string;
  totalFiles: number;
}

export type TransferEvent =
  | { type: "enqueued"; id: number; kind: JobKindView }
  | { type: "stateChanged"; id: number; state: JobStateView }
  | { type: "progress"; id: number; sent: number; total: number }
  | { type: "queuePaused"; reason: string }
  | { type: "workerStopped" };

export interface DeviceSnapshot {
  devices: DeviceInfo[];
  /** Storage list of the *first* device, populated when present. */
  storages: Storage[];
  /** Last error encountered while talking to the device, if any. */
  error: string | null;
  /** Heuristic: orchestrator-friendly hint for the user. */
  permissionHint: boolean;
}
