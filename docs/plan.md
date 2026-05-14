# ADB + Tar 스트리밍 + 큐잉 고속 전송 계획서

## 1. 문서 목적

이 문서는 CrossMTP에 **ADB + Tar 스트리밍 + 큐잉** 기반 고속 전송 경로를 도입하기 위한 계획서다.

기존 macOS MVP의 fallback 전송 경로는 `libmtp` 기반 전송 안정화다. 이 문서는 그 방향을 폐기하지 않고, capability가 확인된 기기에서 ADB 고속 전송을 추천 경로로 올릴 수 있는 조건과 제한을 정의한다.

핵심 목표는 다음과 같다.

1. 많은 작은 파일을 전송할 때 MTP의 파일 단위 handshake 병목을 줄인다.
2. 임시 tar 파일을 디스크에 만들지 않고 on-the-fly tar stream으로 전송한다.
3. 스캔, tar 생성, ADB stdin 쓰기, 진행률 계산을 큐 기반 pipeline으로 분리한다.
4. 속도 개선을 하더라도 취소, 실패, 충돌, 부분 전송 상태를 모호하게 만들지 않는다.
5. ADB가 불가능한 기기에서는 기존 MTP 경로로 명확히 fallback한다.

---

## 2. 제품 위치

### 2.1 fallback 경로와 추천 경로를 분리한다

macOS MVP의 **fallback 경로**는 기존 계획서의 `libmtp` 기반 구현이다. ADB 조건을 만족하지 않거나 사용자가 원하지 않는 경우에도 MTP 업로드/다운로드는 계속 사용할 수 있어야 한다.

macOS MVP의 **추천 경로**는 device/session capability에 따라 동적으로 결정한다. ADB 고속 업로드 조건이 모두 확인된 기기에서는 UI가 “고속 ADB 업로드”를 권장 선택값으로 제시하고, 사용자가 명시적으로 MTP로 내릴 수 있게 한다. 단, 첫 사용 시에는 USB debugging의 의미를 설명하고 사용자가 고속 모드 사용을 승인해야 한다.

ADB 고속 전송은 다음 조건을 만족할 때만 추천하거나 실행한다.

* Android 기기에서 USB debugging이 활성화되어 있음
* 사용자가 해당 Mac을 ADB debugging 대상으로 승인함
* 앱이 사용할 `adb` executable을 확인함
* 대상 경로가 Android shell에서 읽기/쓰기 가능한 shared storage 경로임
* 기기 쪽 `tar` 명령이 사용 가능하고 필요한 옵션/파일명 동작이 호환됨
* 현재 파일 구성이 Phase 5 벤치마크로 정한 추천 규칙에 해당함

이 조건을 만족하지 않으면 UI는 ADB 모드를 실패한 기능처럼 보이게 하지 않고, “ADB 고속 모드 사용 불가” 이유를 표시한 뒤 MTP 전송을 계속 사용할 수 있게 한다. “기본”이라는 표현은 문서와 UI에서 fallback인지 추천 선택값인지 구분해 사용한다.

### 2.2 ADB 모드는 고속 업로드부터 시작한다

초기 구현 범위는 **로컬 Mac -> Android 업로드**에 한정한다.

포함:

* 폴더 업로드
* 다수 파일 업로드
* drag-and-drop 업로드
* 진행률 표시
* 사용자 취소
* duplicate name conflict 정책
* 기기 연결 해제, device offline, authorization missing 처리

초기 제외:

* Android -> Mac 다운로드의 tar streaming 최적화
* 단일 파일 전송 전용 ADB fast path
* Android 내부 파일 삭제/rename/create-folder UI 확장
* ADB over Wi-Fi
* multi-device 동시 ADB 전송
* root 권한 전제 기능

다운로드 최적화는 업로드 모드가 실기기에서 안정화된 뒤 별도 phase로 판단한다.

UI는 이 제한을 숨기지 않는다. 다운로드 작업은 “현재 다운로드는 MTP 사용”처럼 capability 상태로 표시해, 업로드만 고속화되는 이유가 사용자에게 모호하지 않게 한다.

---

## 3. 기대 성능 모델

### 3.1 빨라지는 이유

