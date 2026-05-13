# Phase 3 회고 — macOS UI (Tauri + React)

작성일: 2026-04-13
담당: Lead, Frontend, Backend (Devil's Advocate 점검 포함)

## 목표 (계획서 기준)

* 최소 탐색 UI 구현
* 전송 큐 UI
* 드래그 앤 드롭 업로드 UX
* 오류/가이드 메시지 UX 반영

산출물: 연결 상태 화면, 디렉토리 브라우저, 전송 상태 패널
통과 기준: 비개발자도 단일 기기 연결 후 업로드/다운로드 수행 가능

## 산출물

### 새 디렉토리: `apps/desktop/`
* **frontend** (React 18 + TypeScript + Vite 5)
  * `index.html`, `vite.config.ts`, `tsconfig.json`, `package.json`
  * `src/main.tsx`, `src/App.tsx`, `src/types.ts`, `src/styles.css`
  * `src/components/Banner.tsx` — 연결 상태 + destination 선택 + 충돌 정책 picker
  * `src/components/Browser.tsx` — breadcrumb, 디렉토리 entries, 드롭존
  * `src/components/QueuePanel.tsx` — JobCard with state badge, progress bar, cancel button
* **Tauri Rust shell** (`src-tauri/`)
  * `Cargo.toml`, `tauri.conf.json`, `build.rs`, `capabilities/main.json`, `icons/icon.png` (placeholder)
  * `src/main.rs` (entry), `src/lib.rs` (~530 LOC bridge)

### 워크스페이스 변경
* `apps/desktop/src-tauri`을 Cargo workspace member로 추가
* `mtp-session`/`orchestrator`를 path dependency로 연결

### Tauri ↔ Orchestrator 브릿지 (`apps/desktop/src-tauri/src/lib.rs`)
명령:
* `device_snapshot()` — 기기 + storage 목록 fetch + Orchestrator spawn + event pump 시작
* `list_entries(storage_id, parent_id)` — 폴더 listing
* `enqueue_download(args)` / `enqueue_upload(args)` — JobSpec → orchestrator
* `cancel_job(job_id)` — orchestrator cancel
* `default_dest_dir()` — `~/Downloads` 기본값
* `pick_dest_dir()` — `tauri-plugin-dialog`로 폴더 선택

이벤트 파이프라인:
```
mtp-session::Device
   ↓ owned by
orchestrator::Worker thread (single)
   ↓ Event channel
crossmtp-event-pump thread
   ↓ app.emit("transfer-event", payload)
React state via @tauri-apps/api/event listen()
```

서비스 상태:
* `AppState { inner: Mutex<Option<DeviceBridge>>, last_snapshot: ... }`
* `DeviceBridge { orchestrator: Arc<Orchestrator>, info, storages }`
* `device_snapshot` 호출 시 매번 bridge를 drop하고 재구성 — transient 단절 시 stale state 방지.

JobState → WireState `From` impl로 직렬화. camelCase serde rename으로 React 타입과 1:1 매칭.

### React UI 결정
* **Dual-pane 거부**: 계획대로 single-pane (browser 좌, queue 우 360px). 양쪽 device 동시 탐색은 MVP 외.
* **드롭존 + Tauri native drag-drop**: HTML5 drop API는 webview 내부에서 실제 file path를 노출하지 않음. Tauri 2의 `tauri://drag-drop` 이벤트로 path 수신 + dropzone 시각만 HTML5로.
* **충돌 정책 글로벌 setter**: per-job confirm dialog 대신 banner의 select. MVP 단순성 우선.
* **에러 메시지 가이드**: `permission_hint`이 true이면 banner에 "Image Capture/AFT 종료" 안내.
* **상태별 색상**: queued/validating=회색, transferring=blue, completed=green, failed=red, skipped=yellow, cancelled=gray. AGENTS.md "skipped는 사용자 결정의 결과" 원칙 반영.

## 통과 기준 vs 실측

| 기준 | 결과 |
|---|---|
| 연결 상태 화면 | ✅ Banner.tsx, connected/disconnected 분기 + permissionHint 가이드 |
| 디렉토리 브라우저 | ✅ Browser.tsx, breadcrumb + double-click 진입 + 정렬(폴더 우선) |
| 업로드/다운로드 액션 버튼 | ✅ entry row마다 download/열기 버튼 |
| 드래그 앤 드롭 영역 | ✅ dropzone + Tauri native drag-drop event 청취 |
| 전송 큐 패널 | ✅ QueuePanel.tsx, JobCard 렌더 + state badge + progress bar + cancel |
| 진행률/실패 메시지 | ✅ JobCard.meta + .err 영역 |
| Frontend 빌드 | ✅ `npm run build` 성공 (152 KB JS, 4 KB CSS) |
| Rust shell 빌드 | ✅ `cargo build -p crossmtp-desktop` 성공 |
| **GUI 런타임 시각 검증** | ⚠️ **미검증**. 본 세션은 헤드리스, 윈도우를 띄울 수 없음. 사용자가 `npm run tauri dev`로 직접 확인 필요. |
| 비개발자도 단일 기기 연결 후 업로드/다운로드 가능 | ⚠️ 위와 동일 사유로 미검증 |

## 의사결정 기록

1. **Tauri 2.x 사용**. 1.x 대비 plugin 시스템 정비됨, 권한 모델 명확. 약간 신규지만 안정.
2. **`@tauri-apps/cli` 로컬 npm 설치** (vs `cargo install`). 설치 시간 단축 + 프로젝트 내 버전 고정 + dev/build script가 npm 안에서 일관.
3. **Listing은 orchestrator 외부에서**. orchestrator는 transfer 전담, listing은 별도 짧은-수명 device open으로 처리. 대안: worker에 listing request 채널 추가 → 한 번에 한 device만 쓰는 single-owner 보장. **반대론자가 우려한 race risk가 있어 Phase 4 첫 항목으로 이월.**
4. **`device_snapshot` 매 호출 재구성**. 안전하지만 비싸다. 충분한 사용성 데이터 모이면 cached fast-path 추가.
5. **icon.png는 placeholder** (1024×1024 단색 PNG 5.7 KB). 정식 아이콘은 베타 직전.
6. **`#[allow(dead_code)]` 두 곳**: WireKind::Upload (Enqueued 이벤트가 kind 정보를 안 가져옴 → 향후 Phase 4에서 Event 확장 시 unused 풀림), DeviceBridge.info/storages (캐시지만 현재 React가 매번 재요청).

## 반대론자 코멘트

> 1. **listing이 device를 두 번 open한다**. `device_snapshot`이 한 번, `list_entries`가 매번. 같은 process에서 짧은 간격으로 두 번 open하면 macOS USB daemon race가 재발할 수 있음. Phase 1에서 정확히 이것 때문에 verify-q에서 in-process 단일 device handle이 필수였다. **Phase 4 첫 fix**: orchestrator에 `Cmd::ListEntries { storage_id, parent_id, reply: oneshot::Sender<Result<Vec<Entry>>> }`를 추가하고 frontend `list_entries` 명령은 그걸 통해 worker에서 같은 핸들을 사용.
> 2. **Enqueued 이벤트의 kind는 placeholder**. `OrchEvent::Enqueued { id }`에 JobKind를 포함하지 않아 React에 빈 Download가 전달됨. React는 invoke()의 return으로 JobView를 만들고 enqueued event는 ordering 신호로만 쓰지만, 멀티-소스 enqueue 시 race 가능. orchestrator의 Event::Enqueued에 spec 동봉하도록 확장 권장.
> 3. **`pick_dest_dir`이 std mpsc로 dialog blocking**. Tauri runtime thread가 dialog callback을 부르므로 데드락 가능성. tokio `oneshot` + async command로 변경 권장. (Phase 4)
> 4. **drop event 등록은 한 번뿐**. 그래서 `uploadFiles`를 ref로 우회. 정공법은 Tauri side에서 drop을 받아 React에 forward하면서 ref hack 제거. 작동은 함.
> 5. **error 모달이 없음**. browserError state 하나로 inline 표시만. 같은 error가 여러 번 발생하면 사용자에게 잘 안 보임. toast 시스템 도입 검토.
> 6. **취소 버튼이 cancelling 동안 disable되지 않음**. 더블 클릭으로 cancel 메시지 두 번 보내도 idempotent라 OK이지만, UI 상 혼란 가능.
> 7. **드래그앤드롭 영역이 항상 활성**. 기기 미연결 상태에서 드롭하면 enqueue가 실패하고 inline error가 뜸. 비활성화 또는 disabled 시각 추가 권장.
> 8. **`tauri.conf.json`의 `csp: null`**. 개발 편의로 CSP를 끈 상태. 베타 전 strict CSP 도입.
> 9. **번들 아이콘이 placeholder**. macOS dock에 단색 사각형이 뜸. 베타 전 교체.
> 10. **GUI 미검증**. 가장 큰 리스크. 코드는 컴파일되고 모든 호출 경로가 타입 안전하지만 실제 click flow를 사용자가 한 번도 못 해봤음. Phase 4 초기에 사용자 + 개발자 합동 클릭스루 필요.

## 알려진 제약 (Known Limitations)

* GUI 런타임 시각 검증 미수행
* Listing이 별도 device open (race risk)
* Enqueued 이벤트가 JobKind 미동봉
* Drop 영역 항상 활성, 기기 없을 때 비활성 표시 없음
* CSP 꺼짐 (개발 단계)
* 아이콘 placeholder
* 다중 기기 UI 없음 (orchestrator도 1-device)
* 폴더 선택 dialog가 blocking std mpsc 사용

## Phase 4 (검증/안정화) 인수 항목

1. **GUI clickthrough**: dev 모드 실행 후 비개발자 시나리오 1회 종주
2. **Listing → orchestrator 채널화** (반대론자 #1)
3. **100 MB+ / 1 GB+ 파일** 진행률 + cancel 검증
4. **연속 100개 small files** 큐 stress
5. **케이블 분리** 시나리오 (전송 중)
6. **화면 잠금** 시나리오 (전송 중)
7. **한글 파일명 / 깊은 경로**
8. **부분 다운로드 cleanup**
9. **`pick_dest_dir` async 변환** (반대론자 #3)
10. **error toast 시스템** (반대론자 #5)
11. **반대론자 10건 전부 매핑**

## 사용 방법

### 개발 모드 (frontend HMR + Tauri shell)
```bash
# 1. 데몬 정리 (macOS)
killall "Android File Transfer" "Android File Transfer Agent" icdd 2>/dev/null

# 2. 폰: 잠금 해제 + USB → 파일 전송(MTP) 선택

# 3. 데스크톱 앱 실행
cd apps/desktop
npm run tauri dev
```

### 프로덕션 빌드 (.app 번들)
```bash
cd apps/desktop
npm run tauri build
# 결과: src-tauri/target/release/bundle/macos/CrossMTP.app
```

### 빌드만 검증 (앱 실행 없이)
```bash
cd apps/desktop && npm run build              # vite + tsc
cd ../.. && cargo build -p crossmtp-desktop   # Rust shell
```
