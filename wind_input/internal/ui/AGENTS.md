<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-06-04 -->

# internal/ui

> **主题颜色 v3（2026-06-04）**：`ResolvedPalette` 删 5 个窗口色组（CandidateWindow/PopupMenu/Tooltip/Status/Toast），颜色全部扁平进 `Palette.Tokens map[string]color.Color`（保留顶层语义便捷字段 `Bg/Text/Accent…` + `ResolvedToolbarPalette`）。本层消费改动最小：直接读窗口色组的两处改查 `Tokens`——`renderer.go getModeIndicatorColors`→`Tokens["toast_bg"/"toast_text"]`、`toolbar_renderer.go getTooltipColors`→`Tokens["tooltip_bg"/"tooltip_text"]`；`viewbox_toolbar.go resolveToolbarViews` 的 `${token}` 名改 `toolbar_*`。状态泡/Tooltip/菜单/Toast 仍走 `theme.ResolveXxxViews`（其内部默认色已改取功能 token），消费方法名不变。`pal.Toolbar`（ResolvedToolbarPalette 保留）不变。

## Purpose
跨平台 UI 层. 历史上仅 Windows 原生渲染, 经跨平台重构后拆为:

1. **平台无关数据/命令模型** — 类型定义 (`types_neutral.go`) + `uicmd.Command` 投递 (`events.go` / `uicmd_convert.go` / `protocol.go`)
2. **跨平台渲染核心** — `renderer.go` / `renderer_layout.go` / `text_drawer.go`(freetype) / `font_config.go` / `fontspec.go` / `dpi_neutral.go` 用 `gogpu/gg` 出 `*image.RGBA`, 不再 `//go:build windows`; Win 与 darwin 共用同一渲染源
3. **Windows 文本后端** — GDI + DirectWrite + CGO 后端 (`gdi_text.go` / `dwrite_*.go` / `text_drawer_windows.go` / `text_backend_windows.go`) + Win32 窗口/工具栏/菜单 (`//go:build windows`)
4. **darwin 渲染消费** — `text_backend_darwin.go` 只走 freetype; `manager_darwin.go` 保留 cmdCh/eventCh, macOS forwarder 订阅 cmdCh 把 gg 出的位图经 SHM+push 交给 IMKit `.app` 贴 NSPanel

`Manager` 在两个平台同名 struct + 同名方法集 (60+); `TextBackendManager` 两平台同公开 API (darwin 仅 freetype 分支); coordinator 调用面零变化.

## Key Files

### 平台无关 (Win + darwin 共用)
| File | Description |
|------|-------------|
| `types_neutral.go` | 纯数据类型与枚举常量集中: `StatusState/StatusWindowConfig/StatusMenuAction` (+变体)、`ToastLevel/ToastPosition/ToastOptions`、`ToolbarState/ToolbarCallback/ToolbarContextMenuAction`、`GlobalHotkeyEntry`、`MenuItem/PopupMenuCallback`、`CandidateLayout/PositionPreference`、`UnifiedMenu*` 常量 + `UnifiedMenuState/ThemeMenuItem/SchemaMenuItem`。`UnifiedMenuState` 含 `OmitToolbarToggle`/`OmitAdvanced` (darwin 置 true 以隐藏「显示工具栏」「高级(即时候选/定位)」Win 兼容项)。导出 `ToastAccentColor(ToastLevel) color.Color` (Win toast_renderer 与 darwin forwarder 共用的 accent 配色单一来源)。**不可** import windows/cgo/syscall |
| `unified_menu_build.go` | `BuildUnifiedMenuItems(state UnifiedMenuState) []MenuItem` + `aboutText`: 跨平台构建统一右键菜单树 (方案/简繁/全角/标点/主题/重载配置/重启服务/设置/关于); 据 `state.Omit*` 裁剪 Win 专属项 (原在 manager.go, Win-only) |
| `protocol.go` | `Candidate` (=candidate.Candidate)、`CandidateCallback`、`CandidateRect`、`RenderResult` |
| `events.go` | `uicmdItem` (Manager 内部 channel 元素, 含 `uicmd.Command` + 旁路 Candidates/Callback/MenuState); `toUICandidates` 平台无关候选切片转换 |
| `uicmd_convert.go` | Win 业务类型 ↔ uicmd wire 镜像的双向映射 (`toUIToolbarState`/`fromUIToolbarState` 等); `ToastLevel` int↔string 等不能直接 cast 的映射在此 |
| `uicmd_convert_test.go` | 双向映射 roundtrip 测试 |

