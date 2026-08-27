# obs-rs

Rust로 구현하는 OBS 스타일의 방송·녹화 애플리케이션이다.

## Preview 용어

`Preview`라는 단어가 UI 공간, 논리 좌표, 합성 결과를 모두 의미하지 않도록 다음 용어를 구분한다.

| 용어 | 의미 | 대표 코드 이름 |
|---|---|---|
| Preview Workspace | 도커를 제외한 중앙 작업 공간 전체 | `ui::preview::show`의 `CentralPanel` |
| Preview Viewport | 합성 결과를 표시하는 화면상의 16:9 사각형 | `viewport_rect` |
| Scene Canvas | SceneItem이 배치되는 고정 논리 좌표 공간 | `SceneCanvas` |
| Composite Frame | Compositor가 Source들을 합성한 최종 영상 | 향후 `CompositeFrame` |
| Editor Overlay | 선택 테두리와 가이드처럼 출력에 포함되지 않는 UI | `paint_editor_overlay` |
| Transform Gizmo | 이동·크기·회전 조절 핸들 | `ui::preview::gizmo` |
| Viewport Transform | Canvas 좌표와 화면 좌표 사이의 변환 | `ViewportTransform` |

```text
PreviewWorkspace
┌────────────────────────────────────────┐
│ 중앙 작업 공간 배경                       │
│                                        │
│   PreviewViewport                      │
│   ┌────────────────────────────────┐   │
│   │ CompositeFrame                 │   │
│   │                                │   │
│   │ EditorOverlay                  │   │
│   │  └─ TransformGizmo             │   │
│   └────────────────────────────────┘   │
│                                        │
└────────────────────────────────────────┘
```

현재 Scene Canvas는 `1920×1080`이다. 창이나 도커 크기가 바뀌면 Preview Viewport만 다시 맞춰지고 SceneItem의 Canvas 좌표는 바뀌지 않는다. Preview Workspace의 여백과 Editor Overlay는 방송·녹화 출력에 포함되지 않는다.

Preview Viewport는 기본적으로 Workspace 가용 크기의 75% 안에서 Canvas 종횡비를 유지해 배치한다. PreviewToolbar의 `−`, 백분율 입력, `+`, `Fit`으로 40–100% 범위에서 조절할 수 있으며 Fit 메뉴는 Workspace 맞춤, 50/75/100%, Reset View 프리셋을 제공한다. 이 배율은 화면 표시만 바꾸며 SceneItem의 Canvas 좌표나 출력 해상도에는 영향을 주지 않는다.

SceneItem이 Canvas 밖으로 나가면 Canvas와 겹치는 부분만 Composite Frame에 들어간다. 선택된 SceneItem의 Canvas 바깥 부분은 편집을 계속할 수 있도록 Preview Workspace 여백에 흐리게 표시하지만 실제 출력에는 포함하지 않는다.

## Source와 SceneItem

`Source`는 캡처 장치나 이미지, 색상 같은 전역 리소스다. `SceneItem`은 Source를 특정 Scene에 배치한 인스턴스다. 같은 Source를 여러 Scene에서 재사용할 수 있지만 위치, 크기, 표시 여부, 잠금, Crop, 합성 순서는 SceneItem마다 다르다.

```text
Scene 선택
  → SceneItem 목록
  → Compositor
  → CompositeFrame
  → PreviewViewport
       + EditorOverlay
```

Sources 도커의 위쪽 항목이 앞쪽에 합성된다. Preview에서 Source를 이동하거나 크기를 조절하는 동안에는 UI의 임시 Transform만 바꾸고, 마우스를 놓을 때 최종 Transform을 프로젝트 DB에 한 번 저장한다.

현재는 Compositor와 Composite Frame이 아직 없으므로 Preview Viewport에 빈 프레임을 표시한다. UI는 Source를 Viewport 안에 직접 합성하지 않으며, 선택된 SceneItem의 Canvas 바깥 부분과 Transform Gizmo만 Editor Overlay로 표시한다. 향후 Compositor가 GPU Texture인 Composite Frame을 만들면 Preview UI는 그 Texture와 Editor Overlay를 함께 표시한다.

## 다국어 지원

UI 번역은 Fluent FTL 언어팩을 사용한다. 기본 언어와 fallback은 `en-US`이며 현재 `ko-KR`을 함께 제공한다.

```text
assets/locales/
├─ en-US/app.ftl
└─ ko-KR/app.ftl
```

UI 코드는 번역 문자열을 직접 갖지 않고 `TextKey`를 통해 `LocalizationManager`에 요청한다. `View → Language`에서 언어를 바꾸면 `UiAction::SetLocale`이 앱에 전달되고 다음 UI 프레임부터 선택한 언어가 적용된다. 번역 키가 선택 언어에 없으면 영어를 사용한다.

선택 언어는 프로젝트 DB가 아닌 사용자 앱 설정에 저장한다. Windows에서는 `%APPDATA%/obs-rs/settings.toml`이며 다음 형식이다.

```toml
locale = "ko-KR"
```

한글 글리프는 OS의 CJK 시스템 폰트를 egui fallback으로 등록한다. 배포 패키지가 시스템 폰트에 의존하지 않아야 하는 단계에서는 라이선스를 확인한 전용 폰트 파일을 앱 asset으로 포함한다.
