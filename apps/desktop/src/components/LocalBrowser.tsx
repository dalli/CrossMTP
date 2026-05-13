import { PointerEvent } from "react";
import { LocalEntry } from "../types";
import { formatBytes } from "./Banner";

interface Props {
  currentPath: string;
  entries: LocalEntry[];
  error: string | null;
  onEnter: (e: LocalEntry) => void;
  onUp: () => void;
  onDragItem: (entry: LocalEntry, point: { x: number; y: number }) => void;
}

export function LocalBrowser({
  currentPath,
  entries,
  error,
  onEnter,
  onUp,
  onDragItem,
}: Props) {
  const handlePointerDown = (e: PointerEvent<HTMLDivElement>, entry: LocalEntry) => {
    if (e.button !== 0) return;
    onDragItem(entry, { x: e.clientX, y: e.clientY });
  };

  return (
    <div className="browser">
      <div className="toolbar" style={{ display: "flex", gap: "8px", alignItems: "center" }}>
        <button onClick={onUp} disabled={!currentPath || currentPath === "/"} style={{ padding: "4px 8px" }}>
          ↑ 위로
        </button>
        <span className="crumb" style={{ flex: 1, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
          {currentPath || "로컬 디스크"}
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
            key={e.path}
            onPointerDown={(evt) => handlePointerDown(evt, e)}
            onDoubleClick={() => {
              if (e.isDir) onEnter(e);
            }}
          >
            <span className="icon">{e.isDir ? "📁" : "📄"}</span>
            <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }} title={e.name}>
              {e.name}
            </span>
            <span className="size" style={{ marginLeft: "auto" }}>
              {e.isDir ? "" : formatBytes(e.size)}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}
