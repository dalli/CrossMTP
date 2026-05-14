// ADB 고속 업로드 capability gate + opt-in flow (plan.md §8 Phase 4).
//
// Single React component covering all three Phase 4 UI surfaces:
//   1. Capability strip: shows whether the ADB fast path is usable on
//      the currently selected device, with the *reason* when it isn't
//      (plan.md §2.1 "fallback인지 추천 선택값인지 구분").
//   2. Source/dest entry + "plan" button.
//   3. Conflict manifest modal: surfaces `skippedSame` and `renamed`
//      lists before tar streaming starts (plan.md §5 manifest-driven
//      batch dialog, Phase 3 retro §6-2).
//
// All wire types come from `types.ts`. The component does not talk to
// the MTP queue — ADB jobs land on the same `transfer-event` channel,
// so the rest of the UI (QueuePanel) doesn't need to know whether a
// transferring job is MTP or ADB.

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { AdbPlanReport, AdbStatusWire } from "../types";

interface Props {
  /** Initial dest folder on the device — defaults to a safe path. */
  defaultDestPath?: string;
}

export function AdbPanel({ defaultDestPath = "/sdcard/Download/CrossMTP" }: Props) {
  const [status, setStatus] = useState<AdbStatusWire | null>(null);
  const [loading, setLoading] = useState(false);
  const [optedIn, setOptedIn] = useState(false);
  const [selectedSerial, setSelectedSerial] = useState<string | null>(null);
  const [sourcePath, setSourcePath] = useState<string>("");
  const [destPath, setDestPath] = useState<string>(defaultDestPath);
  const [plan, setPlan] = useState<AdbPlanReport | null>(null);
  const [planError, setPlanError] = useState<string | null>(null);
  const [planning, setPlanning] = useState(false);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [lastEnqueued, setLastEnqueued] = useState<number | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const s = await invoke<AdbStatusWire>("adb_status");
      setStatus(s);
      // Auto-select the first usable device so the form has something
      // to bind to. Users with multiple devices can pick from the dropdown.
      const firstUsable = s.devices.find((d) => d.canTarUpload);
      if (firstUsable && !selectedSerial) setSelectedSerial(firstUsable.serial);
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
  }, [selectedSerial]);

  useEffect(() => {
    if (optedIn) refresh();
  }, [optedIn, refresh]);

  const pickSource = useCallback(async () => {
    try {
      const picked = await openDialog({ multiple: false, directory: true });
      if (typeof picked === "string") setSourcePath(picked);
    } catch {
      // user cancelled
    }
  }, []);

  const runPlan = useCallback(async () => {
    if (!selectedSerial || !sourcePath || !destPath) {
      setPlanError("기기 / 로컬 폴더 / 기기 경로를 모두 지정해주세요.");
      return;
    }
    setPlanning(true);
    setPlanError(null);
    setPlan(null);
    try {
      const report = await invoke<AdbPlanReport>("adb_plan_upload", {
        serial: selectedSerial,
        source: sourcePath,
        destPath,
      });
      setPlan(report);
      // Open the manifest dialog whether or not there are conflicts:
      // even a clean plan benefits from a final confirm so the user
      // sees the file count before the stream starts. plan.md §5
      // `overwriteConfirmation: always` for ADB ⇒ batch confirm.
      setConfirmOpen(true);
    } catch (e) {
      setPlanError(e instanceof Error ? e.message : String(e));
    } finally {
      setPlanning(false);
    }
  }, [selectedSerial, sourcePath, destPath]);

  const confirmAndEnqueue = useCallback(async () => {
    if (!plan) return;
    try {
      const id = await invoke<number>("enqueue_adb_tar_upload", {
        planToken: plan.planToken,
      });
      setLastEnqueued(id);
      setConfirmOpen(false);
      setPlan(null);
    } catch (e) {
      setPlanError(e instanceof Error ? e.message : String(e));
    }
  }, [plan]);

  const cancelConfirm = useCallback(() => {
    setConfirmOpen(false);
    setPlan(null);
  }, []);

  if (!optedIn) {
    return (
      <div className="adb-strip collapsed">
        <span className="adb-label">ADB 고속 업로드</span>
        <span className="adb-meta">
          USB debugging이 켜진 Android 기기에서 사용 가능한 실험적 빠른 경로
        </span>
        <button onClick={() => setOptedIn(true)} className="primary">
          사용하기
        </button>
      </div>
    );
  }

  const selectedDevice = status?.devices.find((d) => d.serial === selectedSerial) ?? null;
  const gateReason = computeGateReason(status, selectedDevice);

  return (
    <div className="adb-strip">
      <div className="adb-row">
        <span className="adb-label">ADB 고속 업로드</span>
        <button onClick={refresh} disabled={loading}>
          {loading ? "검사 중..." : "기기 검사"}
        </button>
        <button onClick={() => setOptedIn(false)} className="ghost">
          닫기
        </button>
      </div>

      {gateReason && (
        <div className={`adb-banner ${gateReason.kind}`}>
          {gateReason.message}
        </div>
      )}

      {status?.adbAvailable && status.devices.length > 0 && (
        <div className="adb-row">
          <label className="adb-label">기기</label>
          <select
            value={selectedSerial ?? ""}
            onChange={(e) => setSelectedSerial(e.target.value || null)}
          >
            {status.devices.map((d) => (
              <option key={d.serial} value={d.serial}>
                {(d.model ?? d.serial) +
                  " (" +
                  d.serial +
                  ", " +
                  d.state +
                  (d.canTarUpload ? ", 고속 가능" : ", 고속 불가") +
                  ")"}
              </option>
            ))}
          </select>
        </div>
      )}

      <div className="adb-row">
        <label className="adb-label">로컬 폴더</label>
        <input
          type="text"
          value={sourcePath}
          placeholder="/path/to/upload"
          onChange={(e) => setSourcePath(e.target.value)}
        />
        <button onClick={pickSource}>선택...</button>
      </div>
      <div className="adb-row">
        <label className="adb-label">기기 경로</label>
        <input
          type="text"
          value={destPath}
          placeholder="/sdcard/Download/..."
          onChange={(e) => setDestPath(e.target.value)}
        />
        <button
          onClick={runPlan}
          disabled={planning || !selectedDevice?.canTarUpload}
          className="primary"
        >
          {planning ? "준비 중..." : "전송 준비"}
        </button>
      </div>

      {planError && <div className="adb-error">{planError}</div>}
      {lastEnqueued !== null && (
        <div className="adb-info">전송 작업이 큐에 추가되었습니다 (job #{lastEnqueued}).</div>
      )}

      {confirmOpen && plan && (
        <ConflictDialog
          plan={plan}
          onConfirm={confirmAndEnqueue}
          onCancel={cancelConfirm}
        />
      )}
    </div>
  );
}

