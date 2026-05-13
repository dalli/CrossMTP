# 문제 해결

CrossMTP는 macOS의 USB MTP 동작 특성상 자주 마주치는 시나리오가 정해져 있습니다.
대부분 폰 또는 macOS 시스템 측 상태가 원인입니다.

## 1. 앱이 "기기 없음"으로 표시됨

원인 후보 (자주 발생하는 순):

### a) 폰이 MTP 모드가 아님

Android는 USB 연결 시 기본 모드가 **"충전 전용"** 입니다. CrossMTP가 보려면
"파일 전송" 또는 "MTP" 모드로 명시 전환해야 합니다.

→ 폰 잠금 해제 → 알림 센터의 "USB로 충전" 항목 탭 → "파일 전송" 선택

### b) 충전 전용 케이블

USB 케이블 중 일부는 데이터 라인이 빠져 있습니다. 다른 케이블로 시도.

### c) macOS 데몬이 USB 인터페이스를 선점

`Image Capture` (icdd), `Android File Transfer`가 살아 있으면 폰을 자동으로 잡고
놓아주지 않습니다. CrossMTP가 같은 인터페이스를 열려 하면 `LIBMTP PANIC: Unable to
initialize device` 또는 `libusb_claim_interface = -3 (EACCES)` 가 뜹니다.

```bash
killall "Android File Transfer" "Android File Transfer Agent" icdd 2>/dev/null
```

종료 후 CrossMTP 새로고침 (우상단 버튼). macOS가 데몬을 다시 띄우기 전에 빠르게
사용해야 합니다.

### d) 폰 잠금

Android 13+ 기기는 잠금 상태에서 MTP 권한을 자동으로 회수합니다. 폰을 깨워 잠금
해제 후 새로고침.

## 2. 다운로드 / 업로드가 "Failed"로 끝남

큐 카드의 빨간 영역에 사유가 표시됩니다. 자주 발생하는 사유:

| 메시지 | 의미 | 조치 |
|---|---|---|
| `device cannot be opened` | macOS 데몬이 USB를 잡음 | 위 1c 참고 |
| `device storage unavailable` | 폰 잠금 / MTP 권한 미승인 | 폰 깨우고 허용 탭 |
| `MTP connection error` | 케이블 분리, 폰 reboot 등 | 케이블 재연결 후 새로고침 |
| `device reported: ...` | libmtp가 폰에서 받은 raw 메시지 | 메시지에 따라 별개 대응 |
| `local IO error` | 다운로드 dest_dir 쓰기 권한 없음 | 다른 폴더 선택 |
| `transfer failed` | 원인 불명, 보통 USB 전기 노이즈/파손 | 케이블 재연결, 다른 포트 |

## 3. 큐 카드가 "Cancelling" 에서 멈춤

libmtp가 cancel 신호를 즉시 처리하지 못하는 케이스. 일반적으로 수 초 안에
`Cancelled` 또는 `Failed`로 전이됩니다. 30초 이상 멈춰 있으면:

* 새로고침 버튼으로 device snapshot 재구성
* 그래도 안 풀리면 앱 종료 후 재실행

## 3.5 폴더(디렉토리) 업로드

CrossMTP는 폴더를 드롭하면 **구조를 유지하며 재귀적으로** 업로드합니다:

* 드롭한 폴더와 같은 이름이 device에 이미 있으면 **merge** (기존 폴더를 재사용)
* 없으면 새로 생성 (`LIBMTP_Create_Folder` 사용)
* 안의 모든 파일은 각각 개별 job으로 큐에 들어감 — 큐 패널에서 진행률/취소 가능
* 심볼릭 링크와 특수 파일은 조용히 건너뜀 (무한 루프 방지)
* 파일 이름 충돌은 상단 "충돌 시" 정책을 따름 (폴더 충돌은 항상 merge)

알려진 제한:
* 대량의 파일이 포함된 폴더 (수백~수천 개)는 큐 빌드 단계에서 일시적으로 멈춘 듯
  보일 수 있습니다 (listing + create_folder 호출이 순차적으로 worker 경유).
* libmtp가 device 이름 규칙에 맞춰 폴더 이름을 sanitise할 수 있습니다. 드물게
  로컬과 device의 이름이 달라지면 다음 업로드에서 merge 대신 새 폴더가 생성될 수
  있음. 필요 시 device의 폴더 이름을 기준으로 재시도.

## 4. 같은 이름 파일을 업로드할 수 없음

업로드 충돌 정책이 `Skip` (기본) 또는 `Overwrite` (현재 미구현)으로 설정된 경우
두 번째 업로드는 Skipped 또는 Failed로 끝납니다.

* 우상단 "충돌 시 [...]" → **이름 변경** 선택 후 재시도
* 또는 폰에서 기존 파일을 삭제 (CrossMTP는 delete 미지원)

> Phase 5+ 로드맵: upload Overwrite는 delete 능력이 추가되면 지원 예정.

## 5. 작은 파일에서 진행률이 안 보임

libmtp는 파일이 충분히 크지 않으면 `progress callback`을 호출하지 않습니다.
실측 경계는 수십 KB ~ 수 MB 사이에서 기기마다 다름. 작은 파일은 즉시 Completed로
표시되며 이는 정상입니다.

## 6. macOS "확인되지 않은 개발자" 경고

베타 단계는 코드 sign / notarization이 안 되어 있습니다.

* System Settings → Privacy & Security → "그래도 열기"
* 또는 터미널: `xattr -d com.apple.quarantine /Applications/CrossMTP.app`

## 7. CLI 디버깅

UI에서 안 보이는 세부 동작은 CLI로 확인:

```bash
killall "Android File Transfer" "Android File Transfer Agent" icdd 2>/dev/null
cargo run -p mtp-cli -- verify-q
```

verify-q는 다음 5단계를 한 프로세스에서 실행해 어디서 실패하는지 명확히 보여줍니다:
1. devices 발견
2. storages 표시
3. root listing
4. 파일 download
5. round-trip upload (+ Skip / Rename / Cancel 시나리오)

## 8. 로그 / 알려진 버그

* [`docs/test-checklist.md`](test-checklist.md) — 검증된/미검증 시나리오 목록
* [`docs/retrospectives/`](retrospectives/) — 각 phase 회고에 알려진 제한사항 정리

## 9. 그래도 안 되면

이슈 보고에 다음을 포함해주세요:

```bash
# macOS 버전
sw_vers

# 아키텍처
uname -m

# libmtp 버전
pkg-config --modversion libmtp

# 시스템에 보이는 raw MTP device
mtp-detect 2>&1 | head -30

# CrossMTP CLI 출력
cargo run -p mtp-cli -- verify-q 2>&1 | tail -50
```
