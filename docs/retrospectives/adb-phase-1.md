# ADB Phase 1 회고 — ADB Session Layer

작성일: 2026-05-14
대상 계획: [docs/plan.md](../plan.md) §8 Phase 1, §4.2
선행 회고: [adb-phase-0.md](adb-phase-0.md)

## 0. 진행 방식

Phase 0 retro의 "조건부 진입 가능" 조건 중 다음을 부분 충족한 상태로 Phase 1을 시작했다.

- ✅ 호스트 측 결정 게이트 5건 확정 (adb 탐색 순서, tar baseline, AppleDouble hard-exclude, storage 매트릭스, manifest probe 명령).
- ⚠️ 두 번째 기기 검증은 여전히 미충족. Phase 1 산출물은 단일 기기(Xiaomi `ea33d2fe` / Android 15)로 검증한다. 두 번째 기기 검증은 Phase 5 입력으로 이월.
- ⚠️ 케이블 분리 자동화 검증은 Phase 2로 이월 (이번 phase는 child process API만 제공, 라이브 stream 사용은 다음 phase).

## 1. 산출물

### 새 crate: `crates/adb-session`

UI/Tauri 의존성 없음. `thiserror` 외 외부 dep 0. workspace `Cargo.toml`에 등록.

```
crates/adb-session/
├── Cargo.toml
└── src/
    ├── lib.rs         # 공개 표면 + re-export
    ├── error.rs       # AdbError + Result
    ├── capability.rs  # AdbCapabilities (Phase 1 default)
    ├── discovery.rs   # discover_adb() + DiscoveryEnv trait
    ├── devices.rs     # parse_devices_output() + AdbDevice + DeviceState
    ├── process.rs     # AdbRunner trait + CommandRunner + AdbProcess
    └── session.rs     # AdbSession (top-level)
```

**핵심 API**

- `AdbSession::open() -> Result<AdbSession>` — 탐색 + 핸들 구축. `adb` 미발견 시 `AdbError::AdbNotAvailable`.
- `AdbSession::list_devices()` — `adb devices -l` 호출 + 분류된 `Vec<AdbDevice>`.
- `AdbSession::pick_ready_device()` — 첫 ready device. ready 없으면 가장 구체적인 상태 에러 (Unauthorized/Offline/NoPermissions) surface.
- `AdbSession::require_device(serial)` — serial로 조회. 없으면 `DeviceNotFound`.
- `AdbSession::shell(serial, &[args])` — `adb -s <serial> shell <args...>` 한 번 실행, stdout/stderr/exit 캡처.
- `AdbSession::spawn(serial, &[args], label) -> AdbProcess` — piped stdio child. Phase 2 tar streaming 진입점.
- `AdbProcess::{pid, take_stdin, take_stdout, take_stderr, terminate(grace), kill}` — §6.1 cancel 시퀀스용.

**capability struct** (Phase 1 진실):

```rust
AdbCapabilities {
    adb_availability_probe: true,
    adb_tar_upload: false,          // Phase 2
    can_run_shell: true,
    can_track_child_processes: true,
}
```

### 에러 모델 (§4.2 명세 매핑)

| §4.2 case | AdbError variant |
|---|---|
| adb 탐색 실패 | `AdbNotAvailable` |
| device not found | `DeviceNotFound { serial }` |
| unauthorized | `Unauthorized { serial }` |
| offline | `Offline { serial }` |
| no permissions | `NoPermissions { serial }` |
| 일반 실패 | `CommandFailed { code, stderr }` |
| 신호로 종료 | `CommandTerminated` |
| 파싱 실패 | `ParseError(raw)` |
| IO | `Io(io::Error)` |

추가 헬퍼:
- `is_likely_user_action_required()` — UI가 "폰에서 허용 누르세요" 분기를 위한 single check (MTP의 `is_likely_permission_issue`와 의도적으로 동형).
- `is_fatal_for_session()` — capability probe 루프에서 무한 재시도 방지.

