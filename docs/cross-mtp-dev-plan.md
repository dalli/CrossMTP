# CrossMTP 개발 계획서 (macOS MVP 마감 기준)

## 1. 문서 목적

이 문서는 CrossMTP의 macOS MVP를 확실하게 완료하기 위한 현재 기준 계획서다.

현재 macOS MVP의 제품 방향은 **Tauri + React 전용 탐색기 UI와 Rust/libmtp 전송 백엔드**를 안정화하는 것이다. 과거에 검토한 Finder 연동(FUSE/VFS 마운트) 방식은 매력적인 후속 후보지만, 현재 MVP 완료 범위에는 포함하지 않는다.

핵심 목표는 다음과 같다.

1. macOS에서 Android MTP 기기를 안정적으로 감지하고 탐색한다.
2. 업로드, 다운로드, 드래그 앤 드롭, 진행률, 취소, 충돌 처리를 일관된 상태 모델로 제공한다.
3. 케이블 분리, 화면 잠금, MTP 권한 미승인 등 실패 상황에서 앱이 멈추거나 완료/실패 상태가 섞이지 않게 한다.
4. 실기기 테스트 체크리스트를 릴리스 기준으로 삼는다.

---

## 2. 제품 방향

### macOS MVP는 전용 앱 방식으로 완료한다

현재 구현은 다음 구조를 이미 갖추고 있다.

1. React 기반 로컬/기기 브라우저
2. Tauri command/event bridge
3. Rust Transfer Orchestrator
4. Rust MTP Session Layer
5. 시스템 `libmtp`

따라서 macOS MVP 마감은 새로운 파일시스템 계층을 도입하는 작업이 아니라, 이미 구현된 전송 앱을 신뢰 가능하게 만드는 작업이다.

### FUSE/Finder 마운트는 후속 후보로 보류한다

FUSE 방식은 Finder와 자연스럽게 통합되는 장점이 있지만, macOS 권한, macFUSE 의존성, Finder의 대량 메타데이터 I/O, POSIX와 MTP 의미 차이, 삭제/쓰기 실패 표현 한계 때문에 MVP 마감 리스크가 크다.

후속 검토 조건:

* 현재 macOS MVP에서 전송 상태 모델과 에러 모델이 충분히 검증되었을 것
* 실제 사용자 테스트에서 전용 UI 방식의 한계가 제품 가치에 치명적이라고 확인될 것
* macFUSE 설치/권한 플로우와 Finder I/O 방어 전략을 별도 프로토타입으로 검증할 것

---

## 3. macOS MVP 범위

### 포함 기능

* 단일 Android MTP 기기 연결 감지
* 기기 정보와 저장소 표시
* 기기 디렉토리 탐색
* 로컬 디렉토리 탐색
* 기기 -> 로컬 다운로드
* 로컬 -> 기기 업로드
* 파일/폴더 드래그 앤 드롭 업로드
* 폴더 업로드 시 내부 폴더 생성 및 기존 같은 이름 폴더 merge
* 진행률 이벤트 표시
* 사용자 취소
* 충돌 정책
  * `Skip`
  * `Rename`
  * `Overwrite`는 다운로드에 한정
* 실패 원인을 이해할 수 있는 에러 메시지
* 부분 다운로드 파일 정리

### 제외 기능

* Finder 마운트 / FUSE / macFUSE 연동
* 이름 변경
* 삭제
* 사용자용 새 폴더 버튼
* 미디어 미리보기 / 썸네일
* 다중 기기 동시 연결
* 백그라운드 자동 재연결
* Windows 구현
* Linux 구현
* 코드 서명 / notarization

---

## 4. 아키텍처 원칙

### 4.1 MTP Session Layer

역할:

* `libmtp` 초기화
* 기기 열기와 해제
* 저장소 조회
* 디렉토리 목록 조회
* 파일 다운로드
* 파일 업로드
* 폴더 생성
* libmtp 에러를 CrossMTP 에러 모델로 정규화

