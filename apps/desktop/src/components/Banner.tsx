import { useMemo, useState } from "react";
import { ConflictPolicy, DeviceSnapshot } from "../types";

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
  const [showNotifications, setShowNotifications] = useState(true);
  const connected = !!(snapshot && snapshot.devices.length > 0 && snapshot.storages.length > 0);
  const device = snapshot?.devices[0];
  const storage = snapshot?.storages[0];
  const notification = useMemo(() => {
    const title = connected
      ? `${device?.manufacturer ?? ""} ${device?.model ?? ""}`.trim() || "기기 연결됨"
      : "기기 없음";
    const lines = [
      connected && storage
        ? `${storage.description ?? ""} · ${formatBytes(storage.freeBytes)} 여유 / ${formatBytes(storage.maxBytes)}`
        : snapshot?.error
          ? snapshot.error
          : "Android 폰을 USB로 연결한 뒤 폰에서 'MTP / 파일 전송'을 선택하세요.",
    ];

    if (snapshot?.permissionHint && !connected) {
      lines.push(
        "힌트: macOS의 Image Capture / Android File Transfer가 USB를 잡고 있으면 인식되지 않습니다. 해당 앱을 종료하고 새로고침해주세요.",
      );
    }
    lines.push(...envHints.map((hint) => `⚠ ${hint}`));

    return { title, lines };
  }, [connected, device?.manufacturer, device?.model, envHints, snapshot?.error, snapshot?.permissionHint, storage]);

  return (
    <>
      <div className="app-menu">
        <button
          aria-expanded={showNotifications}
          className={showNotifications ? "active" : ""}
          onClick={() => setShowNotifications((visible) => !visible)}
        >
          알림
        </button>
        <label style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 12, color: "var(--text-dim)" }}>
          충돌 시
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
            <option value="rename">이름 변경</option>
            <option value="skip">건너뛰기</option>
            <option value="overwrite">덮어쓰기 (다운로드만)</option>
          </select>
        </label>
        <button className="primary" onClick={onRefresh} disabled={loading}>
          {loading ? "..." : "새로고침"}
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
            aria-label="알림 닫기"
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
