# 설치 가이드

CrossMTP는 macOS arm64 (Apple Silicon)을 1차 타겟으로 합니다. Intel Mac은
가능성이 높지만 미검증입니다.

## 1. 시스템 요구사항

* macOS 13 (Ventura) 이상 권장
* arm64 (Apple Silicon) 1차 검증
* USB 데이터 케이블
* Android 4.0 (Ice Cream Sandwich) 이상의 MTP 호환 기기

## 2. 의존성 설치

### libmtp (필수)

CrossMTP는 시스템에 설치된 `libmtp`를 동적 링크합니다. 번들에 포함되어 있지
않으므로 별도 설치 필요.

```bash
brew install libmtp
```

설치 후 버전 확인:

```bash
pkg-config --modversion libmtp
# → 1.1.23 또는 그 이상
```

### Homebrew 자체

설치되어 있지 않다면:

```bash
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
```

## 3. CrossMTP 설치

### 옵션 A: 번들 (.app)

```bash
# 빌드 후 (개발자)
open apps/desktop/src-tauri/target/release/bundle/macos/CrossMTP.app

# 또는 .app을 /Applications 로 복사
cp -R apps/desktop/src-tauri/target/release/bundle/macos/CrossMTP.app /Applications/
```

> 베타 단계는 코드 sign / notarization이 안 되어 있어 처음 실행 시 macOS가
> "확인되지 않은 개발자" 경고를 띄울 수 있습니다. **System Settings →
> Privacy & Security → "그래도 열기"** 로 우회하거나, 터미널에서:
>
> ```bash
> xattr -d com.apple.quarantine /Applications/CrossMTP.app
> ```

### 옵션 B: 소스에서 빌드

```bash
git clone <repo-url> CrossMTP
cd CrossMTP

# 의존성 (Rust / Node)
rustup default stable           # 1.75+
brew install node               # 22+ 권장

# 프론트엔드 의존성
cd apps/desktop && npm install && cd ../..

# 개발 모드 (HMR)
cd apps/desktop && npm run tauri dev

# 또는 번들 빌드
cd apps/desktop && npm run tauri build
```

## 4. 첫 실행 전 점검

CrossMTP를 처음 실행하기 직전, **반드시 macOS USB 데몬을 종료**해주세요. 이
앱들이 살아 있으면 CrossMTP가 폰의 USB 인터페이스를 잡지 못합니다.

```bash
killall "Android File Transfer" "Android File Transfer Agent" icdd 2>/dev/null
```

종료한 데몬은 **macOS가 자동으로 재시작합니다**. CrossMTP를 쓰는 동안에는
반복적으로 종료해야 할 수 있습니다 (특히 Image Capture가 가장 자주 부활).

> Phase 5+ 로드맵: CrossMTP 시작 시 데몬 자동 감지 + 종료 권유 다이얼로그를
> 추가할 예정.

## 5. 폰 측 설정

1. USB 케이블로 폰 연결 (반드시 **데이터 가능 케이블** — 충전 전용은 인식 안 됨)
2. 폰 잠금 해제
3. 알림 센터 또는 빠른 설정에서 USB 사용 모드 → **"파일 전송"** 또는 **MTP** 선택
4. 처음 연결 시 폰이 PC 신뢰 여부를 묻는 다이얼로그 → 허용

폰 종류별 메뉴 위치:

* **Samsung Galaxy**: 알림 센터 → "USB 충전 중" 탭 → "파일 전송"
* **Google Pixel**: 알림 센터 → "USB로 이 기기를 충전 중" 탭 → "파일 전송"
* **Xiaomi / POCO / Redmi**: 알림 센터 → "USB로 충전" 탭 → "파일 전송"
* **그 외**: 설정 → 개발자 옵션 → 기본 USB 구성 = "파일 전송"

## 6. 동작 검증

### 개발자: CLI

```bash
cargo run -p mtp-cli -- verify-q
```

성공하면 다음 5단계가 모두 통과해야 합니다:
1. device 발견
2. storage 표시
3. root 폴더 listing
4. 파일 다운로드
5. round-trip 업로드

### 일반 사용자: GUI

1. CrossMTP.app 실행
2. 상단 배너에 폰 모델명이 뜨는지 확인
3. 좌측 폴더 트리에서 Pictures 진입
4. 파일 옆 "↓ 다운로드" 클릭
5. 우측 큐 패널에 진행률 카드 → "완료"

## 7. 자주 발생하는 문제

[`docs/troubleshooting.md`](troubleshooting.md) 참고.
