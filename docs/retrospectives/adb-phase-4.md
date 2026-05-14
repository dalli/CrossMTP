# ADB Phase 4 회고 — UI opt-in + byte-level progress

작성일: 2026-05-14
대상 계획: [docs/plan.md](../plan.md) §8 Phase 4, §2.1, §5, §4.5
선행 회고: [adb-phase-0.md](adb-phase-0.md), [adb-phase-1.md](adb-phase-1.md), [adb-phase-2.md](adb-phase-2.md), [adb-phase-3.md](adb-phase-3.md)

## 0. 진행 방식

Phase 3 retro §6 "Phase 4 인수 항목" 6건을 입력으로 받아 다음 순서로 진행했다.

1. `adb-session::tar_upload::upload_tar_with_progress` — byte-level progress callback wrapper (Phase 3 retro §6-3).
2. `orchestrator::AdbContext::probe` — `probe_device` + `smoke_check_extract` 를 묶어 `DeviceCapabilities` 캐시 (Phase 3 retro §6-5).
3. orchestrator `execute_adb_tar_upload` 가 byte-level progress 를 `Event::Progress` 로 발화하도록 wiring.
4. Tauri shell 에 ADB IPC commands 4개 추가: `adb_status` / `adb_plan_upload` / `enqueue_adb_tar_upload` / `adb_cancel_job`.
5. Plan token registry 도입 — `ConflictPlan` 은 서버에 머무르고 frontend 로는 opaque `planToken` 만 전달 (Phase 3 retro §6-4).
6. Per-serial orchestrator 캐시 — MTP 전용 orchestrator 와 ADB orchestrator 를 분리해 `AdbContext` 단일 소유 원칙 유지.
7. React `AdbPanel` 컴포넌트 — capability gate + 폼 + conflict manifest 모달 (plan.md §2.1 + §5).
8. `QueuePanel` 에 `Skipped N — 보기` 패널 + `adbTarUpload` job kind 라우팅 추가.
9. plan.md §8 Phase 4 본문에 진행 상태 + 추가 통과 기준 정식 반영.

실기기 라이브 검증은 Phase 3 과 동일한 사유로 이번 라운드에서도 **의도적으로 제외**한다. 본 호스트에 `adb` 가 PATH/SDK 어느 후보에도 없어 라이브 라운드를 돌리지 못했으며, Phase 0 의 4022-file fixture 재현 + 두 번째 기기 검증과 함께 Phase 5 라운드에서 처리하는 것이 비용 효율적이다.

## 1. 산출물

### `adb-session::upload_tar_with_progress`

```rust
pub type ProgressCallback = Box<dyn FnMut(u64) + Send>;

pub fn upload_tar_with_progress(
    session: &AdbSession,
    serial: &str,
    source_root: &Path,
    dest_path: &str,
    plan: ConflictPlan,
    cancel: CancelHandle,
    on_progress: Option<ProgressCallback>,
) -> Result<UploadOutcome>;
```

- 기존 `upload_tar` 는 `upload_tar_with_progress(..., None)` 의 thin wrapper 로 남겨 호환성 유지.
- `CancelAwareSink` 가 `bytes_written` 카운터를 들고 매 write 마다 100ms throttle 로 콜백 호출. 마지막에 한 번 더 호출해 truncated tail 을 방지.
- `Box<dyn FnMut>` 로 owned callback 을 받음. `&mut dyn` 으로 받으면 lifetime invariance 때문에 `'a` 가 무한히 번지므로 명시적으로 거절.

### `orchestrator::AdbContext::probe`

```rust
impl AdbContext {
    pub fn probe(session: Arc<AdbSession>, serial: String) -> Result<Self, AdbError> {
        let mut caps = adb_session::probe_device(&session, &serial)?;
        caps.tar_extract_smoke_ok =
            adb_session::smoke_check_extract(&session, &serial).unwrap_or(false);
        Ok(Self { session, serial, capabilities: Some(caps) })
    }
    pub fn can_tar_upload(&self) -> bool { /* ... */ }
}
```