원칙:

* `libmtp` handle은 thread-safe로 가정하지 않는다.
* `Device`는 한 worker가 소유하고, 동시에 여러 thread에서 접근하지 않는다.
* 플랫폼 capability는 실제 구현된 기능만 노출한다.

### 4.2 Transfer Orchestrator Layer

역할:

* 단일 active worker 유지
* 큐 순차 처리
* 상태 전이 이벤트 발행
* 진행률 이벤트 발행
* 취소 flag 관리
* 충돌 정책 적용
* 실패 시 큐 종료/일시정지 정책 결정

필수 상태:

* `queued`
* `validating`
* `transferring`
* `cancelling`
* `completed`
* `failed`
* `cancelled`
* `skipped`

원칙:

* 전송 완료, 실패, 취소 상태가 모호하게 섞이면 안 된다.
* 기기 연결 문제로 queue를 pause할 때, 재개 후에도 사용자가 해당 작업을 취소할 수 있어야 한다.
* 자동 복구를 구현하지 않은 상태에서 자동 복구처럼 보이는 문구를 쓰지 않는다.

### 4.3 Presentation Layer

역할:

* 연결 상태와 권한 안내 표시
* 기기/로컬 브라우저 표시
* 큐와 진행률 표시
* 사용자 취소 전달
* 충돌 정책 선택
* Tauri event를 UI 상태로 반영

원칙:

* UI는 전송 상태의 진실 원천이 아니다. 상태 원천은 orchestrator event다.
* 큐가 pause된 경우 사용자가 이어서 전송할지, 큐를 비울지 명확히 선택할 수 있어야 한다.
* 기기가 없거나 권한이 없을 때 드롭/전송 버튼은 실패 이유를 예측 가능하게 보여줘야 한다.

---

## 5. 실패 시나리오 기준

macOS MVP 완료 전 반드시 다룰 시나리오:

* 케이블 분리 중 전송
* 화면 잠금 중 탐색 또는 전송
* MTP 권한 미승인
* 동일 파일명 충돌
* 로컬 대상 폴더 없음
* 로컬 대상 폴더 권한 부족
* 기기 저장공간 부족
* 전송 중 사용자 취소
* 앱 종료 또는 interruption

기대 결과:

* UI freeze 없음
* 무한 대기 없음
* 부분 다운로드 파일 정리
* 큐 상태 불일치 없음
* 사용자가 다음 조치를 이해할 수 있는 메시지 제공

---

## 6. 테스트 전략

### 6.1 자동 테스트

자동 테스트는 하드웨어 없이 검증 가능한 영역을 맡는다.

우선순위:

* 상태 helper
* 로컬/원격 rename 정책
* queued 상태 취소
* queue pause 상태 표시
* no-device 상태에서 read-only command 실패
* path/name 처리 유틸리티

자동 테스트가 증명하지 못하는 것:

* 실제 기기별 libmtp 동작
* USB daemon 선점 문제
* 케이블 분리 타이밍
* 화면 잠금 중 기기별 동작 차이
* 저장공간 부족 에러의 실제 libmtp 문구

### 6.2 실기기 테스트

릴리스 후보는 `docs/test-checklist.md`를 기준으로 판단한다.

최소 통과 기준:

* 최소 2종 이상의 Android 기기에서 연결 및 저장소 조회 성공
* 작은 파일 업로드/다운로드 성공
* 100MB 이상 업로드/다운로드 성공
* 1GB 이상 다운로드에서 UI freeze 없음
* 전송 중 취소 성공
* 전송 중 케이블 분리 시 앱 hang 없음
* MTP 권한 미승인 상태에서 사용자가 원인을 이해할 수 있음

---

## 7. 현재 마감 로드맵

### Phase A. 문서 정합성 정리

목표:

* macOS MVP 방향을 현재 구현과 일치시킨다.
* FUSE/Finder 마운트 계획은 후속 후보로 분리한다.

