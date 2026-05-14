// ADB 고속 업로드 capability strip — Phase A.0 단순화 버전.
//
// 폴더 / 경로 / 전송 흐름은 MTP UI의 drag-drop 자동 라우팅
// (App.tsx `routeFoldersViaAdb`)이 모두 처리하므로, 이 패널의 역할은
// 다음 세 가지만 남깁니다:
//   1. 사용 on/off 토글 (capability strip 자체를 펼치기/접기)
//   2. 연결된 ADB device 목록과 상태 표시
//   3. 사용 불가 사유 안내 (USB debugging 미승인 / tar 없음 등)
//
// plan.md §2.1 "fallback인지 추천 선택값인지 구분"의 *정보 표시*만
// 담당하고, 실제 업로드 트리거는 더 이상 여기서 하지 않습니다.

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { AdbStatusWire } from "../types";

export function AdbPanel() {
  const [open, setOpen] = useState(false);
  const [status, setStatus] = useState<AdbStatusWire | null>(null);
  const [loading, setLoading] = useState(false);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const s = await invoke<AdbStatusWire>("adb_status");
      setStatus(s);
    } catch (e) {
      setStatus({
        adbAvailable: false,
        adbPath: null,
        adbSource: null,
        error: e instanceof Error ? e.message : String(e),
        devices: [],
      });
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (open && !status) refresh();
  }, [open, status, refresh]);

  if (!open) {
    return (
      <div className="adb-strip collapsed">
        <span className="adb-label">ADB 고속 업로드</span>
        <span className="adb-meta">
          USB debugging이 켜진 Android 기기에서 폴더 업로드가 자동으로 빨라집니다.
        </span>
        <button onClick={() => setOpen(true)} className="ghost">
          상태 보기
        </button>
      </div>
    );
  }

  const ready = status?.devices.filter(
    (d) => d.state === "device" && d.canTarUpload && d.tarExtractSmokeOk,
  ) ?? [];
  const banner = computeBanner(status, ready.length);

  return (
    <div className="adb-strip">
      <div className="adb-row">
        <span className="adb-label">ADB 고속 업로드</span>
        <button onClick={refresh} disabled={loading}>
          {loading ? "검사 중..." : "다시 검사"}
        </button>
        <button onClick={() => setOpen(false)} className="ghost">
          접기
        </button>
      </div>

      {banner && <div className={`adb-banner ${banner.kind}`}>{banner.message}</div>}

      {status?.adbAvailable && status.devices.length > 0 && (
        <ul className="adb-list">
          {status.devices.map((d) => (
            <li key={d.serial}>
              <b>{d.model ?? d.serial}</b>{" "}
              <span className="adb-meta">
                {d.serial} · {d.state}
                {d.state === "device" &&
                  (d.canTarUpload && d.tarExtractSmokeOk
                    ? " · 고속 가능"
                    : ` · 고속 불가 (tar=${d.hasTar ? "ok" : "없음"}, smoke=${d.tarExtractSmokeOk ? "ok" : "실패"})`)}
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function computeBanner(
  status: AdbStatusWire | null,
  readyCount: number,
): { kind: "ok" | "warn" | "error"; message: string } | null {
  if (!status) return null;
  if (!status.adbAvailable) {
    return {
      kind: "error",
      message:
        "adb 바이너리를 찾지 못했습니다. Android platform-tools를 설치하거나 CROSSMTP_ADB 환경변수를 설정해주세요." +
        (status.error ? ` (${status.error})` : ""),
    };
  }
  if (status.devices.length === 0) {
    return {
      kind: "warn",
      message:
        "ADB로 인식된 기기가 없습니다. USB debugging이 켜져 있고 'Allow USB debugging' 프롬프트를 수락했는지 확인해주세요.",
    };
  }
  if (readyCount === 0) {
    return {
      kind: "warn",
      message:
        "사용 가능한 기기가 없습니다 — MTP 경로로 업로드됩니다. 위 목록에서 사유를 확인하세요.",
    };
  }
  if (readyCount === 1) {
    return {
      kind: "ok",
      message: "ADB 고속 업로드 사용 가능 — 폴더를 끌어다 놓으면 자동으로 적용됩니다.",
    };
  }
  return {
    kind: "warn",
    message:
      "고속 가능한 기기가 2개 이상이라 자동 라우팅을 비활성화했습니다 (한 대만 연결하면 자동 적용).",
  };
}