Phase 3 retro §6-5 의 "smoke check auto-cache" 인수 항목. `AdbContext` 가 한 번 만들어진 시점부터 `DeviceCapabilities::tar_extract_smoke_ok` 가 채워진 상태로 cached 됨. orchestrator `execute_adb_tar_upload` 가 가장 먼저 확인하는 필드.

### byte-level progress wiring

```rust
let evt_tx = self.evt_tx.clone();
let on_progress: adb_session::ProgressCallback = Box::new(move |bytes| {
    let _ = evt_tx.send(Event::Progress { id, sent: bytes, total: bytes });
});
let outcome = adb_session::upload_tar_with_progress(
    &session, &serial, &source, &dest_path, plan, adb_cancel.clone(),
    Some(on_progress),
);
```

`sent == total` 으로 보내는 이유: USTAR stream 의 최종 크기를 사전에 계산하지 않으므로 `total` 을 정확히 알 수 없다. UI 측에서는 `useBulkProgress` 와 별개로 "byte counter 가 움직이고 있다" 시그널만 제공하면 충분하다. 정확한 percentage 가 필요하면 Phase 5 에서 `walk` 의 사전 스캔 결과를 `total_bytes` 로 채울 수 있지만 현 단계에서는 over-promise.

### Tauri IPC: 4 commands + plan registry

| Command | 입력 | 출력 | 비고 |
|---|---|---|---|
| `adb_status` | — | `AdbStatusWire { adbAvailable, adbPath, devices: [{serial, state, canTarUpload, hasTar, tarExtractSmokeOk, ...}] }` | 매 호출 시 discover + per-device probe + smoke check |
| `adb_plan_upload` | `serial`, `source`, `destPath` | `AdbPlanReportWire { planToken, clean, skippedSame, renamed: [{original, newName}] }` | manifest probe + planner; `ConflictPlan` 은 서버 보관 |
| `enqueue_adb_tar_upload` | `planToken` | `jobId` | token 1회용. 사용 즉시 registry 에서 제거 |
| `adb_cancel_job` | `serial`, `jobId` | — | per-serial orchestrator 의 `cancel` 호출 |

Plan token registry 결정:

- `ConflictPlan` 은 `HashMap<String, ConflictAction>` 기반이라 serde 가 자동 derive 되지 않고, 또 사용자 입력에 따라 수천 개 entry 까지 갈 수 있어 IPC 직렬화 비용이 비싸다.
- 대안 1: `Vec<(String, WireAction)>` 으로 직렬화 → frontend → 다시 직렬화해서 enqueue. 두 번 직렬화하면 plan 의 무결성을 frontend 가 보장해야 하는 신뢰 경계 문제가 생긴다.
- 대안 2 (채택): plan 을 서버 측 registry 에 두고 `planToken: u64` 만 frontend 로. 1회용 token 이므로 race 가 없고, 서버는 plan 의 단일 source of truth.

Per-serial orchestrator 결정:

- 기존 `Orchestrator` 는 단일 worker 에 `AdbContext` 가 하나만 붙는다. multi-device ADB 를 지원하려면 orchestrator 자체가 multi-serial 을 알아야 하는데 그 변경은 plan.md §2.2 "초기 제외 — multi-device 동시 ADB 전송" 와 배치된다.
- 대신 Tauri 가 `HashMap<serial, Arc<Orchestrator>>` 를 들고 첫 enqueue 시점에 lazy spawn. MTP orchestrator 와 ADB orchestrator 가 별도 thread 로 동시에 돌지만, 한 serial 당 하나의 worker 라는 원칙은 유지.

### React `AdbPanel`

`apps/desktop/src/components/AdbPanel.tsx` 신설. 핵심 흐름:

1. **Capability strip** (default collapsed) — "사용하기" 버튼을 누르면 `adb_status` 호출.
2. **Gate banner** — `computeGateReason()` 가 `(status, device)` 를 보고 `ok` / `warn` / `error` 중 하나의 메시지를 반환:
   - adb 미발견 → error (`platform-tools 설치 안내`)
   - 기기 없음 → warn (`USB debugging 활성화 안내`)
   - `state != device` → warn (`unauthorized / offline 등 노출`)
   - `!hasTar` → warn (`MTP 권장`)
   - `!tarExtractSmokeOk` → warn (`MTP 권장`)
   - `canTarUpload` → ok (`고속 모드 활성화`)
