# Phase 4 회고 — 통합 테스트 및 안정화

작성일: 2026-04-13
담당: Lead, Backend, Frontend, QA, Devil's Advocate

## 목표 (계획서 기준)

* 실기기 반복 테스트
* 실패 시나리오 검증
* 메모리 누수, UI 프리징, 상태 꼬임 제거

산출물: 테스트 체크리스트, 알려진 제한사항 문서, MVP 릴리스 후보
통과 기준:
* 필수 실패 시나리오를 모두 재현하고 기대 결과 충족
* 주요 크래시와 무한 대기 현상 없음

## 이번 phase에서 한 일 (코드 + 검증)

### #1 listing → orchestrator worker 채널화 (긴급, GUI 첫 실행에서 PANIC)

**증상**: Tauri dev 첫 실행 시 device 더블 open으로 macOS USB 데몬과 race,
`LIBMTP PANIC: Unable to initialize device` 무한 반복. UI는 떴지만 기기 정보 0.

**원인**: `device_snapshot`이 `mtp-session::Session`에서 device를 한 번 open,
그 직후 `list_entries`가 또 새 Session에서 device를 open. 같은 process에서
짧은 간격의 두 open이 macOS의 image-capture daemon들과 경쟁.

**fix** (`crates/orchestrator/src/lib.rs`, `apps/desktop/src-tauri/src/lib.rs`):
* `Cmd::ListEntries { storage_id, parent_id, reply }`, `Cmd::ListStorages { reply }` 추가
* `Orchestrator::list_entries` / `list_storages`로 worker에 위임 (oneshot reply)
* Worker `handle_cmd`가 자기 소유 `self.device`로 처리
* Tauri `list_entries` command가 bridge.orchestrator를 통과
* `device_snapshot`에 `force` 파라미터, 평소엔 기존 bridge 재사용 (storages만 갱신), 명시적 새로고침 시에만 재구성

**효과**: 재실행 시 PANIC 0회. UI에서 폴더 진입/breadcrumb/다운로드/업로드 모두 정상.
**Phase 3 retro 반대론자 #1 흡수.**

### #2 Enqueue event payload

**증상**: 다운로드 버튼 → 큐 카드의 파일명이 비어 있음. 사용자가 무엇을
다운로드하는지 알 수 없음.

**원인**: `Event::Enqueued { id }`만 emit, JobKind 누락. Tauri lib이 placeholder
WireKind::Download (빈 문자열)을 만들어 forward.

**fix**: `Event::Enqueued { id, kind: JobKind }` 확장 + `From<JobKind> for WireKind`로
1:1 매핑. **Phase 3 retro #2 흡수.**

### #3 부분 다운로드 cleanup

**증상 (가설)**: cancel/Failed 시 partial 파일이 dest_dir에 남음.

**fix**: `Worker::execute`의 download 경로에서 `res.is_err() && download_path.exists()`
이면 `std::fs::remove_file`. 사용자 검증 ✅ — 10 MB+ 파일 cancel 후 잔존 파일 없음.
**Phase 2 retro #2 흡수.**

### #4 progress total=0 보호

* React `JobCard.tsx`가 이미 `total > 0 ? job.total : expectedSize`로 fallback.
* expectedSize는 #2 fix로 enqueued event를 통해 들어오므로 신뢰 가능.
* **Phase 2 retro #1 흡수.**

### #5 pick_dest_dir async 변환

**증상**: dialog 버튼 → blocking std mpsc recv가 Tauri runtime thread를 점유하면
deadlock 위험.

**fix**: tokio dependency 추가, `pick_dest_dir` async, `tokio::task::spawn_blocking`
으로 recv를 off-runtime thread로. 사용자 검증 ✅ — picker 정상 open/close.
**Phase 3 retro #3 흡수.**

### #6 dropzone disabled state

* `storage` prop이 null이면 `.dropzone.disabled` 클래스 추가, drag/drop 핸들러 no-op.
* 메시지도 "기기가 연결되면 여기로 파일을 드래그할 수 있습니다"로 분기.
* **Phase 3 retro #7 흡수.**

