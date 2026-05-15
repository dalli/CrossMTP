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
import { useTranslation } from "react-i18next";
import { AdbStatusWire } from "../types";

export function AdbPanel() {
  const { t } = useTranslation();
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
        <span className="adb-label">{t("adb.panel.strip.label")}</span>
        <span className="adb-meta">
          {t("adb.panel.strip.desc")}
        </span>
        <button onClick={() => setOpen(true)} className="ghost">
          {t("adb.panel.btn.show")}
        </button>
      </div>
    );
  }

  const ready = status?.devices.filter(
    (d) => d.state === "device" && d.canTarUpload && d.tarExtractSmokeOk,
  ) ?? [];
  const banner = computeBanner(status, ready.length, t);

  return (
    <div className="adb-strip">
      <div className="adb-row">
        <span className="adb-label">{t("adb.panel.strip.label")}</span>
        <button onClick={refresh} disabled={loading}>
          {loading ? t("adb.panel.btn.refresh_loading") : t("adb.panel.btn.refresh")}
        </button>
        <button onClick={() => setOpen(false)} className="ghost">
          {t("adb.panel.btn.hide")}
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
                    ? t("adb.panel.status.capable")
                    : t("adb.panel.status.incapable", { hasTar: d.hasTar ? t("adb.panel.status.ok") : t("adb.panel.status.none"), smoke: d.tarExtractSmokeOk ? t("adb.panel.status.ok") : t("adb.panel.status.failed") }))}
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
  t: any,
): { kind: "ok" | "warn" | "error"; message: string } | null {
  if (!status) return null;
  if (!status.adbAvailable) {
    return {
      kind: "error",
      message:
        t("adb.banner.no_bin") +
        (status.error ? ` (${status.error})` : ""),
    };
  }
  if (status.devices.length === 0) {
    return {
      kind: "warn",
      message: t("adb.banner.no_device"),
    };
  }
  if (readyCount === 0) {
    return {
      kind: "warn",
      message: t("adb.banner.none_ready"),
    };
  }
  if (readyCount === 1) {
    return {
      kind: "ok",
      message: t("adb.banner.ready"),
    };
  }
  return {
    kind: "warn",
    message: t("adb.banner.too_many"),
  };
}