3. **Source / dest 입력** + `전송 준비` 버튼.
4. **Conflict manifest 모달** — clean / skippedSame / renamed 카운트 + 펼쳐서 200개까지 목록 (그 이상은 "... 외 N개"). plan.md §5 `overwriteConfirmation: always` 정책에 따라 conflict 가 없어도 한 번 더 confirm.
5. **enqueue 후 transfer-event 채널 공유** — ADB orchestrator pump 가 같은 `transfer-event` 채널로 emit 하므로 `QueuePanel` 코드는 ADB / MTP 구분이 필요 없다.

### `QueuePanel` 변경

- `JobKindView` 에 `adbTarUpload` variant 추가.
- `labelFor(job)` 와 `directionFor(job)` 헬퍼로 분기. ADB job 은 `[ADB] <destPath basename>` 형태로 표기.
- `Skipped N — 보기` 토글 — 터미널 상태 `skipped` 인 모든 job 의 `reason` 을 펼침. plan.md §6.2 통과 기준 "완료 화면에서 `skipped` 항목 목록을 확인할 수 있음" 직결.

### CSS

`styles.css` 끝에 `.adb-strip`, `.adb-banner`, `.adb-modal*`, `.queue-skipped*`, `button.ghost` 추가. 디자인 토큰은 기존 변수 (`--bg-elevated` / `--border` / `--text-dim` / `--accent` / `--err` / `--ok` / `--warn`) 재사용.

## 2. 통과 기준 vs 실측

§8 Phase 4 통과 기준 (본 phase 에서 새로 명시한 3건 포함):

| 기준 | 결과 |
|---|---|
| ADB 불가 상태에서 이유 표시 | ✅ `computeGateReason()` 의 5단계 분기 |
| ADB 가능 상태에서 추천 선택값과 MTP fallback 선택지가 구분됨 | ✅ MTP UI 는 그대로, ADB 는 별도 strip + 모달로 분리 |
| 같은 파일 기본값 Skip / 다른 파일 Rename / overwrite 명시 선택 | ✅ `UploadPolicy::plan_defaults()` 사용, frontend 는 정책 자체를 노출하지 않음 |
| `Ask every time` → manifest 기반 일괄 dialog | ✅ `ConflictDialog` 가 plan 의 `skippedSame` / `renamed` 를 한 번에 표시 |
| 완료 화면에서 `Skipped N개 — 보기` 표시 | ✅ `QueuePanel` 토글 패널 |
| 전송 중 진행률과 취소 동작 | ✅ byte-level progress → `Event::Progress` → `transfer-event`; `adb_cancel_job` |
| 기존 MTP 전송 UI 퇴행하지 않음 | ✅ 기존 컴포넌트 prop 시그니처 유지, 새 `AdbPanel` 은 Banner 아래 strip 으로만 추가 |
| `AdbContext::probe` 자동 캐시 | ✅ `probe_device` + `smoke_check_extract` 결합, `DeviceCapabilities::tar_extract_smoke_ok` 채워서 반환 |
| `upload_tar_with_progress` 100ms throttle | ✅ `CancelAwareSink::last_emit` Instant 기반, 단위 테스트 `cancel_aware_sink_tracks_bytes_written` |
| `ConflictPlan` 서버 측 registry | ✅ `AppState::adb_plans: Mutex<HashMap<u64, AdbPlanEntry>>` + atomic token id |

자동 테스트 결과:

```
$ cargo test --workspace
adb-session:   49 passed; 0 failed   (+1 vs Phase 3 — cancel_aware_sink_tracks_bytes_written)
orchestrator:   7 passed; 0 failed
tar-stream:    56 passed; 0 failed
total:        112 passed; 0 failed
```

Frontend:

```
$ npx tsc --noEmit
exit=0

$ npm run build
✓ 40 modules transformed.
dist/index.html                   0.39 kB
dist/assets/index-DMaGj_Cl.css    8.25 kB
dist/assets/index-5sKRSawR.js   169.06 kB
✓ built in 281ms
```

