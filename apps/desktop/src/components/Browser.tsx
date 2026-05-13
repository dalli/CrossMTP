import { PointerEvent, useMemo, useState } from "react";
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

type SortKey = "name" | "type" | "date" | "size";
type SortOrder = "asc" | "desc";

const formatDate = (secs?: number | null) => {
  if (!secs) return "-";
  const d = new Date(secs * 1000);
  return d.toLocaleString("ko-KR", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  }).replace(/\. /g, "-").replace(/\./g, "");
};

const getFileType = (name: string, kind: "file" | "folder") => {
  if (kind === "folder") return "폴더";
  const ext = name.split(".").pop();
  return ext && ext !== name ? ext.toUpperCase() : "파일";
};

export function Browser({
  storage,
  breadcrumb,
  entries,
  error,
  onEnter,
  onCrumb,
  onDragItem,
}: Props) {
  const [sortKey, setSortKey] = useState<SortKey>("name");
  const [sortOrder, setSortOrder] = useState<SortOrder>("asc");

  const handlePointerDown = (e: PointerEvent<HTMLDivElement>, entry: Entry) => {
    if (e.button !== 0) return;
    onDragItem(entry, { x: e.clientX, y: e.clientY });
  };

  const toggleSort = (key: SortKey) => {
    if (sortKey === key) {
      setSortOrder(sortOrder === "asc" ? "desc" : "asc");
    } else {
      setSortKey(key);
      setSortOrder("asc");
    }
  };

  const sortedEntries = useMemo(() => {
    return [...entries].sort((a, b) => {
      // Keep folders at the top
      if (a.kind === "folder" && b.kind !== "folder") return -1;
      if (a.kind !== "folder" && b.kind === "folder") return 1;

      let cmp = 0;
      switch (sortKey) {
        case "name":
          cmp = a.name.localeCompare(b.name);
          break;
        case "type":
          cmp = getFileType(a.name, a.kind).localeCompare(getFileType(b.name, b.kind));
          break;
        case "date":
          cmp = (a.modifiedSecs ?? 0) - (b.modifiedSecs ?? 0);
          break;
        case "size":
          cmp = a.size - b.size;
          break;
      }
      return sortOrder === "asc" ? cmp : -cmp;
    });
  }, [entries, sortKey, sortOrder]);

  const renderHeader = (key: SortKey, label: string) => (
    <div 
      className={`col col-${key} ${sortKey === key ? "active" : ""}`} 
      onClick={() => toggleSort(key)}
    >
      {label}
      {sortKey === key && (sortOrder === "asc" ? " ▴" : " ▾")}
    </div>
  );

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

      <div className="table-header">
        {renderHeader("name", "파일명")}
        {renderHeader("type", "종류")}
        {renderHeader("date", "날짜")}
        {renderHeader("size", "크기")}
      </div>

      <div className="entries" style={{ flex: 1, overflowY: "auto", position: "relative" }}>
        {sortedEntries.length === 0 && !error && (
          <div style={{ padding: 20, color: "var(--text-dim)", fontSize: 12, textAlign: "center" }}>
            (비어있음)
          </div>
        )}
        {sortedEntries.map((e) => (
          <div
            className="entry"
            key={`${e.storageId}-${e.itemId}`}
            onPointerDown={(evt) => handlePointerDown(evt, e)}
            onDoubleClick={() => {
              if (e.kind === "folder") onEnter(e);
            }}
          >
            <div className="col-name">
              <span className="icon">{e.kind === "folder" ? "📁" : "📄"}</span>
              <span title={e.name}>{e.name}</span>
            </div>
            <div className="col-type">{getFileType(e.name, e.kind)}</div>
            <div className="col-date">{formatDate(e.modifiedSecs)}</div>
            <div className="col-size">{e.kind === "folder" ? "-" : formatBytes(e.size)}</div>
          </div>
        ))}
      </div>
    </div>
  );
}
