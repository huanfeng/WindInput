<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-06-04 -->

# pkg/theme

> ⛔ **主题 schema 已实现并冻结为 v3（2026-06-04）**。冻结字段语义/类型变更须走迁移（升版本+迁移器）；新增可选字段才是非破坏扩展。冻结契约见 `docs/design/theme-v3-freeze-report.md`，权威设计语义见 `docs/design/theme-schema-v3.md`。（`theme-v26-freeze-report.md` 为历史/迁移对照。）

## Purpose
主题系统。定义候选窗口、工具栏、弹出菜单、Tooltip、模式指示器的颜色与盒模型结构，提供从 YAML 文件动态加载主题、颜色递归求值、base 单链继承与派生工具。几何归一为 views（候选窗）+ internal/ui 内置常量（其它窗口，P8 View 化后）；**原 layout/density 几何机制已删（V3-D）**。

## Key Files

### 顶层 / 加载
| File | Description |
|------|-------------|
| `theme.go` | **v3 `Theme` 顶层结构体**：`Meta` + `Base string`（单链继承基主题 ID）+ 内联块 `Colors *PaletteSchema`(yaml:colors) / `Views` / `Behavior` / `Resources`。`HasV3Schema()` 判定是否有 colors 块。**V3-D：`Layout *LayoutSchema` 字段已删**；旧 `Layout any`/`Palette any` 外链字段 + `Overrides` + `HasV25Schema` 此前已删 |
| `colors.go` | `ParseColor`/`MustParseHexColor`：解析 `#RRGGBB` 或 `#RRGGBBAA` |
| `manager.go` | `Manager`：多路径搜索加载主题、列出可用主题、返回解析后主题。`loadThemeFileWithDir` 解析 **base 单链继承**：`loadRawThemeByName` 逐级加载、检测环、自底向上 `deepMergeTheme`（先合并后求值） |
| `merge_theme.go` | **v3-C base 继承的原始 Theme 深合并**：`deepMergeTheme`（colors tokens map 逐 key 覆盖、views 复用 mergeViews、behavior 逐指针字段覆盖、resources map 覆盖）。**V3-D：layout 合并分支已删**。求值铁律=合并作用于 raw，求值只在最终合并结果上跑一次 |
| `default_themes.go` | `emptyTheme()`：空主题（加载失败兜底，无 colors → resolvedV25 为 nil） |

### 几何 / 颜色 / 派生 / 求值
| File | Description |
|------|-------------|
| `layout.go` | **V3-D：仅剩 `BuildIndexLabelsFromSlots`（序号槽位拼接，供 views.index.labels）+ `Padding{Top/Right/Bottom/Left int}` 通用边距类型**（被 views.Fill.Slice、bgimage、internal/ui 的 Edges 别名复用）。原 `LayoutSchema`/`Raw*Layout`/`*Layout` 几何结构 + `LayoutMeta`/`intPtr` 已随 layout/density 几何机制删除 |
| `palette.go` | **v3 colors schema**：`PaletteSchema{Meta, Primary, Derive, AutoDark bool, Tokens map[string]Color}`（`Color=LightDark[string]`）；自定义 `UnmarshalYAML` 把 colors mapping 的 meta/primary/derive/auto_dark 抽出、其余键逐值按 LightDark 解析进 `Tokens`（扁平、无 light/dark 顶层分块、无嵌套窗口色组）。删 `PaletteVariant` + 6 个窗口色组 struct |
| `resolved.go` | `ResolvedV25`（**V3-D：`Layout` 字段已删**）/ `ResolvedPalette`（**v3**：顶层语义便捷字段 `Bg/Surface/.../Shadow` 从 Tokens 镜像 + `Tokens map[string]color.Color` 全部解析后 token + 保留 `Toolbar ResolvedToolbarPalette`；删 5 个 `Resolved*Palette` 组）：renderer 消费形态。**`ResolvedLayout` 结构已删** |
| `derive.go` | 从 primary 派生语义色（HSL-shift；hct 暂等价 HSL-shift）。**v3**：`applyDeriveToTokens`（缺失语义 token 由 primary 逐 light/dark 派生填入）+ `applyAutoDarkToTokens`（auto_dark：未给 dark 分支由 light 补，默认关） |
| `refexpand.go` | **v3 颜色递归求值**：`resolveColorTokens(tokens, primary, isDark)` 逐 token 在 isDark 环境下求不动点（`LightDark.Select(isDark)` 选分支 → `${token}` 多跳展开查 colors 表 → hex/transparent 终值 ParseColor）；循环/未知 token 报错（fail fast）。primary 为隐式 token |
| `resolver.go` | `(*Manager).ResolveV25`：在**已 base 合并的内联 Theme** 上求值（colors→derive→token 递归求值；views/behavior/resources）。不再做外链加载或 overrides（V3-C 删）、**亦不再做 layout/density 基线补全（V3-D 删）**。求值后调 `validateViews` 加载期校验，失败 fail fast（不进入渲染） |
| `validate.go` | **V3-D 加载期校验（决策 11）**：`validateViews(views, tokens, resources)` 遍历 views 各节点（含 states 递归 + toolbar/menu）校验 `${token}` 引用可达（在已解析 colors 表）、`image.ref` 在 resources 或为合法 path/data URI、颜色字面为合法 hex；首错即返回带节点路径的 error。循环/未知 token（colors 表内）由 refexpand fail fast、base 链环由 loadThemeFileWithDir fail fast，本文件补**使用点**引用校验 |

