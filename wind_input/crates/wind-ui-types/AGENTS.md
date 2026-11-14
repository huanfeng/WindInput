<!-- Parent: ../../AGENTS.md -->
<!-- Updated: 2026-08-13 -->

# wind-ui-types

## Purpose
协调器 ↔ 任意前端的**表现层协议（纯数据）**：正向 `UiCommand`（协调器→渲染端）、反向 `UiEvent`（渲染端→协调器）及其载荷类型。消费者：桌面渲染线程（wind-ui）、macOS `.app`（经 host-render 转发）、Android Kotlin 壳（经 FFI 回调）。进程内契约、非线协议（线协议在 wind-ipc）。

## Key Files
| File | Description |
|------|-------------|
| `src/command.rs` | `UiCommand`（44 变体）+ `GlobalHotkeyEntry` + `HOVER_PAGE_*`；`#[cfg(windows)] HostRenderArc` 是唯一平台载荷 |
| `src/event.rs` | `UiEvent`（渲染端反向事件） |
| `src/preview.rs` | 设置端主题预览的请求 / 回执（`RenderCandidatePreview` 载荷，回执走请求自带的 `SyncSender`，请求方放弃时置位 `cancel`）+ 渲染器版本常量 `CANDIDATE_PREVIEW_RENDER_VERSION`（进预览缓存键） |
| `src/menu.rs` | 菜单族：`MenuItemSpec`/`MenuKind`（含稳定 id 双向映射 `to_menu_id`/`from_menu_id`）/`MenuAnchor`/`CandidateOp`/`ToolbarAction` |
| `src/candidate.rs` / `src/toast.rs` / `src/toolbar.rs` | `CandidateItem` / `ToastKind`·`ToastPosition` / `ToolbarState` |
| `src/diag.rs` | 诊断视图族 + 纯格式化 `format_diag_lines`（含全部纯测试） |

## For AI Agents

### 三条红线（破任何一条即破坏 Android 构建承诺）
1. **仅纯数据与纯映射逻辑**：零 IO、零日志（不引 tracing）、零 serde、零渲染。行为实现放消费方（渲染色表在 wind-ui、剪贴板走 coordinator 的 HostServices）。
2. **新增依赖必须通过 `cargo check-android`**（alias 见 `wind_input/.cargo/config.toml`；CI 强制）。当前依赖面仅 wind-theme（SetTheme 载荷）。
3. **平台专属载荷一律 target-specific 依赖 + `#[cfg]` 变体**（先例：`SetHostRender` + cfg(windows) 的 wind-bridge）。禁止无条件平台依赖。

### 演化约定
- **不要给枚举加 `#[non_exhaustive]`**：全部消费者同 workspace，穷尽匹配正是加变体时的编译期消费者清单——新变体一加，编译器逐个点名 wind-ui 的 match 与 wind-mobile 的映射层，机械化「逐一标注接/不接」流程。（移动端的**绑定层**在 WindInputAndroid 仓，不在本 workspace 内；它只看 wind-mobile 的门面，故此处的编译期点名到 wind-mobile 为止。）
- 加 `UiCommand` 变体时按域放进对应注释分节；wind-ui 侧 `manager.rs` 的 `pub use` 清单同步补（DiagSections 三件套走 input_diag_hud 链式转发，勿重复列出，否则 E0252）。
- derive/手写 impl 是行为契约：`MenuItemSpec` 的 `PartialEq` 是 popup_menu 增量重绘判据、`DiagSections`/`InputDiagView` 的手写 `Default`（全开/置顶）不可 derive 化。

### Testing Requirements
纯数据 crate，host 直接 `cargo test -p wind-ui-types`（任意平台）。

## Dependencies
- Internal: `wind-theme`；`wind-bridge`（仅 `cfg(windows)`，HostRenderArc 载荷）
- External: 无

<!-- MANUAL: 此行以下为人工补充区，重新生成时保留 -->
