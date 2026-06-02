<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-05-27 -->

# pkg/theme

## Purpose
主题系统。定义候选窗口、工具栏、弹出菜单、Tooltip、模式指示器的颜色与布局结构，提供从 YAML 文件动态加载主题、颜色解析、layout/palette 双形态组合与派生工具。

## Key Files

### v2 / Legacy（运行中，P5 后逐步退役）
| File | Description |
|------|-------------|
| `theme.go` | `Theme` 顶层结构体（含 Meta、CandidateWindow、Style、Toolbar、PopupMenu、Tooltip、ModeIndicator）；`Resolve()` 方法将字符串颜色解析为 `color.Color`；扩展了 v2.5 字段 `Layout`/`Palette`/`Overrides` |
| `colors.go` | `ParseColor`/`MustParseHexColor`：解析 `#RRGGBB` 或 `#RRGGBBAA` |
| `manager.go` | `Manager`：多路径搜索加载主题、列出可用主题、返回解析后主题 |
| `default_themes.go` | `emptyTheme()`：空主题 |

### v2.5（layout+palette 双形态、density、派生）
| File | Description |
|------|-------------|
| `layout.go` | 原始解析层 `LayoutSchema` + `Raw*` 结构体（`RawCandidateWindowLayout` 等）：距离/圆角/间隙/边框 用 `*int` 表示，nil=未写、非 nil（含 0）=显式值；plain 结构体（`Padding`/`BandLayout`/...）供 `resolved.go` 复用 |
| `palette.go` | `PaletteSchema` 数据类型：light/dark 双变体颜色 + 背景图 |
| `resolved.go` | `ResolvedV25` / `ResolvedLayout` / `ResolvedPalette`：renderer 消费形态（plain int） |
| `density.go` | density 基线表（compact/cozy/comfortable，直接返回 `ResolvedLayout`）与 `mergeWithDensityBaseline`（Raw `*int` → plain int 合并） |
| `derive.go` | 从 primary 派生语义色（HSL-shift；hct 暂等价 HSL-shift） |
| `refexpand.go` | 展开 palette 中的 `${name}` 引用 |
| `resolver.go` | `(*Manager).ResolveV25`：双形态加载器入口，外链 ID/内联对象统一处理 |
| `inline.go` | `(*Manager).InlineTheme` / `ExternalizeTheme`：内联与外链互转 |

### v2.6（盒模型 View，P2 切片-0/1 + P4-A 状态泡 + P4-B Tooltip + P4-C 工具栏 + P4-D 菜单）
| File | Description |
|------|-------------|
| `views.go` | 盒模型 View 主题 schema：`Views`/`ViewNode`/`ViewEdges`/`ViewFill`/`ViewBorder`（YAML schema，距离/边框用 `*int` 显式语义）+ `ResolvedViews`/`RVNode`（渲染消费 plain：几何逻辑像素 + 颜色 `color.Color`；`ResolvedViews.ShadowColor` 顶层）+ `defaultViews()` 基线 + `mergeViews`/`mergeViewNode`/`mergeEdges`（指针非 nil / string 非空 / slice 非 nil 覆盖 + Selected/Hover 递归）。`Theme.Views`→`ResolvedV25.Views`→`ResolvedTheme.Views` 透传：`ResolveV25` 原样透传主题 views（仅显式字段，**不** merge 基线），渲染器（internal/ui）以合成桥为基线、用主题 views 覆盖几何+颜色字段（颜色 token 在 ui 侧 `resolveViewColor` 解析）；字号/杂项仍走合成桥（字号用户全局优先）。**P4-A**：`Views` 新增 `Status *ViewNode`（状态泡，独立窗口单节点）；新增 `ResolvedStatusViews{BgColor, TextColor}`（状态泡解析后颜色，几何/字号由运行时 `StatusWindowConfig` 提供）；颜色 token 在 ui 侧 `resolveTokenColor`（通用 resolver 入口）+ `(*StatusRenderer).resolveStatusColors` 解析，映射 `Palette.Status`。**P4-B**：`Views` 新增 `Tooltip *ViewNode`（Tooltip 编码提示）；新增 `ResolvedTooltipViews{BgColor, TextColor}`（仅颜色，几何 render 内置）；ui 侧 `(*TooltipWindow).resolveTooltipColors` 映射 `Palette.Tooltip`。**P4-C**：`Views` 新增 `Toolbar *ToolbarViews`（`ToolbarViews`/`ToolbarButtonNode`/`ToolbarModeStates`/`ToolbarSettingsNode`：button base + mode 中/英状态覆盖 + settings 齿轮 icon/hole）；新增 `ResolvedToolbarViews`（扁平颜色集）；ui 侧 `(*ToolbarRenderer).resolveToolbarViews` 映射 `Palette.Toolbar`（button base 默认 = FullWidthOff*，零回归）。**P4-D**：`Views` 新增 `Menu *MenuViews`（`MenuViews`/`MenuHoverState`：背景/边框/文本/分隔/禁用 + hover 状态）；新增 `ResolvedMenuViews`（7 色扁平）；ui 侧 `(*PopupMenu).resolveMenuColors` 映射 `Palette.PopupMenu`|