### v2.6（盒模型 View，P2 切片-0/1 + P4-A 状态泡 + P4-B Tooltip + P4-C 工具栏 + P4-D 菜单）
| File | Description |
|------|-------------|
| `views.go` | 盒模型 View 主题 schema：`Views`/`ViewNode`/`ViewEdges`/`ViewFill`/`ViewBorder`（YAML schema，距离/边框用 `*int` 显式语义）+ `ResolvedViews`/`RVNode`（渲染消费 plain：几何逻辑像素 + 颜色 `color.Color`；`ResolvedViews.ShadowColor` 顶层）+ `defaultViews()` 基线 + `mergeViews`/`mergeViewNode`/`mergeEdges`（指针非 nil / string 非空 / slice 非 nil 覆盖 + Selected/Hover 递归）。`Theme.Views`→`ResolvedV25.Views`→`ResolvedTheme.Views` 透传：`ResolveV25` 原样透传主题 views（仅显式字段，**不** merge 基线），渲染器（internal/ui）以合成桥为基线、用主题 views 覆盖几何+颜色字段（颜色 token 在 ui 侧 `resolveViewColor` 解析）；字号/杂项仍走合成桥（字号用户全局优先）。**P4-A**：`Views` 新增 `Status *ViewNode`（状态泡，独立窗口单节点）；新增 `ResolvedStatusViews{BgColor, TextColor}`（状态泡解析后颜色，几何/字号由运行时 `StatusWindowConfig` 提供）；颜色 token 在 ui 侧 `resolveTokenColor`（通用 resolver 入口）+ `(*StatusRenderer).resolveStatusColors` 解析，映射 `Palette.Status`。**P4-B**：`Views` 新增 `Tooltip *ViewNode`（Tooltip 编码提示）；新增 `ResolvedTooltipViews{BgColor, TextColor}`（仅颜色，几何 render 内置）；ui 侧 `(*TooltipWindow).resolveTooltipColors` 映射 `Palette.Tooltip`。**P4-C**：`Views` 新增 `Toolbar *ToolbarViews`（`ToolbarViews`/`ToolbarButtonNode`/`ToolbarModeStates`/`ToolbarSettingsNode`：button base + mode 中/英状态覆盖 + settings 齿轮 icon/hole）；新增 `ResolvedToolbarViews`（扁平颜色集）；ui 侧 `(*ToolbarRenderer).resolveToolbarViews` 映射 `Palette.Toolbar`（button base 默认 = FullWidthOff*，零回归）。**P4-D**：`Views` 新增 `Menu *MenuViews`（`MenuViews`/`MenuHoverState`：背景/边框/文本/分隔/禁用 + hover 状态）；新增 `ResolvedMenuViews`（7 色扁平）；ui 侧 `(*PopupMenu).resolveMenuColors` 映射 `Palette.PopupMenu`。**P7-5**：候选窗序号样式/标签/强调条开关归口 views——`ViewFill` 新增 `Shape`（`circle`\|`none`，仅 `views.index` 消费序号项圆形/无背景）；`AccentBarMetrics` 新增 `Enabled *bool`（强调条开关，原 `layout.accent_bar.enabled` 退役）；`defaultViews().Index.Labels` 提供默认数字槽位基线；SetTheme（ui）改读 `rv.Views.Index.Background.Shape` / `rv.Views.Index.Labels` / `rv.Views.Metrics.AccentBar.Enabled`。**P7-B**：逐元素字体激活——`ViewNode.FontSize`（逻辑像素绝对值）/`FontWeight`/`FontFamily`（平台字体族名）由 `ResolveCandidateViews` 读入 `RVNode`（新增 `RVNode.FontFamily`）；ui 侧对显式字号 ×DPI scale、未写回退运行时派生，字体族空=继承全局、未知名由平台文本引擎回退。**P7-C（图与层）**：新增通用图片对象 `ViewImage{ref,mode,slice,opacity,z,anchor,offset,size}`（P0 D5）；`ViewFill` 加 `Image *ViewImage`（背景填充图）；`ViewNode` 加 `Layers []ViewImage`（z 层级覆盖图，D4）；顶层 `Theme.Resources map[string]string`（名→路径/data URI，相对路径相对 theme.yaml；`ResolveV25` 解析为绝对路径填 `ResolvedV25.Resources`）。`RVNode` 加 `BgImage *RVImage` + `Layers []RVImage`（`RVImage`=plain spec，不含解码位图）；`ResolveCandidateViews` 经 `toRVImage` 每帧廉价填充 spec。**位图解码不在每帧路径**：ui 侧 `(*Renderer).imageForRef` 按 ref 经 `theme.LoadBackgroundImage` 一次性解码并缓存（SetTheme 换主题清空），`fillFor` 装配 `Fill{Image}`。背景图来源已由 palette 迁至 `views.window.background.image`（SetTheme 不再无条件清空）。**C2**：渲染消费接通——`(*Renderer).appendThemeLayers` 把 `RVNode.Layers` 解码后追加到 `View.Layers`（与 accent rail/光标层共存，offset/size 经 sc 缩放、z 层级）；`fillFor` 推广到 window/preedit/index 背景（任意非状态节点可带背景图）。item 背景图与选中态色冲突，留 P7-D。新增 `transparent` ColorToken（让 band 透出窗口背景图）。样例主题 `themes/jidian-classic`（九宫格渐变背景 + 右下角水印层）端到端验收。**P7-D（states 补全）**：`ViewNode` 加 `Disabled *ViewNode`（与 Selected/Hover 对称，`mergeViewNode` 递归合并）；新增 `RVState{BgColor, BgImage(高亮位图), TextColor, BorderColor, BorderWidth *int, FontWeight}`（状态 patch，零值/nil=沿用基态）；`RVNode` 的扁平 `SelectedBg/HoverBg color.Color` **替换为** `Selected/Hover/Disabled *RVState`。`candidate_views.go` 新增 `resolveState`，**states 按元素独立填充**：`Item` 三态（selected 默认 palette `SelectedBg`+`SelectedText`、hover 默认 `HoverBg`、disabled 无默认=预留）+ `Index`/`Comment` 各自 selected/hover（**无 palette 默认→未配置即 nil=沿用基态，默认与普通态一致**）。渲染（ui `viewbox_build*.go`）：`itemStateFor` 选活动态（selected 优先 hover）；`applyItemState` 给候选项 View 应用背景（**高亮位图 Fill.Image 优先于底色**）+ 边框；**候选文字按 `item.selected` 着色/加粗；序号/注释各按自身 `views.index.selected`/`views.comment.selected`**（`elementTextState`/`elementFill`，序号圆背景也可随态变）。**关键：选中文字不再统一牵动整行——序号有独立配色（蓝圆白数字不被误染）**。`resolver.go`：`SelectedText` 未配回退由 `OnAccent` 改 `Text`（普通色）→未配 `selected_text` 的主题选中字＝普通字（零回归）；要反差须显式配 `palette.selected_text` 或 `views.*.selected.color`。index/comment 的 `disabled` 态 schema 可解析但未 wire（同 item.disabled，预留）。**P7-E（预留字段形状 + 暗色图）**：① **渐变**`ViewFill.Gradient *ViewGradient{Type,Angle,Stops[]{Color,Pos}}`（CSS 风格 linear，**仅定 schema、渲染 later**，mergeViewNode 整体替换，RVNode 不消费）；② **结构化阴影**`ViewMetrics.Shadow *ViewShadowSpec{OffsetX,OffsetY,Blur,Spread *int,Color}`——offset_x/y + color 已实现（解析进 `ResolvedViews.ShadowOffsetX/Y` + 覆盖 ShadowColor，渲染用 X/Y），**blur/spread 预留**（解析不消费）；legacy 标量 `shadow_offset` 仍兜底（X=Y=它）；③ **暗色位图**：`Theme.Resources` 值由 `string` 改 `ResourceRef`（`resource_ref.go`，支持标量或 `{light,dark}`，YAML/JSON 双向 union marshal），`ResolveV25` 按 isDark 经 `PathFor` 选变体填 `ResolvedV25.Resources`（仍 `map[string]string`，下游无感）；④ **方向变体（D7）**：不加字段——views/Image 的 vertical 覆盖是冻结后非破坏扩展（见 theme-view-architecture.md）。守护测试 TestResourceRef_YAMLUnion/TestViewShadowSpec_Resolve/TestViewGradient_MergePreserved。**字号/单位重构（向旧效果对齐）**：① 几何字段（ViewEdges / ViewBorder.Width·Radius / ViewMetrics 各项 / ViewShadowSpec / AccentBar 宽·偏移）`*int`→`*Dimension`（`dimension.go`，支持 px/dp 单位：裸数字=dp 随 DPI 缩放、`"Npx"`=设备像素不缩放；YAML/JSON union marshal；渲染层 `Dimension.Scaled(scale)` 按单位换算）。② **`ViewNode.FontSize` 语义由「逻辑像素绝对值」改为「相对主候选字体的有符号偏移」**（-4/+2；ui `refreshResolvedViews` 统一 `字号=base+offset×scale`，**移除全部派生魔法**：旧 base-4 硬派生、文本序号 +2、注释 index±2、pager `max(14/12)` 下限、圆圈 `max(18/11)` 下限）。③ `Views` 新增 `ModeLabel ViewNode`（临时拼音等模式徽标，预编辑栏内；font_size 偏移 + color，基线 base-4 / `${comment}`，主题可覆盖）；ui 侧 `RenderConfig.IndexFontSize` 与 `getCommentColor` 已删除。④ 圆圈序号直径 = 序号字号 + `index.padding`（盒模型）；pager 字号 = `views.footer_bar.font_size` 偏移。守护 TestDimension_*/TestRefreshResolvedViews_RelativeFont。**P8（其它窗口几何 View 化）**：`other_views.go` 抽出通用 `resolveViewNode`(ViewNode→RVNode)，候选窗 `ResolveCandidateViews` 改调它（零行为变化），`resolveState` 泛化接受 colorResolver。**v3 颜色统一**：候选窗与其它窗口共用 `resolveColorToken(s, pal)`（`${name}`→`pal.Tokens[name]` / transparent / hex / 空·未知→nil）；删旧 `makeColorResolver`+`resolveCandidateViewColor`；`ResolveStatusViews/ResolveTooltipViews/ResolveToastViews/ResolveMenuViews` 默认色改取 `pal.Tokens["status_bg"]/["tooltip_bg"]/["toast_bg"]/["menu_*"]` 等功能 token。**切片1/2**：`ResolveStatusViews`/`ResolveTooltipViews` 让状态泡/Tooltip 消费完整盒模型 `RVNode`（几何+border+font+颜色；ui 侧 `(*StatusRenderer).resolveStatusNode`/`(*TooltipWindow).resolveTooltipNode` 叠加运行时覆盖 + 未配兜底现状值；扁平 `ResolvedStatusViews`/`ResolvedTooltipViews` 已停用）。**切片3**：`MenuViews` 由扁平 7 色重构为 `Root`/`Item`/`Separator` 三具名 `ViewNode`（item 含 hover/disabled patch），删 `MenuHoverState`；`ResolvedMenuViews` 改为 `Root`/`Item`/`Separator` 三 `RVNode`；`ResolveMenuViews` 映射 `Palette.PopupMenu`，ui 侧 `(*PopupMenu).resolveMenuViews` 消费、`buildMenuTree` 读盒模型（行高仍 FixedH，item 上下 padding 不独立以规避均摊坑；布局尺寸常量保留）。**切片5(toast)**：`Views` 加 `Toast *ViewNode`（mergeViews 透传，共 5 个独立窗口字段）；`ResolveToastViews` 映射 `Palette.Toast`；`toast_renderer.go` 从 gg 直绘改 View 树（`resolveToastNode` + root 列布局，标题/正文双字号文本叶子，accent 条=level 运行时色后处理，padding/radius 按现状兜底，bg 经 forceAlphaOpaque 不透明）。**切片4(toolbar) 延后**（需含按钮尺寸/布局动态化的更全面重构，几何与 HitTest 强耦合）。**切片6(图与层)**：背景图/layers 经 `RVNode.BgImage`/`Layers`（`resolveViewNode` 早已解析）由 ui 侧共享 `imageResolver` 消费（`fillFor` 装配背景图、`appendLayers` 追加覆盖层），覆盖 status/tooltip/menu(root)/toast；theme 包零改动（能力补充，主题不配即纯色，零回归）。**V3-D（metrics 归位 + states 递归）**：删 `ViewMetrics`/`AccentBarMetrics`/`Views.Metrics`/`mergeMetrics`/`mergeAccentBarMetrics`——列表级几何**归位到对应节点**：`ViewNode` 新增「仅特定节点」字段 `Shadow *ViewShadowSpec`（仅 window，原 metrics.shadow）/`Gap`/`BandGap *Dimension`（仅 candidate_list，原 item_spacing/band_gap）/`Enabled *bool`+`Width`/`Offset *Dimension`+`HeightRatio *float64`（仅 accent_bar，原 metrics.accent_bar）；`mergeViewNode` 合并这些字段（Shadow 逐字段）；`ResolveCandidateViews` 改从 `views.Window.Shadow`/`views.CandidateList.Gap·BandGap`/`views.AccentBar.*` 读取填 `ResolvedViews`（扁平几何字段不变，下游零改，golden 逐字节守）；ui 侧 `HasAccentBar` 改读 `views.AccentBar.Enabled`。**删 `RVState`**（决策 9）：`RVNode.Selected/Hover/Disabled` 类型 `*RVState`→`*RVNode`（递归完整盒模型 patch）；`resolveState` 复用 `resolveViewNode` 返回 `*RVNode`（保留「全空且无 palette 默认→nil」语义）；注意 `RVNode.BorderWidth` 是 `Dimension`（零值=未设），消费方（ui `applyItemState`）改判 `!= Dimension{}`|

