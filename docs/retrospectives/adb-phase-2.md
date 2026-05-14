# ADB Phase 2 회고 — Tar Stream Builder + 통합

작성일: 2026-05-14
대상 계획: [docs/plan.md](../plan.md) §8 Phase 2, §4.3, §5, §6.1
선행 회고: [adb-phase-0.md](adb-phase-0.md), [adb-phase-1.md](adb-phase-1.md)

## 0. 진행 방식

Phase 1 retro의 "Phase 2 인수 항목" 6건을 입력으로 받아 다음 순서로 진행했다.

1. `tar-stream` 신규 crate 분리 (UI/Tauri 의존성 0, `thiserror`만 사용).
2. `adb-session`에 `tar_upload`/`manifest`/`device_caps` 모듈 추가.
3. `mtp-cli adb`에 `caps`/`manifest`/`tar-upload` 서브커맨드 추가.
4. 자동 테스트(stub-runner 기반) 작성 + 실기기(Xiaomi `ea33d2fe`) 라이브 검증.

검증 기기: Phase 1과 동일한 1대(Xiaomi `24117RK2CG` / Android 15 / toybox 0.8.11). 두 번째 기기 라운드는 Phase 5 입력으로 이월 (Phase 1 retro §5와 동일 결정).

## 1. 산출물

### 새 crate: `crates/tar-stream`

UI/Tauri/네트워크 의존성 없음. `thiserror`만 사용. workspace `Cargo.toml`에 등록.

```
crates/tar-stream/
├── Cargo.toml
└── src/
    ├── lib.rs        # 공개 표면 + re-export
    ├── error.rs      # TarError + Result
    ├── exclude.rs    # macOS metadata hard-exclude
    ├── path.rs       # TarPath: 검증된 상대 경로
    ├── sanitize.rs   # rename pattern + timestamp 안전화
    ├── progress.rs   # ProgressCounter / Snapshot
    ├── conflict.rs   # ConflictAction / ConflictPlan / RenameRule
    ├── traversal.rs  # lazy directory walk
    ├── header.rs     # USTAR + GNU LongLink
    └── stream.rs     # TarStreamBuilder (pipeline driver)
```

**핵심 API**

- `TarStreamBuilder::new(root).with_conflict_plan(plan).write_to(&mut sink)` — 디스크에 임시 tar 파일 없이 sink로 USTAR 바이트 스트림 직접 출력.
- `TarPath::new(p)` — 절대경로/`..`/NUL/빈 component 거부. 검증 후 `/`-구분 문자열 보장.
- `is_macos_metadata(name)` — `._*` prefix + `.DS_Store`/`.Spotlight-V100`/`.Trashes`/`.fseventsd` 정확 매치. 정책 토글 없음(default deny).
- `sanitize_tar_path` / `sanitize_timestamp` / `sanitize_rename_pattern` — FAT/Android shared storage 안전 문자, `:`/`<`/`>`/`"`/`|`/`?`/`*`/`\\`/`/`/NUL 치환. `{name|ext|n|timestamp}`만 변수로 허용.
- `RenameRule::default_paren_n()` — plan §5 default `{name} ({n}){ext}`.
- `header::file_header` / `dir_header` — USTAR(100/155 split), 초과 시 GNU `L` LongLink 확장 + fallback header.

### `adb-session` 확장

**`manifest.rs`** — plan §5 표준 probe `find <root> -type f -exec stat -c '%n %s %Y' {} \;` 구현.

- `build_probe_command(root)` — 단일 quoted 문자열로 반환. adb shell argv flattening에서 `;`가 outer shell에 먹히는 문제를 회피 (§3 의사결정 참조).
- `parse_manifest_output(stdout, root)` — 공백 포함 파일명(한글 포함)을 right-split으로 안전 파싱.
- `probe(session, serial, root)` — `No such file` stderr를 빈 manifest로 매핑하여 plan §5 "conflict-impossible fast-path" 자연 지원.
- `is_same_file(local_size, local_mtime, remote, tolerance)` — plan §5 default `mtimeToleranceSeconds: 2` 직접 검증.