### 跨平台渲染核心 (无 build tag, Win + darwin 共用)
| File | Description |
|------|-------------|
| `renderer.go` | `Renderer`: gg 渲染候选词列表 (文字/颜色/高亮/序号圈), 出 `*image.RGBA`; 嵌入 `TextBackendManager` |
| `renderer_layout.go` | `RenderCandidates` 候选窗渲染入口（DPI 刷新后按 Layout 委派 `renderHorizontalV2`/`renderVerticalV2`）+ 横竖排共用辅助 `candidateDisplayText`/`indexLabel`/`hasSideEffectAction`；盒模型 View 引擎为唯一路径（旧固定化渲染器已删） |
| `text_drawer.go` | `TextDrawer` 接口 + `freeTypeDrawer` (gg/text 实现, 含 glyph 级字体 fallback) + `fontCache`; 彩色 emoji 段经平台 hook `drawColorEmoji` 画到独立 `emojiOverlay` (gg.Context.Image() 返回拷贝, 不能直接画), EndDraw 叠回; emoji 段宽用 `colorEmojiAdvance` 取整 em |
| `font_config.go` | `FontConfig`: 字体路径/大小/样式; 调 `systemfont.ResolveFile/HasFamily/ResolveDWFamily` 解析系统字体; `textFallbackFonts()` 调平台 hook `platformTextFallbackFonts()` 注入原生回退链 |
| `font_fallback_other.go` / `emoji_sbix_other.go` | `//go:build !darwin` 平台 hook 空实现 (`platformTextFallbackFonts`/`drawColorEmoji`/`colorEmojiAdvance` 返回空; Win 走 DirectWrite 彩色路径) |
| `fontspec.go` | `FontSpecToName` + `knownFontNames`: 字体规格字符串归一化 (纯字符串处理, 原在 gdi_text.go) |
| `dpi_neutral.go` | `GetDPIScale`/`ScaleForDPI`/`ScaleIntForDPI` + `SetDPIScaleProvider`; Win 在 dpi.go init 注入真实 DPI, darwin 默认 1.0 |

