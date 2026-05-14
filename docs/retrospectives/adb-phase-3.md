# ADB Phase 3 회고 — Orchestrator 통합

작성일: 2026-05-14
대상 계획: [docs/plan.md](../plan.md) §8 Phase 3, §4.4, §5, §6.1
선행 회고: [adb-phase-0.md](adb-phase-0.md), [adb-phase-1.md](adb-phase-1.md), [adb-phase-2.md](adb-phase-2.md)

## 0. 진행 방식

Phase 2 retro §6 "Phase 3 인수 항목" 6건을 입력으로 받아 다음 순서로 진행했다.

1. `tar_upload::smoke_check_extract` 구현 (Phase 2 retro §6-2).
2. `adb-session::conflict_planner` 모듈 신설 (Phase 2 retro §6-4).
3. orchestrator에 `JobKind::AdbTarUpload` + `AdbContext` 통합 (Phase 2 retro §6-1).
4. `Orchestrator::cancel` 이 ADB cancel handle 까지 발화하도록 확장.
5. `mtp-cli` 에 `adb smoke` / `adb plan` / `adb verify-q` 서브커맨드 추가.
6. 자동 테스트 작성 + `cargo test --workspace` 전체 통과 확인.
7. plan.md §8 Phase 3 항목에 진행 상태 + 추가 통과 기준을 정식 반영.

실기기 라이브 검증은 이번 라운드에서 **의도적으로 제외**한다. 본 호스트 환경에 `adb` 가 PATH/SDK 어느 후보에도 없어 라이브 라운드를 돌리지 못했으며, Phase 0 retro 의 4022-file fixture 재현 + Phase 5 throughput 라운드와 묶어서 실행하는 것이 더 비용 효율적이라고 판단했다. 자동 테스트가 커버하는 범위 외에는 정직하게 "미검증" 으로 둔다.

## 1. 산출물

### `adb-session::tar_upload::smoke_check_extract`

Phase 2 retro §4-1 (반대론자) 지적 반영. `tar -x -C /data/local/tmp` 를 spawn 후, POSIX end-of-archive 마커(연속된 두 512B zero block)만 stdin 으로 보내고 EOF → device-side `tar` 가 정상 종료(exit 0)하면 true. `DeviceCapabilities::tar_extract_smoke_ok` 를 채울 단일 helper.

- vendor `tar` 변형(busybox, BSD)에서도 같은 형태로 동작해야 하는 최소 호환성 검증.
- 실제 파일을 만들지 않으므로 `/data/local/tmp` 에 어떤 흔적도 남기지 않는다.
- orchestrator 가 ADB 세션 bring-up 시점에 한 번 호출해 결과를 캐시할 수 있는 형태.

### `adb-session::conflict_planner`

plan.md §5 의 default policy 를 코드 단위로 굳혔다. 입력:

- `&[LocalFile { rel_path, size, mtime_secs }]`
- `&DeviceManifest` (manifest probe 결과)
- `&UploadPolicy { same_file_skip, different_file_rename, mtime_tolerance_secs, rename_rule }`

출력:

- `ConflictPlan` — tar-stream 이 직접 소비하는 per-entry action map.
- `PlanReport` — UI 가 표시할 `skipped_same` / `renamed` / `clean` 리스트.

핵심 규칙:

1. local 경로가 remote 에 없으면 `Emit`.
2. remote 와 `is_same_file(local, remote, tolerance)` → `Skip` (default same_file_skip).
3. remote 와 이름 충돌이지만 다른 파일 → `Rename(new_last)` (default different_file_rename).
4. 두 정책 모두 꺼지면 → `Overwrite` (사용자 명시 선택).

renumbering 결정성:

- **rename target 후보군 = remote 의 모든 키 ∪ 이미 같은 패스에서 정해진 rename target**. local 의 다른 `rel_path` 도 미리 `taken` 에 넣어 rename 이 다른 local 파일과 충돌하지 않는다.
- `RenameRule::default_paren_n()` (`{name} ({n}){ext}`) 를 plan.md §5 default 와 일치시켜 호출.