MTP 업로드는 파일마다 metadata 조회, object 생성, 데이터 전송, 완료 확인 단계가 반복된다. 작은 파일 수가 많을수록 실제 USB bandwidth보다 왕복 지연과 protocol overhead가 더 큰 병목이 된다.

ADB + Tar 스트리밍은 다수 파일을 하나의 연속 stream으로 바꾼다.

* Mac은 파일 트리를 lazy scan하면서 tar header와 file payload를 순서대로 생성한다.
* 생성된 stream은 `adb shell tar -x -C <dest>`의 stdin으로 바로 들어간다.
* Android 기기는 stream을 받는 즉시 대상 디렉토리에 풀어 쓴다.
* 파일별 MTP handshake가 사라져 작은 파일 묶음에서 효과가 크다.

### 3.2 성능 한계

고속 모드는 “항상 최고 속도”를 보장하지 않는다. 실제 상한은 다음 요소 중 가장 느린 곳에서 결정된다.

* Android USB port 규격
* 케이블 품질
* Android 저장소 쓰기 속도
* Android CPU와 thermal throttling
* `adb` daemon throughput
* macOS 파일 읽기 속도
* tar 생성/해제 비용

문서와 UI에서는 “물리적 한계에 근접 가능” 같은 표현을 조건부 설명으로만 사용하고, 검증 전에는 보장 문구로 쓰지 않는다.

---

## 4. 아키텍처

### 4.1 전송 backend capability

기존 backend를 단일 추상 API로 억지 통합하지 않고, capability 중심으로 분리한다.

예상 capability:

* `mtpUpload`
* `mtpDownload`
* `mtpBrowse`
* `adbTarUpload`
* `adbAvailabilityProbe`
* `recommendedUploadPath`
* `recommendedDownloadPath`

UI는 현재 선택된 device/session이 어떤 capability를 제공하는지 확인한 뒤 전송 옵션을 노출한다. `recommendedUploadPath`는 `mtp` 또는 `adbTar`처럼 명시적인 값이어야 하며, fallback 가능 여부와 별도로 계산한다.

추천 경로 계산은 매 전송 때 임의로 벤치마크하지 않는다. Phase 5 측정 결과로 정한 정적 규칙을 사용하고, 입력은 device capability, 전송 방향, 파일 수, 총 byte 수, 평균 파일 크기, 사용자의 명시적 이전 선택이다. 예를 들어 “파일 수가 임계값 이상이고 평균 파일 크기가 임계값 이하인 업로드”만 ADB를 추천할 수 있다.

### 4.2 ADB Session Layer

역할:

* `adb` executable 탐색
* `adb devices -l` 결과 파싱
* device authorization 상태 확인
* 단일 target device 선택
* shell command 실행
* stdin/stdout/stderr stream 관리
* `adb` 종료 코드와 stderr를 CrossMTP error model로 정규화

`adb` 탐색 순서 (Phase 0 결정):

1. `CROSSMTP_ADB` 환경변수
2. `~/Library/Android/sdk/platform-tools/adb`
3. `PATH`의 `adb`
4. `/opt/homebrew/bin/adb`

어떤 후보에서도 발견되지 않으면 `AdbNotAvailable` 에러로 분기하고, UI에서 platform-tools 설치 안내 링크를 보여준다.

주의:

* `unauthorized`, `offline`, `no permissions`, `device not found`는 사용자가 이해할 수 있는 별도 에러로 분리한다.
* Android shell quoting은 임의 문자열 결합으로 처리하지 않는다.
* 대상 경로는 shared storage root 기준으로 검증하고, `..` path traversal을 허용하지 않는다.
* device 식별자는 **serial만** 안정 식별자로 사용한다. `adb devices -l`의 `transport_id`는 재접속 시 재할당되므로 캐시·재사용하지 않는다. (Phase 0 케이블 재접속 검증에서 `transport_id 1 → 3` 확인.)
* `tar -cf -` host 측 stderr/exit code를 1차 시그널로 `DeviceDisconnected`에 매핑한다. 직후 따라오는 `adb` child의 exit는 같은 에러로 dedupe.

### 4.3 Tar Stream Builder

역할:

* 로컬 파일/디렉토리를 lazy traversal한다.
* tar header를 on-the-fly로 생성한다.
* 파일 내용을 고정 크기 chunk로 읽어 stream에 공급한다.
* 전체 파일 수와 전체 byte 수를 가능한 범위에서 계산한다.
* 진행률 이벤트에 필요한 현재 파일명, 완료 byte, 전체 byte를 제공한다.

원칙:

* 디스크에 임시 tar 파일을 만들지 않는다.
* symbolic link, socket, device file 등 Android shared storage에 부적절한 entry는 명시적으로 skip 또는 fail 처리한다.
* macOS 메타데이터는 **default deny hard-exclude** 로 처리한다. 사용자 설정으로 노출하지 않는다. 대상 패턴: `._*` (AppleDouble), `.DS_Store`, `.Spotlight-V100`, `.Trashes`, `.fseventsd`. (Phase 0에서 device에 `._a.txt`, `._한글.txt`가 그대로 추출됨을 확인.)
* 호환 검증 단계에서 BSD `tar(1)`로 fixture를 만들 때는 환경변수 `COPYFILE_DISABLE=1`을 강제한다. 자체 Tar Stream Builder는 이 환경변수와 무관하게 동작해야 한다.
* tar path는 상대 경로만 허용한다.

### 4.4 Transfer Orchestrator 통합

ADB 고속 업로드도 기존 transfer state machine을 사용한다.

필수 상태:

* `queued`
* `validating`
* `transferring`
* `cancelling`
* `completed`
* `failed`
* `cancelled`
* `skipped`

ADB 모드에서 추가로 추적할 내부 단계:

* `probing_adb`
* `scanning_sources`
* `starting_remote_tar`
* `streaming_tar`
* `finalizing_remote_tar`

내부 단계는 사용자에게 필요한 경우에만 짧게 표시하고, terminal state는 기존 상태 모델과 일치시킨다.

### 4.5 Pipeline 구조

초기 구현은 복잡한 병렬 처리보다 backpressure가 명확한 bounded pipeline을 우선한다.

구성:

1. Scanner: 로컬 파일 트리를 순회하고 entry metadata를 만든다.
2. Tar Writer: entry를 tar header + payload stream으로 변환한다.
3. ADB Writer: tar stream을 `adb shell tar -x -C <dest>` stdin으로 쓴다.
4. Progress Reporter: byte/file 단위 진행률을 orchestrator event로 발행한다.

bounded queue를 사용해 메모리 사용량을 제한한다. 초기 chunk 크기는 1 MiB를 기준으로 시작하고, 실기기 측정 후 256 KiB, 1 MiB, 4 MiB 후보를 비교한다. 이 값은 애플리케이션 내부 read/write buffer 크기이며, USB packet 크기나 ADB daemon 내부 전송 단위를 의미하지 않는다.

---

## 5. 충돌 처리 정책

충돌 처리는 MTP와 ADB 고속 업로드가 같은 사용자 정책을 공유해야 한다. 백엔드별 구현 방식은 달라도, 사용자가 이해하는 규칙은 다음 원칙을 따른다.

기본 정책:

* 이름, 파일 크기, 수정시각이 같으면 같은 파일로 간주하고 `Skip`한다.
* 이름은 같지만 파일 크기 또는 수정시각이 다르면 다른 파일로 간주하고 `Rename`한다.
* `Overwrite`는 사용자가 설정 또는 충돌 dialog에서 명시적으로 선택한 경우에만 수행한다.

같은 파일 판정은 실용적 휴리스틱이다. Android/MTP/macOS 간 수정시각 정밀도와 반올림 차이가 있으므로, 기본 수정시각 허용 오차는 2초로 둔다. 즉, 이름과 크기가 같고 수정시각 차이가 2초 이내이면 같은 파일로 판단한다. hash 비교는 원격 파일에서 비용과 지원 문제가 크므로 MVP 기본 범위에 넣지 않는다.

설정 메뉴에는 `Settings > Transfers > File conflicts`를 추가한다.

사용자 설정 항목:

* 동일 파일 판정: `name + size + modified time`
* 수정시각 허용 오차: `0초`, `2초`, `5초`, `10초`
* 같은 파일일 때: `Skip`, `Ask every time`, `Overwrite`
* 같은 이름의 다른 파일일 때: `Rename`, `Ask every time`, `Overwrite`, `Skip`
* rename 방식: `{name} ({n}){ext}`, `{name} - {timestamp}{ext}`, `{name} - copy{ext}`
* 고급 rename pattern: `{name}`, `{ext}`, `{n}`, `{timestamp}` 변수만 허용
* overwrite 확인: `항상 확인`, `이번 전송 큐에서는 다시 묻지 않음`, `항상 overwrite`

