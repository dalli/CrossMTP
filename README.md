# CrossMTP

macOS용 Android MTP 파일 전송 데스크톱 앱.
**상태**: macOS MVP 안정화 중.

CrossMTP는 macOS에서 Android 기기와의 파일 전송을 *안정적으로* 수행하는 것을
유일한 목적으로 합니다. 화려한 파일 관리자 기능보다 **끊겨도 망가지지 않는 전송**과
**원인을 알 수 있는 실패 메시지**를 우선합니다.

> 자세한 제품 방향과 비범위는 [`docs/cross-mtp-dev-plan.md`](docs/cross-mtp-dev-plan.md)
> 와 [`AGENTS.md`](AGENTS.md) 참고.

## 무엇이 가능한가 (MVP)

* Android 기기 자동 감지 + 연결 상태 표시
* 좌측 로컬 파일 탐색, 우측 Android 저장소 탐색
* 패널 간 드래그 앤 드롭 전송
  * PC → Android: 현재 Android 폴더로 업로드
  * Android → PC: 현재 로컬 폴더로 다운로드
  * Finder → Android 패널: 현재 Android 폴더로 업로드
* **파일 다운로드** (단일 active worker, 진행률, 취소, 부분 파일 정리)
* **파일/폴더 업로드** (재귀 업로드 + 진행률 + 취소). 폴더 업로드 시 구조를 유지하고 같은 이름 폴더는 merge.
* 큐 패널
  * 디렉토리 전송은 `디렉토리명 (N개 파일)`로 compact 표시
  * 헤더에 남은 파일 수 / 전체 파일 수 표시
* 충돌 정책: `Skip` / `Rename` / `Overwrite` (다운로드 한정)
* 같은 이름 파일이 이미 있을 때 크기와 수정 시간을 비교하여 같은 파일이면 자동 skip
* 기기 분리·화면 잠금·MTP 권한 미승인 등 실패 시나리오 graceful 처리

## 무엇이 안 되는가 (의도적 비범위)

* Finder 마운트 / FUSE / macFUSE 연동
* 이름 변경 / 삭제 / 파일 관리자 UI의 "새 폴더" 버튼 (단, 재귀 폴더 업로드는 내부적으로 폴더 생성을 사용)
* 미디어 미리보기 / 썸네일
* 다중 기기 동시 연결
* 백그라운드 자동 재연결
* Linux / Windows 지원 (각각 후속 phase)
* 코드 sign / notarization (베타 단계는 unsigned 배포)

## 사용법

1. 앱을 실행하고 Android 기기가 인식될 때까지 기다립니다.
2. 좌측에서 로컬 대상 폴더를 열고, 우측에서 Android 대상 폴더를 엽니다.
3. 파일 또는 폴더를 반대편 패널로 드래그해서 놓습니다.
4. 전송 큐에서 진행률, 완료, 실패, skip 상태를 확인합니다.

동일 파일 자동 skip 기준:

* 같은 이름의 파일이 이미 있어야 합니다.
* 크기가 같아야 합니다.
* 양쪽 수정 시간이 있으면 2초 이내 차이까지 같은 파일로 봅니다.
* Android MTP provider가 수정 시간을 제공하지 않으면 크기 일치만으로 같은 파일로 봅니다.

## 빠른 시작

### 사전 준비

```bash
# 1. libmtp 설치 (필수)
brew install libmtp

# 2. 사용 중인 macOS USB 데몬 종료 (CrossMTP가 폰을 잡으려면 필요)
killall "Android File Transfer" "Android File Transfer Agent" icdd 2>/dev/null
```

> macOS는 Android 폰이 USB로 연결되면 Image Capture / Android File Transfer가
> 자동으로 USB 인터페이스를 선점합니다. 이 데몬들이 살아있으면 CrossMTP가
> 폰을 열지 못합니다. **CrossMTP를 쓰는 동안에는 위 앱들을 닫아두세요.**

### 폰 측 준비

1. USB 케이블로 폰을 연결 (충전 전용 케이블 X)
2. 폰 잠금 해제
3. USB 알림 → **"파일 전송"** 또는 **MTP** 선택
4. 처음이라면 폰에서 PC를 신뢰해도 되는지 묻는 다이얼로그가 뜸 → 허용

### 앱 실행 (개발 모드)

```bash
cd apps/desktop
npm install        # 처음 1회
npm run tauri dev
```

### 앱 실행 (번들)

```bash
# 1. 번들 빌드
cd apps/desktop
npm run tauri build

# 2. .app 실행
open ../../target/release/bundle/macos/CrossMTP.app
```

개발 중 로컬 설치:

```bash
rm -rf /Applications/CrossMTP.app
ditto ../../target/release/bundle/macos/CrossMTP.app /Applications/CrossMTP.app
open /Applications/CrossMTP.app
```

### CLI로 검증 (개발자용)

```bash
# orchestrator 전체 시나리오 자동 실행
cargo run -p mtp-cli -- verify-q

# 개별 호출
cargo run -p mtp-cli -- devices
cargo run -p mtp-cli -- storages
cargo run -p mtp-cli -- ls 0x00010001
```

## 아키텍처 한눈에

```
┌─────────────────────────────────────────┐
│  React UI (apps/desktop/src/)           │
│  Banner · LocalBrowser · Browser        │
│  QueuePanel · custom drag/drop          │
└────────────┬────────────────────────────┘
             │ tauri::invoke / listen
┌────────────▼────────────────────────────┐
│  Tauri Shell (apps/desktop/src-tauri/)  │
│  Commands · Event pump                  │
└────────────┬────────────────────────────┘
             │ enqueue / cancel / list_*
┌────────────▼────────────────────────────┐
│  Orchestrator (crates/orchestrator/)    │
│  Single-active worker · State machine   │
│  Conflict · identical-file skip         │
│  Cancel · Progress                      │
└────────────┬────────────────────────────┘
             │ owns one Device handle
┌────────────▼────────────────────────────┐
│  Session Layer (crates/mtp-session/)    │
│  Safe wrapper · Capabilities · Errors   │
└────────────┬────────────────────────────┘
             │ bindgen FFI
┌────────────▼────────────────────────────┐
│  libmtp 1.1.23 (system, via brew)       │
└─────────────────────────────────────────┘
```

핵심 원칙:
1. **단일 활성 worker** — 한 process가 한 device handle만 소유
2. **명시적 상태 머신** — Queued/Validating/Transferring/Cancelling/Completed/Failed/Cancelled/Skipped
3. **capability-honest** — 백엔드가 진짜 지원하는 것만 UI에 노출
4. **실패 시나리오 우선** — 정상보다 장애 처리에 코드 비중

## 문제 해결

* [`docs/troubleshooting.md`](docs/troubleshooting.md)
* [`docs/install.md`](docs/install.md)
* [`docs/test-checklist.md`](docs/test-checklist.md)

## 회고 / 개발 기록

* [Phase 0](docs/retrospectives/phase-0.md) — libmtp 기술 검증
* [Phase 1](docs/retrospectives/phase-1.md) — Session Layer
* [Phase 2](docs/retrospectives/phase-2.md) — Transfer Orchestrator
* [Phase 3](docs/retrospectives/phase-3.md) — macOS UI (Tauri + React)
* [Phase 4](docs/retrospectives/phase-4.md) — 통합 안정화
* [Phase 5](docs/retrospectives/phase-5.md) — 배포 준비

## 라이선스 / 의존성

* libmtp (LGPL-2.1) — 시스템 동적 링크
* Tauri 2 / React 18 / Vite 5
* Rust 1.75+
