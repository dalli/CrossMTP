import { PointerEvent } from "react";
import { Entry, Storage } from "../types";
import { formatBytes } from "./Banner";

interface BreadcrumbNode {
  id: number;
  name: string;
}

interface Props {
  storage: Storage | null;
  breadcrumb: BreadcrumbNode[];
  entries: Entry[];
  error: string | null;
  onEnter: (e: Entry) => void;
  onCrumb: (idx: number) => void;
  onDragItem: (entry: Entry, point: { x: number; y: number }) => void;
}

export function Browser({
  storage,
  breadcrumb,
  entries,
  error,
  onEnter,
  onCrumb,
  onDragItem,
}: Props) {
  const handlePointerDown = (e: PointerEvent<HTMLDivElement>, entry: Entry) => {
    if (e.button !== 0) return;
    onDragItem(entry, { x: e.clientX, y: e.clientY });
  };

  return (
    <div className="browser">
      <div className="toolbar">
        {breadcrumb.length === 0 && <span className="crumb">기기 없음</span>}
        {breadcrumb.map((c, i) => (
          <span key={`${c.id}-${i}`} style={{ display: "inline-flex", alignItems: "center" }}>
            {i > 0 && <span className="sep">&nbsp;/&nbsp;</span>}
            <span className="crumb" onClick={() => onCrumb(i)}>
              {c.name}
            </span>
          </span>
        ))}
        <span style={{ marginLeft: "auto", color: "var(--text-dim)" }}>
          {storage ? `0x${storage.id.toString(16).padStart(8, "0")}` : ""}
        </span>
      </div>

      {error && (
        <div
          style={{
            padding: "12px 16px",
            color: "var(--err)",
            fontSize: 12,
            borderBottom: "1px solid var(--border)",
            whiteSpace: "pre-wrap",
            wordBreak: "break-word",
          }}
        >
          {error}
        </div>
      )}

      <div className="entries" style={{ flex: 1, overflowY: "auto", position: "relative" }}>
        {entries.length === 0 && !error && (
          <div style={{ padding: 20, color: "var(--text-dim)", fontSize: 12, textAlign: "center" }}>
            (비어있음)
          </div>
        )}
        {entries.map((e) => (
          <div
            className="entry"
            key={`${e.storageId}-${e.itemId}`}
            onPointerDown={(evt) => handlePointerDown(evt, e)}
            onDoubleClick={() => {
              if (e.kind === "folder") onEnter(e);
            }}
          >
            <span className="icon">{e.kind === "folder" ? "📁" : "📄"}</span>
            <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{e.name}</span>
            <span className="size" style={{ marginLeft: "auto" }}>
              {e.kind === "folder" ? "" : formatBytes(e.size)}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}
