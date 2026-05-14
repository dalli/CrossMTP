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
* **🚀 ADB + Tar 고속 업로드 자동 라우팅 (v0.1.0+)** — USB debugging이 켜진 Android 기기에서는
  폴더 drag-drop이 자동으로 ADB tar 스트리밍 경로를 사용해 **MTP 대비 수 배 이상 빠르게** 전송됩니다.
  사용자는 평소처럼 폴더를 끌어다 놓기만 하면 됩니다. 자세한 내용은 [고속 업로드](#-adb--tar-고속-업로드-v010) 참고.
* 큐 패널
  * 디렉토리 전송은 `디렉토리명 (N개 파일)`로 compact 표시
  * ADB 경로로 간 전송은 `[ADB] 폴더명`으로 표시되어 어느 백엔드를 썼는지 사후 확인 가능
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

> 💡 **폴더를 끌어다 놓을 때**: USB debugging이 켜져 있으면 자동으로 ADB 고속 업로드가 사용됩니다.
> 큐에 `[ADB] 폴더명`으로 표시되면 고속 경로로 전송 중인 것입니다.
> 자세한 조건과 사용 가능 여부는 아래 섹션을 참고하세요.

동일 파일 자동 skip 기준:

* 같은 이름의 파일이 이미 있어야 합니다.
* 크기가 같아야 합니다.
* 양쪽 수정 시간이 있으면 2초 이내 차이까지 같은 파일로 봅니다.
* Android MTP provider가 수정 시간을 제공하지 않으면 크기 일치만으로 같은 파일로 봅니다.

## 🚀 ADB + Tar 고속 업로드 (v0.1.0+)

작은 파일이 수천 개 들어있는 폴더(사진 라이브러리, 문서 백업, 프로젝트 폴더…)를 MTP로 올려본 적이 있다면
파일마다 handshake가 반복되며 느려지는 경험을 해보셨을 겁니다. CrossMTP는 USB debugging이 켜진 기기에서
**`adb shell tar -x -C <dest>` stdin 스트리밍**으로 이 병목을 우회합니다. 임시 tar 파일을 만들지 않고
on-the-fly로 스트림을 올리기 때문에 디스크 비용 없이도 사실상 USB 속도에 가까운 처리량을 냅니다.

### 사용자가 해야 할 것

> **사용 흐름은 평소와 똑같습니다.** 폴더를 MTP UI로 끌어다 놓기만 하면 알아서 빨라집니다.

다만 한 번만 다음을 준비해 두면 됩니다:

1. 폰: **개발자 옵션 → USB 디버깅** ON
2. 폰: USB 케이블로 Mac에 연결
3. 폰: "USB debugging 허용" 다이얼로그가 뜨면 **허용** ("이 컴퓨터에서 항상 허용" 체크 권장)
4. Mac: **Android platform-tools** 설치 (보통 Android Studio 설치 시 같이 설치됨)

   ```bash
   # 또는 brew로 설치
   brew install --cask android-platform-tools
   ```

준비가 되면 앱 좌측 하단의 `ADB 고속 업로드` 패널에서 `상태 보기`를 눌러
"ADB 고속 업로드 사용 가능 — 폴더를 끌어다 놓으면 자동으로 적용됩니다." 라는 초록 배너를 확인할 수 있습니다.

### 자동 라우팅 조건 (이 모두 만족 시 ADB 사용)

| 조건 | 만족 시 동작 |
| --- | --- |
| 끌어 놓은 항목이 **폴더** | ✅ ADB 후보 — 단일 파일은 항상 MTP |
| 현재 선택된 storage가 **internal storage** | ✅ ADB 후보 — SD카드/OTG는 MTP fallback |
| ADB device가 **정확히 1개** + tar smoke OK | ✅ ADB 사용 — 여러 기기 연결 시 모호함 회피 |
| `/sdcard` 매핑 성공 | ✅ — `adb shell ls /sdcard` 검증 |

위 중 하나라도 불만족이면 **조용히 MTP로 fallback**합니다 (실패가 아닙니다).

### 충돌이 있을 때

자동 라우팅은 전송 시작 전에 device-side `find ... stat`로 manifest를 받아 충돌을 미리 계산합니다.
충돌이 1건이라도 있으면 다음과 같은 확인 dialog가 뜹니다:

```
'폴더명' 폴더를 고속(ADB) 업로드합니다.

• 새 파일: 1234
• 동일 파일 건너뜀: 56
• 이름 변경 후 업로드: 7

진행하시겠어요? (아니오를 선택하면 기존 MTP로 전송)
```

* **예**: ADB tar 스트리밍으로 진행 (skip/rename은 manifest 결정대로 일괄 적용)
* **아니오**: 기존 MTP 경로로 전송 (파일 단위 충돌 처리 동작)

### 알려진 제한

* **단일 파일 업로드는 ADB로 자동 라우팅하지 않습니다.** 작은 파일 묶음에서는 큰 이득이 있지만
  단일 대용량 파일은 MTP와 차이가 작거나 더 느릴 수 있어 보수적으로 제외했습니다 (plan.md §11).
* **SD카드/OTG storage는 MTP만 사용합니다.** storage 경로 매핑이 OEM마다 가변적이라 자동 매핑은
  post-MVP로 미뤘습니다.
* **Android 11+ scoped storage 정책으로 일부 폴더는 `adb shell` 권한으로 쓸 수 없습니다.**
  실패하면 명확한 에러로 표시되며, 같은 폴더를 MTP로 시도해 보면 동작할 수 있습니다.
* USB debugging은 보안에 민감한 권한입니다. **공용 PC에서는 사용을 권하지 않습니다.**

자세한 설계와 검증 결과는 [`docs/plan.md`](docs/plan.md), [`docs/retrospectives/`](docs/retrospectives/) 참고.

## 빠른 시작

### 사전 준비

```bash
# 1. libmtp 설치 (필수)
brew install libmtp

# 2. (선택) ADB 고속 업로드를 쓰려면 Android platform-tools 설치
brew install --cask android-platform-tools

# 3. 사용 중인 macOS USB 데몬 종료 (CrossMTP가 폰을 잡으려면 필요)
killall "Android File Transfer" "Android File Transfer Agent" icdd 2>/dev/null
```

> macOS는 Android 폰이 USB로 연결되면 Image Capture / Android File Transfer가
> 자동으로 USB 인터페이스를 선점합니다. 이 데몬들이 살아있으면 CrossMTP가
> 폰을 열지 못합니다. **CrossMTP를 쓰는 동안에는 위 앱들을 닫아두세요.**
>
> **ADB 고속 업로드 옵션**: platform-tools 없이도 MTP 경로로 모든 기능이 동작합니다.
> 폴더 업로드 속도를 끌어올리고 싶을 때만 platform-tools + USB debugging을 켜면 됩니다.

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
│  Session Layer (crates/mtp-session/)    │   ┌─────────────────────────────┐
│  Safe wrapper · Capabilities · Errors   │   │  ADB Session                │
│                                         │   │  (crates/adb-session/)      │
│                                         │   │  · device probe / smoke     │
│                                         │   │  · tar -x stdin streaming   │
│                                         │   │  · manifest probe           │
│                                         │   │  · conflict planner         │
└────────────┬────────────────────────────┘   └─────────────┬───────────────┘
             │ bindgen FFI                                   │ adb subprocess
┌────────────▼────────────────────────────┐   ┌─────────────▼───────────────┐
│  libmtp 1.1.23 (system, via brew)       │   │  platform-tools `adb`       │
└─────────────────────────────────────────┘   └─────────────────────────────┘
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

MTP 기반 MVP:

* [Phase 0](docs/retrospectives/phase-0.md) — libmtp 기술 검증
* [Phase 1](docs/retrospectives/phase-1.md) — Session Layer
* [Phase 2](docs/retrospectives/phase-2.md) — Transfer Orchestrator
* [Phase 3](docs/retrospectives/phase-3.md) — macOS UI (Tauri + React)
* [Phase 4](docs/retrospectives/phase-4.md) — 통합 안정화
* [Phase 5](docs/retrospectives/phase-5.md) — 배포 준비

ADB + Tar 고속 업로드 (v0.1.0):

* [ADB Phase 0](docs/retrospectives/adb-phase-0.md) — `adb shell tar -x` 기술 검증
* [ADB Phase 3](docs/retrospectives/adb-phase-3.md) — Orchestrator 통합 + smoke + planner
* [ADB Phase 4](docs/retrospectives/adb-phase-4.md) — UI opt-in + byte-level progress + plan registry
* 설계: [`docs/plan.md`](docs/plan.md)

## 라이선스 / 의존성

* libmtp (LGPL-2.1) — 시스템 동적 링크
* Tauri 2 / React 18 / Vite 5
* Rust 1.75+