실기기 검증 (Phase 5 입력으로 이월):

1. `adb_status` → Xiaomi `ea33d2fe` 환경에서 `canTarUpload=true`, `tarExtractSmokeOk=true` 기대.
2. `adb_plan_upload` 3-file fixture → `clean=3, skippedSame=0, renamed=0` 기대.
3. `enqueue_adb_tar_upload` → byte-level `transfer-event` 가 UI 에 10/s 빈도로 도달하는지 확인.
4. 4022-file fixture 로 cancel 라이브 라운드 → device-side stray `tar -x` PID 0개 확인.

## 3. 의사결정 기록

1. **`ProgressCallback` 을 `Box<dyn FnMut + Send>` 로 결정한 이유**.
   처음 `&mut dyn FnMut(u64) + Send + 'a` 로 시도했으나 `&mut dyn Trait` 의 invariance 때문에 `'a` 가 함수 전체로 번지며 caller 측에서 명시적 named lifetime 을 강요했다. orchestrator 는 callback 의 lifetime 을 신경쓸 일이 없고 (job 단위로 새로 만들어 한 번 소비), `Box` 의 1회 heap alloc 비용은 100ms throttle 빈도에 비해 무시 가능. owned API 가 호출 위치에서도 훨씬 깔끔하다.

2. **byte-level progress 의 `sent = total = bytes_written` 결정**.
   USTAR archive 의 최종 크기는 사전 계산 가능하지만 (header + padded payload + EOA marker), 사전 스캔 비용을 본 phase 에 끼워넣으면 enqueue 가 무거워진다. UI 는 이미 `transferring && bulkProgress` 우선 표시이고 ADB 의 경우 별도 bulk progress 가 없으므로 "bytes counter 가 움직이는가" 만 보여줘도 사용자 신뢰 측면에서 충분. 정확한 percentage 는 Phase 5 throughput round 에서 사전 스캔 도입을 평가.

3. **`ConflictPlan` 을 서버 측 registry 에 두기로 한 이유**.
   Phase 3 retro §4-5 가 IPC wire format 의 필요성을 짚었지만, 본 phase 에서 다시 검토한 결과 wire 직렬화는 두 가지 문제가 있다:
   - `ConflictPlan::map` 이 `HashMap<String, ConflictAction>` 라 frontend 가 통과시키면 신뢰 경계가 모호해진다 (frontend 가 plan 을 수정해서 다시 enqueue 하면 어떻게 될까?).
   - 대량 파일 시나리오 (5000 files) 에서 plan 을 IPC 로 두 번 통과시키는 비용이 1MB+ 가 될 수 있다.
   1회용 opaque token 으로 plan 의 단일 source of truth 를 서버에 두면 두 문제 모두 해소. 프로세스 재시작 시 token 이 무효화되는 건 expected (실제 전송은 enqueue 직후 시작되므로 process boundary 를 넘기지 않음).

4. **Per-serial orchestrator vs 단일 multi-context orchestrator**.
   단일 orchestrator 에 multi-context 를 부착하려면 `Worker::execute` 가 매 job 마다 serial → context 매칭을 해야 하고 cancel handle 도 (serial, jobId) 페어 키가 된다. 본 phase 의 단일 device 가정 (plan.md §2.2) 하에서는 over-engineering. `HashMap<serial, Arc<Orchestrator>>` 가 의도와 데이터 흐름을 더 직접적으로 표현한다. multi-device 가 나중에 필요해지면 그 시점에 통합.

5. **`AdbPanel` 을 Banner 안이 아니라 그 아래 별도 strip 으로 둔 이유**.
   기존 Banner 는 "지금 보고 있는 MTP 기기의 상태 + 충돌 정책 설정" 이라는 단일 책임. ADB 는 별도 backend 이고 opt-in 이라 같은 strip 에 넣으면 두 정책 (MTP `conflictPolicy` 와 ADB 의 plan-driven 정책) 이 시각적으로 동등해 보여 사용자 혼란이 커진다. Strip 을 분리하면 plan.md §2.1 "fallback인지 추천 선택값인지 구분" 이 UI 자체에서 자명해진다.