## For AI Agents

### v3 schema 关键约定

- **顶层块按角色命名**：`colors`（颜色 token）/`views`（盒模型树，候选窗几何 + 其它窗口外观节点）/`behavior`（用户可覆盖默认）/`resources`（图片）；`base` 单链继承。**旧 `layout: <id>` + `palette: <id>` 外链 + `overrides` 已删；V3-D 起顶层 `layout` 几何块 + density 机制亦删**（其它窗口几何由 views 节点或 internal/ui 内置常量承载）。
- **base 单链继承（V3-C）**：`base: <themeId>` 引一个基主题（按 themeDirs 找 `<base>/theme.yaml`，与普通主题同路径）。"复用别人配色" = 写一个只定义 colors 的 base 主题再 `base:` 它；疏密档位 = 写只含 views 几何的 base 主题再 `base:` 它。
  - **求值顺序铁律 = 先合并后求值**：`resolved = resolve(deepMerge(rawBase链, rawSelf))`。继承作用于**原始未求值 raw Theme**（colors token map 逐 key 覆盖、views 复用 mergeViews/mergeViewNode、behavior 逐指针字段覆盖），合并完才统一 resolve（derive→token 展开→变体选取）。这样派生主题只换 `colors.primary`，全套派生语义色会基于**新 primary** 重新派生（守护测试 `TestInheritResolveOrder`）。
  - **禁止成环**：base 链检测环并报错（`loadThemeFileWithDir`）。暂只单 base，不做 import 多源。