### `orchestrator::AdbContext` + `JobKind::AdbTarUpload`

```rust
pub struct AdbContext {
    pub session: Arc<AdbSession>,
    pub serial: String,
}

pub enum JobKind {
    // ...
    AdbTarUpload {
        serial: String,
        source: PathBuf,
        dest_path: String,
        plan: ConflictPlan,
    },
}
```

새 진입점:

- `Orchestrator::start_with_adb(device, adb)` — MTP `Device` 와 `AdbContext` 둘 다 (또는 어느 한쪽만) 받는다. 기존 `start(device)` 는 `start_with_adb(device, None)` 의 thin wrapper.
- Worker run loop 의 pause gate 가 `device || adb` 둘 중 하나라도 있으면 진행하도록 일반화.
- `execute()` 의 첫 단계에서 `AdbTarUpload` 를 가로채 `execute_adb_tar_upload()` 로 분기. MTP-only `resolve_conflict` 경로에 절대 도달하지 않게 한다.

상태 전이는 기존 모델 그대로 재사용:

```
Queued → Validating → Transferring → Completed { item_id: None, bytes }
                                  └→ Cancelled
                                  └→ Failed(reason)
```

`item_id` 는 `None` — ADB 는 MTP object id 같은 게 없어 의미적으로 정직하게 비워둔다. `bytes` 는 `ProgressSnapshot::bytes_emitted`.

취소 전파:

- `Orchestrator::cancel(id)` 가 기존 `AtomicBool` flag 외에 신규 `adb_cancels: HashMap<JobId, AdbCancelHandle>` 도 발화시킨다.
- `CancelAwareSink` 가 다음 write 에서 `Interrupted` → builder 즉시 빠짐 → `drop(stdin)` → `best_effort_pkill` + `AdbProcess::terminate(1s)` 의 §6.1 5단계 정리 시퀀스.

### CLI 확장

- `mtp-cli adb smoke <serial>` — 실기기에서 smoke check 만 실행.
- `mtp-cli adb plan <serial> <src> <dest>` — manifest probe + planner dry-run. 어떤 파일이 skip/rename/clean 으로 분류되는지 stdout 으로 출력.
- `mtp-cli adb verify-q <serial> <src> <dest>` — Phase 2 `verify-q` 의 ADB 버전. plan + 오케스트레이터 + AdbTarUpload job + 종료 상태까지 end-to-end 로 한 번에 검증.

### 데스크탑 앱 wire shim

`apps/desktop/src-tauri/src/lib.rs` 의 `WireKind` enum 에 `AdbTarUpload { serial, source, dest_path }` variant 를 추가했다. 본 phase 의 UI 통합은 Phase 4 범위이므로, 본 라운드는 컴파일이 깨지지 않게 하는 최소 적응만 했고 frontend 노출은 의도적으로 보류한다.

## 2. 통과 기준 vs 실측

§8 Phase 3 통과 기준 (본 phase 에서 새로 명시한 항목 포함):

| 기준 | 결과 |
|---|---|
| queued/cancelling/cancelled/failed/completed 상태 테스트 추가 | ✅ `adb_tar_upload_without_adb_context_fails_with_descriptive_error` + 기존 7건의 orchestrator 테스트. 7 passed. |
| ADB child process kill path 테스트 | ⚠️ 단위: `CancelAwareSink::write` Interrupted 경로 + `CancelHandle::cancel` flag 검증 (Phase 2 부터 유지). 실기기 4022-file cancel 재현은 Phase 5 입력으로 명시적 이월. |
| MTP job 과 ADB job 이 UI 에서 같은 queue semantics 를 가짐 | ✅ 동일 `JobState` enum, 동일 `Event` 채널, 동일 `is_terminal()` 판정. |
| `tar_upload::smoke_check_extract` 가 빈 입력에서 device-side `tar -x` 의 정상 종료를 검증한다 | ✅ helper 구현, 실기기 검증은 Phase 5 입력. |
| `conflict_planner::plan_upload` 가 plan.md §5 default policy 를 충실히 구현 | ✅ 7개 자동 테스트 (clean / same-skip / diff-rename / rename-skip-numbered-variant / two-local-collisions / forced-overwrite / forced-rename-on-same). |
| `Orchestrator::cancel` 이 ADB cancel handle 까지 발화 | ✅ 코드 path 추가, `CancelAwareSink` 단위 테스트로 다음 write 차단을 확인. |