## For AI Agents

### v2.5 schema 关键约定

- **layout + palette 解耦**：layout 描述尺寸，palette 描述颜色，theme.yaml 通过 `layout: <id>` + `palette: <id>` 组合
- **双形态**：
  - 外链：`layout: "compact-horizontal"` 字符串 ID → 加载器查 `themes/_layouts/<id>.yaml`
  - 内联：`layout: {meta: ..., density: ..., ...}` 对象 → 直接解析
  - 两种形态产出的 `ResolvedV25` 等价
- **overrides 仅外链形态使用**：`Theme.Overrides.Layout / .Palette` 为深度合并 map；内联形态直接改内联块即可
- **density 基线**：用户写 `density: compact|cozy|comfortable` 等同于先填该档基线，再用显式字段覆盖；未指定 density 视为 compact
- **显式 0 语义**：layout 原始解析层（`LayoutSchema` / `Raw*`）把"距离/圆角/间隙/边框"类字段建模为 `*int`：nil=未写（回退 density 基线），非 nil（含 0）=用户显式值。因此 `border_radius: 0`、`padding: {top: 0}`、`band_gap: 0` 等"显式关闭"语义会被保留，不被基线覆盖。字号/高度/最大宽度/Scale 仍用零值=回退基线（0 无物理意义）。`mergeWithDensityBaseline` 直接产出 plain int 的 `ResolvedLayout`
- **颜色派生**：palette 顶层语义色（bg/surface/border/text/text_dim/text_hint/accent/on_accent/shadow）未显式给出时按 derive.algorithm 派生；用户显式值始终优先
- **`${name}` 引用**：仅在 palette 文件内有效；可引用 `${primary}` 或同变体内的语义色名；禁止两级嵌套引用
- **背景图**：相对路径相对 palette 文件解析；`data:` URI 原样透传；模式：nine_slice / stretch / tile / center
- **候选序号 index**：`index.labels` 是序号显示的**唯一来源**（字符串数组，≤10），槽位 0→序号 1、…、槽位 9→第 10 个候选（index 0）；不足 10 项或空串槽位回退默认数字（1..9,0），单个标签不应含 `/`（渲染器以 `/` 切分槽位）。"风格"（`1.`/`①`/`❶`…）只是编辑器侧把对应字符填入 labels 的预设，schema 不再有 `style` 字段。`index.circle`（bool）是与 labels **正交**的圆形背景开关。`adapter.go`：`IndexLabels = buildIndexLabelsFromSlots(labels)`、`IndexStyle = circle ? "circle" : "text"`

### 主题搜索路径（优先级从高到低）
1. `%APPDATA%\WindInput\themes\<name>\theme.yaml`
2. `<exeDir>\data\themes\<name>\theme.yaml`

v2.5 共享零件目录：
- `themes/_layouts/<id>.yaml`
- `themes/_palettes/<id>.yaml`

### Testing Requirements
- `go test ./pkg/theme/` 覆盖：颜色解析、density 基线、派生、引用展开、双形态等价、Overrides、Inline/Externalize round-trip

### v2 → v2.5 兼容
- 旧 theme.yaml（v2 格式：light/dark 直挂组件颜色）继续可读
- `(*Theme).HasV25Schema()` 判定是否走 v2.5 解析路径
- 渲染器在 P2 阶段切换到 v2.5；过渡期两路径并存

## Dependencies
### Internal
- 无

### External
- `gopkg.in/yaml.v3`
- `image/color`（标准库）

<!-- MANUAL: -->