**`device_caps.rs`** — Phase 1 retro §4-5 (반대론자) 반영. layer-level이 아닌 **per-device** capability.

- `probe_device(session, serial) -> DeviceCapabilities` — `which tar`, `which find`, `which stat`, `toybox --version` 4개 shell 호출로 가용성 + tar impl tag 수집.
- `can_tar_upload()` — tar+find+stat + smoke 통과시에만 true. UI 추천 경로 게이트의 1차 조건.

**`tar_upload.rs`** — plan §4.5 + §6.1 통합 진입점.

- `upload_tar(session, serial, source_root, dest_path, plan, cancel) -> UploadOutcome`
  - 1. `is_safe_dest_path(dest)` — `/sdcard/*` 또는 `/storage/emulated/0/*` whitelist + `..`/`\n`/`` ` ``/`$` 거부.
  - 2. `mkdir -p <dest>` 멱등 생성.
  - 3. `AdbSession::spawn(["shell", "tar", "-x", "-C", dest], "tar-x")`.
  - 4. `TarStreamBuilder::write_to(&mut child.stdin)` 직접 wiring.
  - 5. `drop(stdin)` 으로 EOF 시그널 + `AdbProcess::wait_capture()` 로 정상 종료 대기.
  - 6. 취소 시 `pkill -f 'tar -x -C <dest>'` (device-side) + host SIGTERM→SIGKILL.
- `CancelHandle` + `CancelAwareSink` — write 도중 cancel flag를 보면 `io::Error::Interrupted` 반환하여 builder가 즉시 정리하고 빠지도록.

**`process.rs::AdbProcess::wait_capture()`** — Phase 1 retro §4-2 (반대론자) 반영. SIGTERM 없이 EOF 기반 정상 종료 path. 기존 `terminate()`는 cancel 전용으로 의미 분리.

**`capability.rs::AdbCapabilities::phase2_default()`** — layer-level `adb_tar_upload`를 true로 승격. per-device 게이트는 `DeviceCapabilities::can_tar_upload()`가 별도로 담당하므로 거짓말이 아님.

### CLI: `mtp-cli adb {caps|manifest|tar-upload}`

- `mtp-cli adb caps <serial>` → per-device 4-shell probe 결과 + `can_tar_upload` 결정.
- `mtp-cli adb manifest <serial> <root>` → 정렬된 entry table (path / size / mtime).
- `mtp-cli adb tar-upload <serial> <src> <dest>` → 라이브 스트리밍 업로드 + 진행 통계.

## 2. 통과 기준 vs 실측

§8 Phase 2 통과 기준:

| 기준 | 결과 |
|---|---|
| 생성된 stream을 로컬 `tar -tf -` 또는 임시 extraction test로 검증 | ✅ 자동: `stream.rs` 단위 테스트에서 own parser로 USTAR 헤더 + 페이로드 + 패딩 round-trip 검증 (10건). 실기기: 3-file 업로드 후 device-side `ls -laR` 로 layout 확인. |
| 특수 파일, 권한 부족 파일, 긴 파일명, 한글 파일명 테스트 추가 | ✅ symlink → `EntryKind::Unsupported` (default skip / strict fail 두 경로 모두 테스트), 한글 파일명 round-trip (자동+실기기), 300-byte 단일 segment 경로 → GNU LongLink 분기 검증. 권한 부족은 `TarError::Io { path, source }` 로 surface (수동 확인 미실행, Phase 5 이월). |
| 이름/크기/수정시각 기반 same-file skip과 different-file rename 테스트 추가 | ✅ `manifest::is_same_file` tolerance 안/밖 경계 검증, `ConflictPlan` Skip/Rename/Overwrite 분기별 stream output 검증. |
| rename pattern sanitize와 안전한 timestamp 포맷 테스트 추가 | ✅ FAT-unsafe 문자 치환, 한글/공백 보존, trailing dot/space strip, `.gitignore` 같은 dot-leading 케이스 보존, timestamp `:` 없음 + fixed-width 검증. |
| 중첩 폴더 conflict에서 overwrite가 발생하지 않음을 자동 테스트 또는 실기기 테스트로 검증 | ✅ `nested_rename_does_not_overwrite_neighbouring_file` 테스트: `sub/a.txt` rename → `sub/a (2).txt`, 기존 `sub/a (1).txt` 와 충돌 없음. |

자동 테스트 결과:

```
$ cargo test --workspace
tar-stream:    56 passed; 0 failed
adb-session:   41 passed; 0 failed  (+19 vs Phase 1)
orchestrator:   6 passed; 0 failed
total:        103 passed; 0 failed
```

실기기 검증 (Xiaomi `ea33d2fe`):

```
$ cargo run -q -p mtp-cli -- adb caps ea33d2fe
tar=true find=true stat=true smoke_ok=false tar_impl=Some("toybox toybox 0.8.11-android")
can_tar_upload=false   # smoke_ok=false (Phase 2 본체에서는 미실행, Phase 3 진입)

