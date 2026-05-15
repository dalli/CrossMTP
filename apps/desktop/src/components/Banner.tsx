import { useMemo, useState } from "react";
import { ConflictPolicy, DeviceSnapshot } from "../types";

import { useTranslation } from "react-i18next";

interface Props {
  snapshot: DeviceSnapshot | null;
  loading: boolean;
  conflictPolicy: ConflictPolicy;
  envHints: string[];
  onRefresh: () => void;
  onConflictChange: (p: ConflictPolicy) => void;
}

export function Banner({
  snapshot,
  loading,
  conflictPolicy,
  envHints,
  onRefresh,
  onConflictChange,
}: Props) {
  const { t } = useTranslation();
  const [showNotifications, setShowNotifications] = useState(true);
  const connected = !!(snapshot && snapshot.devices.length > 0 && snapshot.storages.length > 0);
  const device = snapshot?.devices[0];
  const storage = snapshot?.storages[0];
  const notification = useMemo(() => {
    const title = connected
      ? `${device?.manufacturer ?? ""} ${device?.model ?? ""}`.trim() || t("device.connected")
      : t("device.none");
    const lines = [
      connected && storage
        ? `${storage.description ?? ""} · ${t("storage.free", { free: formatBytes(storage.freeBytes), total: formatBytes(storage.maxBytes) })}`
        : snapshot?.error
          ? snapshot.error
          : t("hint.connect"),
    ];

    if (snapshot?.permissionHint && !connected) {
      lines.push(t("hint.macos"));
    }
    lines.push(...envHints.map((hint) => `⚠ ${hint}`));

    return { title, lines };
  }, [connected, device?.manufacturer, device?.model, envHints, snapshot?.error, snapshot?.permissionHint, storage, t]);

  return (
    <>
      <div className="app-menu">
        <button
          aria-expanded={showNotifications}
          className={showNotifications ? "active" : ""}
          onClick={() => setShowNotifications((visible) => !visible)}
        >
          {t("notice")}
        </button>
        <label style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 12, color: "var(--text-dim)" }}>
          {t("conflict.policy")}
          <select
            value={conflictPolicy}
            onChange={(e) => onConflictChange(e.target.value as ConflictPolicy)}
            style={{
              background: "var(--bg-elevated)",
              color: "var(--text)",
              border: "1px solid var(--border)",
              borderRadius: 6,
              padding: "3px 6px",
            }}
          >
            <option value="rename">{t("conflict.rename")}</option>
            <option value="skip">{t("conflict.skip")}</option>
            <option value="overwrite">{t("conflict.overwrite")}</option>
          </select>
        </label>
        <button className="primary" onClick={onRefresh} disabled={loading}>
          {loading ? "..." : t("refresh")}
        </button>
      </div>
      {showNotifications && (
        <div className={`banner ${connected ? "connected" : "disconnected"}`} role="status">
          <span className="dot" />
          <div className="notification-copy">
            <div>{notification.title}</div>
            {notification.lines.map((line, i) => (
              <div key={`${line}-${i}`} className="meta">
                {line}
              </div>
            ))}
          </div>
          <button
            aria-label={t("close.notice")}
            className="icon-button"
            onClick={() => setShowNotifications(false)}
          >
            x
          </button>
        </div>
      )}
    </>
  );
}

export function formatBytes(b: number): string {
  if (b < 1024) return `${b} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = b / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(1)} ${units[i]}`;
}