6. **`AdbPanel` 초기 collapsed 상태**.
   첫 화면부터 ADB strip 이 항상 펼쳐져 있으면 ADB 를 안 쓰는 사용자에게는 잡음. "사용하기" 버튼을 한 번 눌러야 status probe + 폼이 보이도록 함. plan.md §7 "첫 사용 opt-in" 원칙과 일치.

7. **`ConflictDialog` 가 conflict 가 없어도 띄우는 이유**.
   plan.md §5 `overwriteConfirmation: always` 가 ADB 의 default 이고 본 phase 의 정책 모델은 그 default 만 구현한다. conflict 가 0건이어도 "총 N개 파일 전송 — 시작" confirm 을 한 번 받는 게 dest path 오타나 잘못된 폴더 선택을 막아 사용자에게 더 안전하다.

## 4. 반대론자 (Devil's Advocate) 코멘트

> "통과는 했지만 다음을 직시할 것."
>
> 1. **실기기 라이브 검증 0건 (3 phase 연속)**. Phase 0 이후 모든 phase 가 자동 테스트로만 통과했다. byte-level progress 가 실제 기기에서 정말 100ms 단위로 도달하는지, conflict dialog 가 5000-file 시나리오에서 freeze 없이 렌더되는지, `adb_status` 의 per-device probe + smoke check 가 USB hub 상황에서 hang 하지 않는지 - 모두 검증되지 않았다. Phase 5 진입은 의무.
>
> 2. **Plan token registry 의 leak 시나리오**. `adb_plan_upload` 가 token 을 발급하고 frontend 가 `enqueue` 를 안 부르면 token 이 영구히 메모리에 남는다. 본 phase 는 token 의 TTL 도, 최대 동시 token 수도 정의하지 않았다. 사용자가 plan 만 반복적으로 만들고 dest 를 바꾸며 살펴보는 흐름에서 plan map 이 무한히 자라는 게 가능.
>
> 3. **`enqueue_adb_tar_upload` 의 event pump leak**. Per-serial orchestrator 의 pump 가 orchestrator drop 시 자동 종료되긴 하지만, 본 phase 는 orchestrator 자체를 drop 하는 경로가 없다. 사용자가 ADB 를 한 번 켜고 안 쓰면 worker thread 가 idle blocked 로 영구히 살아있다. 명시적 shutdown 경로 부재.
>
> 4. **`AdbContext::probe` 실패 시 사용자 경험**. `probe_device` 가 실패하면 `Err` 를 surface 하지만, `smoke_check_extract` 가 실패하면 silently `false` 로 캐시한다. 사용자는 "왜 smoke check 가 실패했는지" 알 길이 없다 — UI 의 "tar -x smoke check 실패" 메시지 외에. orchestrator log 를 frontend 로 surface 하는 채널이 없다.
>
> 5. **`adb_status` race**. 사용자가 "기기 검사" 를 빠르게 두 번 누르면 두 번째 호출이 첫 번째의 in-flight smoke check 와 race 한다. 본 phase 는 `Mutex<Option<Arc<AdbSession>>>` 으로 session 만 직렬화하고 probe 자체는 직렬화하지 않는다. 같은 serial 에 대해 두 smoke check 가 동시에 device 에 spawn 될 수 있음 — toybox 가 견딘다고 가정 중.
>
> 6. **`AdbPanel` 의 dest path validation 부재**. frontend 가 `/sdcard/Download/CrossMTP` 같은 default 만 검증하고, 사용자가 `/system/...` 같은 위험한 경로를 입력해도 일단 plan 호출까지는 간다. 백엔드의 `is_safe_dest_path` 가 최종 방어선이지만 UX 적으로는 form-level validation 으로 미리 막아주는 게 친절. 본 phase 에서는 미구현.
>
> 7. **CSS 변수 의존성**. `.adb-banner.ok/warn/error` 가 하드코딩된 색상 (`#0a3d2e` 등) 을 쓴다. 기존 banner 가 같은 색상을 쓰는 것과 의도적으로 맞췄지만, 라이트 테마가 도입되면 모든 곳을 동시에 수정해야 한다. design token 으로 추출하지 않은 결정은 본 phase 의 scope 를 줄이려는 trade-off.