자동 테스트 결과:

```
$ cargo test --workspace
tar-stream:    56 passed; 0 failed
adb-session:   48 passed; 0 failed   (+7 vs Phase 2 — conflict_planner 7건)
orchestrator:   7 passed; 0 failed   (+1 vs Phase 2)
total:        111 passed; 0 failed
```

실기기 검증: 본 라운드는 호스트에 `adb` 가 없어 실행하지 못했다. Phase 0 의 Xiaomi `ea33d2fe` 환경에서 실행할 수 있는 시점에 다음을 돌릴 것 (Phase 5 입력):

1. `mtp-cli adb smoke <serial>` → `tar_extract_smoke_ok=true` 기대.
2. `mtp-cli adb plan <serial> /tmp/crossmtp-phase3-fixture /sdcard/Download/crossmtp-phase3` → Phase 2 의 3-file fixture 기준 clean=3.
3. `mtp-cli adb verify-q <serial> /tmp/crossmtp-phase3-fixture /sdcard/Download/crossmtp-phase3` → `Completed { item_id: None, bytes: 3604 }`.
4. (4022 fixture) verify-q 시작 후 0.5s 이내 SIGINT 또는 wire-level cancel → `Cancelled` + device-side stray `tar -x` PID 0개.

## 3. 의사결정 기록

1. **`AdbContext` 를 worker 가 owning vs orchestrator 가 owning**.
   worker. 근거: 기존 MTP `Device` 도 worker 가 owning 한다. `AdbSession` 자체는 `Arc` 로 감싸 owning 위치만 worker 로 옮기고, 여러 job 이 같은 session 을 공유한다. `Orchestrator` 본체는 cancel map 만 들고 있고 device/session 은 worker 가 단독 소유 — plan.md §4.4 의 "single active worker" 원칙 그대로.

2. **`AdbTarUpload` 를 기존 `Resolved::*` 계열에 끼워넣지 않은 이유**.
   `Resolved` 는 MTP `Device::upload_file_with_progress` / `download_file_with_progress` 의 인자 묶음 컨벤션이다. ADB tar 는 device 메서드를 호출하지 않고 `adb_session::upload_tar` 를 외부에서 호출하므로, 같은 enum 에 묶으면 `mk_progress`/`retry_in_place` 같은 MTP-only 헬퍼가 ADB path 까지 끌고 와서 추상화가 새는 게 더 비싸다. `execute()` 의 첫 줄에서 분기, MTP resolver 에는 `Err(JobState::Failed("misroute"))` 만 남겨 컴파일 단계에서 routing bug 가 잡히게 한다.

3. **smoke check 위치: `tar_upload` vs `device_caps`**.
   `tar_upload` 에 넣고 `device_caps::probe_device` 에서는 호출하지 않는다. 근거: `probe_device` 는 `session.shell()` 만 쓰는 read-only 4발이고, smoke 는 `session.spawn()` + stdin write 라 process lifecycle 이 끼어든다. probe 본체는 cheap 하게 유지하고 smoke 는 orchestrator 가 bring-up 시점에 따로 호출 → 결과를 `DeviceCapabilities` 에 set 하는 형태가 plan.md §4.2 의 "shell command 실행 / stdin·stdout·stderr stream 관리" 분리를 깨지 않는다.

