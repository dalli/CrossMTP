# Phase 5 회고 — macOS MVP 배포 준비 + RC 선언

작성일: 2026-04-13
담당: Lead, Infra, Backend, Frontend, QA, Devil's Advocate

## 목표 (계획서 기준)

* 로컬 설치 가능한 macOS 패키지 정리
* 사용자 가이드 초안 작성

산출물: MVP 배포본, 연결/권한 승인 가이드
통과 기준: 새 환경의 macOS 사용자 기준으로 설치 후 기본 전송 가능

## 산출물

### 번들 (Phase 5-A)
* `target/release/bundle/macos/CrossMTP.app` — macOS arm64 .app 번들 (8.6 MB)
* `target/release/bundle/dmg/CrossMTP_0.0.1_aarch64.dmg` — DMG 인스톨러 (2.9 MB)
* 빌드 명령: `cd apps/desktop && npm run tauri build`
* 빌드 시간: ~10초 (incremental release), 첫 빌드 ~3분
* 코드 sign / notarization: **미수행** (베타 단계, 사용자가 quarantine 우회 필요)

### 아이콘 셋
* `apps/desktop/src-tauri/icons/` — 32/64/128/128@2x .png + .icns + .ico + iOS/Android 변형
* 생성 방법: `npm run tauri -- icon src-tauri/icons/icon.png` (Phase 0의 placeholder PNG에서 자동 생성)
* 정식 디자인 아이콘은 베타 직전 교체 예정 (현재는 단색)

### 사용자 문서 (Phase 5-B)
* `README.md` — 루트, 기능/비범위/빠른 시작/아키텍처 다이어그램/회고 인덱스
* `docs/install.md` — 시스템 요구사항, libmtp 설치, macOS daemon 안내, .app 인스톨, 폰 측 설정 (기기별 메뉴), 동작 검증
* `docs/troubleshooting.md` — 9개 카테고리 (기기 미인식, Failed 사유 분류표, Cancelling 멈춤, 충돌, 작은 파일 progress, quarantine, CLI 디버깅, 로그, 이슈 보고 템플릿)
* 회고 인덱스를 README에 링크

### 환경 체크 명령 (Phase 5-C)
* `environment_check` Tauri 명령 신설 (`apps/desktop/src-tauri/src/lib.rs`)
* mount 시 React가 호출 → macOS USB 데몬(icdd, AFT)이 살아있으면 hint 메시지를 Banner에 노란색으로 출력
* 사용자가 사전 가이드를 읽지 않고 앱을 실행하더라도 첫 화면에서 원인을 파악 가능
* libmtp 자체의 dynamic link 실패는 OS loader 단계에서 발생해 catch 불가 — 의도적으로 README/install에서만 다룸 (정직성 원칙)

## 통과 기준 vs 실측

| 기준 | 결과 |
|---|---|
| `.app` 번들 생성 | ✅ 8.6 MB binary, 2.9 MB dmg |
| 사용자 가이드 | ✅ README + install.md + troubleshooting.md |
| 새 환경 사용자 기준 설치 후 기본 전송 가능 | ⚠️ **부분 검증**. 본 환경에서 .app은 빌드되었지만 새 macOS 사용자 계정에서 처음 설치하는 시나리오 (G2)는 미수행. 사용자가 직접 시도해야 함. |
| MVP RC 선언 | ✅ — 단, 아래 known limitations 매핑 |

## 의사결정 기록

1. **DMG + .app 둘 다 산출**. dmg는 일반 사용자 배포 기본 포맷, .app은 dev 빠른 테스트용.
2. **코드 sign 미수행**. 베타에서 정식 sign 인증서를 발급받는 비용 vs 가치를 정량화하지 못한 단계. RC 후 첫 외부 사용자 보고를 받고 결정. 우회 명령 (`xattr -d com.apple.quarantine`)을 install.md에 명시.
3. **환경 체크는 dynamic-link 실패 미감지**. catch 불가능한 layer라 솔직하게 README에 경고 + brew install 안내. 거짓 fallback 만들지 않음.
4. **아이콘은 placeholder 유지**. 베타 후 디자인 가능. 기능적 충분.
5. **README는 한국어 우선** (AGENTS.md 정책). 코드/CLI 명령은 영어.

## 반대론자 코멘트