`Ask every time`은 백엔드별로 다르게 실행된다. MTP 경로에서는 파일 단위 전송 전에 물을 수 있지만, ADB 고속 업로드에서는 tar stream 생성 전에 모든 결정을 끝내야 하므로 전송 시작 전 충돌 manifest를 일괄 dialog로 보여주고 한 번에 결정하게 한다. 일괄 manifest를 만들 수 없는 경우 ADB 고속 업로드에서는 `Ask every time`을 지원하지 않고 MTP fallback으로 안내한다.

`0초` 수정시각 허용 오차는 고급 옵션으로만 노출하고, 원격 파일시스템의 시간 정밀도 차이 때문에 같은 파일도 다른 파일로 판정될 수 있음을 표시한다. 기본 mtime 허용 오차는 흔한 정밀도 차이를 흡수할 수 있는 가장 작은 값으로 2초를 사용한다. false positive로 인한 잘못된 skip은 더 비싼 실패이므로, 사용자가 결과를 확인할 수 있도록 전송 완료 화면에 `Skipped N개 — 보기` 목록을 제공한다.

rename pattern은 target filesystem과 tar entry path 양쪽에서 안전한 문자로 sanitize한다. `{timestamp}` 기본 포맷은 `yyyyMMdd-HHmmss`처럼 `:`를 포함하지 않는 형식을 사용하고, 사용자 pattern에 Android shared storage나 FAT 계열 저장소에서 위험한 문자가 들어가면 저장 전에 미리 표시하거나 안전 문자로 치환한다.

`overwriteConfirmation: perQueue`에서 queue는 한 번의 사용자 액션으로 enqueue된 job 묶음을 뜻한다. 예를 들어 한 번의 drag-and-drop, 한 번의 업로드 버튼 클릭, 한 번의 conflict 일괄 dialog 승인으로 생성된 작업 묶음이 같은 confirmation scope를 공유한다. 앱을 켜둔 동안 전체 queue에 영구 적용하지 않는다.

초기 기본값:

* 동일 파일 판정: `name + size + modified time`
* 수정시각 허용 오차: `2초`
* 같은 파일일 때: `Skip`
* 같은 이름의 다른 파일일 때: `Rename`
* rename 방식: `{name} ({n}){ext}`
* overwrite 확인: `항상 확인`

도메인 모델은 단일 `ConflictPolicy`만으로 표현하지 않고 충돌 유형별 rule set으로 분리한다.

예상 설정 모델:

```text
sameFilePolicy: skip | ask | overwrite
differentFilePolicy: rename | ask | overwrite | skip
renamePattern: "{name} ({n}){ext}"
mtimeToleranceSeconds: 2
overwriteConfirmation: always | perQueue | never
```

Tar extraction은 기본적으로 같은 이름 파일을 덮어쓸 수 있으므로, ADB 고속 업로드에서는 위 정책을 tar stream 생성 전에 확정해야 한다.

ADB 적용 원칙:

* `Skip`: 같은 파일로 판정된 entry를 tar stream에서 제외한다.
* `Rename`: 다른 파일로 판정된 충돌 entry의 tar path에 안전한 새 이름을 반영한다.
* `Overwrite`: 사용자가 명시적으로 선택했고 overwrite 확인 정책을 통과한 entry만 허용한다.
* `Ask`: 전송 시작 전 manifest 기반 일괄 dialog에서만 허용한다.
* `overwriteConfirmation: always`: ADB 고속 업로드에서는 충돌 manifest와 overwrite 대상 목록을 전송 시작 전에 일괄 표시하고 한 번에 승인받는다.

대상 존재 여부 확인은 파일마다 `adb shell ls`를 호출하면 성능 이점이 사라진다. 1-depth listing만으로 conflict를 계산하는 방식은 중첩 폴더와 사진 라이브러리 같은 실제 대량 파일 사용 사례에서 불완전하므로 ADB 고속 경로의 기본안으로 두지 않는다.

