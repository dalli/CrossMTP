# Phase 2 회고 — Transfer Orchestrator

작성일: 2026-04-13
담당: Lead, Backend, QA (Devil's Advocate 점검 포함)

## 목표 (계획서 기준)

* 단일 전송 큐 구현
* 진행률 이벤트 전달
* 취소/실패/충돌 처리 도입

산출물: 작업 상태 머신, 큐 관리자, 이벤트 스트림 인터페이스
통과 기준:
* 연속 10개 파일 전송 테스트에서 상태 불일치 없음
* 전송 중 취소와 케이블 분리 시 UI 및 상태가 정상 종료

## 산출물

### `mtp-session` 확장
* `Device::download_file_with_progress(file_id, dest, progress)`
* `Device::upload_file_with_progress(source, sid, pid, name, progress)`
* progress closure 시그니처: `FnMut(sent: u64, total: u64) -> bool`
  (true 반환 = cancel 요청)
* trampoline `progress_thunk` (`extern "C"`)이 fat pointer를 통해 closure 호출.
  `CANCEL_OBSERVED` thread-local로 cancel 여부를 호출 frame에 전달
  (libmtp가 ERROR_CANCELLED를 일관되게 set하지 않기 때문).
* `Capabilities::supports_progress_callback / supports_cancel` 둘 다 `true`로 승격
  (정직성 원칙: 진짜로 구현된 후에만 true).

### 새 crate: `crates/orchestrator`
**한 파일 응집형** (~370 LOC). 분할은 코드량이 명백히 한계에 닿을 때.

핵심 타입:
```rust
pub struct Orchestrator { /* worker thread handle, cmd_tx, cancel map */ }
pub struct JobSpec { kind: JobKind, conflict: ConflictPolicy }
pub enum JobKind { Download {..}, Upload {..} }
pub enum ConflictPolicy { Skip, Rename, Overwrite }
pub enum JobState {
    Queued, Validating, Transferring, Cancelling,
    Completed { item_id, bytes },
    Failed(String), Cancelled, Skipped(String),
}
pub enum Event { Enqueued, StateChanged, Progress, WorkerStopped }
```

설계 원칙:
* **single active worker**: device 핸들을 worker thread 1개가 소유.
  `Device: Send + !Sync`라 컴파일러가 강제.
* **state machine**: 모든 전이는 worker가 단방향으로 수행, `Event::StateChanged`로
  사전 통보. UI는 이벤트만 본다.
* **conflict resolution은 Validating 단계**: 실제 전송이 시작되기 전에 결정.
  `Rename`은 destination 목록을 한 번 listing하고 unique한 이름 산출.
  `Overwrite`는 download에서만 가능 (local fs overwrite). upload overwrite는
  delete API가 없으므로 `Failed` (정직성 원칙).
* **cancel 두 경로**:
  1. `Queued` 상태 → 큐에서 즉시 제거, `Cancelled`로 전이
  2. `Transferring` 상태 → `AtomicBool` 플래그를 progress closure가 매 콜백마다
     체크, true면 `true` 반환해 libmtp 중단 → `MtpError::Cancelled`로 surface →
     `Cancelled` 전이
* **Drop 안전**: `Orchestrator::drop`이 `Cmd::Shutdown` 보낸 후 thread join.
  worker는 큐 drain 후 `WorkerStopped` 이벤트 emit 후 종료.

### `mtp-cli`에 `verify-q` 추가
단일 프로세스에서 전체 orchestrator 시나리오 자동 실행:
1. download (Overwrite)
2. upload round-trip (Skip)
3. upload again with Rename → 새 파일명
4. upload again with Skip → Skipped 상태
5. enqueue + 즉시 cancel → Cancelled

이벤트 스트림은 `wait_terminal` / `drain_until_terminal_with_label`이
state 전이와 progress를 stdout으로 print해 검증.

## 통과 기준 vs 실측

| 기준 | 결과 |
|---|---|
| 단일 큐 | ✅ `Worker::queue: VecDeque<PendingJob>`, single active |
| 상태 머신 | ✅ 7개 상태 + Skipped, 전이 모두 worker 단독 |
| 진행률 이벤트 | ✅ `Event::Progress` infrastructure 작동. 단, **검증한 파일이 88 B로 너무 작아 libmtp가 progress callback을 호출하지 않음** — 실제 progress 이벤트는 출력 안 됨. Phase 4에서 큰 파일로 재검증 필요. |
| 취소 (큐 단계) | ✅ `verify-q` 5단계, Queued→Cancelled |
| 취소 (전송 중) | ⚠️ **미검증**. 88 B 파일은 즉시 끝나므로 in-flight cancel 시점이 없음. 코드 경로(progress closure → AtomicBool → libmtp non-zero return → `MtpError::Cancelled`)는 존재. Phase 4에서 큰 파일로 검증. |
| 충돌 (Skip) | ✅ Skipped("crossmtp-verify-q-25.bin exists on device") |
| 충돌 (Rename) | ✅ item_id 26 → 27 (다른 파일로 생성됨) |
| 충돌 (Overwrite, upload) | ✅ Failed로 surface (의도된 동작, MVP delete 미구현) |
| 연속 10개 파일 상태 불일치 없음 | ⚠️ **미검증**. verify-q는 5개 jobs 검증. 10개 무작위 파일 시나리오는 Phase 4 체크리스트에. |
| 케이블 분리 시 UI/상태 정상 종료 | ⚠️ **미검증**. transferring 중 cable pull은 수동 시나리오. Phase 4. |

### 실기기 검증 로그 (요약)
```
device: Alldocube iPlay50_mini_Pro (serial 840B080D...)
caps: progress=true cancel=true (post-Phase-2)
storage 0x00010001 (free 167.4 GB)
picked file: id=25 '.thumbcache_idx_FPNyuLhAtVnAeldjikus' (88 B)

--- 1) download via orchestrator ---
  [1] Queued -> Validating -> Transferring -> Completed { item_id: None, bytes: 88 }
--- 2) upload (round-trip) ---
  [2] Completed { item_id: Some(26), bytes: 88 }
--- 3) upload again (Rename) ---
  [3] Completed { item_id: Some(27), bytes: 88 }
--- 4) upload again (Skip) ---
  [4] Skipped("crossmtp-verify-q-25.bin exists on device")
--- 5) cancel-before-run ---
  [5] Queued -> Cancelled
```

### 참고: 누적된 기기 검증 범위 (Phase 0 + Phase 1 + Phase 2)
* **Xiaomi POCO F7 Pro** (Phase 1): list / storages / ls / pull / push round-trip
* **Alldocube iPlay50_mini_Pro** (Phase 2): orchestrator 5 시나리오 전체

→ 계획서의 "최소 2종 기기" 기준 충족. (의도한 건 아니었지만 결과적으로 다른 기기에서
검증되어 cross-device generality에 한 번 더 신호.)

## 의사결정 기록

1. **Worker는 한 파일 (`lib.rs`)**. ~370 LOC면 분할 비용 > 응집 비용. 모듈 분해는
   600 LOC 또는 도메인이 명확히 다른 코드가 추가될 때.
2. **Skipped를 Cancelled와 분리**. 사용자가 명시적으로 정책을 골라 "넘어감"과
   "강제 중단"은 UI에서 다른 의미를 가져야 함.
3. **Overwrite-upload는 Failed로**. 흉내 내려면 send-then-delete 패턴이 필요한데
   delete가 MVP 범위 밖. 거짓 성공보다 명시적 실패가 낫다 (AGENTS.md "honest" 원칙).
4. **Cancel 채널이 2단**:
   * `Orchestrator::cancel` → `Arc<AtomicBool>` flush
   * 동시에 `Cmd::Cancel(id)` 전송 (worker가 큐에서 fast-remove 시)
   둘 다 send-and-forget. flag-only로는 in-flight cancel만 지원, channel-only로는
   in-queue cancel만 지원하므로 둘이 필요.
5. **`CANCEL_OBSERVED` thread-local**. libmtp가 cancel 시 `LIBMTP_ERROR_CANCELLED`를
   set하지 않는 경우가 많아 호출자가 따로 기억해야 함. 같은 thread가 호출-콜백-결과
   체인을 갖기 때문에 thread_local로 충분.
6. **single subscriber event channel**. 다중 구독은 UI 레이어의 fan-out 책임.
   orchestrator는 가능한 한 작게.

## 반대론자 코멘트

> 1. **Progress 콜백은 코드 path만 검증, 실 데이터 미검증**. 88 B는 너무 작다.
>    Phase 4 체크리스트에 100 MB+ 파일 케이스 강제. 추가로 libmtp는 일부 기기에서
>    progress를 sent=0 또는 total=0으로만 보고할 수 있음 → UI에 거짓 0%/100%로
>    보이지 않게 보호 로직 필요.
> 2. **In-flight cancel 미검증**. 실제 데이터 전송 중 cancel을 누르면 `AtomicBool`
>    flag가 progress callback에서 보이는지, libmtp가 즉시 멈추는지, partial-write된
>    local file이 leak되지 않는지 확인 안 됨. Phase 4. 추가로 download가 cancel되면
>    `dest_dir/<name>`에 부분 파일이 남음 — orchestrator가 파일 unlink 해야 함.
> 3. **Rename 검색이 O(N²)**. `unique_remote_name`이 매 후보마다 HashSet 조회로
>    1000회까지 시도. 작은 폴더에선 무시할 수준이지만 큰 폴더에선 재고려.
> 4. **`list_entries` 두 번 호출**. validation 단계에서 한 번 listing, transferring
>    단계에서 libmtp가 내부 listing. 큰 디렉토리에서 비용 두 배. Phase 3에서
>    UI가 이미 listing을 들고 있으면 cache hit 가능 → 그때 최적화.
> 5. **`Orchestrator::cancel`이 cancel map에 없는 id에 대해 silent**. 이미 종료된
>    job이거나 잘못된 id면 그냥 무시. 디버그 로깅 추가 권장.
> 6. **케이블 분리 시 동작 불명**. 검증 안 됨. libmtp가 hang하면 worker thread가
>    join에서 영원히 막혀 `Orchestrator::drop`이 죽는다. Phase 4 또는 Phase 3
>    (UI가 timeout watchdog) 에서 처리.
> 7. **`thread::Builder::spawn().expect`** — 워커 spawn 실패 시 panic. 데스크톱
>    환경에서는 사실상 일어나지 않지만 Lib이라면 Result로 보내는 게 정석.
> 8. **`Event` channel에 `WorkerStopped` 이후에도 sender drop은 일어남** — UI가
>    이를 종료 신호로 쓰면 OK이지만 `WorkerStopped` 이벤트 자체는 redundant 가능.
>    당장 둠.

## 알려진 제약 (Known Limitations)

* In-flight cancel: 코드 path 존재, 실데이터 미검증
* 진행률: 큰 파일에서 미검증
* 케이블 분리: 미검증, hang 가능성
* 부분 다운로드 파일 cleanup 없음
* 단일 device, 단일 worker
* delete 미구현 → upload Overwrite 불가
* MVP delete 부재로 verify-q 누적 파일이 기기에 쌓임 (`crossmtp-verify-q-*.bin`)

## Phase 3 (macOS UI) 인수 항목

1. **이벤트 → React 상태 매핑**:
   * `StateChanged` → 작업 카드 색상/문구
   * `Progress` → progress bar (0% / 100% 거짓 보호 포함)
   * `Skipped`는 빨간색이 아니라 노란색/회색 (사용자 결정의 결과)
2. **에러 메시지 카탈로그**:
   * `MtpError::DeviceLocked` → "폰을 깨우고 USB 알림에서 '파일 전송' 선택"
   * `MtpError::StorageUnavailable` → "기기에서 MTP를 허용해주세요"
   * macOS daemon contention → "Android File Transfer / Image Capture가 USB를
     선점 중입니다. 종료 후 재연결" + 종료 버튼
3. **충돌 정책 UI**: 작업 enqueue 전에 사용자에게 물어보거나 글로벌 default 설정
4. **드래그앤드롭 업로드** → upload JobSpec 자동 생성
5. **취소 버튼**: in-flight cancel 케이스의 실 데이터 검증을 UI 동선과 함께
6. **반대론자 8건** 모두 매핑

## Phase 4 (검증) 인수 항목

* 100 MB+ 파일 download/upload progress 비율 검증
* 1 GB+ 파일
* 100개 small files 연속 (단일 큐로)
* 케이블 분리 시나리오 (전송 중)
* 화면 잠금 시나리오 (전송 중)
* 한글 파일명 / 깊은 경로
* 부분 다운로드 cleanup
* progress total=0 케이스 보호 (UI 측)

## 사용 방법

```bash
cargo build
killall "Android File Transfer" "Android File Transfer Agent" icdd 2>/dev/null
cargo run -p mtp-cli -- verify-q
```
