# ADB Phase 0 회고 — 기술 검증

작성일: 2026-05-14
대상 계획: [docs/plan.md](../plan.md) §8 Phase 0
스크립트: [scripts/adb-phase0-probe.sh](../../scripts/adb-phase0-probe.sh), [scripts/adb-phase0-throughput.sh](../../scripts/adb-phase0-throughput.sh)
원시 결과: `scripts/.adb-phase0/<serial>/`

## 0. 진행 방식

§8 Phase 0의 검증 대상은 두 종류로 나뉜다.

- **트랙 A (호스트 단독)** — adb 배포 전략, 코드 서명 영향, 검증 스크립트화, 결정 게이트 확정.
- **트랙 B (실기기 필요)** — tar 호환성, shared storage 쓰기 매트릭스, manifest probe 정확도, throughput, 취소/케이블 분리.

본 회고는 트랙 A를 완료하고, 트랙 B를 **검증 기기 1대**(Xiaomi `24117RK2CG`, Android 15 / SDK 35, toybox 0.8.11 tar)로 수행한 결과를 정리한다. §8의 "최소 2종 Android 기기" 기준은 **부분 충족**이며 두 번째 기기 검증 전 Phase 1 착수는 보류한다.

## 1. 결정 게이트별 결과

### 1.1 `adb` 배포 방식

**결정: MVP는 "사용자가 platform-tools를 설치" 방식**, 앱은 다음 순서로 탐색한다.

1. `CROSSMTP_ADB` env var
2. `~/Library/Android/sdk/platform-tools/adb`
3. PATH의 `adb`
4. (검토) `/opt/homebrew/bin/adb`

근거:

- 시스템에 설치된 Google 공식 `adb` 바이너리는 universal Mach-O이며 **Apple notarization을 통과**한 상태(Team ID `EQHXZ8M8AV`, hardened runtime, `flags=0x10000(runtime)`).
- 자체 번들 시 (a) Google 바이너리 재배포 라이선스 확인, (b) 우리 앱의 notarization 묶음에 외부 서명 바이너리 포함 시 Gatekeeper 충돌 가능성, (c) platform-tools 업데이트 책임 부담이 모두 발생.
- MAS(앱스토어) 배포 시 sandbox에서 임의 child process 실행이 제한되므로 **MAS는 post-MVP 검토**로 미룬다. MVP는 DMG/notarized direct distribution.

**영향**: §4.2 ADB Session Layer는 "탐색 결과 없음 → `Adb not available` 에러 + 설치 가이드 링크"를 first-class 에러로 분기.

### 1.2 tar 호환성

**결정: toybox tar(`c x t f C v h m o`)와 `--xform`을 baseline으로 가정**. `--owner/--group/--mode` 같은 권한 옵션은 안 쓴다.

근거 (검증 기기 1대 기준, [tar.txt](../../scripts/.adb-phase0/ea33d2fe/tar.txt)):

- toybox 0.8.11 tar이 `/system/bin/tar`로 노출.
- stdin 추출은 `tar -x -C <dest>`만으로 동작 ([tar-extract.txt](../../scripts/.adb-phase0/ea33d2fe/tar-extract.txt)).
- 한글 파일명, 중첩 디렉토리 모두 그대로 추출됨.

**남은 위험**: toybox가 아닌 BusyBox tar 또는 vendor 변형 tar이 들어간 기기에서 `--xform`이 없을 가능성. 두 번째 기기 검증 전에는 `--xform` 의존 코드를 작성하지 않는다.

### 1.3 macOS 메타데이터 누출 (계획서에 없던 발견)

**결정: tar 생성 전 entry 필터로 `._*`(AppleDouble), `.DS_Store`, `.Spotlight-V100`, `.Trashes`를 무조건 제외.** 사용자 설정으로 노출하지 않는다.

근거:

- smoke test 결과 device에 `._a.txt`, `sub/._한글.txt`가 그대로 추출됨 ([tar-extract.txt](../../scripts/.adb-phase0/ea33d2fe/tar-extract.txt)).
- 이는 macOS BSD tar이 AppleDouble을 자동으로 동반 entry로 만든 결과. CrossMTP가 자체 `Tar Stream Builder`로 교체하면 자연히 사라지지만, BSD tar으로 호환 검증을 할 때는 `COPYFILE_DISABLE=1` 환경변수가 필요.
- §4.3은 "정책으로 분리"라 적었지만 실측 결과 **default off**가 안전. 정책 토글이 아니라 hard exclude로 격상한다.

### 1.4 Shared storage 쓰기 매트릭스

**결정: 추천 대상 경로는 `/sdcard/Download`, `/sdcard/Documents`, `/sdcard/DCIM` 까지로 한정.** `/storage/emulated/0/Android/data`는 검증 기기에서 쓰기 가능했지만 OEM/USB-debug 컨텍스트 의존이므로 **운영 코드에서 의존 금지**.

