<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-06-02 -->

# pkg/theme

> ⛔ **候选窗 schema 已冻结为 v2.6（2026-06-02）**。冻结字段语义/类型变更须走迁移（升版本+迁移器）；新增可选字段才是非破坏扩展。冻结声明 + 契约见 `docs/design/theme-view-architecture.md`「⛔ v2.6 schema 冻结声明」与 `docs/design/theme-v26-freeze-report.md`。未冻结：其它窗口几何（P8）。

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
| `layout.go` | 原始解析层 `LayoutSchema` + `Raw*` 结构体（toolbar/status/tooltip/popup_menu/toast）：距离/圆角/间隙/边框 用 `*int` 表示，nil=未写、非 nil（含 0）=显式值；plain 结构体（`Padding`/`ToolbarLayout`/...）供 `resolved.go` 复用。**P7-5：候选窗几何/序号/强调条/行高已迁 views/behavior，layout 不再承载候选窗**（`BuildIndexLabelsFromSlots` 仍在此，供 views.index.labels 拼接）|
| `palette.go` | `PaletteSchema` 数据类型：light/dark 双变体颜色 + 背景图 |
| `resolved.go` | `ResolvedV25` / `ResolvedLayout`（P7-5 起仅 toolbar/status/tooltip/popup_menu/toast，无候选窗）/ `ResolvedPalette`（`Palette.CandidateWindow` 颜色仍在）：renderer 消费形态（plain int） |
| `density.go` | density 基线表（compact/cozy/comfortable，直接返回 `ResolvedLayout`）与 `mergeWithDensityBaseline`（Raw `*int` → plain int 合并） |
| `derive.go` | 从 primary 派生语义色（HSL-shift；hct 暂等价 HSL-shift） |
| `refexpand.go` | 展开 palette 中的 `${name}` 引用 |
| `resolver.go` | `(*Manager).ResolveV25`：双形态加载器入口，外链 ID/内联对象统一处理 |
| `inline.go` | `(*Manager).InlineTheme` / `ExternalizeTheme`：内联与外链互转 |