$ cargo run -q -p mtp-cli -- adb tar-upload ea33d2fe \
    /tmp/crossmtp-phase2-fixture /sdcard/Download/crossmtp-phase2
files_emitted=3 files_skipped=0 bytes=3604 host_exit=Some(0)

$ cargo run -q -p mtp-cli -- adb shell ea33d2fe -- ls -laR /sdcard/Download/crossmtp-phase2
/sdcard/Download/crossmtp-phase2:
  a.txt          6   2026-05-14 11:44
  sub/           
  한글.txt       7   2026-05-14 11:44
/sdcard/Download/crossmtp-phase2/sub:
  b.txt          7   2026-05-14 11:44
# ._a.txt, .DS_Store: 모두 제외됨 ✓

$ cargo run -q -p mtp-cli -- adb manifest ea33d2fe /sdcard/Download/crossmtp-phase2
manifest under /sdcard/Download/crossmtp-phase2 (3 entries):
  6  mtime=1778726684  a.txt
  7  mtime=1778726684  sub/b.txt
  7  mtime=1778726684  한글.txt
```

## 3. 의사결정 기록

1. **`tar-stream` 별도 crate vs `adb-session` 내부 모듈**.
   별도 crate. 근거: plan §4.3은 "임시 tar 파일을 만들지 않는다"만 요구하지 adb-specific 동작은 없다. Phase 3에서 orchestrator가 sink를 바꿔서 (예: MTP fallback 시 디스크 임시 + libmtp, 또는 테스트 fixture 생성용 메모리 sink) 같은 builder를 재사용해야 한다. adb-session 안에 두면 tar 변경할 때마다 adb 의존성을 끌고 들어가고, 빌드 그래프가 망가진다.

2. **USTAR + GNU LongLink, PAX는 미채택**.
   Phase 0 retro §1.2가 toybox 0.8.11 tar을 baseline으로 고정했다. PAX 확장 header는 일부 vendor tar에서 무시되거나 attribute 충돌이 나기 때문에 안전한 최소 단위인 USTAR로 가고, 길이 초과만 GNU `L` LongLink로 처리. `--xform` 등 GNU 확장에도 의존하지 않음.

3. **adb shell argv flattening 회피: `sh -c '<script>'` ❌ → single-string command ✅**.
   초기에 `["sh", "-c", "<script>"]` 형태로 보냈더니 `sh -c '<script>' /sdcard/...` 가 되어 `$0`이 root path를 가져가고 find가 cwd `/` 부터 검색하는 사고가 났다. adb shell은 받은 argv를 **공백으로 join 후 device-side sh에 재전달**한다. 그래서 `;`, `{}`, single-quote 같은 토큰을 의미가 유지된 채 보내려면 호스트 쪽에서 이미 single-quoted 한 줄 명령으로 만들어야 한다. `build_probe_command`는 이제 길이 1짜리 `Vec<String>`을 반환한다.

4. **`AdbProcess::wait_capture()` 신설 vs `terminate()` 재사용**.
   Phase 1의 `terminate(grace)`는 SIGTERM을 항상 보낸다. 정상 종료(EOF 후 대기) path에 그걸 쓰면 device-side tar이 inode 생성 도중에 죽어서 **3-file 업로드가 0-file 도착**으로 끝나는 사고가 났다 (live 첫 시도에서 그대로 재현). `wait_capture()`는 stdout/stderr 드레인 → `child.wait()` 만. 신호 없음. 의미를 코드 수준에서 분리하여 미래 수정에서 같은 사고가 안 나도록 한다.

5. **per-device `DeviceCapabilities` vs layer-wide `AdbCapabilities`**.
   Phase 1 retro §4-5 반대론자 코멘트 직접 반영. tar/find/stat/scoped-storage는 device마다 다를 수 있고, plan §4.1은 capability를 "device/session" 단위로 정의한다. 두 구조체를 모두 노출하고 UI는 `AdbCapabilities` (layer 가능 여부) ∧ `DeviceCapabilities::can_tar_upload()` (device 가능 여부) 의 교집합으로 추천 경로를 결정한다.

6. **macOS metadata는 builder의 traversal 단에서 hard-exclude**.
   plan §4.3 / Phase 0 retro §1.3. `is_macos_metadata` 체크가 `walk_dir`의 `read_dir` 루프와 single-file root 분기 양쪽에 들어가 있어 어떤 진입점에서도 `._*`/`.DS_Store` 가 stream에 새지 않는다. 실기기 검증에서 `._a.txt` + `.DS_Store` 가 device에 도달하지 않음을 확인.

7. **`is_safe_dest_path` 화이트리스트**.
   plan §1.4 결정사항 (`/sdcard/Download`, `/sdcard/Documents`, `/sdcard/DCIM`, `/storage/emulated/0/*`) 을 코드 수준에서 enforce. `/data/local/tmp` 같은 ADB-only 경로는 의도적으로 거부 — MTP fallback과 의미가 다르고, 사용자 컨텍스트(파일 매니저로 보임)를 깨는 곳에 쓰지 않는다.

8. **cancel은 sink 측 cancel-aware wrapper + device-side `pkill -f`**.
   Phase 0 retro §2.2가 "host pipeline 종료만으로는 device-side tar 안 죽는다"를 직접 측정으로 보였다. `CancelAwareSink` 가 다음 write에서 `Interrupted` 반환 → builder 즉시 빠짐 → `drop(stdin)` → `best_effort_pkill` → `AdbProcess::terminate(1s)` 4단 정리. plan §6.1 5-step 시퀀스의 코드화.

## 4. 반대론자 (Devil's Advocate) 코멘트

> "통과는 했지만 다음을 직시할 것."
>
> 1. **smoke check 미구현**. `DeviceCapabilities::tar_extract_smoke_ok` 가 항상 false다. `tar -x` 가 empty input에 어떻게 반응하는지를 한 번 봐 두면 미래에 vendor tar 변형을 만나도 빠르게 결정 가능. Phase 3 진입 시 `tar_upload::smoke_check_extract(session, serial)` helper로 분리해 캐시 가능한 형태로 만들 것.
> 2. **취소 path 실기기 테스트 부재**. `CancelHandle` + `CancelAwareSink` 동작은 자동 테스트가 커버하지만, "전송 중에 cancel → `pkill -f` 가 실제로 device-side tar PID를 잡는가" 는 라이브로 재현하지 않았다. Phase 0 retro §2.2가 "4022 파일 잔존 + stray tar 3개"를 측정으로 확인했으므로, Phase 3 진입 전 같은 fixture로 cancel 테스트를 한 번 더 돌릴 필요가 있다.
> 3. **manifest probe 대량 파일 검증 부재**. 3-file fixture로만 검증. plan §9.2의 "5,000개 이상 작은 파일 묶음"에서 `find ... stat` 출력이 잘리거나 라인 버퍼링 문제가 생길 수 있다 (`adb shell`의 출력 처리 한계). Phase 5 throughput 라운드에서 같이 확인.
> 4. **`build_probe_command`가 single-string인 이상 device-side `sh`가 항상 그 script를 받는다**. `adb shell <single-string>` 시 device-side에서 default shell이 sh가 아니면 (recovery 모드, vendor shell) 깨질 수 있다. 일반 부팅 device에서는 `/system/bin/sh` 가 toybox로 link되어 있어서 문제 없지만, vendor 변형 대응이 필요하면 명시적으로 `sh -c` 를 한 번 더 감싸야 한다 (단, §3 의사결정 3에서 본 argv flatten 함정을 회피하려면 더 까다로운 quoting 필요).
> 5. **AppleDouble는 traversal에서만 잡힌다**. 사용자가 명시적으로 `._foo.txt` 를 single-file root로 업로드하면 (Phase 4 drag-and-drop의 엣지 케이스), traversal의 single-file branch가 잡아서 빈 entries를 반환한다 — 즉 사용자는 "왜 파일이 안 올라가지?" 가 된다. 단일 metadata 파일 명시 업로드는 명시 에러로 분기하는 게 더 친절. Phase 4 UI 단계에서 다룰 일.
> 6. **`{n}`-rename에서 n을 결정하는 주체가 명확하지 않다**. `RenameRule::render(source, n, ts)` 는 n을 받기만 한다. orchestrator가 manifest probe 결과로 "다음 빈 n" 을 계산해야 하지만 그 계산은 Phase 3 영역. 본 phase는 builder 입력 (`ConflictAction::Rename(String)`) 에서 끝.

## 5. 알려진 제약 (Known Limitations)

- **단일 검증 기기**: Phase 1과 동일 (Xiaomi `ea33d2fe`, Android 15, toybox 0.8.11 1대).
- **smoke check 미실행**: per-device cap이 `can_tar_upload=false` 로 보수적으로 유지됨. Phase 3에서 active probe 후 캐시.
- **취소 실기기 미검증**: 코드 path는 있으나 4022-file 시나리오 재현 없음. Phase 0 retro 의 fixture를 Phase 3 진입 시 재실행.
- **`tar -tf -` 외부 검증 미실행**: USTAR 호환성은 자체 parser로만 검증. BSD tar/GNU tar 양쪽 cross-check 는 별도 task.
- **plan.md §3 patch 미반영**: Phase 0 retro §3의 6건 제안 + Phase 1 retro §4-4가 본 phase에서도 반영되지 않음. Phase 3 진입 전 별도 PR로 정식 반영 예정.

## 6. Phase 3 인수 항목

1. **orchestrator `JobKind::AdbTarUpload` 도입**: 기존 queue + state machine 재사용. 진행률 이벤트는 `ProgressSnapshot` → `Event::Progress { id, sent, total }` 매핑.
2. **smoke check 캐시**: 세션 bring-up 시점에 `tar_extract_smoke_ok` 채우고 그 결과를 capability 추천 결정에 반영.
3. **취소 path 실기기 재현**: Phase 0 retro의 2000×4 KiB fixture로 mid-stream cancel → device-side tar PID 0개 + 잔존 파일 manifest 검증.
4. **conflict planner 통합**: manifest probe 결과 + `is_same_file` + `RenameRule` → `ConflictPlan` 자동 생성. orchestrator 단의 conflict UI semantic을 ADB job에 매핑.
5. **반대론자 6건 흡수**: §4의 1~6을 Phase 3/4/5 작업 항목으로 매핑.
6. **plan.md §3 patch 반영**: Phase 0 + Phase 1 retro 의 정오표를 plan에 정식 반영.

## 7. 사용 방법

```bash
# 빌드
cargo build --workspace

# 자동 테스트 (워크스페이스 전체)
cargo test --workspace
# tar-stream:  56 passed
# adb-session: 41 passed
# orchestrator: 6 passed

# 실기기 — per-device caps
cargo run -q -p mtp-cli -- adb caps <serial>

# 실기기 — manifest probe (충돌 진단)
cargo run -q -p mtp-cli -- adb manifest <serial> /sdcard/Download/myfolder

# 실기기 — 스트리밍 업로드
cargo run -q -p mtp-cli -- adb tar-upload <serial> ./local/dir /sdcard/Download/remote
```

## 8. 한 줄 요약

Phase 2는 builder + manifest + per-device probe + 실 스트리밍을 모두 정직하게 분리해서 붙였고, 자동 103건 + 실기기 1대 라이브 round-trip 으로 통과했다. cancel 실기기 재현과 두 번째 기기 검증은 Phase 3/5 입력으로 명시적으로 이월.