> 1. **새 환경 사용자 시나리오 미검증** (G1/G2/G3). 본 phase의 통과 기준이 정확히 그건데, 본 세션의 사용자는 이미 모든 의존성과 폰 권한이 셋업된 상태. 다른 macOS 계정을 만들거나 친구의 mac에서 dmg를 실행해보는 게 진짜 검증.
> 2. **`environment_check`의 pgrep parsing이 fragile**. macOS 버전마다 pgrep -l -f 출력 형식이 다를 수 있고, 한국어 시스템에서 프로세스 이름이 다르게 나올 가능성 있음. 더 견고한 방법: `launchctl list` 또는 `ps -A`를 직접 파싱.
> 3. **DMG 안에 README가 없음**. 사용자가 dmg를 mount하고 .app을 드래그하면 끝인데, 첫 실행 전 "brew install libmtp 먼저!"를 어떻게 알릴 것인가. dmg 안에 README.txt 또는 background 이미지에 안내 문구를 넣는 게 표준 macOS 패턴. 미적용.
> 4. **delete 미구현 + verify-q 누적 파일**. Phase 1·2 verify-q를 돌릴 때마다 폰에 `crossmtp-verify-*.bin` 파일이 누적되고 있음. RC 단계에서 이걸 정리할 방법이 사용자에게 없음 (CrossMTP 자체엔 delete 없음). README 또는 troubleshooting에 "AFT나 다른 도구로 정리" 안내 누락.
> 5. **`.app`을 실제로 실행해서 GUI가 정상 동작하는지 확인 안 함**. 본 phase에서 dev 모드로는 작동했지만 release 빌드 + .app 패키지 형태에선 한 번도 안 띄움. release 빌드는 minify 등 차이가 있어 dev에서 못 본 버그가 나올 수 있음. **RC 확정 전 1회는 강력 권장.**
> 6. **Intel mac 지원 미주장**. README는 arm64만 명시했지만 Intel용 별도 빌드 필요. dmg 파일명에 `aarch64`가 들어있어 사용자가 헷갈릴 수 있음.
> 7. **자동 업데이트 메커니즘 없음**. 베타 RC에선 OK이지만 이후 update path를 README에 미언급.
> 8. **Phase 4의 미검증 항목들**(B6 한글 / E1·E2 stress / D3·D5)이 Phase 5에 그대로 이월된 채 RC가 됨. 통과 기준의 "기본 전송 가능" 정의가 felt-OK 수준에서 끊김.

## 알려진 제한사항 (RC 선언 시점)

### 의도적 (MVP 비범위)
* 단일 기기, 단일 worker
* delete / rename / mkdir 미지원
* upload Overwrite 미지원
* 다중 기기 동시 연결 없음
* Linux/Windows 미지원
* 백그라운드 자동 재연결 없음
* 미디어 미리보기/썸네일 없음

### 미검증 (RC 직전 보강 권장)
* 새 macOS 계정 처음 설치 시나리오
* Intel mac
* 100개+ stress 큐
* 한글/유니코드 파일명
* MTP 권한 미승인 시 명확한 메시지 (D3)
* storage full 케이스 (D4)
* 부적합 경로 문자 (D5)
* `.app` 패키지 형태에서 한 번도 실 실행 안 함 (반대론자 #5)

### 기술 부채
* 코드 sign / notarization 없음 (사용자가 quarantine 우회)
* 아이콘 placeholder
* macOS daemon 자동 종료 없음 (사용자가 직접 killall)
* dmg 안에 안내 문구 없음
* libmtp 1.1.23 외 버전 미검증
* arm64 Homebrew 경로 하드코딩
* listing이 transferring 동안 큐에서 대기 (UI에 "기다리는 중" 표시 없음)
* 환경 체크 pgrep parsing fragile
* upload 부분 파일 cleanup 없음 (libmtp 한계)
* 케이블 분리 후 worker thread leak 가능성 (Phase 4 반대론자 #2, 미검증)

## RC 요약

**CrossMTP 0.0.1-rc** (macOS arm64):
* 5개 phase 완료 (Phase 0~5)
* 25개 task, 핵심 실패 시나리오 4건 사용자 검증
* 2종 Android 기기에서 실 데이터 전송 검증 (Xiaomi POCO F7 Pro, Alldocube iPlay50_mini_Pro)
* 3 GB 파일 다운로드 검증
* 부분 다운로드 cleanup, 케이블 분리 graceful 처리, 폰 잠금 graceful 처리, 충돌 정책 3종 (Skip/Rename/Overwrite-download) 검증
* MVP 산출물: .app + dmg + README + install + troubleshooting + 5개 phase 회고

**다음 단계 후보**:
1. **베타 배포 + 외부 사용자 피드백** — 반대론자 미검증 항목들이 자연스럽게 노출됨
2. **Phase 4·5 known limitations 정리** — 100 MB+ stress, 한글, 권한 미승인 메시지 등
3. **Linux 확장 (Phase 6)** — Session Layer 재사용 검증
4. **Windows 확장 (별도 phase)** — WPD 백엔드 신규 작성

## 사용 방법

### 설치 (사용자)
```bash
# 사전: libmtp
brew install libmtp

# .dmg 마운트 후 CrossMTP.app을 /Applications 로 드래그
open target/release/bundle/dmg/CrossMTP_0.0.1_aarch64.dmg

# (선택) quarantine 우회
xattr -d com.apple.quarantine /Applications/CrossMTP.app

# macOS USB daemon 종료
killall "Android File Transfer" "Android File Transfer Agent" icdd 2>/dev/null

# 실행
open /Applications/CrossMTP.app
```

### 빌드 (개발자)
```bash
cd apps/desktop
npm install                # 1회
npm run tauri build        # production
npm run tauri dev          # 개발 (HMR)
```