### v2.6（盒模型 View，P2 切片-0/1 + P4-A 状态泡 + P4-B Tooltip + P4-C 工具栏 + P4-D 菜单）
| File | Description |
|------|-------------|
| `views.go` | 盒模型 View 主题 schema：`Views`/`ViewNode`/`ViewEdges`/`ViewFill`/`ViewBorder`（YAML schema，距离/边框用 `*int` 显式语义）+ `ResolvedViews`/`RVNode`（渲染消费 plain：几何逻辑像素 + 颜色 `color.Color`；`ResolvedViews.ShadowColor` 顶层）+ `defaultViews()` 基线 + `mergeViews`/`mergeViewNode`/`mergeEdges`（指针非 nil / string 非空 / slice 非 nil 覆盖 + Selected/Hover 递归）。`Theme.Views`→`ResolvedV25.Views`→`ResolvedTheme.Views` 透传：`ResolveV25` 原样透传主题 views（仅显式字段，**不** merge 基线），渲染器（internal/ui）以合成桥为基线、用主题 views 覆盖几何+颜色字段（颜色 token 在 ui 侧 `resolveViewColor` 解析）；字号/杂项仍走合成桥（字号用户全局优先）。**P4-A**：`Views` 新增 `Status *ViewNode`（状态泡，独立窗口单节点）；新增 `ResolvedStatusViews{BgColor, TextColor}`（状态泡解析后颜色，几何/字号由运行时 `StatusWindowConfig` 提供）；颜色 token 在 ui 侧 `resolveTokenColor`（通用 resolver 入口）+ `(*StatusRenderer).resolveStatusColors` 解析，映射 `Palette.Status`。**P4-B**：`Views` 新增 `Tooltip *ViewNode`（Tooltip 编码提示）；新增 `ResolvedTooltipViews{BgColor, TextColor}`（仅颜色，几何 render 内置）；ui 侧 `(*TooltipWindow).resolveTooltipColors` 映射 `Palette.Tooltip`。**P4-C**：`Views` 新增 `Toolbar *ToolbarViews`（`ToolbarViews`/`ToolbarButtonNode`/`ToolbarModeStates`/`ToolbarSettingsNode`：button base + mode 中/英状态覆盖 + settings 齿轮 icon/hole）；新增 `ResolvedToolbarViews`（扁平颜色集）；ui 侧 `(*ToolbarRenderer).resolveToolbarViews` 映射 `Palette.Toolbar`（button base 默认 = FullWidthOff*，零回归）。**P4-D**：`Views` 新增 `Menu *MenuViews`（`MenuViews`/`MenuHoverState`：背景/边框/文本/分隔/禁用 + hover 状态）；新增 `ResolvedMenuViews`（7 色扁平）；ui 侧 `(*PopupMenu).resolveMenuColors` 映射 `Palette.PopupMenu`。**P7-5**：候选窗序号样式/标签/强调条开关归口 views——`ViewFill` 新增 `Shape`（`circle`\|`none`，仅 `views.index` 消费序号项圆形/无背景）；`AccentBarMetrics` 新增 `Enabled *bool`（强调条开关，原 `layout.accent_bar.enabled` 退役）；`defaultViews().Index.Labels` 提供默认数字槽位基线；SetTheme（ui）改读 `rv.Views.Index.Background.Shape` / `rv.Views.Index.Labels` / `rv.Views.Metrics.AccentBar.Enabled`。**P7-B**：逐元素字体激活——`ViewNode.FontSize`（逻辑像素绝对值）/`FontWeight`/`FontFamily`（平台字体族名）由 `ResolveCandidateViews` 读入 `RVNode`（新增 `RVNode.FontFamily`）；ui 侧对显式字号 ×DPI scale、未写回退运行时派生，字体族空=继承全局、未知名由平台文本引擎回退。**P7-C（图与层）**：新增通用图片对象 `ViewImage{ref,mode,slice,opacity,z,anchor,offset,size}`（P0 D5）；`ViewFill` 加 `Image *ViewImage`（背景填充图）；`ViewNode` 加 `Layers []ViewImage`（z 层级覆盖图，D4）；顶层 `Theme.Resources map[string]string`（名→路径/data URI，相对路径相对 theme.yaml；`ResolveV25` 解析为绝对路径填 `ResolvedV25.Resources`）。`RVNode` 加 `BgImage *RVImage` + `Layers []RVImage`（`RVImage`=plain spec，不含解码位图）；`ResolveCandidateViews` 经 `toRVImage` 每帧廉价填充 spec。**位图解码不在每帧路径**：ui 侧 `(*Renderer).imageForRef` 按 ref 经 `theme.LoadBackgroundImage` 一次性解码并缓存（SetTheme 换主题清空），`fillFor` 装配 `Fill{Image}`。背景图来源已由 palette 迁至 `views.window.background.image`（SetTheme 不再无条件清空）。**C2**：渲染消费接通——`(*Renderer).appendThemeLayers` 把 `RVNode.Layers` 解码后追加到 `View.Layers`（与 accent rail/光标层共存，offset/size 经 sc 缩放、z 层级）；`fillFor` 推广到 window/preedit/index 背景（任意非状态节点可带背景图）。item 背景图与选中态色冲突，留 P7-D。新增 `transparent` ColorToken（让 band 透出窗口背景图）。样例主题 `themes/jidian-classic`（九宫格渐变背景 + 右下角水印层）端到端验收。**P7-D（states 补全）**：`ViewNode` 加 `Disabled *ViewNode`（与 Selected/Hover 对称，`mergeViewNode` 递归合并）；新增 `RVState{BgColor, BgImage(高亮位图), TextColor, BorderColor, BorderWidth *int, FontWeight}`（状态 patch，零值/nil=沿用基态）；`RVNode` 的扁平 `SelectedBg/HoverBg color.Color` **替换为** `Selected/Hover/Disabled *RVState`。`candidate_views.go` 新增 `resolveState`，**states 按元素独立填充**：`Item` 三态（selected 默认 palette `SelectedBg`+`SelectedText`、hover 默认 `HoverBg`、disabled 无默认=预留）+ `Index`/`Comment` 各自 selected/hover（**无 palette 默认→未配置即 nil=沿用基态，默认与普通态一致**）。渲染（ui `viewbox_build*.go`）：`itemStateFor` 选活动态（selected 优先 hover）；`applyItemState` 给候选项 View 应用背景（**高亮位图 Fill.Image 优先于底色**）+ 边框；**候选文字按 `item.selected` 着色/加粗；序号/注释各按自身 `views.index.selected`/`views.comment.selected`**（`elementTextState`/`elementFill`，序号圆背景也可随态变）。**关键：选中文字不再统一牵动整行——序号有独立配色（蓝圆白数字不被误染）**。`resolver.go`：`SelectedText` 未配回退由 `OnAccent` 改 `Text`（普通色）→未配 `selected_text` 的主题选中字＝普通字（零回归）；要反差须显式配 `palette.selected_text` 或 `views.*.selected.color`。index/comment 的 `disabled` 态 schema 可解析但未 wire（同 item.disabled，预留）。**P7-E（预留字段形状 + 暗色图）**：① **渐变**`ViewFill.Gradient *ViewGradient{Type,Angle,Stops[]{Color,Pos}}`（CSS 风格 linear，**仅定 schema、渲染 later**，mergeViewNode 整体替换，RVNode 不消费）；② **结构化阴影**`ViewMetrics.Shadow *ViewShadowSpec{OffsetX,OffsetY,Blur,Spread *int,Color}`——offset_x/y + color 已实现（解析进 `ResolvedViews.ShadowOffsetX/Y` + 覆盖 ShadowColor，渲染用 X/Y），**blur/spread 预留**（解析不消费）；legacy 标量 `shadow_offset` 仍兜底（X=Y=它）；③ **暗色位图**：`Theme.Resources` 值由 `string` 改 `ResourceRef`（`resource_ref.go`，支持标量或 `{light,dark}`，YAML/JSON 双向 union marshal），`ResolveV25` 按 isDark 经 `PathFor` 选变体填 `ResolvedV25.Resources`（仍 `map[string]string`，下游无感）；④ **方向变体（D7）**：不加字段——views/Image 的 vertical 覆盖是冻结后非破坏扩展（见 theme-view-architecture.md）。守护测试 TestResourceRef_YAMLUnion/TestViewShadowSpec_Resolve/TestViewGradient_MergePreserved|