Phase 0 종료 시 다음 결정 규칙으로 ADB 고속 경로의 충돌 처리 가능 여부를 확정하고, Phase 2에서는 확정된 안을 구현한다.

1. **Android-side manifest probe 성공**: `adb shell`을 한 번 실행해 대상 tree의 manifest를 받고, Mac 쪽에서 `Skip`/`Rename`/`Overwrite`/`Ask` 계획을 계산한다. 표준 probe 명령은 `find <root> -type f -exec stat -c '%n %s %Y' {} \;` 이며, mtime은 1초 정수 정밀도로 §5 default `mtimeToleranceSeconds: 2`와 호환된다. `find -printf '%P\t%s\t%T@\n'`은 보조 후보지만 mtime이 float이고 root 디렉토리 토큰이 비어 있어 파서가 더 복잡하므로 primary로 두지 않는다. `find`/`toybox` 호환성과 scoped storage 쓰기 가능 범위를 Phase 0에서 같이 검증한다.
2. **충돌 불가능 fast-path**: 대상이 앱이 방금 새로 만든 빈 디렉토리이거나, 사용자가 명시적으로 선택한 빈 디렉토리임을 확인할 수 있으면 manifest probe 없이 ADB 고속 업로드를 허용한다.
3. **Android-side manifest probe 실패**: 충돌 가능성이 있는 업로드에서는 ADB 고속 업로드에서 사용자 충돌 정책을 흉내 내지 않고 MTP fallback으로 안내한다.

Phase 0 검증 대상 기기의 80% 이상에서 manifest probe가 정확한 파일/폴더 목록과 mtime을 반환하고, scoped storage 쓰기 가능 범위가 예측 가능하면 ADB 고속 업로드에 충돌 정책을 적용한다. 그렇지 않으면 충돌 가능성이 있는 업로드는 MTP fallback으로 안내한다. 80% 기준은 Phase 0 표본 수가 작을 경우 보수적으로 해석하고, 실패 기기가 주요 지원 대상이면 ADB 고속 업로드를 추천 경로로 두지 않는다.

ADB 고속 경로에서 no-merge auto rename을 별도 대체 정책으로 도입하지 않는다. 사용자가 설정한 충돌 정책이 ADB에서 다르게 적용된다고 오해할 수 있고, merge 없는 새 root 추출은 same-file skip과 different-file rename 설정의 의미를 무효화하기 때문이다. ADB 고속 경로에서 “알려진 제한”만으로 불완전한 1-depth conflict 계산을 통과시키지 않는다.

---

## 6. 취소와 실패 처리

### 6.1 사용자 취소

취소 요청이 들어오면 다음 순서로 처리한다.

1. orchestrator job state를 `cancelling`으로 전환한다.
2. Tar Stream Builder가 새 entry 생성을 중단한다.
3. ADB Writer가 host 측 `adb` child process에 SIGTERM을 보내고 짧은 grace(예: 1초) 후 SIGKILL.
4. **device-side `tar -x` PID를 명시적으로 종료한다.** `adb shell tar -x` 실행 직전에 `echo $$; exec tar ...` 형태로 PID를 캡처해 두거나, 실패 시 `adb shell pkill -f 'tar -x -C <dest>'`로 보조 정리한다. 호스트 pipeline 종료만으로는 device-side tar이 살아남을 수 있음을 Phase 0에서 확인했다 (취소 후 4022 파일 잔존 + device-side stray tar 3개).
5. `adb` child와 device-side tar 양쪽의 종료를 확인한다.
6. terminal state를 `cancelled` 또는 실패 원인이 더 명확한 `failed`로 확정한다.

Phase 1 통과 기준에 "취소 후 device-side `tar` PID가 살아 있지 않음"을 명시적 자동/수동 테스트로 포함한다.

취소 후 Android에 일부 파일이 남을 수 있다. 초기 버전은 자동 rollback을 약속하지 않고, “일부 파일이 대상 폴더에 남았을 수 있음”을 명확히 표시한다. 가능하면 전송 시작 시각, 대상 root, 이미 stream에 쓴 entry 목록을 보존해 취소 후 사용자가 확인할 수 있는 “남았을 수 있는 파일” 목록을 보여준다.