### Windows-only (`//go:build windows`)
| File | Description |
|------|-------------|
| `manager.go` | `Manager` Win 版主体, channel 消息循环, `Start()`/`WaitReady()`/`processOneCommand` 按 `uicmd.CommandType` 分发 |
| `manager_candidate.go` | 候选窗口管理：显示/隐藏/更新候选列表和分页 |
| `manager_config.go` | 配置更新：字体、主题、布局、Tooltip 延迟等 |
| `manager_indicator.go` | 状态指示器（模式切换时短暂显示的浮动提示） |
| `manager_toolbar.go` | 工具栏管理：显示/隐藏/更新状态（中英文、全角、标点） |
| `manager_screenshot.go` | `doTakeScreenshot`：截图所有可见 UI 窗口存盘 (`uicmd.CmdScreenshot` 触发) |
| `window.go` | Win32 候选词窗口创建、WndProc、GDI 渲染 |
| `window_mouse.go` | 候选词窗口鼠标事件处理（点击选词、鼠标悬停） |
| `window_registry.go` | `WindowRegistry[T]`：泛型 HWND→`*T` 映射，供 WndProc 回调安全查找窗口实例 |
| `layered_window.go` | `UpdateLayeredWindowFromImage`：将 `image.RGBA` 渲染到分层窗口（`WS_EX_LAYERED`），处理 RGBA→BGRA 转换、CreateDIBSection、UpdateLayeredWindow |
| `text_backend_windows.go` | `TextBackendManager` Win 版：统一管理 GDI/FreeType/DirectWrite 三后端生命周期；`SetTextRenderMode` 三分支切换 |
| `text_drawer_windows.go` | `gdiDrawer` + `directWriteDrawer`：`TextDrawer` 的 GDI/DirectWrite 实现 (依赖 `TextRenderer`/`DWriteRenderer`) |
| `dwrite_cgo_windows.go` | CGO 桥接文件（仅 Windows）：C trampoline `cDrawGlyphRunTrampoline` 从 XMM 寄存器正确接收 float 参数后转发给 Go 导出函数 `goDrawGlyphRunBridge`；解决 Windows x64 COM 回调中 float 参数无法通过 `syscall.NewCallback` 可靠提取的问题；`dwCGODrawGlyphRunCallback()` 返回 C 函数指针供 COM vtable 使用 |
| `dwrite_text.go` | DirectWrite 文字渲染实现（IDWriteFactory/IDWriteTextLayout COM 接口调用）；`TextRenderer`(GDI)/`DWriteRenderer` 类型定义在此与 gdi_text.go |
| `viewbox.go` | **盒模型 View 渲染引擎**(P1，设计见 docs/design/archive/theme-view-architecture.md)：`View`/`Fill`/`Border`/`ImageLayer`(图片或纯色层)/`TextStyle`/`Shadow`/`GlyphKind`(矢量箭头)/`Edges` 类型 + measure/arrange（行/列流式、margin/padding、交叉轴对齐、`Stretch` 撑满、`Grow` 弹性占位右对齐）；布局层经 `TextMeasurer` 注入文本度量，可纯断言单测 |
| `viewbox_paint.go` | 盒模型绘制层：`PaintTree` 三趟遍历（形状/背景图/z<0 → 文本 → z>0 覆盖图），复用 `theme.DrawBackground` + 注入 `TextDrawer`；含 chevron 箭头绘制 |
| `viewbox_image_resolver.go` | **共享位图基础设施（P8 切片6）**：`imageResolver`（解码缓存 + mutex 并发安全）的 `imageForRef`/`fillFor`/`appendLayers`，从 `renderer.go` 原 P7-C 实现抽出；候选窗 `Renderer`（委派，零行为变化）与 status/tooltip/menu/toast 各渲染器共享同型基础设施；resources 表（ref→path/dataURI）按帧从各自 `resolvedV3.Resources` 传入（不持快照），换主题 `reset()` 清缓存 |
| `viewbox_build.go` | `(r *Renderer) buildHorizontalCandidateTree`：从 **`r.resolvedViews`(ResolvedViews)** + 候选构建横排候选窗 View 树（外观取值 single-scale，派生公式留 build）；覆盖 selected/hover 背景、accent 强调条（z<0 纯色层）、pager 翻页区；序号经 `effectiveIndexLabels`(全局>主题>默认) + `indexLabel`；返回 `candWindowTree`(root + items + pagerUp/Down) |
| `viewbox_build_vertical.go` | `buildVerticalCandidateTree`（竖排：每候选一行全宽、翻页区底部居中，同从 `r.resolvedViews` 取外观）+ 横竖共用的 `buildPager`（chevron + 页码 + 命中按钮） |
| `viewbox_render.go` | `renderHorizontalV2`/`renderVerticalV2` + 共享 `renderTree`（布局→绘制→`DrawDebugBanner`→命中矩形提取），复用 `acquireDrawContext` 共享缓冲；盒模型 View 引擎为候选窗唯一渲染路径（无开关，旧渲染器已删）。`refreshResolvedViews`（render*V2 每帧调用）填充 `r.resolvedViews`：主题路径直接吃 `theme.ResolveCandidateViews(*themeViews, palette)`（views 已 merge `defaultViews` 基线，几何+颜色权威），再回填运行时字号/行高/竖排宽（`Text/Index/PreeditBar.FontSize` + `ItemHeight` + `VerticalMaxWidth`）；无主题（仅测试）时由测试预填。合成桥（P2 临时 `viewbox_views_bridge.go`）已于 P6 阶段2e 删除 |
| `viewbox_status.go` | **状态泡 View 化（P4-A）**：`buildStatusTree`（单文本节点 bg 圆角 + padding + 居中文本，复用包级 `Layout`/`PaintTree`）+ `(r *StatusRenderer).resolveStatusColors`（颜色优先级：自定义 cfg > `views.status` token > `Palette.Status` 默认）+ `resolveTokenColor`（通用 token 解析 resolver 入口，各窗口注入自己的颜色表）。`status_renderer.go` 的 `Render` 改走此路径（取代旧 gg 直绘 + `getColors`）；几何/字号仍由运行时 `StatusWindowConfig` 提供 |
| `viewbox_tooltip.go` | **Tooltip View 化（P4-B）**：`buildTooltipTree`（多行 LayoutColumn + `\t` 列对齐 LayoutRow，每列 `FixedW` 取最大宽对齐、缺列补空占位 cell；行数上限 20 / 单列截断 / 多列末列截断逻辑预处理）+ `(*TooltipWindow).resolveTooltipColors`（views.tooltip token > `Palette.Tooltip` > 默认）。`tooltip.go:render` 改走此路径 + `newSharedDrawContext`（取代旧 gg 直绘 + `getTooltipColors`）；`truncateLineToWidth` 形参改 `TextMeasurer` 以可单测 |
| `toolbar_window.go` | 工具栏 Win32 窗口创建和消息循环 |
| `toolbar_window_event.go` | 工具栏鼠标事件（拖拽、按钮点击） |
| `toolbar_renderer.go` | 工具栏渲染：`Render` 走盒模型 View 引擎（见 viewbox_toolbar.go），整条背景/边框/按钮框/mode 文字走 View，grip/全半角/标点/齿轮矢量符号后处理（`paintGrip`/`paintWidthSymbol`/`paintPunctSymbols`/`paintGear`，定位用 View rect）。**L1 几何单一真相源**：`HitTest`/`GetButtonBounds`/`GetToolbarSize` 不再各算线性公式，统一查 `computeGeometry()`（一次 `buildToolbarTree`+`Layout` 派生）——`GetButtonBounds`=按钮 View 的 `Rect()`(content)，`HitTest`=`viewOuterRect`(margin 盒，平铺整条满高)，`GetToolbarSize`=root 尺寸；设计见 `docs/design/theme-toolbar-geometry.md`。含 `RenderTooltip`（按钮悬停提示，仍 gg 直绘） |
| `viewbox_toolbar.go` | **工具栏 View 化（P4-C）+ L2 几何进 schema**：`buildToolbarTree(state, rtv, toolbarGeom)`（整条 LayoutRow：grip 占位 + 4 按钮框，按钮 Stretch 撑高 + margin，mode 是带背景文本叶子，width/punct/settings 是无 Text 框）返回 `toolbarTree`(各按钮 View 引用)；`(*ToolbarRenderer).resolveToolbarViews`（button base 默认 FullWidthOff* + mode 中/英 token 覆盖，映射 `Palette.Toolbar`）。**L2**：几何取自 `toolbarGeom`（`resolveToolbarGeom` 从 `views.toolbar` 的 `height/grip_width/button_width/button_padding/button_radius` *Dimension×scale，缺省=内置默认）；root 去 FixedW、measure 汇总宽=grip+4×button 槽位=114（去旧 116 尾部死区）；间距用按钮 margin 保命中带无缝 |
| `toolbar_shellhook.go` | 工具栏 Shell Hook 集成：`RegisterShellHookWindow` + 动态注册 `SHELLHOOK` 消息；拦截 `HSHELL_WINDOWENTERFULLSCREEN=53`/`HSHELL_WINDOWEXITFULLSCREEN=54` 通过 `ToolbarCallback.OnForegroundFullscreenChange` 派发 |
| `popup_menu.go` | `PopupMenu`：自定义弹出菜单窗口，支持子菜单、勾选状态、主题；`Show`/`Hide`/`Destroy`；键盘导航通过全局低级键盘钩子（`WH_KEYBOARD_LL`）实现；子菜单共享父菜单渲染资源（`newPopupMenuShared`） |
| `popup_menu_event.go` | 弹出菜单事件处理：鼠标移动/点击/离开事件、键盘导航（`handleKeyDown`：↑↓/←→/Enter/Esc/字母快捷键）、`checkMouseState` 跨进程点击检测；`menuKbNavActive` 抑制键盘导航时的幻象鼠标事件 |
| `popup_menu_render.go` | 弹出菜单渲染：`render` 走盒模型 View 引擎（见 viewbox_menu.go）+ 分隔线后处理；含 updateWindow/trackMouseLeave |
| `viewbox_menu.go` | **菜单 View 化（P4-D）**：`buildMenuTree`（root LayoutColumn + 每项 LayoutRow：check/text(Grow)/arrow，勾选✓/箭头▸/文本走 View 文本叶子，hover 满宽背景，分隔项收集供后处理画线）返回 `menuTree`；`(*PopupMenu).resolveMenuColors`（7 色映射 `Palette.PopupMenu`，views.menu token 覆盖含 hover 状态）。几何 hardcode×scale 与命中测试一致 |
| `global_hotkey.go` | `GlobalHotkeyEntry`：全局热键定义；`ParseHotkeyString` 将配置字符串（如 `"ctrl+\`"`）解析为 `GlobalHotkeyEntry`；`globalHotkeyState`：通过 `RegisterHotKey`/`UnregisterHotKey` 管理线程级全局热键注册，`handleWMHotkey` 响应 `WM_HOTKEY` 消息并异步调用 callback；`resolveVK` 通用虚拟键码解析 |
| `tooltip.go` | Tooltip（编码提示）窗口管理 + 事件；`render` 走盒模型 View 引擎（见 viewbox_tooltip.go）；含 `splitLines`/`truncateLineToWidth`/`itoaCompact` 文本辅助 |
| `toast_window.go` | `ToastWindow`：独立 layered 通知窗口，用于错误提示 / 词库就绪等一次性 toast；支持 4 个 Level（Info/Success/Warn/Error）× 4 个 Position（Center/BottomRight/TopRight/Top）+ 自动隐藏（版本号取消）+ 左键点击关闭 + 右键回调预留；目标显示器以鼠标光标定位，避免跨屏错位 |
| `toast_renderer.go` | `ToastRenderer`：toast 图像渲染（标题 + 多行正文 + 圆角矩形 + Level accent 边框），复用 `TextBackendManager`（DirectWrite） |
| `monitor.go` | 多显示器支持：获取目标显示器工作区，用于窗口位置计算 |
| `dpi.go` | Win DPI 检测 (`GetEffectiveDPI`/WM_DPICHANGED)；`init()` 注入 `dpiScaleProvider` 给跨平台 `dpi_neutral.go` |
| `gdi_text.go` | GDI 文字渲染实现 (`TextRenderer` + `isUIChromeSymbolRune`/`isPureSymbolText` 符号字体判定)；`FontSpecToName` 已移至跨平台 `fontspec.go` || `uicmd_post.go` | `postCmd` 投递 helper + `snapshotCandidatesMarkers/Config/PinState` 全量快照构造器 (供 setter 末尾投递 snapshot 命令到 cmdCh) |
| `uicmd_events.go` | 反向事件通道: `Events() <-chan uicmd.Event` + `wrapCandidateCallbacks/wrapToolbarCallbacks/wrapHotkeyCallback` 双流并行包装 (原 callback + 推一份 uicmd.Event) |
| `uicmd_post_test.go` | snapshot helper + wrap callback 双流行为 + 背压测试 |