- **颜色派生（v3 双维）**：`derive`（primary→缺失语义色，逐 light/dark）+ `auto_dark`（未给 dark 分支由 light 补，默认 false）；均作用于 colors token 表、在使用点求值之前；用户显式值始终优先
- **`${name}` 引用（v3）**：colors token 值可互引 `${token}`（含隐式 `${primary}`），支持多跳 + LightDark 任意交错嵌套；循环引用与未知 token 报错（非静默）。views 节点颜色字段 `${name}` 由 `resolveColorToken`(候选窗)/各 `ResolveXxxViews`(其它窗口) 查 `ResolvedPalette.Tokens[name]`——**theme.yaml 节点 token 名须与 colors token key 完全一致**（顶层语义 `${bg}/${accent}/${selection}/${hover}/${selection_text}…` + 功能 token `${menu_*}/${tooltip_*}/${status_*}/${toast_*}/${toolbar_*}`）
- **背景图**：相对路径相对 self theme.yaml 解析；`data:` URI 原样透传；模式：nine_slice / stretch / tile / center
- **候选序号 index（P7-5 起归口 views）**：`views.index.labels` 是序号显示的**唯一来源**（字符串数组，≤10），槽位 0→序号 1、…、槽位 9→第 10 个候选（index 0）；不足 10 项或空串槽位回退默认数字（1..9,0，由 `defaultViews().Index.Labels` 基线提供），单个标签不应含 `/`（渲染器以 `/` 切分槽位）。"风格"（`1.`/`①`/`❶`…）只是编辑器侧把对应字符填入 labels 的预设。`views.index.background.shape`（`circle`\|`none`，空=none）是与 labels **正交**的序号项背景形状：`circle` 画圆形底，`none` 仅文本——序号文本始终显示。SetTheme（ui）：`IndexLabels = BuildIndexLabelsFromSlots(views.index.labels)`、`IndexStyle = (shape=="circle") ? "circle" : "text"`（内部 sentinel `"text"`=无背景）