function computeGateReason(
  status: AdbStatusWire | null,
  device: { canTarUpload: boolean; state: string; hasTar: boolean; tarExtractSmokeOk: boolean } | null,
): { kind: "ok" | "warn" | "error"; message: string } | null {
  if (!status) return null;
  if (!status.adbAvailable) {
    return {
      kind: "error",
      message:
        "adb 바이너리를 찾지 못했습니다. Android platform-tools를 설치하거나 CROSSMTP_ADB 환경변수를 설정해주세요. (" +
        (status.error ?? "unknown") +
        ")",
    };
  }
  if (status.devices.length === 0) {
    return {
      kind: "warn",
      message:
        "ADB로 인식된 기기가 없습니다. USB debugging이 켜져 있고 'Allow USB debugging' 프롬프트를 수락했는지 확인해주세요.",
    };
  }
  if (!device) {
    return { kind: "warn", message: "기기를 선택해주세요." };
  }
  if (device.state !== "device") {
    return {
      kind: "warn",
      message:
        "선택한 기기가 사용 가능한 상태가 아닙니다 (" +
        device.state +
        "). 폰의 USB debugging 권한을 확인해주세요.",
    };
  }
  if (!device.hasTar) {
    return {
      kind: "warn",
      message:
        "이 기기에서 tar 명령을 찾지 못했습니다. ADB 고속 모드를 사용할 수 없습니다 — MTP 업로드를 사용해주세요.",
    };
  }
  if (!device.tarExtractSmokeOk) {
    return {
      kind: "warn",
      message:
        "tar -x smoke check 실패. 이 기기는 ADB 고속 모드에서 안정적이지 않을 수 있습니다 — MTP 업로드를 권장합니다.",
    };
  }
  if (device.canTarUpload) {
    return {
      kind: "ok",
      message:
        "ADB 고속 업로드가 활성화되었습니다. USB debugging 권한이 유효한 동안에만 작동합니다.",
    };
  }
  return null;
}

interface DialogProps {
  plan: AdbPlanReport;
  onConfirm: () => void;
  onCancel: () => void;
}

function ConflictDialog({ plan, onConfirm, onCancel }: DialogProps) {
  const total = plan.clean.length + plan.skippedSame.length + plan.renamed.length;
  return (
    <div className="adb-modal-backdrop" onClick={onCancel}>
      <div className="adb-modal" onClick={(e) => e.stopPropagation()}>
        <h3>전송 확인</h3>
        <p>
          총 <b>{total}</b>개 파일 — 새 전송 <b>{plan.clean.length}</b>, 동일 파일 건너뜀{" "}
          <b>{plan.skippedSame.length}</b>, 이름 변경 <b>{plan.renamed.length}</b>
        </p>

        {plan.skippedSame.length > 0 && (
          <details>
            <summary>건너뛸 파일 ({plan.skippedSame.length})</summary>
            <ul className="adb-list">
              {plan.skippedSame.slice(0, 200).map((p) => (
                <li key={p}>{p}</li>
              ))}
              {plan.skippedSame.length > 200 && (
                <li>... 외 {plan.skippedSame.length - 200}개</li>
              )}
            </ul>
          </details>
        )}

        {plan.renamed.length > 0 && (
          <details>
            <summary>이름 변경될 파일 ({plan.renamed.length})</summary>
            <ul className="adb-list">
              {plan.renamed.slice(0, 200).map((r) => (
                <li key={r.original}>
                  {r.original} → {r.newName}
                </li>
              ))}
              {plan.renamed.length > 200 && <li>... 외 {plan.renamed.length - 200}개</li>}
            </ul>
          </details>
        )}

        <div className="adb-modal-actions">
          <button onClick={onCancel} className="ghost">
            취소
          </button>
          <button onClick={onConfirm} className="primary">
            전송 시작
          </button>
        </div>
      </div>
    </div>
  );
}