### darwin-only (`//go:build darwin`)
| File | Description |
|------|-------------|
| `manager_darwin.go` | `Manager` darwin stub: 保留 cmdCh/eventCh; 60+ method 投递 uicmd.Command; 含命令直通车按键模拟下发 `SendKeyTap(key, mods)`/`SendKeySeq([]uicmd.KeyCombo)`/`SendKeyHold`/`SendKeyRelease`/`SendKeyType(text)` (各 postCmd 对应 uicmd.CmdKeyXxx, 由 forwarder 转 ipc push 帧给 .app); `SubscribeCommands(func(cmd, candidates []Candidate))` 启 goroutine 把 cmdCh + 旁路候选推给 macOS forwarder (候选含完整字段供 ui.Renderer); Win 渲染/窗口/钩子 no-op; 含 `StatusWindow`/`GetCapsLockState`/`ParseHotkeyString` 等 stub |
| `text_backend_darwin.go` | `TextBackendManager` darwin 版: 仅 freetype (gg/text) 后端; 用 `ResolvePrimaryFont` (allow TTC, 因 PingFang 是 .ttc 且当前 gg/text 支持集合) 而非 `ResolveTextPrimaryFont` (TTF-only); `SetTextRenderMode` 忽略 mode 恒走 freetype; `SetGDIFontParams`/`SetDWriteFontFallbackForPUA` no-op 占位 |
| `font_fallback_darwin.go` | `platformTextFallbackFonts()` darwin 字形级回退链 (Apple Symbols → Helvetica → PingFang/Hiragino/STHeiti/Songti → Apple Color Emoji), 经 `systemfont.ResolveFile` 解析家族名; 被 `font_config.go` 的 `textFallbackFonts()` 调用 |
| `emoji_sbix_darwin.go` | 彩色 emoji 渲染: gg/text v0.48.x 的 ColorFont 接口无解析器实现 (DrawWithEmoji 永远回退单色), 故用 `emoji.SBIXParser` 直接从 Apple Color Emoji 提取 sbix 位图自合成; `drawColorEmoji()`(返回 advance+handled, 仅处理全 emoji 段)/`colorEmojiAdvance()`(取 emoji 字体 hmtx advance 供测量) |
| `*_darwin_test.go` | `manager_darwin_test.go` (命令投递测试) + `emoji_sbix_darwin_test.go` / `emoji_integration_darwin_test.go` (sbix emoji 直接渲染 + 完整管线彩色渲染验证) |