### CLI: `mtp-cli adb {where|devices|probe|shell}`

`crates/mtp-cli`에 dep 추가. 기존 MTP 서브커맨드 회귀 없음.

- `mtp-cli adb where` → 탐색 결과 path + source 표시
- `mtp-cli adb devices` → 분류된 device 리스트
- `mtp-cli adb probe [serial]` → 탐색 → device 선택 → shell `getprop ro.build.version.release`로 end-to-end 동작 확인
- `mtp-cli adb shell <serial> -- <cmd...>` → 디버그용 임의 shell 실행

## 2. 통과 기준 vs 실측

§8 Phase 1 통과 기준:

| 기준 | 결과 |
|---|---|
| ADB 없음 / unauthorized / offline / connected가 자동·수동 테스트로 구분 | ✅ 자동: stub-runner 단위 테스트로 4종 상태 모두 분기 검증. 수동: `mtp-cli adb devices`로 실기기 1대 connected 분기 확인. unauthorized/offline은 Phase 5 실기기 라운드에서 manual reproduce 예정. |
| UI/command layer가 ADB 가능 여부를 capability로 받을 수 있음 | ✅ `AdbCapabilities` 노출. `AdbSession::capabilities()`로 접근. UI 통합은 Phase 4 영역. |

자동 테스트 결과 (`cargo test -p adb-session`):

```
test result: ok. 22 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

대상 시나리오:
- discovery 6종 (env var precedence / env miss fallthrough / SDK > PATH > brew / brew last / 전부 미발견)
- devices 파서 7종 (ready / unauthorized / offline / no permissions / daemon chatter / 빈 리스트 / unknown state preserve)
- process 2종 (runner stub end-to-end / nonzero exit → CommandFailed)
- session 7종 (pick_ready 분기 3종, require_device 분기 2종, shell args verbatim, stderr propagation)

실기기 검증 (Xiaomi `ea33d2fe`, Android 15, toybox 0.8.11):

```
$ cargo run -q -p mtp-cli -- adb where
adb path:   /Users/dalli/Library/Android/sdk/platform-tools/adb
adb source: AndroidSdk

$ cargo run -q -p mtp-cli -- adb devices
ea33d2fe                 state=Device         transport_id=5 model=24117RK2CG product=zorn_global