## For AI Agents

### v2.5 schema 关键约定

- **layout + palette 解耦**：layout 描述尺寸，palette 描述颜色，theme.yaml 通过 `layout: <id>` + `palette: <id>` 组合
- **双形态**：
  - 外链：`layout: "compact-horizontal"` 字符串 ID → 加载器查 `themes/_layouts/<id>.yaml`
  - 内联：`layout: {meta: ..., density: ..., ...}` 对象 → 直接解析
  - 两种形态产出的 `ResolvedV25` 等价
- **overrides 仅外链形态使用**：`Theme.Overrides.Layout / .Palette` 为深度合并 map；内联形态直接改内联块即可
- **density 基线**：用户写 `density: compact|cozy|comfortable` 等同于先填该档基线，再用显式字段覆盖；未指定 density 视为 compact
- **显式 0 语义**：layout 原始解析层（`LayoutSchema` / `Raw*`）把"距离/圆角/间隙/边框"类字段建模为 `*int`：nil=未写（回退 density 基线），非 nil（含 0）=用户显式值。因此 `border_radius: 0`、`padding: {top: 0}`、`item_gap: 0`（toolbar）等"显式关闭"语义会被保留，不被基线覆盖。字号/高度/最大宽度/Scale 仍用零值=回退基线（0 无物理意义）。`mergeWithDensityBaseline` 直接产出 plain int 的 `ResolvedLayout`
- **颜色派生**：palette 顶层语义色（bg/surface/border/text/text_dim/text_hint/accent/on_accent/shadow）未显式给出时按 derive.algorithm 派生；用户显式值始终优先
- **`${name}` 引用**：仅在 palette 文件内有效；可引用 `${primary}` 或同变体内的语义色名；禁止两级嵌套引用
- **背景图**：相对路径相对 palette 文件解析；`data:` URI 原样透传；模式：nine_slice / stretch / tile / center
- **候选序号 index（P7-5 起归口 views）**：`views.index.labels` 是序号显示的**唯一来源**（字符串数组，≤10），槽位 0→序号 1、…、槽位 9→第 10 个候选（index 0）；不足 10 项或空串槽位回退默认数字（1..9,0，由 `defaultViews().Index.Labels` 基线提供），单个标签不应含 `/`（渲染器以 `/` 切分槽位）。"风格"（`1.`/`①`/`❶`…）只是编辑器侧把对应字符填入 labels 的预设。`views.index.background.shape`（`circle`\|`none`，空=none）是与 labels **正交**的序号项背景形状：`circle` 画圆形底，`none` 仅文本——序号文本始终显示。SetTheme（ui）：`IndexLabels = BuildIndexLabelsFromSlots(views.index.labels)`、`IndexStyle = (shape=="circle") ? "circle" : "text"`（内部 sentinel `"text"`=无背景）

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