## For AI Agents

### Working In This Directory

**架构理解**:
- coordinator 调 `ui.Manager.ShowCandidates(...)` 等外观方法; Manager 内部构造 `uicmd.Command` 投递到 `cmdCh chan uicmdItem`
- Win 端: `processOneCommand` 消费 cmdCh → 调本地 do* 方法画窗口
- darwin 端: `processOneCommand` 不存在; 未来 macOS forwarder 直接订阅 cmdCh, 把命令序列化转发到 IMKit `.app`
- 反向事件: Win 端 `wrapXxxCallbacks` 拦截 callback 同时推 `uicmd.Event` 到 `eventCh`; `Manager.Events() <-chan uicmd.Event` 暴露给订阅方

**平台无关层 (`types_neutral.go` 等) 红线**:
- 不能 import windows / cgo / syscall / unsafe
- 任何新增"两个平台都用到的数据类型"集中在 types_neutral.go, 避免在 *_darwin.go 复刻
- darwin Manager 在 `manager_darwin.go` 内独立 struct + 同名方法, 与 Win 版字段不共享

**双端 (Win/darwin) 统一菜单注意事项** (经一次漏项回归后补, 改右键菜单前必读):
- **单一构建源, 声明式裁剪**: 右键菜单只有 `BuildUnifiedMenuItems` 一个构建函数, Win 与 darwin 共用。平台差异**不靠**两套菜单树, 而靠 `UnifiedMenuState.Omit*` 标志裁剪 —— Win 路径 `coordinator.buildUnifiedMenuState()` 不设标志(全显示); darwin 路径 `coordinator.UnifiedMenuItems()`(`handle_ui_callbacks_darwin.go`) 置 `OmitToolbarToggle=true`/`OmitAdvanced=true`。新增"平台专属项"时加一个 `Omit*` 字段并**仅在对应平台**的 state 构建处置位, 不要在 `*_darwin.go` 复刻整棵菜单。
- **加一个菜单项 = 改三处, 缺一即静默失效**: ① `types_neutral.go` 定义 `UnifiedMenu*` ID 常量; ② `unified_menu_build.go` 在 `BuildUnifiedMenuItems` 里 `append`; ③ `coordinator/handle_ui_callbacks.go` 的 `handleUnifiedMenuAction` 加 `case` 派发。只缺②→项不显示但**编译通过**(常量未被引用不报错); 只缺③→点击无反应。**回归案例**: macOS 移植把 `BuildUnifiedMenuItems` 从 `manager.go` 搬到 `unified_menu_build.go` 时漏搬"重载配置/重启服务", 常量与 dispatch 都在、编译绿、菜单却少两项 —— 搬运共用函数后务必逐项点验最终菜单。
- **两端都显示的项, 底层动作必须两端都可用**: 如"重启服务"→`coordinator.resetAndResync()`→`bridge.Server.RestartService()` 在 Win/darwin 均有实现; "重载配置"→`config.Load()` 跨平台。若某动作仅单端可用, 应改用 `Omit*` 裁剪到该平台, 而非两端都挂上去。
- **darwin 经 bridge 下发, ID 跨语言对齐**: darwin 端菜单树经 `toBridgeMenuItems`→`encodeUnifiedMenuPayload`(`bridge/unified_menu.go`) 编成 `CmdMenuShow` payload, Swift 递归建 NSMenu, 点击回 `CmdMenuAction`(id)。ID 必须落在 `int32` 且与现有区间不冲突(当前 101–305); 改 ID 编码 / flags 语义需同步 Swift 解码端。