Android-side manifest probe를 채택한 경우에는 취소 시점 이후 mtime을 가진 대상 파일 목록을 함께 표시한다. manifest probe가 불가능한 기기는 충돌 가능성이 있는 ADB 고속 업로드를 MTP fallback으로 안내하므로, 취소 정리 UX도 MTP의 파일 단위 상태를 따른다.

### 6.2 실패 시나리오

반드시 다룰 실패:

* USB cable disconnect
* Android device lock 또는 storage 접근 중단
* ADB authorization revoked
* `adb` executable 없음
* `tar` command 없음
* 대상 경로 접근 불가
* Android 저장공간 부족
* 로컬 파일 읽기 실패
* 전송 중 앱 interruption

기대 결과:

* UI freeze 없음
* `completed`와 `failed`가 섞이지 않음
* child process 누수 없음
* queue가 제어 불가능한 상태로 남지 않음
* 사용자에게 다음 조치가 보이는 에러 메시지 제공
* 완료 화면에서 `skipped` 항목 목록을 확인할 수 있음

---

## 7. 보안과 권한

ADB 모드는 MTP보다 강한 기기 접근 권한을 요구한다.

제품 문구 원칙:

* USB debugging을 켜야 한다는 사실을 숨기지 않는다.
* Android에 표시되는 RSA fingerprint 승인 흐름을 안내한다.
* 공용 PC에서는 ADB debugging을 사용하지 않는 것이 낫다고 안내한다.
* ADB mode는 첫 사용 시 사용자가 직접 승인한 경우에만 사용한다.
* 승인 이후 capability가 계속 유효한 기기에서는 ADB 업로드를 추천 선택값으로 둘 수 있다.

구현 원칙:

* 임의 shell command injection을 막기 위해 대상 경로와 tar entry path를 엄격히 escape 또는 인자화한다.
* Android shell에서 실행하는 command는 최소화한다.
* root 권한이나 `su`를 사용하지 않는다.
* 앱이 ADB key를 직접 생성/관리하지 않고 platform `adb` 동작에 맡긴다.

---

## 8. 구현 로드맵

### Phase 0. 기술 검증

> **진행 상태 (2026-05-14)**: 검증 기기 1종(Xiaomi `24117RK2CG` / Android 15 / toybox 0.8.11)으로 트랙 A 결정 + 트랙 B 핵심 항목(stdin streaming, storage matrix, manifest probe, 케이블 분리/재접속) 완료. "최소 2종" 기준은 본 라운드에서 의도적으로 생략하며 MVP 진입 전 별도 라운드로 미룬다. 세부는 [retrospectives/adb-phase-0.md](retrospectives/adb-phase-0.md) 참고.

목표:

* macOS에서 `adb shell tar -x -C <dest>` stdin streaming이 실제 기기에서 동작하는지 확인한다.
* tar command 제공 여부와 Android 버전/OEM별 차이를 확인한다.
* Android shared storage 경로별 쓰기 가능 범위를 확인한다.
* 앱이 사용할 `adb` 제공 방식을 결정한다.
* chunk size별 throughput을 측정한다.

통과 기준:

* 최소 2종 Android 기기에서 1GB 단일 파일과 5,000개 이상 작은 파일 묶음 업로드 성공
* 검증한 Android 버전, OEM, `tar` 구현체/toybox 버전, 지원하지 않는 tar 옵션을 문서화
* `/sdcard`, `/storage/emulated/0`, `Download`, `Documents`, 앱별 제한 경로 등에서 쓰기 가능/불가 결과 기록
* `adb` 번들링, 시스템 PATH 사용, 사용자가 platform-tools를 설치하는 방식 중 MVP 배포안을 선택하고 코드 서명/sandboxing 영향을 기록
* manifest probe 채택 여부를 §5의 80% 기준에 따라 결정
* 취소 시 child process가 종료됨
* 케이블 분리 시 앱/프로세스가 hang되지 않음

### Phase 1. ADB Session Layer 추가

목표:

* `adb` 탐색과 device 상태 probe를 구현한다.
* authorization/offline/no-device 에러를 정규화한다.
* shell command 실행과 child process lifecycle을 테스트 가능하게 만든다.

통과 기준:

* ADB 없음, unauthorized, offline, connected 상태가 자동 테스트와 수동 테스트로 구분됨
* UI 또는 command layer가 ADB 가능 여부를 capability로 받을 수 있음

### Phase 2. Tar Stream Builder 구현

목표:

* 임시 파일 없는 tar stream 생성
* lazy traversal
* progress byte/file counting
* skip 대상 entry 처리
* Phase 0에서 확정한 conflict 구현안을 tar stream 생성 전에 적용

통과 기준:

* 생성된 stream을 로컬 `tar -tf -` 또는 임시 extraction test로 검증
* 특수 파일, 권한 부족 파일, 긴 파일명, 한글 파일명 테스트 추가
* 이름/크기/수정시각 기반 same-file skip과 different-file rename 테스트 추가
* rename pattern sanitize와 안전한 timestamp 포맷 테스트 추가
* 중첩 폴더 conflict에서 overwrite가 발생하지 않음을 자동 테스트 또는 실기기 테스트로 검증

### Phase 3. Orchestrator 통합

> **진행 상태 (2026-05-14)**: orchestrator에 `JobKind::AdbTarUpload` + `AdbContext` 통합 완료. smoke check + 통합 conflict planner (`adb-session::plan_upload`) + CLI `verify-q` end-to-end 경로 포함. 자동 테스트 워크스페이스 전체 통과. 취소 실기기 재현(4022 fixture)과 두 번째 기기 검증은 Phase 5 입력으로 이월. 세부는 [retrospectives/adb-phase-3.md](retrospectives/adb-phase-3.md) 참고.

목표:

* 기존 transfer queue에 `adbTarUpload` job type을 추가한다.
* 기존 상태 모델과 cancellation model을 재사용한다.
* 실패와 취소 terminal state를 명확히 만든다.
* smoke check를 ADB 세션 bring-up 시점에 캐시 가능한 helper로 분리한다.
* manifest probe 결과 + `is_same_file` + `RenameRule` 을 자동으로 `ConflictPlan`으로 정리하는 planner를 도입한다.

통과 기준:

* queued/cancelling/cancelled/failed/completed 상태 테스트 추가
* ADB child process kill path 테스트
* MTP job과 ADB job이 UI에서 같은 queue semantics를 가짐
* `tar_upload::smoke_check_extract` 가 빈 입력에서 device-side `tar -x` 의 정상 종료를 검증한다
* `conflict_planner::plan_upload` 가 plan.md §5 default policy (skip same / rename diff / 2s tolerance / `{name} ({n}){ext}`) 를 충실히 구현한다
* `Orchestrator::cancel` 이 ADB cancel handle 까지 발화시켜 §6.1 5단계 정리 시퀀스를 트리거한다

### Phase 4. UI opt-in 추가

목표:

* 기기 capability에 따라 “고속 ADB 업로드” 옵션을 노출한다.
* ADB debugging 필요 조건과 보안 주의사항을 짧게 안내한다.
* 사용자가 추천 업로드 경로와 MTP fallback 경로 중 선택할 수 있게 한다.
* 다운로드는 현재 MTP 경로를 사용한다는 사실을 capability label로 표시한다.
* `Settings > Transfers > File conflicts`에서 충돌 처리 rule set을 선택할 수 있게 한다.

통과 기준:

* ADB 불가 상태에서 이유 표시
* ADB 가능 상태에서 추천 선택값과 MTP fallback 선택지가 구분됨
* 같은 파일 기본값은 `Skip`, 다른 파일 기본값은 `Rename`, overwrite는 명시 선택으로만 동작함
* `Ask every time`은 ADB 고속 업로드에서 manifest 기반 일괄 dialog로 표시되거나 MTP fallback으로 안내됨
* 완료 화면에서 `Skipped N개 — 보기`가 표시됨
* 전송 중 진행률과 취소 동작 표시
* 기존 MTP 전송 UI가 퇴행하지 않음

### Phase 5. 실기기 성능/실패 검증

목표:

* 실제 throughput과 실패 동작을 문서화한다.
* MTP 대비 성능 개선 폭을 파일 구성별로 측정한다.
* 알려진 제한사항을 README 또는 troubleshooting 문서에 반영한다.

통과 기준:

* 최소 2종 Android 기기, 2종 파일 세트에서 측정값 기록
* 케이블 분리, 저장공간 부족, authorization 해제, 사용자 취소 검증
* 5,000개 이상 작은 파일 묶음에서 ADB 경로가 MTP 대비 최소 3배 빠르지 않으면 해당 구성에서 ADB를 추천 선택값에서 제외하고 실험적 기능으로 낮춤
* 1GB 이상 단일 파일에서 ADB 경로가 MTP 대비 0.8배 미만이면 단일 대용량 파일 구성에서는 ADB를 추천하지 않음
* “검증되지 않은 failure scenario”를 명시

---

## 9. 테스트 계획

### 9.1 자동 테스트

하드웨어 없이 검증할 항목:

* tar path normalization
* path traversal 차단
* conflict rename helper
* same-file 판정: name + size + mtime tolerance
* conflict rule set serialization/default migration
* ask policy batch decision planning
* rename pattern sanitization
* skip list 적용
* tar header 생성
* 진행률 byte counting
* ADB stderr -> CrossMTP error mapping
* child process cancellation abstraction

### 9.2 수동 실기기 테스트

필수 파일 세트:

* 1GB 이상 단일 파일
* 5,000개 이상 작은 파일 묶음
* 한글/공백/특수문자 파일명
* 중첩 폴더
* 대상 경로에 같은 이름 파일이 있는 폴더

필수 failure scenario:

* 전송 중 케이블 분리
* 전송 중 사용자 취소
* 전송 중 기기 잠금
* USB debugging authorization 해제
* Android 저장공간 부족
* `adb` process 강제 종료

측정 항목:

* 총 전송 시간
* 평균 MB/s
* 파일 수 기준 처리량
* CPU 사용률 체감 또는 샘플
* UI freeze 여부
* 실패 후 queue 상태

---

## 10. 알려진 리스크

* ADB 사용은 일반 사용자에게 MTP보다 설정 부담이 크다.
* 일부 Android 기기에는 `tar`가 없거나 옵션 호환성이 다를 수 있다.
* Android 11+ scoped storage와 OEM 정책 때문에 `adb shell` 사용자가 shared storage 일부 경로에 쓰지 못할 수 있다.
* `adb` 배포 방식은 제품 리스크다. Mac App Store 배포, 앱 sandboxing, 코드 서명/notarization, platform-tools 설치 요구 중 어떤 방식을 택하느냐에 따라 사용자 진입 장벽과 배포 가능성이 달라진다.
* Tar extraction 중 취소되면 대상 폴더에 부분 파일이 남을 수 있다.
* 대상 conflict를 정확히 사전 계산하려면 추가 listing 비용이 든다.
* ADB와 MTP를 동시에 사용하면 기기 상태와 사용자 기대가 복잡해질 수 있다.
* USB debugging을 요구하는 기능은 보안 설명이 부실하면 제품 신뢰를 떨어뜨린다.

---

## 11. MVP 반영 기준

ADB 고속 업로드를 macOS MVP에 포함하려면 다음 기준을 모두 만족해야 한다.

* 기존 MTP 전송 안정화가 후퇴하지 않음
* ADB 기능은 첫 사용 opt-in이며 fallback과 추천 선택값이 명확히 구분됨
* 취소와 실패 상태가 기존 queue model과 일관됨
* 실기기 테스트에서 hang, ambiguous completion, child process leak이 없음
* 성능 개선이 실제 측정으로 확인되고, 개선 폭이 부족한 파일 구성에서는 ADB를 추천 선택값으로 두지 않음
  * **주의 (Phase 0 결과)**: 작은 파일 묶음(2000×4KiB)의 device-side `tar -x` 처리량이 검증 기기에서 0.43 MiB/s로 나왔고, 격리 측정 결과 병목은 USB나 ADB 자체가 아니라 device-side inode 생성·fsync 비용이었다. "MTP 대비 3배" 기준 달성 여부는 MTP 베이스라인 측정 전까지 보류이며, 실패 시 ADB 경로는 추천 선택값이 아닌 실험적 기능으로 격하될 수 있다.
* 사용자가 USB debugging의 의미를 이해할 수 있는 UI/문서가 있음

위 기준을 만족하지 못하면 ADB 고속 업로드는 “실험적 기능” 또는 post-MVP 후보로 유지한다.