통과 기준:

* README, 개발 계획서, 테스트 체크리스트가 서로 다른 MVP를 말하지 않는다.

### Phase B. 큐/상태 안정화

목표:

* pause/resume/cancel 경로의 상태 불일치 제거
* retry queue에 남은 작업도 취소 가능하게 유지
* 부분 다운로드 정리 정책 유지

통과 기준:

* queued 작업 취소가 terminal `cancelled`로 끝난다.
* device error로 pause된 작업은 재개 전/후 취소 가능하다.
* 실패 후 같은 job id가 UI에서 사라지거나 제어 불가능해지지 않는다.

### Phase C. 자동 테스트 보강

목표:

* 하드웨어 없이 검증 가능한 상태/충돌 로직 테스트를 추가한다.

통과 기준:

* `cargo test --workspace` 통과
* `npm run build` 통과
* 새 테스트가 최소한 queue/cancel/rename helper를 커버한다.

### Phase D. 실기기 안정화

목표:

* 수동 테스트 체크리스트를 실제 기기로 채운다.
* 실패 시나리오의 미검증 항목을 알려진 제한사항으로 분리한다.

통과 기준:

* `docs/test-checklist.md`의 release candidate 필수 항목 충족
* 미충족 항목은 릴리스 차단/비차단 여부가 명확히 기록됨

### Phase E. 배포 준비

목표:

* unsigned macOS beta 배포 기준을 명확히 한다.
* libmtp 설치와 macOS USB daemon 종료 안내를 사용자가 따라할 수 있게 한다.

통과 기준:

* 새 macOS 환경에서 설치 전제조건을 이해할 수 있음
* 앱 실행 전/후 문제 해결 문서가 README에서 연결됨

---

## 8. macOS 이후 확장 전략

### Linux

조건:

* macOS MVP에서 Session Layer와 Transfer Orchestrator의 상태 모델이 안정화되었을 것
* Linux 권한/udev 문제를 별도 문서와 설치 플로우로 다룰 준비가 되었을 것

접근:

* `libmtp` 계열 backend를 재사용하되, 권한과 패키징은 Linux 전용으로 분리한다.
* UI와 orchestrator는 가능한 한 재사용한다.

### Windows

조건:

* macOS MVP에서 사용자 가치가 검증되었을 것
* Linux 확장 여부와 무관하게 Windows backend 범위를 별도 산정할 것

접근:

* Windows는 `libmtp` 포팅이 아니라 WPD backend 추가로 본다.
* 공통 계층은 UI와 Transfer Orchestrator이며, Session Layer는 Windows 전용 구현이 필요하다.

---

## 9. 출시 판단 기준

macOS MVP 출시 가능 조건:

* 기본 탐색, 업로드, 다운로드, 취소, 충돌 처리가 실기기에서 확인됨
* 전송 중 실패가 ambiguous completion으로 보이지 않음
* 케이블 분리와 권한 미승인 상황에서 앱이 멈추지 않음
* README와 troubleshooting 문서만으로 기본 설치/실행이 가능함

출시 불가 조건:

* 완료/실패 상태가 뒤섞임
* 전송 중 취소가 재현성 있게 먹지 않음
* 기기 분리 후 UI가 무한 대기함
* 동일 이름 충돌에서 실제 결과와 UI 표시가 다름

---

## 10. 요약

CrossMTP의 macOS MVP는 현재 구현된 전용 앱 방식을 기준으로 마감한다.

우선순위는 다음 순서다.

1. 문서와 실제 구현 방향을 일치시킨다.
2. 전송 상태 머신과 queue/cancel 경로를 안정화한다.
3. 자동 테스트로 하드웨어 없는 로직을 고정한다.
4. 실기기 테스트 체크리스트로 릴리스 여부를 판단한다.
5. FUSE/Finder 마운트는 macOS MVP 이후 별도 검증 후보로 관리한다.
