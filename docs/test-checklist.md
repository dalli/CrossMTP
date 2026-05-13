# CrossMTP 수동 테스트 체크리스트 (Phase 4 통합 안정화)

CI/유닛 테스트만으로는 MTP 동작 정합성을 보장할 수 없습니다 (AGENTS.md 정책).
릴리스 후보 자격 = 본 체크리스트 통과.

---

## 환경

* macOS arm64 (Apple Silicon)
* Homebrew libmtp ≥ 1.1.23
* Android 기기 1대, USB 데이터 케이블, 충분한 free space (≥ 2 GB)
* macOS 데몬 사전 정리: `killall "Android File Transfer" "Android File Transfer Agent" icdd 2>/dev/null`

각 항목 옆에 결과 ([✅/⚠️/❌] + 메모)를 적습니다.

---

## A. 기본 연결/탐색

| ID | 시나리오 | 기대 결과 | 결과 |
|---|---|---|---|
| A1 | 앱 실행 후 자동 device snapshot | 배너 connected, 첫 storage 표시 | |
| A2 | 폴더 더블클릭 진입 | 하위 entries 표시, breadcrumb 갱신 | |
| A3 | breadcrumb 클릭으로 상위 이동 | 해당 폴더 listing | |
| A4 | 새로고침 버튼 (force=true) | bridge 재구성 후 동일 결과 | |
| A5 | 폰 화면 잠금 후 새로고침 | 명확한 에러 메시지 + permissionHint | |
| A6 | 케이블 분리 후 새로고침 | "기기 없음" 배너, 무한 대기 없음 | |

## B. 다운로드

| ID | 시나리오 | 기대 결과 | 결과 |
|---|---|---|---|
| B1 | 작은 파일 (< 1 MB) 다운로드 | Queued→Validating→Transferring→Completed, 파일 on-disk 크기 일치 | |
| B2 | 큰 파일 (> 100 MB) 다운로드 | progress 이벤트가 단조 증가, total > 0, 100% 도달 후 Completed | ✅ 2026-04-13 (3 GB로 우회 검증) |
| B3 | 1 GB+ 파일 다운로드 | UI 프리징 없이 progress 갱신 지속, 메모리 안정 | ✅ 2026-04-13 사용자 3 GB 확인 |
| B4 | 다운로드 중 취소 (transferring 상태에서) | Cancelled 전이, 부분 파일 자동 삭제 (PC fs 확인) | ✅ 2026-04-13 사용자 확인 |
| B5 | 다운로드 중 취소 (queued에서 시작 전) | 즉시 Cancelled, 큐에서 빠짐 | |
| B6 | 한글 파일명 다운로드 | 파일명 깨지지 않음, on-disk 일치 | |
| B7 | dest_dir 미존재 | orchestrator가 mkdir_all로 생성 | |
| B8 | dest_dir 권한 없음 | 명확한 Failed 이유 표시 | |

## C. 업로드

| ID | 시나리오 | 기대 결과 | 결과 |
|---|---|---|---|
| C1 | 작은 텍스트 파일 드래그앤드롭 업로드 | Completed, 새로고침 후 폴더에 보임 | |
| C2 | 큰 파일 (> 100 MB) 업로드 | progress 단조 증가, 100% 후 Completed | |
| C3 | 같은 이름 두 번 업로드 (Skip 정책) | 두 번째는 Skipped, 디바이스에 단 1개 | |
| C4 | 같은 이름 두 번 업로드 (Rename 정책) | 두 번째는 새 이름 (suffix), 디바이스에 2개 | |
| C5 | 같은 이름 업로드 (Overwrite 정책) | upload는 Failed (MVP 미구현, 명확한 사유 표시) | |
| C6 | 한글 파일명 업로드 | 디바이스에서 한글로 보임 | |
| C7 | 업로드 중 취소 | Cancelled 전이, 디바이스에 부분 파일 정리 (libmtp 동작에 의존) | |
| C8 | 여러 파일 동시 드롭 | 큐가 순차 처리 (single active worker) | |

## D. 충돌/실패 시나리오 (필수)

| ID | 시나리오 | 기대 결과 | 결과 |
|---|---|---|---|
| D1 | 전송 중 케이블 분리 | Failed 명확 이유, 다음 작업이 큐에서 정상 동작 가능 | ✅ 2026-04-13 사용자 확인 |
| D2 | 전송 중 폰 화면 잠금 | Failed 또는 정상 완료 (기기별), UI hang 없음 | ✅ 2026-04-13 사용자 확인 |
| D3 | 폰의 MTP 권한 미승인 상태에서 시작 | StorageUnavailable + permissionHint | |
| D4 | 디바이스 free space 부족 (큰 파일) | Failed "storage full" | |
| D5 | 부적합 문자가 포함된 경로 | InvalidArgument | |
| D6 | 전송 중 앱 강제 종료 | 다음 실행에서 잔존 partial 파일 정리 (현재는 미구현) | |

## E. 큐/상태 일관성

| ID | 시나리오 | 기대 결과 | 결과 |
|---|---|---|---|
| E1 | 연속 10개 작은 파일 download enqueue | 모두 Completed, 상태 불일치 없음 | |
| E2 | 연속 100개 작은 파일 (stress) | 모두 Completed, 메모리 안정 | |
| E3 | 다운로드 + 업로드 혼합 큐 | 순차 처리, 충돌 없음 | |
| E4 | 큐 비어있을 때 cancel 무시 | no-op, 에러 없음 | |
| E5 | 종료/재시작 후 큐 비어있음 | persisted state 없음 (의도된 동작) | |

## F. UX

| ID | 시나리오 | 기대 결과 | 결과 |
|---|---|---|---|
| F1 | 큐 카드의 다운로드/업로드 배지 색상 구분 | 파랑 ↓ / 초록 ↑ | ✅ 2026-04-13 시각 확인 |
| F2 | progress total=0 케이스 | UI가 expectedSize fallback 사용, 0% 멈춤 없음 | |
| F3 | 기기 미연결 시 dropzone 비활성 시각 | opacity 낮음, 드롭 무시 | |
| F4 | 저장 폴더 picker | 다이얼로그 정상 open/close, deadlock 없음 | |
| F5 | error inline 표시 | 가독성 OK | |

## G. 번들/패키징 (Phase 5 입력)

| ID | 시나리오 | 기대 결과 | 결과 |
|---|---|---|---|
| G1 | `npm run tauri build` 정상 종료 | `.app` 번들 생성 | |
| G2 | 새 macOS 사용자 계정에서 `.app` 실행 | libmtp dylib 미설치 시 명확한 안내 또는 정상 동작 | |
| G3 | 첫 실행 가이드 (USB MTP 설정 + 데몬 안내) | 사용자가 1회 read로 이해 가능 | |

---

## 통과 기준 (release candidate)

* A1, A2, A3 ✅
* B1, B2, B4 ✅
* C1, C2, C3, C4 ✅
* D1, D3 ✅
* E1 ✅
* F1, F2, F3, F4 ✅

위가 전부 ✅이면 MVP RC로 본다. 나머지는 알려진 제한사항 문서로 흡수.