### 主题搜索路径（优先级从高到低）
1. `%APPDATA%\WindInput\themes\<name>\theme.yaml`
2. `<exeDir>\data\themes\<name>\theme.yaml`

base 主题与普通主题同路径（`<base>/theme.yaml`）。唯一隐藏基础主题：`_base`（`_` 前缀 → 不出现在主题列表；default/msime/jidian 均 `base: _base`）。
**旧 `_layouts/` `_palettes/` 共享零件目录已删除**（V3-C 转为 base 主题）；旧两套 base `windy-blue`/`msime-base` 已合并为单一 `_base`。

### Testing Requirements
- `go test ./pkg/theme/` 覆盖：颜色解析、派生、引用展开、base 单链继承（`TestResolveV25_Inherited` / `TestInheritResolveOrder` 守护「先合并后求值」）、暗色变体、加载期校验
- **回归门槛**：`TestV26GoldenSnapshot`（default/msime × light/dark 逐字节）—— 改 schema/继承机制后必须仍逐字节绿。**仅纯结构调整（删段/换序、值不变）才可 UPDATE_GOLDEN 重落**，且须先用 firstDiff 逐项确认 diff 只在被调整段（如 V3-D 删 layout 段：diff 仅 `110,112d109` 3 行 layout）；改了任何颜色/几何**值**导致的 diff 一律视为回归，禁重落

### 兼容性
- **一刀切只读 v3**（v2.5/v2.6 未发布，无兼容包袱）。无 colors 块的主题（emptyTheme 兜底）解析为 nil resolvedV25。
- `(*Theme).HasV3Schema()` 判定是否走 v3 解析路径

## Dependencies
### Internal
- 无

### External
- `gopkg.in/yaml.v3`
- `image/color`（标准库）

<!-- MANUAL: -->