근거 ([storage.txt](../../scripts/.adb-phase0/ea33d2fe/storage.txt)):

| Path | Result on zorn/A15 |
|---|---|
| `/sdcard` | ok |
| `/sdcard/Download` | ok |
| `/sdcard/Documents` | ok |
| `/sdcard/DCIM` | ok |
| `/storage/emulated/0` | ok |
| `/storage/emulated/0/Download` | ok |
| `/storage/emulated/0/Android/data` | ok *(의외, 의존 금지)* |

**남은 위험**: Android 11+ scoped storage가 더 엄격한 다른 OEM(Samsung One UI, Pixel stock) 결과를 트랙 B 2nd device에서 재측정해야 한다.

### 1.5 Manifest probe (충돌 처리 가능 여부)

**결정: `find <dest> -type f -exec stat -c '%n %s %Y' {} \;` 를 표준 manifest probe로 채택.** `find -printf`는 보조 후보.

근거 ([manifest.txt](../../scripts/.adb-phase0/ea33d2fe/manifest.txt)):

- `find -printf '%P\t%s\t%T@\n'` — 사용 가능하지만 mtime이 `1778723264.0` 형태 float, 첫 행에 root 디렉토리가 공백 path로 나옴. **파서가 root path 빈 토큰 케이스를 처리해야 함**.
- `find -exec stat -c '%n %s %Y'` — 정수 mtime, 더 robust. **이쪽을 primary로 사용**.
- mtime 정밀도는 1초. 계획서 §5의 기본 mtime tolerance 2초는 그대로 유효.

**§5의 80% 기준 판정**: 검증 기기 1대에서 manifest probe가 정확한 파일/폴더 목록과 mtime을 반환했다. 표본이 작아 **잠정 채택**이며, 두 번째 기기에서 동일 결과면 confirm. 따라서 Phase 2 충돌 처리 구현 설계는 manifest probe 채택을 전제로 진행 가능.

### 1.6 chunk size

**결정: 1 MiB 초기값 유지.** Phase 5에서 256 KiB / 1 MiB / 4 MiB 후보를 재측정.

근거: 트랙 B에서 256 MiB 단일 파일이 36.57 MiB/s. chunk size를 따로 바꿔 측정하지 않았지만, 작은 파일 케이스의 병목이 chunk size가 아니라 device-side 파일 생성 비용임이 §2에서 확인되어, 지금 chunk size를 더 튜닝할 이유가 없다.

## 2. 트랙 B 측정 결과와 위험 신호

### 2.1 throughput

| Workload | Size | Wall | Rate |
|---|---|---|---|
| 단일 파일 (256 MiB) | 256 MB | 7 s | **36.57 MiB/s** |
| 2000 × 4 KiB 파일 | 8 MB | 18 s | **0.43 MiB/s** |

분리 측정으로 병목 위치 확정:

- 로컬 `tar -cf - many` (13 MB tar 생성): **0.6 s**
- 같은 stream을 `adb shell cat > /dev/null`로 송출: **0.6 s**
- 같은 stream을 `adb shell tar -x -C <dest>`로 추출: **18 s**

→ **병목은 USB/MTP가 아니라 device-side tar의 inode 생성·fsync·MediaProvider 스캔.** 작은 파일 묶음의 ADB 이득은 §3.1이 기대한 만큼 크지 않을 수 있다. **§11의 "MTP 대비 3배" 기준은 MTP 베이스라인 측정 전까지 미확정**이며, 실패하면 ADB는 추천 경로가 아니라 실험적 기능으로 격하될 수 있다.

### 2.2 취소 동작 (계획서의 가정과 다름)

- Host pipeline에 `kill -INT` → **leftover files: 4022**, **device-side tar 프로세스 3개 살아 있음** ([throughput.txt](../../scripts/.adb-phase0/ea33d2fe/throughput.txt)).
- §6.1 "ADB Writer가 stdin을 닫거나 child process를 종료한다"만으로는 **device-side tar이 안 죽는다**.

**결정: §6.1을 다음과 같이 보강하여 Phase 1 설계 입력으로 사용.**

1. ADB Writer는 host 측 `adb` child process에 SIGTERM → 짧은 grace 후 SIGKILL.
2. **추가로** `adb shell` session을 통해 진행 중인 device-side tar PID를 추적(`tar -x` 직전에 `$!` 캡처) 하거나, 실패 시 `pkill -f "tar -x -C <dest>"`로 정리한다.
3. 위 단계 이후 `<dest>` 아래에 남은 파일은 §6.1의 "남았을 수 있는 파일" UX로 노출. **자동 rollback은 여전히 약속하지 않는다.**
4. Phase 1 통과 기준에 "취소 후 device-side `tar` PID가 살아 있지 않음"을 명시적 테스트로 추가.