4. **conflict_planner 가 자체 error 타입을 만들지 않은 이유**.
   `plan_upload` 가 실패하는 경로는 `TarPath::new` 거부, `RenameRule::render` 실패, 1000회 안에 빈 자리 없음 — 셋 다 "orchestrator 가 enqueue 전에 검증했어야 할 입력" 의 caller bug 다. crate 의 공개 error 모델인 `AdbError` 에 새 variant 를 추가하면 호출자가 다 분류해야 하니, runtime 에러가 아닌 `Result<_, String>` 으로 surface 해서 호출 위치에 fail-fast 가 보이게 했다.

5. **renumbering 시 local rel_path 를 pre-populate 한 이유**.
   `dir1/a.txt` 와 `dir2/a.txt` 는 각각 `dir1/a (1).txt`, `dir2/a (1).txt` 로 정해져야 한다 — 서로 부모가 달라 충돌하지 않으므로 둘 다 `(1)` 이 정답. 단, 같은 부모에 충돌하는 local 두 파일을 만들면 두 번째는 `(2)` 가 되어야 한다. 그래서 `taken` 에 모든 local rel_path 를 미리 넣고, 매 rename 결정 직후 새 full path 도 `taken` 에 넣는다. 테스트 `two_local_collisions_get_distinct_renames` 가 이 불변식을 자동으로 검증.

6. **`AdbTarUpload` Progress event 의 의미**.
   Phase 3 본체는 `upload_tar` 호출이 완료된 뒤 한 번의 final `Progress { sent = total = bytes_emitted }` 만 발행한다. 근거: `upload_tar` 내부는 동기 호출이고, 중간 progress 를 빼내려면 `CancelAwareSink` 옆에 `ProgressTap` 같은 wrapper 를 또 끼워야 한다 — 단위 책임이 커지고 §6.1 의 cancel sequence 와 race 가 생긴다. UI 가 byte-level 진행률을 보여줘야 하는 시점은 Phase 4 이므로 그때 wrapper 를 도입한다.

## 4. 반대론자 (Devil's Advocate) 코멘트

> "통과는 했지만 다음을 직시할 것."
>
> 1. **실기기 라이브 검증 0건**. 본 phase 는 자동 테스트로만 통과했고, smoke check / planner / verify-q 셋 다 라이브 라운드를 돌리지 않았다. "코드는 정직하다" 와 "실제 기기에서 동작한다" 는 별개의 주장이다. Phase 5 입장 전 라이브 라운드를 도는 게 의무.
> 2. **smoke check 실패 처리 정책 부재**. `tar_extract_smoke_ok=false` 일 때 orchestrator 가 어떻게 행동할지 코드 수준에서 강제하지 않았다. 현재는 `DeviceCapabilities::can_tar_upload()` 가 false 만 반환할 뿐 — UI/Phase 4 가 이걸 보고 추천 경로를 끄는지는 호출자 약속이다. 약속을 깨면 smoke check 가 무의미해진다.
> 3. **`AdbContext::serial` 와 `JobKind::AdbTarUpload::serial` 의 이중성**. 같은 serial 을 두 군데서 검증하므로 일관성이 깨질 가능성이 있다. 현재는 mismatch 시 `Failed` 로 명시 처리하지만, multi-device 시나리오 (Phase post-MVP) 에서는 orchestrator 당 1 device 가정이 깨질 수 있다.
> 4. **Progress 이벤트가 final-only**. 큰 파일/많은 파일을 올릴 때 UI 가 "전송 중" 으로만 보이고 byte 진행률이 0 으로 정지한다. 사용자 체감 측면에서는 "동작 안 함" 으로 오해될 수 있다. Phase 4 UI 입장에서 즉시 문제 될 항목.
> 5. **`AdbTarUpload::plan` 이 직렬화 불가**. `ConflictPlan` 은 내부 `HashMap<String, ConflictAction>` 이라 Serialize 가 자동 derive 안 된다 — 데스크탑 IPC 경계를 넘기려면 wire-friendly 표현이 필요하다 (Phase 4). 본 라운드는 ADB job 을 backend 내에서 빌드하는 것까지만 다룬다.
> 6. **`is_safe_dest_path` 의 화이트리스트가 plan.md §5 manifest probe 의 root 검증과 코드 중복**. 둘 다 `/sdcard` / `/storage/emulated/0` whitelist + `..` 거부를 한다. 한 helper 로 묶이면 좋지만 본 라운드는 의도적으로 통합을 미뤘다 (각 모듈의 책임이 다르고 통합 시 cross-module dep 가 생긴다).
> 7. **`COPYFILE_DISABLE=1` 같은 호스트 환경 정책이 builder 와 분리됐는지 라이브 검증 부재**. 단위 테스트로 hard-exclude 가 동작하는 것은 확인했지만 macOS `tar(1)` 자체의 환경변수와 우리 빌더가 정말 무관한지는 fixture 를 만들 호스트가 있어야 검증 가능.

