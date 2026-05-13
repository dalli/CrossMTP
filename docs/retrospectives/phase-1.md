# Phase 1 회고 — Core MVP 백엔드 (Session Layer)

작성일: 2026-04-13
담당: Lead, Infra, Backend (Devil's Advocate 점검 포함)

## 목표 (계획서 기준)

* Rust 기반 Session Layer 작성
* 기기 연결/탐색/파일 read/write API
* 에러 모델 초안
* capability 모델

산출물: Tauri와 분리된 Rust 코어 모듈, 공통 에러 타입, capability 모델
통과 기준: CLI 환경에서 탐색·업로드·다운로드 재현 가능, 실패 케이스 로그 구분 가능

## 산출물

### 새 crate: `crates/mtp-session`
* `build.rs`: bindgen으로 `<libmtp.h>` 전체 바인딩 생성. `pkg-config`로 libmtp 위치 자동 감지, macOS는 `xcrun --show-sdk-path`로 sysroot 주입.
* `src/ffi.rs`: bindgen 출력물을 include하는 얇은 모듈. 모든 lint 차단 후 lib.rs에서 안전 래핑.
* `src/error.rs`: `MtpError` enum + `Result<T>` alias. 변형은 의도적으로 작음:
  `NoDevice`, `DeviceLocked`, `StorageUnavailable`, `Connection`, `TransferFailed`,
  `Cancelled`, `Device(String)`, `InvalidArgument(&'static str)`, `Io(io::Error)`.
  `is_likely_permission_issue()` 헬퍼는 UI가 "폰에서 허용 누르세요" 안내를 띄울 때 사용.
* `src/capability.rs`: `Capabilities` struct. macOS libmtp 기본값은 진실되게:
  `can_list/dl/ul=true`, `rename/delete/mkdir=false`, `progress/cancel=false`
  (Phase 2에서 progress/cancel을 켤 예정).
* `src/lib.rs`: 안전 래퍼.
  * `Session::open()` — `OnceLock`으로 `LIBMTP_Init`를 프로세스당 한 번만.
  * `Session::list_devices() -> Result<Vec<Device>>` — `LIBMTP_Detect_Raw_Devices`
    호출 후 raw_device_t 배열을 인덱싱하여 각각 `LIBMTP_Open_Raw_Device_Uncached`.
    open 실패한 슬롯은 조용히 skip (다중 기기 환경에서 한 기기가 잠겨도 나머지는 진행).
  * `Device` 핸들 — `Drop`에서 `LIBMTP_Release_Device`. `Send` 구현, `Sync`는 의도적으로 미구현
    (libmtp 핸들은 thread-unsafe). 단일 활성 워커 모델을 Phase 2가 강제할 것.
  * `Device::list_storages` — `LIBMTP_Get_Storage` 후 device->storage 연결리스트 walk.
  * `Device::list_entries(storage_id, parent_id)` — `LIBMTP_Get_Files_And_Folders`,
    각 노드를 안전 `Entry`로 복사 후 `LIBMTP_destroy_file_t`.
    `parent_id == PARENT_ROOT (0xFFFFFFFF)`이 storage 루트.
  * `Device::download_file(file_id, dest)` — `LIBMTP_Get_File_To_File`.
  * `Device::upload_file(src, storage_id, parent_id, name) -> Result<u32>` —
    `LIBMTP_Send_File_From_File`, 새 item id 반환.
  * `Device::take_error()` — libmtp의 per-device error stack을 drain하여
    `MtpError::Device(text)`로 정규화.

### 새 crate: `crates/mtp-cli`
재현 가능한 개발자 CLI. 모든 서브커맨드는 `mtp-session` 공개 API에만 의존.
* `devices` / `storages` / `ls <sid> [pid]` / `pull <sid> <fid> <dest>` / `push <sid> <pid> <src>`
* `verify` — **단일 프로세스에서** 전체 read+write 흐름 자동 실행.
  macOS는 매 프로세스마다 시스템 데몬(icdd 등)이 USB 인터페이스 재선점을 시도하므로
  여러 cargo run에 걸친 검증보다 단일 프로세스 verify가 훨씬 안정적이다.

### 보조
* `scripts/phase1-verify.sh`: 인터랙티브 manual harness (사용자가 storage/file id를 직접 입력).
  자동 verify 가능해진 후 사용 빈도는 낮아짐. 보존.

## 통과 기준 vs 실측

| 기준 | 결과 |
|---|---|
| Tauri와 분리된 Rust 코어 | ✅ `mtp-session`은 UI 의존성 0 |
| 공통 에러 타입 | ✅ `MtpError` 9개 변형 |
| capability 모델 | ✅ `Capabilities` struct + 정직한 macOS 기본값 |
| CLI에서 탐색 재현 가능 | ✅ `verify`로 storages, root, 중첩 폴더 listing 검증 |
| CLI에서 다운로드 재현 가능 | ✅ 33 B 파일 download → on-disk 크기 일치 |
| CLI에서 업로드 재현 가능 | ✅ round-trip upload, 새 item id 31 수신 |
| 실패 케이스 로그 구분 가능 | ✅ DeviceLocked / StorageUnavailable / Connection / Io / Device(text) 분리. UI hint 분기까지 구현 (`is_likely_permission_issue`) |

### 실기기 검증 로그 (요약)
```
=== verify: device list ===
[0] Xiaomi POCO F7 Pro — serial 4532E6CA74C12C0D9A07010D4E27490F

=== verify: storages on first device ===
storage 0x00010001  내부 공유 저장용량  free=118.6 GB max=224.5 GB

=== verify: list root of storage 0x00010001 ===
... 15 entries (Pictures, DCIM, MIUI, Android, Music, ...) ...

=== verify: list inside folder 'Pictures' (id 7) ===
... Screenshots, *.jpg, KakaoTalk, ... ...

=== verify: download id 15 ('statusbar_gestures.dat', 33 B) → /tmp/...
download OK: 33 B on disk

=== verify: round-trip upload ===
upload OK: new item id 31
```

## 의사결정 기록

1. **bindgen 채택**. Phase 0의 hand-written FFI는 6개 함수에서는 OK였지만 Phase 1은
   struct 필드 접근(`storage->next`, `file->filetype` 등)이 필요하고 layout을 손으로
   유지하는 건 fragile. libclang은 macOS Command Line Tools가 이미 제공하므로 추가
   의존성 0.
2. **`Send`만 구현, `Sync` 거부**. libmtp 핸들은 thread-unsafe. Phase 2 orchestrator는
   "single active transfer worker" 원칙(AGENTS.md)을 단일 소유자 모델로 강제할 것.
3. **에러 enum은 9개로 작게**. 더 잘게 쪼개면 UI 분기와 동기화 비용이 큼. 필요해지면
   추가하되, 추가할 때마다 회고에 사유 기록.
4. **capability 기본값을 정직하게 표시**. `supports_progress_callback=false`는
   "libmtp가 지원하지만 Session API surface가 아직 노출하지 않음" 의미. UI에
   거짓말하지 않기 위해 false로 유지. Phase 2에서 progress 파이프라인 추가 시 true로.
5. **`verify` 단일-프로세스 서브커맨드**. macOS USB 데몬 race 때문에 여러 cargo run에
   걸친 검증은 불안정. 한 번에 모든 안전 호출을 도는 게 실제 사용 패턴(orchestrator도
   단일 프로세스에서 device handle을 길게 들고 있음)과도 일치.

## 반대론자 (Devil's Advocate) 코멘트

> "통과는 했지만 다음 빈틈을 직시할 것."
>
> 1. **macOS USB 인터페이스 race**. icdd, AMPDeviceDiscoveryAgent, Android File Transfer가
>    돌고 있으면 `LIBMTP_Open_Raw_Device_Uncached`가 실패한다. 현재 회피책은
>    "사용자가 종료" 또는 "killall". MVP에서는 UI가 이걸 감지하고 친절한 가이드를
>    띄워야 한다. 단순한 `DeviceLocked` 메시지로는 부족 — 어느 프로세스가 잡고 있는지
>    안내할 수 있으면 좋다 (Phase 3 입력).
> 2. **Verify가 검증한 path는 thin path만**. 33 B 파일 1개, root 폴더 listing 1회.
>    100개 파일 / 1 GB / 한글 경로 / 깊이 5+ / 충돌 / 케이블 분리 / 화면 잠금 등은
>    Phase 2·4 책임이지만 **현재 코드에서 해당 path가 hang할 가능성이 있는지** 알 수 없다.
>    예: `LIBMTP_Get_Files_And_Folders`는 폴더가 매우 크면 메모리 폭증 가능 (전체
>    linked list를 한 번에 빌드하므로). Phase 2에서 streaming/페이징 고려.
> 3. **`take_error()`는 폴더 listing 빈 결과를 에러와 구분 못 한다**. 현재 정책은
>    "head==null이고 error stack 비어있으면 빈 폴더로 간주". 실제로는 transient
>    libmtp 오류가 stack 없이 null만 반환하는 케이스가 존재할 수 있음. Phase 2에서
>    folder count로 cross-check.
> 4. **업로드한 검증 파일이 폰에 그대로 남는다**. delete API가 없어서 cleanup 불가.
>    매 verify 실행마다 폰에 `crossmtp-verify-N.bin`이 누적된다. Phase 2에서
>    delete를 capability=false인 채로 노출하거나, verify가 동일 이름 재사용하도록 변경.
> 5. **`upload_file`은 `LIBMTP_filetype_t_LIBMTP_FILETYPE_UNKNOWN`로 설정**.
>    libmtp는 이걸 OK로 받지만 일부 기기 (오래된 Sony 등)는 type별 폴더로 자동 이동
>    시도. Xiaomi에서는 무사했지만 다른 기기에서 실패 가능.
> 6. **bindgen 빌드 시간 / 재현성**. libclang 버전이 달라지면 bindgen 출력이 흔들린다.
>    CI를 추가할 때는 libmtp/libclang 버전 핀 필요.

## 알려진 제약 (Known Limitations)

* **단일 기기**: code는 N개 기기를 enumerate 가능하지만 e2e는 1개 기기만 검증 (POCO F7 Pro).
* **단일 플랫폼**: macOS arm64 + Homebrew libmtp 1.1.23만 검증.
* **No progress / no cancel**: Phase 1 API surface에 미포함. Phase 2 추가.
* **No conflict handling**: 같은 이름 push 시 device-side 동작 미정의. Phase 2 추가.
* **No streaming list**: 큰 폴더는 메모리 폭증 가능.
* **macOS daemon race 회피책 부재**: killall은 사용자/dev 책임. MVP UI에 가이드 필요.
* **검증 파일 cleanup 불가**: delete API 없음.
* **권한 오류 텍스트 빈약**: "다른 프로세스가 USB 잡고 있음"을 USB 레벨에서
  특정할 방법 부재 (libusb 한계).

## Phase 2 인수 항목

1. **단일 active worker 모델**: device handle 1개를 worker thread가 소유. 모든 transfer는
   채널 기반. UI/CLI는 `JobId`로 commands를 보낸다.
2. **상태 머신**: queued / validating / transferring / cancelling / completed / failed / cancelled.
   상태 전이는 단방향, 이벤트 스트림으로만 노출.
3. **진행률 파이프라인**: `LIBMTP_Send_File_From_File_Descriptor` +
   `LIBMTP_Get_File_To_File_Descriptor` 사용해 callback에서 sent/total을 채널로 emit.
   `Capabilities::supports_progress_callback`을 true로 승격.
4. **취소**: 콜백에서 0이 아닌 값을 리턴하면 libmtp가 transfer 중단. 채널 기반 cancel
   토큰을 콜백에 연결. `Capabilities::supports_cancel`을 true로.
5. **충돌 정책**: skip / overwrite / rename 옵션. rename은 "name (1).ext" 패턴.
6. **검증 파일 누적 문제 해결**: verify가 deterministic name 재사용 + 회고에 누적 항목 남기기.
7. **반대론자 6건 흡수**: 위 코멘트 1~6 모두 Phase 2 또는 Phase 3 design에 매핑.
8. **`mtp-session::Device::list_entries` page-friendly 변형**: 큰 폴더 보호.

## 사용 방법

```bash
# 빌드
cargo build

# 자동 e2e (단일 프로세스 — 권장)
cargo run -p mtp-cli -- verify

# 개별 호출
cargo run -p mtp-cli -- devices
cargo run -p mtp-cli -- storages
cargo run -p mtp-cli -- ls 0x00010001
cargo run -p mtp-cli -- ls 0x00010001 7
cargo run -p mtp-cli -- pull 0x00010001 15 /tmp/out.bin
cargo run -p mtp-cli -- push 0x00010001 0xFFFFFFFF /tmp/local.txt
```

macOS daemon이 USB 인터페이스를 잡고 있으면:
```bash
killall "Android File Transfer" "Android File Transfer Agent" icdd 2>/dev/null
```