### 2.3 케이블 분리 / 재접속 (수동 검증)

전송 중 분리:

- 1 GiB 전송 중 약 5초 시점에 USB 분리.
- Host pipeline (`tar | adb shell tar -x`) **즉시 자연 종료, hang 없음**.
- 로컬 `tar -cf -`는 `tar: Write error`로 빠짐 → pipe peer 종료가 명확한 시그널.
- `adb devices` 즉시 빈 목록. host 측 좀비 프로세스 없음.
- `adb fork-server`(데몬)는 분리와 무관하게 살아 있음 — 정상.
- 대상 디렉토리에 약 246 MiB 부분 파일 잔존. AppleDouble `._big`도 남음 → §1.3 hard-exclude 결정의 운영적 가치 재확인.

재접속:

- 케이블 재연결 즉시(T+0초) `adb devices`에 다시 등장. **fingerprint 재승인 불필요**.
- **`transport_id`가 1 → 3으로 재할당**됨. **Phase 1 ADB Session Layer는 transport_id를 캐시·재사용하지 않고 serial을 안정 식별자로 사용해야 한다.**
- 재접속 직후 새 전송(작은 fixture) `rc=0`, 한글 파일명 정상.
- device-side에 잔존 `tar` 프로세스 없음 (grep 매치는 `com.kbsec.mts.iplus`tar`ngm2` 같은 패키지명 false positive).

부가 검증:

- 재접속 후 새 전송에 `COPYFILE_DISABLE=1` 환경변수를 적용하니 결과물에 `._*` AppleDouble entry가 **사라짐**. §1.3에서 결정한 hard-exclude 방법(빌더 자체 필터 + 호환 검증 시 환경변수)의 실효성 확인.

**Phase 1 설계 입력 추가**:

1. ADB Writer는 `tar -cf -`의 stderr/exit code를 보고 `DeviceDisconnected`를 1차 시그널로 매핑. `adb` child의 exit가 그 직후 따라오면 같은 에러로 dedupe.
2. Session Layer는 device를 **serial 기준**으로만 식별. transport_id, USB 포트 번호 등은 표시용으로만.
3. 재접속이 사용자 시나리오에서 흔하므로, capability probe와 transfer queue가 device 재인식 후 자동 회복되는 경로를 Phase 3에서 명시.

## 3. plan.md에 반영해야 할 수정

Phase 1 시작 전 [docs/plan.md](../plan.md)에 다음을 반영한다.

1. **§4.3에 hard exclude 목록 추가**: AppleDouble (`._*`), `.DS_Store`, `.Spotlight-V100`, `.Trashes`는 정책 토글이 아닌 default deny. 빌더에 `COPYFILE_DISABLE=1` 환경에서 동작하도록 요구사항 추가.
2. **§4.2에 adb 탐색 순서 명시**: `CROSSMTP_ADB` env → `~/Library/Android/sdk/platform-tools/adb` → `PATH` → homebrew. 미발견 시 `AdbNotAvailable` 에러.
3. **§5의 manifest probe 명령 표준화**: `find <root> -type f -exec stat -c '%n %s %Y' {} \;` 를 primary로 명시.
4. **§6.1 취소 절차 5단계화**: 위 §2.2의 순서대로 갱신. "device-side tar PID 종료"를 명시적 단계로 격상.
5. **§8 Phase 0 통과 기준에 "검증 기기 2종"이 미충족임을 기록**하고, Phase 1 시작 전 추가 기기 검증 task를 만든다.
6. **§11의 "MTP 대비 3배" 기준 옆에 "device-side inode 생성 비용이 결정적 병목임이 트랙 B에서 확인됨, MTP 베이스라인 측정 전까지 보류"를 주석으로 추가**.

## 4. Phase 1 진입 가능 여부

**조건부 진입 가능.** 다음을 동시에 진행한다.

- ✅ 호스트 측 결정 게이트 5개(1.1–1.4, 1.6) 확정.
- ⚠️ 두 번째 기기(가능하면 Pixel 또는 Samsung One UI) 트랙 B 재측정. manifest probe와 storage matrix가 동일하게 나오면 80% 기준 confirm.
- ⚠️ 케이블 분리 수동 검증.
- 📝 plan.md §3의 6개 항목 patch 반영.

위 3개가 끝나야 §8 Phase 0의 "최소 2종 / 케이블 분리 hang 없음" 통과 기준을 완전 충족한다. Phase 1 ADB Session Layer 설계 자체는 이번 결정 결과로 시작 가능하다.

## 5. 한 줄 요약

ADB+tar 경로는 살아 있지만, "작은 파일 묶음에서 압도적 우위"라는 §3.1의 직관은 트랙 B 실측에서 약화됐고, 취소 모델은 §6.1보다 한 단계 더 깊이 들어가야 한다. 계획은 폐기가 아니라 patch.
