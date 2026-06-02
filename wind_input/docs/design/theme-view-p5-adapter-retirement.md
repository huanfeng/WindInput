# 主题渲染 v2.6 P5：adapter + ResolvedTheme 退役（统一 ResolvedV25）

> 设计文档（spec）。承接 P1-P4（盒模型 View 引擎已驱动 5 类窗口）。**用户决策：放弃 v2/legacy 主题格式，只支持 v2.5。** 由此可彻底退役 adapter.go + ResolvedTheme + v2 解析路径，渲染层统一吃 ResolvedV25。

## 背景

当前颜色数据流有一个"legacy 适配中转层"：
```
theme.yaml → ResolveV25 → ResolvedV25(Layout/Palette/Views)
  → ResolvedToLegacy(adapter) → ResolvedTheme(v2 颜色结构) → 渲染层 5 类窗口
v2 主题 → Theme.Resolve → ResolvedTheme（同样喂渲染层）
```
`ResolvedTheme` 是 v2 时代的渲染消费结构；v2.5 靠 adapter 转成它来复用渲染层。放弃 v2 后，这个中转层失去存在意义——渲染层可直接吃 `ResolvedV25.Palette`（已覆盖全部颜色，且比 ResolvedTheme 更完整：Status 3 色 vs ModeIndicator 2 色、Toast 独立语义色）。

## 目标

- 渲染层（候选窗 + 状态泡 + Tooltip + 工具栏 + 菜单 + Toast + wind_setting 预览）统一从 `ResolvedV25`（主要是 `.Palette` + `.Layout` + `.Views`）取数据。
- 删除 `adapter.go`(`ResolvedToLegacy`)、`Theme.Resolve`(v2 路径)、`ResolvedTheme` 及 v2 legacy 颜色结构（`ThemeVariant`/`CandidateWindowColors`/`ToolbarColors`/`PopupMenuColors`/`TooltipColors`/`ModeIndicatorColors`）。
- 顺带修正两处语义错配（见 P5-6）。

## 非目标

- 候选窗合成桥（viewbox_views_bridge.go）的"纯 views 驱动"改造（FontSize/ItemHeight/DPI 等运行时字段仍需 RenderConfig 中间层；合成桥可保留为"RenderConfig→ResolvedViews"的几何/颜色装配器，只是数据来源从 ResolvedTheme 换成 ResolvedV25）。彻底删合成桥留作 P6 可选。
- 主题编辑器（P3，用户要求最后）。

## 退役后数据流

```
theme.yaml(v2.5) → ResolveV25 → ResolvedV25(Layout/Palette/Views)
  → 渲染层各窗口直接读 rv.Palette.{CandidateWindow/Toolbar/PopupMenu/Tooltip/Status/Toast} + rv.Layout + rv.Views
  → 候选窗 renderer.SetThemeV25(rv) 填 RenderConfig(几何/颜色) + themeViews
  → 合成桥 buildResolvedViews(RenderConfig) + applyThemeViews(themeViews, palette token)
非 v2.5 主题：加载时拒绝/报错（不再有 v2 回退）
```

## 分步路径（每步独立编译/测试/提交）

### P5-1：manager 缓存 ResolvedV25（地基，低风险）
- `Manager` 增 `resolvedV25 *ResolvedV25` 字段，`ResolveV25` 后与 `resolved` 并存缓存。
- 新增 `(*Manager).GetResolvedV25() *ResolvedV25`。
- adapter 暂不动（仍产 ResolvedTheme 供现有渲染层）。
- 文件：`pkg/theme/manager.go`。纯新增，无破坏。

### P5-2：4 窗口颜色源切到 Palette
- 状态泡/Tooltip/工具栏/菜单的 `resolveXxxViews` 改从 `ResolvedV25.Palette.{Status/Tooltip/Toolbar/PopupMenu}` 读，而非 `ResolvedTheme.{ModeIndicator/Tooltip/Toolbar/PopupMenu}`。
- 各窗口 SetTheme 改接收 `*ResolvedV25`（或其 Palette 子集 + Views）。`manager_indicator.go:applyTheme` 分发改传 ResolvedV25。
- token resolver（resolveTokenColor 的 res 函数）映射到 `ResolvedV25.Palette` 字段名。
- 字段名差异：`ResolvedToolbarPalette` vs `ResolvedToolbarColors` 等需对齐。
- 文件：`internal/ui/{viewbox_status,viewbox_tooltip,viewbox_toolbar,viewbox_menu}.go` + 各窗口 SetTheme + manager_indicator.go + 测试。
- **语义修正前置**：状态泡改读 `Palette.Status`（3 色，含 Border）而非 Toast；Toast 窗口改读 `Palette.Toast`（独立）而非 Tooltip。