### UI 다듬기 (사용자 요청)
* 큐 카드에 색상 dir-badge: 파란 원 ↓ (download), 초록 원 ↑ (upload)
* 다운로드 버튼: "↓ 다운로드"
* 드롭존: "↑ 파일을 이 영역으로..."

### 테스트 체크리스트 (`docs/test-checklist.md`)
A~G 7개 카테고리 60+ 항목. **release candidate 통과 기준**과 표시.

## 통과 기준 vs 실측

### 사용자 검증 결과 (2026-04-13)

| 항목 | 결과 |
|---|---|
| **B4** 다운로드 중 취소 → 부분 파일 자동 삭제 | ✅ |
| **D1** 전송 중 케이블 분리 → Failed, hang 없음 | ✅ |
| **D2** 전송 중 폰 화면 잠금 → graceful, hang 없음 | ✅ |
| **F1** 다운/업 색상 배지 시각 구분 | ✅ |
| Enqueue payload — 큐 카드에 파일명 정상 표시 | ✅ |
| dialog picker — deadlock 없음 | ✅ |

→ **AGENTS.md 핵심 실패 시나리오 4건 (취소·케이블·잠금·partial cleanup) 전부 통과.**

### 통과 기준 vs 실측

| 통과 기준 | 결과 |
|---|---|
| 필수 실패 시나리오 재현 + 기대 결과 충족 | ✅ 위 4건 |
| 주요 크래시 없음 | ✅ Phase 4 #1 fix 후 PANIC 0건 |
| 무한 대기 없음 | ✅ 케이블 분리/잠금 시 graceful Failed |
| 테스트 체크리스트 산출 | ✅ `docs/test-checklist.md` |
| 알려진 제한사항 문서 | ✅ 본 회고 + 체크리스트 |
| MVP 릴리스 후보 | ✅ Phase 5 (배포 준비) 진입 가능 |

### 미검증 (정직 표기)
체크리스트의 다음 항목은 본 phase에서 사용자 확인하지 않음:

* B2/B3 — 100 MB+, 1 GB+ 파일 progress 단조성 + 메모리 안정성
* B6/C6 — 한글 파일명
* B7/B8 — dest_dir 미존재/권한 없음
* C8 — 여러 파일 동시 드롭
* D3/D4/D5 — MTP 권한 미승인 / storage full / 부적합 경로 문자
* E1/E2/E3 — 10/100개 stress, 혼합 큐
* G1~G3 — 번들 + 새 환경 설치 (Phase 5 진행 시 자연스럽게 검증됨)

위 항목들은 Phase 5 또는 RC 후보 검증 단계에서 채워야 함.

## 의사결정 기록

1. **listing을 worker로 옮긴 게 정답**. 대안(다른 Mutex로 직렬화)은 같은 device를 두 번 open하는 근본 race를 못 막음. orchestrator가 단일 owner라는 원칙이 phase 4 #1 fix로 비로소 진정한 의미를 가짐.
2. **dialog는 tokio spawn_blocking**. tauri 2가 이미 tokio runtime 위에서 돌므로 추가 비용 거의 0.
3. **partial cleanup은 download만**. upload 부분 파일 cleanup은 device 측에서 일어나야 하는데 libmtp가 send 도중 cancel 시 어떤 상태로 남기는지 일관성 없음 (기기마다 다름). MVP는 download cleanup만 보장하고 upload는 사용자 새로고침으로 확인.
4. **체크리스트 통과 기준을 작게**. "release candidate" 자격은 *치명적 실패 시나리오*만 강제. 한글/100개 stress는 known limitation으로 분류하되 Phase 5 빌드 후 빠르게 보강.

## 반대론자 코멘트