**Win 端旧文档** (channel/UI 渲染基础不变):
- UI 线程 (Windows 消息循环) 与 coordinator goroutine 通过 `chan uicmdItem` 通信 (历史上是 `chan UICommand`, PR-2 已迁移)
- `Manager.Start()` 创建窗口并进入消息循环（阻塞，必须在独立 goroutine 运行）
- `Manager.WaitReady()` 阻塞直到 UI 线程初始化完成（main.go 中等待）
- GDI 渲染：所有绘制在 `WM_PAINT` 中进行，使用双缓冲避免闪烁
- **分层窗口**（`layered_window.go`）：使用 `WS_EX_LAYERED` + `UpdateLayeredWindow` 实现透明背景，图像数据为预乘 alpha 的 BGRA 格式
- **窗口注册表**（`window_registry.go`）：泛型 `WindowRegistry[T]`，WndProc 中用 HWND 查找对应 Go 结构体，线程安全
- **文字后端**（`text_backend.go`）：`TextBackendManager` 嵌入到需要文字渲染的窗口结构体，统一管理后端切换
- **DirectWrite CGO 桥**（`dwrite_cgo_windows.go`）：仅在使用 CGO 构建时编译（`go build` 标签）；修改 vtable 回调时需同步修改 C trampoline 签名
- **弹出菜单键盘导航**：`installMenuKeyboardHook` 安装全局低级键盘钩子（必须在 UI 线程调用）；`menuKbNavActive`+`menuKbNavMouseX/Y` 用于抑制键盘操作后 Windows 产生的幻象鼠标事件
- 候选窗口位置根据光标坐标（CaretX/Y）和显示器工作区自动调整，防止超出屏幕
- 工具栏支持拖拽移动，位置持久化到配置（`cfg.Toolbar.X`/`Y`）
- 主题颜色通过 `pkg/theme.Theme` 注入到渲染器
- `UnifiedMenuState` 用于构建统一的右键菜单（`BuildUnifiedMenuItems`）
- `Manager.SetPagerDisplayMode(mode)` 设置页码显示方式覆盖，调用后立即生效；`applyPagerOverride()` 在每次 `applyTheme()` 后也会被调用，确保主题切换不丢失覆盖
- `Manager.SetActiveAppPinState(enabled, positionsByMonitor)` 推送当前应用「固定候选位置」状态（compat 规则 `pin_candidate_position`）；coordinator 在焦点切换 / 菜单 toggle / 拖动落盘后调用；`doShowCandidates` 中 `resolveAppPinnedPosition` 按 caret 所在显示器查表，clamp 到工作区后作为候选窗位置（优先级高于会话内 drag pin）
- `CandidateCallback.OnDragEnd(x, y)` 在候选窗拖动结束时回调，coordinator 用于将位置持久化到 `state.yaml`（仅当当前应用启用了 pin 规则时落盘）
- 统一菜单常量：`UnifiedMenuSkipCaretPending=304`（即时候选）、`UnifiedMenuPinCandidatePosition=305`（固定候选位置）；菜单文案中的 `<进程名>` 来自 `UnifiedMenuState.ActiveProcessName`，菜单弹出时由 coordinator 捕获快照传入
- 候选渲染常量 `DefaultCmdbarCandidatePrefix="⚡"`：副作用 cmdbar 候选 (`Actions` 含 `cmdbar.ActionEffect`) 的默认前缀；运行时值存 `RenderConfig.CmdbarPrefix`，通过 `Renderer.SetCmdbarPrefix(prefix)` 或 `Manager.SetCmdbarCandidatePrefix(prefix)` 注入，空串表示完全不显示。仅含 `ActionText`（`type(...)` 上屏）的候选与普通文本候选视觉上等价, 不会加前缀。

