# Phase 0 회고 — 기술 검증

작성일: 2026-04-13
담당: Lead, Infra, Backend (Frontend/QA는 후속 phase부터)

## 목표 (계획서 기준)

* macOS에서 `libmtp` 기반 기기 인식 확인
* 파일 목록 조회
* 단일 파일 업로드/다운로드 성공 여부 확인

산출물: CLI 프로토타입, 테스트 기기 목록, 확인된 제약사항 목록.

통과 기준: 최소 2개 이상 Android 기기에서 연결 및 단일 파일 전송 성공.

## 실제로 한 일

* `brew install libmtp` (1.1.23, arm64) — 시스템 의존성 도입
* Cargo 워크스페이스 부트스트랩 (`Cargo.toml`, `crates/`)
* `crates/mtp-probe`: hand-written FFI 바인딩으로 Phase 0 CLI 작성
  * `LIBMTP_Init`, `LIBMTP_Set_Debug`, `LIBMTP_Detect_Raw_Devices`,
    `LIBMTP_Open_Raw_Device_Uncached`, `LIBMTP_Release_Device`,
    `LIBMTP_Get_{Friendly,Manufacturer,Model,Serial}name` 만 노출
* `build.rs`에서 `pkg-config`로 linker flag 자동 주입
* `.cargo/config.toml`에 `PKG_CONFIG_PATH=/opt/homebrew/lib/pkgconfig` 고정
* `cargo run -p mtp-probe` 정상 실행 확인

실행 결과:

```
[ok] LIBMTP_Init returned
[info] no MTP device attached
```

## 통과 기준 대비 평가

| 항목 | 상태 |
|---|---|
| libmtp가 macOS arm64에서 로드 | ✅ 검증됨 |
| Rust ↔ libmtp linker 경로 | ✅ 검증됨 |
| `LIBMTP_Init` 호출 후 라이브러리가 panic 없이 동작 | ✅ 검증됨 |
| 기기 enumerate 코드 경로가 도달 가능 | ✅ 검증됨 (no-device 분기 정상 처리) |
| 실제 Android 기기 인식 | ❌ **미검증** — 본 환경에 USB로 연결된 MTP 기기 없음 |
| 폴더 목록 조회 | ❌ 미구현 (Phase 1로 이월) |
| 단일 파일 read | ❌ 미구현 (Phase 1로 이월) |
| 2개 이상 기기 검증 | ❌ 미검증 |

**결론**: Phase 0의 *기술 검증* 부분(linkage, init, codepath reachability)은 통과.
*실기기 검증*은 본 환경 제약으로 차단됨. AGENTS.md 규칙대로 솔직히 표기.

## 의사결정 기록

1. **bindgen 대신 hand-written FFI 채택** — Phase 0는 6개 함수만 필요. bindgen 도입은 빌드 의존성(libclang)·컴파일 시간 비용을 늘림. Phase 1에서 Session crate 작업 시 bindgen 재검토 예정.
2. **raw_device 배열 walk을 포기** — 첫 device만 open. C struct layout을 손으로 가정하면 fragile. Phase 1에서 bindgen이 들어오면 정상 iteration 구현.
3. **`.cargo/config.toml`에 PKG_CONFIG_PATH 영속화** — 개발자가 매번 환경변수 export 하지 않게. arm64 Homebrew 경로 가정.

## 반대론자 (Devil's Advocate) 코멘트

> "통과했다고 하지만 실은 아무 기기도 안 붙여봤잖아. 이건 'libmtp 라이브러리가 dylib으로 존재함'을 확인한 수준이고, **Phase 0의 본질적 리스크는 검증되지 않았다.** 다음 단계로 넘어가는 건 빚을 지는 행위다. 최소한 다음을 명시해라:"
>
> * macOS에서 libmtp는 USB 권한 모델이 다르다 (kIOMainPortDefault, IOKit). 실제 디바이스 attach 시 권한 거부가 일어날 수 있고 그건 brew 설치만으로는 검증 불가다.
> * Android 13+ 기기는 MTP 활성화가 매번 사용자 탭을 요구한다. Phase 0 코드는 이 상태를 구분 못 함.
> * 첫 device만 보는 코드는 다중 기기 환경에서 *조용히 잘못된 기기를* 잡을 수 있다. 이 잠재 버그가 그대로 Phase 1에 옮겨가지 않게 TODO 박아둘 것.
> * `LIBMTP_Get_*name` getter들이 caller-free 의무를 안 지키면 leak. 현재는 `take_owned_cstring` 헬퍼로 처리했으나, libmtp 버전마다 ownership 규칙 다른 경우 있음 — 1.1.23 문서 한 번 더 확인 권장.

위 코멘트는 Phase 1 입력으로 가져간다.

## 알려진 제약 (Known Limitations)

* macOS 외 platform 미지원
* arm64 Homebrew 경로 하드코딩 (Intel mac, system path 다른 환경 미고려)
* 단일 device만 enumerate
* 폴더 탐색·전송 미구현
* 실기기 검증 ZERO

## Phase 1 인수 항목

* **B**indgen 도입 검토 → libmtp 헤더 전체 바인딩 → Session crate에 분리
* `LIBMTP_Get_Storage`, `LIBMTP_Get_Files_And_Folders`, `LIBMTP_Get_File_To_File_Descriptor` 노출
* error code → Rust enum 정규화 (`MtpError`)
* device enumeration 정상화 (linked list / array walk)
* capability struct 초안: `can_rename`, `can_delete`, `supports_background_reconnect` 등
* 반대론자 지적 4건 모두 코드 또는 문서로 흡수

## 사용 방법 (개발자용)

```bash
brew install libmtp        # 1.1.23 이상
cargo run -p mtp-probe     # PKG_CONFIG_PATH는 .cargo/config.toml로 자동 주입
```