### P5-3：候选窗 renderer + 合成桥切到 Palette/Layout（最复杂）
- `renderer.SetThemeV25(rv)` 从 `rv.Palette.CandidateWindow` 填 config 颜色、`rv.Layout.CandidateWindow` 填 Style 几何、`rv.Palette.CandidateWindow.AccentBar`/`rv.Palette.Shadow` 等。
- `getCommentColor`/`getShadowColor` 改读 config（已填）或 ResolvedV25，不再读 ResolvedTheme。
- 合成桥 `applyThemeViews` 的 token resolver 从 `ResolvedV25.Palette.CandidateWindow`（字段映射）。
- 运行时字段（FontSize/IndexFontSize/ItemHeight/ModeAccent/...）保持 RenderConfig，不动。
- 文件：`internal/ui/renderer.go` + `viewbox_views_bridge.go` + 测试。

### P5-4：wind_setting 预览切到 ResolvedV25
- `GetThemePreview` 改读 `GetResolvedV25().Palette` 字段，对齐字段名。
- 文件：`wind_setting/app_service.go` + 前端类型（若字段名变）。

### P5-5：删除 legacy（收尾）
- 删 `adapter.go` + `adapter_test.go`；`manager.go` 移除 v2 分支（HasV25Schema false → 加载失败/明确报错，不再 `Theme.Resolve`）。
- 删 `Theme.Resolve` + `ResolvedTheme` + v2 legacy 结构（ThemeVariant/CandidateWindowColors/ToolbarColors/PopupMenuColors/TooltipColors/ModeIndicatorColors）。
- 测试改注入 ResolvedV25 子集替代 `&theme.ResolvedTheme{...}`。
- `HasV25Schema`：保留作"主题合法性校验"（非 v2.5 拒绝加载），或评估是否还需要。
- 文件：`pkg/theme/{adapter,theme,manager,resolver,inline}.go` + 全部相关测试。

### P5-6：语义错配修正（随 P5-2/P5-3 落地，单独验证）
- `Palette.Status`（Background/Border/Text 3 色）正式启用——状态泡用它，不再借 Toast 色。
- Toast 窗口用 `Palette.Toast` 独立色，不再借 Tooltip 色。
- 记录：这是行为变化（颜色可能微变），需用户运行时确认；若希望零回归可让种子主题 palette 的 status/toast 色与原 toast/tooltip 一致。

## 风险与边界

- **运行时字段不可迁移**：FontSize/IndexFontSize/ItemHeight/DPI scale/ModeAccent/ModeLabel/CmdbarPrefix/HidePreedit/Layout/PreeditMode 是运行时状态，保留在 RenderConfig，不属主题数据。
- **字段名对齐**：`ResolvedXxxPalette`(v25) 与 `ResolvedXxxColors`(legacy) 字段命名不同，迁移需逐字段核对。
- **测试基建**：多个测试直接构造 `&theme.ResolvedTheme{...}`，P5-5 需全部改为 ResolvedV25 子集。
- **语义修正是行为变化**：P5-6 可能让 status/toast 颜色微变，需运行时核对或主题对齐保零回归。
- **wind_setting 跨 module**：改 GetThemePreview 字段源 + 可能前端类型；牢记 P4-C 教训——删/改 theme 公共结构务必全仓 grep（wind_input/wind_setting/wind_tsf）。

## 验收标准（全程）

- 每步：`go build ./...`（wind_input + wind_setting）✓ / `go fmt` ✓ / 相关测试 PASS / 运行时核对（涉视觉步）。
- 终态：`adapter.go`/`ResolvedTheme`/`Theme.Resolve`/v2 legacy 结构全部删除；全仓无 `ResolvedTheme` 引用；渲染层统一 ResolvedV25。
- 相关 AGENTS.md 同步。