### Testing Requirements
- UI 代码高度依赖 Windows GDI/Win32，无法做纯 Go 单元测试
- `menu_disable_test.go` 是现有测试（菜单禁用状态逻辑）
- 视觉效果需在 Windows 环境下手动验证
- 布局计算逻辑（`renderer_layout.go`）可提取纯函数单独测试

### Common Patterns
- `UICommand.Type` 字符串值：`"show"`、`"hide"`、`"mode"`、`"toolbar_show"`、`"toolbar_hide"`、`"toolbar_update"`、`"settings"`、`"hide_menu"`、`"show_unified_menu"`
- 修改渲染逻辑后需检查水平/垂直两种候选布局
- DPI 变化（`WM_DPICHANGED`）触发字体和尺寸重新计算
- 新建窗口类型时使用 `WindowRegistry[T]` 管理 HWND 到 Go 实例的映射，避免全局变量
- 弹出菜单字母快捷键格式：菜单项文本末尾 `(X)` 括号内单字母，如 `"设置(S)"`

## Dependencies
### Internal
- `internal/uicmd` — 平台无关命令/事件模型 (cmdCh / eventCh 元素类型)
- `internal/candidate` — `Candidate` 数据 (protocol.go 中 type alias)
- `internal/cmdbar` — 候选词 Action / 副作用判定 (Win 端 renderer_layout.go 用)
- `pkg/theme` — 主题颜色定义
- `pkg/config` — 配置枚举类型 (Layout/PreeditMode 等)

### External
- 共用: `github.com/gogpu/gg` + `gg/text` (2D 渲染 + freetype 文本, 跨平台渲染核心)
- Win: `golang.org/x/sys/windows` (Win32 API), CGO 桥接 DirectWrite
- darwin: gg + 标准库 (image, log/slog, sync); 字体定位走 `pkg/systemfont` darwin catalog

## 全局约束
- 枚举与魔法字符串约束: 见 [`/docs/design/enum-constraint.md`](../../../docs/design/enum-constraint.md)
- macOS 移植设计: 见 [`/docs/design/macos-port.md`](../../../docs/design/macos-port.md) — Manager 命令模型如何接入 IMKit `.app`

<!-- MANUAL: -->