> 1. **사용자가 '모두 통과'라고만 답한 케이스를 그대로 ✅로 적었다.** 상세 결과 (예: 정확한 partial 파일 삭제 시점, cable disconnect 후 다음 작업 enqueue 가능 여부) 미기록. release 직전에 한 번 더 동일 시나리오를 수치 포함해 재검증 권장.
> 2. **케이블 분리 시 worker thread가 hang하지 않음을 어떻게 확신하는가**. UI가 살아있다는 건 worker가 죽은 게 아닐 수도 있고 (libmtp가 마침 timeout으로 풀린 것일 수도). cancel 시 추가 thread leak 없음을 확인하려면 `ps -M`로 thread 수 검사 필요.
> 3. **B2 (100 MB+) 미검증**. progress callback이 작은 파일에서 호출되지 않는다는 게 Phase 2 retro에서 이미 알려짐. 100 MB 검증 없이 progress가 잘 보인다고 단정 못 함. **Phase 5 진입 전 1회는 강력 권장.**
> 4. **listing이 transferring 중에는 큐 뒤에 막힌다**. 사용자가 1 GB 다운로드를 시작하고 다른 폴더 진입을 시도하면 응답 없는 것처럼 보일 수 있음. UI가 "기다리는 중" 표시를 해야 함. (Phase 5 또는 그 이후)
> 5. **5개 retro에서 누적된 미검증 항목이 많음**. release 후 첫 사용자 보고로 새 시나리오 발견 가능성 높음. CHANGELOG / known issues 페이지를 베타 발표 시 명시.
> 6. **`device_snapshot` no-force fast-path가 storages만 갱신하지만 device 자체가 변경된 케이스 (다른 폰 swap)는 감지 못 함**. 사용자가 새로고침 누르면 force=true로 처리되니 운영상 OK이지만 자동 감지 없음.
> 7. **tauri 2 dialog 결과 type annotation**. dev 모드에서 일시 컴파일 에러가 떴다가 Cargo.toml 변경으로 우연히 사라진 정황. 실제로는 Tauri의 FilePath::to_string()이 Display impl로 disambiguate 됐을 가능성. 안정성 위해 명시 type 권장 (작은 fix).

## 알려진 제한사항 (Known Limitations, RC 시점)

* 100 MB+ / 1 GB+ 파일 progress + 메모리 사용량 미검증
* 한글/유니코드 파일명 미검증
* 10/100 파일 stress 미검증
* upload 부분 파일 cleanup 부재 (device 측, libmtp 한계)
* 다중 기기 미지원
* delete/rename/mkdir 미지원 (MVP 범위 밖)
* macOS daemon (icdd, AFT) 자동 종료 부재 — 사용자가 직접 killall
* 1.1.23 외 libmtp 버전 미검증
* arm64 Homebrew 경로 하드코딩 — Intel mac 미고려
* 케이블 분리 후 worker thread leak 가능성 (반대론자 #2, 미검증)
* 코드 sign / notarization 미진행 (Phase 5 책임)

## Phase 5 (배포 준비) 인수 항목

1. **`cargo tauri build`로 .app 번들 생성 + 동작 확인**
2. **새 macOS 사용자 가이드 (README + DEPLOY.md)**
   * `brew install libmtp` 안내
   * macOS daemon (icdd, AFT, Image Capture) 종료 안내
   * 폰 USB 모드 선택 안내
3. **첫 실행 시 libmtp 미설치 감지 + 안내 다이얼로그**
4. **README 재정비** (Phase 0 시점 README 없음, Phase 5에서 작성)
5. **반대론자 #3 (100 MB+) 1회 검증** — Phase 5 빌드 후 RC 확정 직전
6. **반대론자 #1 (사용자 검증 디테일 재확인)** — RC 확정 직전
7. 코드 sign / notarization은 Phase 5에서 결정 (베타 단계는 unsigned 가능)

## 사용 방법 (현 시점)

### 개발 모드
```bash
killall "Android File Transfer" "Android File Transfer Agent" icdd 2>/dev/null
cd apps/desktop && npm run tauri dev
```

### CLI verify (orchestrator E2E)
```bash
cargo run -p mtp-cli -- verify-q
```

### 빌드 검증
```bash
cd apps/desktop && npm run build
cargo build --workspace
```