$ cargo run -q -p mtp-cli -- adb probe
=== adb probe ===
adb: /Users/dalli/Library/Android/sdk/platform-tools/adb (AndroidSdk)
caps: probe=true tar_upload=false shell=true child_tracking=true
device: serial=ea33d2fe state=Device model=Some("24117RK2CG") product=Some("zorn_global")
shell exit=0 ro.build.version.release=15
```

CROSSMTP_ADB env var이 존재하지 않는 path를 가리키면 그대로 fall-through하여 SDK 후보를 사용함도 실제로 확인:

```
$ CROSSMTP_ADB=/nonexistent/path cargo run -q -p mtp-cli -- adb where
adb path:   /Users/dalli/Library/Android/sdk/platform-tools/adb
adb source: AndroidSdk
```

## 3. 의사결정 기록

1. **별도 crate (`adb-session`) vs `mtp-session` 내부 모듈**.
   별도 crate로 분리. 근거: §4.1이 capability를 분리해서 다루고, ADB는 libmtp dep과 빌드 그래프가 다르며, Phase 4 UI에서도 "MTP / ADB" 양쪽을 capability로 노출해야 한다. `mtp-session`을 ADB 코드로 부풀리면 macOS sdk path 의존성과 platform-tools 의존성이 한 crate에 섞여 빌드 시간이 늘어난다.

2. **`AdbRunner` trait + 의존성 주입**.
   §8 Phase 1의 "shell command 실행과 child process lifecycle을 테스트 가능하게 만든다"를 직접 만족시키는 가장 작은 추상. 실 production은 `CommandRunner`, 테스트는 `StubRunner`로 args 캡처 + 시나리오 응답 재생. 트레이트 없이 `std::process::Command`를 직접 호출했다면 위 22개 시나리오를 모두 실기기 의존 없이 검증할 수 없었다.

3. **serial을 유일한 안정 식별자로 사용, transport_id는 display-only**.
   Phase 0 retro §2.3에서 직접 관찰된 결과(케이블 재연결 시 transport_id 1 → 3)를 코드 수준에서 강제. `AdbDevice::transport_id`는 `Option<u32>`로 보존하지만 어떤 API도 이 값을 인자로 받지 않는다.

4. **`is_executable` 검사는 stat + mode 비트 0o111**.
   PATH lookup 시 디렉토리가 PATH에 들어 있어도 안전. windows 빌드 대상은 아니지만 cfg를 분리해 두어 향후 부분적으로 활성화 가능.

5. **`AdbProcess::terminate()`에 SIGTERM → grace → SIGKILL 순서 내장**.
   §6.1의 host-side 절차(steps 1-3)를 라이브러리 수준에서 캡슐화. device-side `tar` PID 정리(step 4)는 의도적으로 미포함 — 그건 Phase 2 orchestrator가 device 식별자와 dest path 컨텍스트를 들고 있을 때만 의미가 있고, 이 layer는 그 컨텍스트를 모른다.

6. **`AdbCapabilities::adb_tar_upload = false`**.
   Phase 1은 streaming 자체를 구현하지 않았으므로 UI에 거짓말하지 않는다. Phase 2가 끝나야 true로 승격. mtp-session에서 Phase 1이 `supports_progress_callback=false`로 시작했던 것과 동일한 정직성 원칙.

7. **`Arc<dyn AdbRunner>` (`Send + Sync`)**.
   Phase 2/3에서 orchestrator worker thread가 session을 들고 다닐 가능성이 큼. 단일 스레드 강제는 mtp-session처럼 호출 측이 정책으로 가져가는 게 맞고, runner 자체는 thread-safe하게 두는 게 단순함.

## 4. 반대론자 (Devil's Advocate) 코멘트

> "통과는 했지만 다음을 직시할 것."
>
> 1. **실기기 unauthorized/offline 미검증**. stub 기반 단위 테스트는 파서/분기를 잘 잡지만, 실제 unauthorized 상태에서 `adb`가 stdout 대신 stderr에 메시지를 쓰는 케이스(예: "device unauthorized. This adb server's $ADB_VENDOR_KEYS ..."), `adb devices -l`이 line을 어떻게 출력하는지는 vendor adb 버전마다 변형이 있을 수 있음. Phase 5 실기기 라운드에서 USB debugging 토글로 재현하고 추가 fixture로 보강 필요.
> 2. **`AdbProcess::terminate()`의 grace 후 stdout 읽기는 race를 가짐**. 자식이 SIGKILL된 뒤 `wait`를 두 번 호출하는 흐름이 들어가 있는데, 두 번째 호출은 OS에 따라 `ECHILD`로 빠질 수 있음. Phase 2에서 streaming child를 실제 돌릴 때 이 path를 다시 점검해야 함.
> 3. **`is_executable` 검사가 symlink target까지는 따라가지 않음**. `std::fs::metadata`는 symlink를 따라가므로 일반 케이스는 OK이지만, 깨진 symlink가 PATH에 있으면 silent skip된다. 의도된 동작이긴 하나 진단 로그가 없으므로 사용자 보고 시 추적 어려움.
> 4. **Phase 0 retro §3에서 plan.md 수정 6건 제안이 있었는데 그 patch가 아직 plan.md에 반영되지 않은 상태에서 Phase 1을 시작했음**. 본 retro는 plan.md를 변경하지 않고 구현으로 끝냈다. Phase 2 진입 전에 §3.1-§3.6 patch를 plan.md에 직접 반영하거나, retro 묶음을 plan의 정식 정오표로 명시할지 결정해야 한다.
> 5. **`AdbCapabilities`가 device-level이 아니라 layer-level**. 실제로는 device마다 tar 가용성/scoped storage 정책이 다를 수 있는데, 본 phase는 그걸 캡슐화하지 않는다. Phase 2에서 manifest probe + `adb shell which tar` 결과로 per-device cap을 채워야 한다.
> 6. **두 번째 검증 기기 미확보**. plan.md §8 Phase 0 통과 기준의 "최소 2종" 미충족 상태가 계속 누적. Phase 5 실기기 라운드 전에 별도 task로 분리해 추적해야 한다.

## 5. 알려진 제약 (Known Limitations)

- **단일 검증 기기**: Xiaomi `ea33d2fe` (Android 15, toybox 0.8.11) 1대. 다른 OEM/Android 버전의 `adb devices -l` 출력 변형 미검증.
- **Linux/Windows 빌드 미테스트**: cfg는 분리해 두었으나 macOS arm64에서만 실측.
- **Streaming child 미사용**: `AdbProcess` API는 정의되어 있으나 production code path에서 사용되지 않음. Phase 2에서 tar -x stream을 연결하면서 실측.
- **per-device capability 미구현**: tar 가용성, scoped storage 결과 등 device 의존 capability는 Phase 2 manifest probe 단계에서 채울 예정.
- **plan.md §3 patch 미반영**: Phase 0 retro에서 제안한 plan.md 6건 patch는 별도 task로 분리되어 있고, 본 retro는 그것을 전제로 구현만 진행.

## 6. Phase 2 인수 항목

1. **Tar Stream Builder 구현**: §4.3 명세 + Phase 0 retro의 hard-exclude (`._*`, `.DS_Store`, `.Spotlight-V100`, `.Trashes`, `.fseventsd`) 적용. `COPYFILE_DISABLE=1`와 무관하게 동작.
2. **AdbProcess streaming 실측**: `AdbSession::spawn(serial, &["shell", "tar", "-x", "-C", dest], "tar-x")` → stdin에 tar payload writing → 종료 + cleanup. Phase 0 §2.2의 device-side PID 추적/`pkill -f` 보조 정리 포함.
3. **per-device capability fill-in**: `which tar`, `find` 결과를 캐시하고 `adbTarUpload` capability를 device 단위로 토글.
4. **manifest probe 통합**: `find <root> -type f -exec stat -c '%n %s %Y' {} \;` 결과 파싱 + §5 충돌 정책 계산.
5. **plan.md §3 patch**: Phase 0 retro에서 제안한 6건을 plan.md에 정식 반영 (별도 PR 또는 본 phase 마무리 단계).
6. **반대론자 6건 흡수**: 위 §4의 1~6을 Phase 2/3/5 작업 항목으로 매핑.

## 7. 사용 방법

```bash
# 빌드 (단일 crate 빠른 빌드)
cargo build -p adb-session

# 자동 테스트
cargo test -p adb-session

# 전체 워크스페이스 빌드 (mtp-cli adb 서브커맨드 사용 시)
cargo build --workspace

# 실기기 확인
cargo run -q -p mtp-cli -- adb where
cargo run -q -p mtp-cli -- adb devices
cargo run -q -p mtp-cli -- adb probe
cargo run -q -p mtp-cli -- adb probe ea33d2fe
cargo run -q -p mtp-cli -- adb shell ea33d2fe -- getprop ro.product.model

# 다른 adb 경로 지정
CROSSMTP_ADB=/custom/path/adb cargo run -q -p mtp-cli -- adb where
```

## 8. 한 줄 요약

ADB Session Layer는 capability/error/lifecycle 세 축으로 정직하게 분리됐고, 자동 22건 + 실기기 1대로 통과했다. Phase 2 streaming은 이 layer 위에서 실측을 통해 진실을 채워 넣는다.