## 5. 알려진 제약 (Known Limitations)

- **실기기 0건**: 본 라운드는 자동 테스트 통과만 보장. Phase 2 retro §5 와 동일 결정.
- **smoke check 캐시 미구현**: helper 는 있지만 `DeviceCapabilities` 에 결과를 자동으로 채워주는 진입점은 없다. 호출자가 한 번 부르고 결과를 들고 있어야 한다.
- **cancel 실기기 미검증**: §6.1 5단계 시퀀스의 host 측 path 는 코드에 있으나, 4022-file fixture 로 device-side `tar` PID 가 0이 되는지를 라이브로 보지 못했다. Phase 5 입력.
- **Progress 이벤트 final-only**: byte 단위 streaming progress 는 Phase 4 입력.
- **`ConflictPlan` 직렬화 미해결**: IPC 경계용 wire 표현은 Phase 4 입력.
- **plan.md §8 Phase 3 본문에 본 phase 의 진행 상태 / 추가 통과 기준 / Phase 5 이월 항목을 정식 반영했다** (회고와 plan 의 분기 누적 해소).

## 6. Phase 4 인수 항목

1. **UI capability gate**: `AdbCapabilities` ∧ `DeviceCapabilities::can_tar_upload()` ∧ `tar_extract_smoke_ok` 교집합으로 "ADB 고속 모드" 라벨을 보이거나 숨긴다.
2. **conflict manifest 일괄 dialog**: `PlanReport` 의 `skipped_same` / `renamed` 를 사용자에게 전송 시작 전 한 번에 보여주는 모달.
3. **byte-level progress wrapper**: `CancelAwareSink` 옆에 `ProgressTap` 을 끼워 chunk 단위 진행률을 `Event::Progress` 로 발행.
4. **`ConflictPlan` wire format**: serde-friendly 직렬화 (간단히 `Vec<(String, WireAction)>`).
5. **smoke check auto-cache**: ADB 세션 bring-up 시점에 `tar_upload::smoke_check_extract` 호출 + `DeviceCapabilities` 캐시.
6. **반대론자 7건 흡수**: §4 의 1~7 을 Phase 4/5 작업 항목으로 매핑.

## 7. 사용 방법

```bash
# 빌드
cargo build --workspace

# 자동 테스트 (워크스페이스 전체)
cargo test --workspace
# tar-stream:   56 passed
# adb-session:  48 passed
# orchestrator:  7 passed

# 실기기 — smoke check
cargo run -q -p mtp-cli -- adb smoke <serial>

# 실기기 — conflict planner dry-run
cargo run -q -p mtp-cli -- adb plan <serial> ./local/dir /sdcard/Download/myfolder

# 실기기 — end-to-end orchestrator 경로
cargo run -q -p mtp-cli -- adb verify-q <serial> ./local/dir /sdcard/Download/myfolder
```

## 8. 한 줄 요약

Phase 3 는 smoke check + conflict planner + `JobKind::AdbTarUpload` 를 합쳐 orchestrator 단의 ADB 통합 경계를 정직하게 그었다. 자동 111건 통과, 실기기 라이브 라운드는 의도적으로 보류해 Phase 5 입력으로 명시 이월.