## 5. 알려진 제약 (Known Limitations)

- **실기기 0건**: Phase 0 이후 누적되는 라이브 검증 부채. Phase 5 의 throughput round 와 묶어서 일괄 처리.
- **Plan token TTL 미정**: registry 가 무한히 자랄 수 있음. Phase 5 에서 sliding TTL 또는 LRU 도입.
- **ADB orchestrator shutdown 경로 부재**: 명시적 `shutdown_adb` command 없음. 프로세스 종료 시점에 `Orchestrator::Drop` 만 의존.
- **Progress total 미지수**: byte-level progress 가 `sent == total` 로만 도착하므로 percentage 계산 불가. UI 는 "전송 중" 만 표시.
- **Dest path frontend validation 부재**: backend `is_safe_dest_path` 가 최종 방어선이지만 form-level pre-flight 없음.
- **Light theme 부재로 ADB banner 색상이 하드코딩됨**.
- **multi-device 동시 ADB 전송 미지원** (plan.md §2.2 의 initial-exclude 와 일치).
- **plan.md §8 Phase 4 본문에 본 phase 의 진행 상태 / 추가 통과 기준 / Phase 5 이월 항목을 정식 반영했다**.

## 6. Phase 5 인수 항목

1. **실기기 throughput round**: Xiaomi `ea33d2fe` + 두 번째 기기에서 1GB / 5000-file / 한글 fixture 측정.
2. **MTP 대비 3배 기준 확인**: plan.md §11 의 MVP 반영 기준. 작은 파일 묶음에서 MTP baseline 측정 → ADB 가 3배 미만이면 추천 선택값에서 격하.
3. **4022-file cancel 실기기 재현**: §6.1 5단계 정리 시퀀스가 실기기에서 device-side stray `tar -x` PID 0개를 보장하는지 확인.
4. **`AdbContext::probe` 실기기 동작**: `tar_extract_smoke_ok` 가 toybox 0.8.11 + 다른 vendor tar 에서 모두 true 인지.
5. **Plan token 메모리 사용량 측정**: 5000-file plan 의 registry entry 가 실제로 몇 KB 인지.
6. **반대론자 7건 흡수**: §4 의 1~7 을 Phase 5 작업 항목으로 매핑.
7. **README + troubleshooting 문서 작성**: plan.md §8 Phase 5 의 "알려진 제한사항을 README 또는 troubleshooting 문서에 반영" 인수 항목.

## 7. 사용 방법

```bash
# 빌드
cargo build --workspace

# 자동 테스트 (워크스페이스 전체)
cargo test --workspace
# adb-session:  49 passed
# orchestrator:  7 passed
# tar-stream:   56 passed
# total: 112 passed

# 프론트엔드 타입체크 + 프로덕션 빌드
cd apps/desktop
npx tsc --noEmit
npm run build

# 데스크탑 앱 실행 (Tauri dev)
npm run tauri dev
# UI 흐름:
#   1. 상단 Banner 아래 "ADB 고속 업로드" strip → "사용하기"
#   2. "기기 검사" 버튼으로 adb_status 실행
#   3. 로컬 폴더 + 기기 경로 입력 → "전송 준비"
#   4. Conflict manifest 모달에서 카운트 확인 → "전송 시작"
#   5. 전송 큐 패널에서 byte-level progress 와 [ADB] prefix 확인
#   6. 완료 후 "Skipped N — 보기" 토글로 skip 사유 확인
```

## 8. 한 줄 요약

Phase 4 는 ADB IPC commands + plan token registry + AdbPanel + Skipped 패널 + byte-level progress 를 묶어 사용자 관점의 ADB opt-in 경로를 완성했다. 자동 112건 통과, frontend tsc + vite 빌드 통과, 실기기 라이브 라운드는 의도적으로 보류해 Phase 5 입력으로 명시 이월.
