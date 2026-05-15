import { PointerEvent, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { LocalEntry } from "../types";
import { formatBytes } from "./Banner";

interface Props {
  currentPath: string;
  entries: LocalEntry[];
  error: string | null;
  onEnter: (e: LocalEntry) => void;
  onCrumb: (path: string) => void;
  onDragItem: (entry: LocalEntry, point: { x: number; y: number }) => void;
}

type SortKey = "name" | "type" | "date" | "size";
type SortOrder = "asc" | "desc";

const formatDate = (secs: number) => {
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

const getFileType = (name: string, isDir: boolean, t: any) => {
  if (isDir) return t("browser.folder");
  const ext = name.split(".").pop();
  return ext && ext !== name ? ext.toUpperCase() : t("browser.file");
};

export function LocalBrowser({
  currentPath,
  entries,
  error,
  onEnter,
  onCrumb,
  onDragItem,
}: Props) {
  const { t } = useTranslation();
  const [sortKey, setSortKey] = useState<SortKey>("name");
  const [sortOrder, setSortOrder] = useState<SortOrder>("asc");

  const handlePointerDown = (e: PointerEvent<HTMLDivElement>, entry: LocalEntry) => {
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
      if (a.isDir && !b.isDir) return -1;
      if (!a.isDir && b.isDir) return 1;

      let cmp = 0;
      switch (sortKey) {
        case "name":
          cmp = a.name.localeCompare(b.name);
          break;
        case "type":
          cmp = getFileType(a.name, a.isDir, t).localeCompare(getFileType(b.name, b.isDir, t));
          break;
        case "date":
          cmp = a.modified - b.modified;
          break;
        case "size":
          cmp = a.size - b.size;
          break;
      }
      return sortOrder === "asc" ? cmp : -cmp;
    });
  }, [entries, sortKey, sortOrder, t]);

  const breadcrumbs = useMemo(() => buildLocalBreadcrumbs(currentPath, t), [currentPath, t]);

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
      <div className="toolbar pathbar">
        {breadcrumbs.map((crumb, i) => (
          <span key={`${crumb.path}-${i}`} style={{ display: "inline-flex", alignItems: "center" }}>
            {i > 0 && <span className="sep">&nbsp;/&nbsp;</span>}
            <span className="crumb" onClick={() => onCrumb(crumb.path)}>
              {crumb.label}
            </span>
          </span>
        ))}
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
        {renderHeader("name", t("browser.name"))}
        {renderHeader("type", t("browser.type"))}
        {renderHeader("date", t("browser.date"))}
        {renderHeader("size", t("browser.size"))}
      </div>

      <div className="entries" style={{ flex: 1, overflowY: "auto", position: "relative" }}>
        {sortedEntries.length === 0 && !error && (
          <div style={{ padding: 20, color: "var(--text-dim)", fontSize: 12, textAlign: "center" }}>
            {t("browser.empty")}
          </div>
        )}
        {sortedEntries.map((e) => (
          <div
            className="entry"
            key={e.path}
            onPointerDown={(evt) => handlePointerDown(evt, e)}
            onDoubleClick={() => {
              if (e.isDir) onEnter(e);
            }}
          >
            <div className="col-name">
              <span className="icon">{e.isDir ? "📁" : "📄"}</span>
              <span title={e.name}>{e.name}</span>
            </div>
            <div className="col-type">{getFileType(e.name, e.isDir, t)}</div>
            <div className="col-date">{formatDate(e.modified)}</div>
            <div className="col-size">{e.isDir ? "-" : formatBytes(e.size)}</div>
          </div>
        ))}
      </div>
    </div>
  );
}

function buildLocalBreadcrumbs(path: string, t: any): { label: string; path: string }[] {
  if (!path) return [{ label: t("browser.local_disk"), path: "" }];

  const isWindows = path.includes("\\");
  const separator = isWindows ? "\\" : "/";
  const parts = path.split(/[\\/]/).filter(Boolean);

  if (isWindows) {
    const drive = parts[0] ?? path;
    const crumbs = [{ label: drive, path: `${drive}\\` }];
    for (let i = 1; i < parts.length; i += 1) {
      crumbs.push({
        label: parts[i],
        path: `${parts.slice(0, i + 1).join(separator)}\\`,
      });
    }
    return crumbs;
  }

  const crumbs = [{ label: "/", path: "/" }];
  let current = "";
  for (const part of parts) {
    current += `/${part}`;
    crumbs.push({ label: part, path: current });
  }
  return crumbs;
}
