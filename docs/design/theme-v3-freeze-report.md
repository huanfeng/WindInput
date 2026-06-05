<!-- 生成: 2026-06-04 | 用途: v3 主题 schema 冻结评估 + 契约 -->

# WindInput v3 主题系统 — 冻结契约（结构归一 + 亮暗统一）

> 状态：**已实现并冻结（2026-06-04）**。权威 spec：`theme-schema-v3.md`（设计语义）；本文为冻结后的**字段面契约 + 求值语义快照**。
> 前身：`archive/theme-v26-freeze-report.md`（v2.5/v2.6，已解冻被 v3 取代，仅作历史/迁移对照）。
> 关联：`pkg/theme/AGENTS.md`（实现现状）、`docs/design/enum-constraint.md`（枚举约束）。

## 一、v3 净效果（相对 v2.6）

v3 是一次**删机制**的破坏性重构（v2.5/v2.6 未正式发布，无兼容包袱，一刀切只读 v3）：

| 维度 | v2.6（删除） | v3 | 净效果 |
|---|---|---|---|
| 几何来源 | `views` + `layout` + `density` 三套并行 | 仅 `views`（盒模型树，候选窗）+ internal/ui 内置常量（其它窗口几何，P8 View 化后） | 三合一；`layout.go`/`density.go` 几何机制删除 |
| 颜色结构 | `palette` 顶层语义 + 6 个嵌套窗口色组（~83% 纯转发冗余） | `colors` 扁平 token（顶层语义 + 提升语义 + 功能前缀） | 二层合一；删 6 个 `Resolved*Palette` |
| 亮暗机制 | 颜色用顶层 light/dark 分块（选整块）；图片逐键 `{light,dark}` | 统一 `LightDark<T>` 逐值原语（颜色与图片同构） | 单一原语贯穿 |
| metrics | `metrics` 杂物抽屉（shadow / item_spacing / band_gap / accent_bar 几何） | 属性归位到对应节点（window / candidate_list / accent_bar） | 删杂物抽屉 |
| states 消费层 | 扁平 `RVState` | 递归 `RVNode` | 状态 patch 与基态同构 |
| 继承 | `palette/layout: string` 外链 + `Overrides` | `base` 单链继承（先合并后求值） | 复用别人配置 = base 一个基主题 |

**回归判据（贯穿全程）**：v3 主题渲染产物逐字节等于 v2.6 基线——`TestV3GoldenSnapshot`（default/msime × light/dark）守护候选窗 + status/tooltip/toast/menu 的解析后颜色/几何值。

## 二、顶层结构（冻结）

```yaml
theme:
  meta:      { name, version?, author?, order? }   # 元数据
  base?:     <themeId>                              # 单链继承（替代旧 palette/layout 外链 + Overrides）
  colors:    <ColorTokens>                          # 提供者①：颜色语义 token（唯一颜色真源）
  resources: <ResourceMap>                          # 提供者②：图片资源
  views:     <Views>                                # 结构：候选窗几何 + 引用 + 其它窗口外观节点
  behavior:  <Behavior>                             # 用户可覆盖的主题推荐默认（按模块）
```

Go 侧 `theme.Theme{ Meta, Base, Colors *PaletteSchema, Views *Views, Behavior *Behavior, Resources map[string]ResourceRef }`。
**`Theme.Layout` 字段已删**（V3-D）；`HasV3Schema()` 判据由 `Colors != nil || Layout != nil` 收敛为 `Colors != nil`。

> **块的受众二分**：`colors`/`resources`/`views` 是**设计师的画布**（整套换主题）；`behavior` 是**用户的旋钮**（主题给推荐默认，用户可独立覆盖、覆盖跨主题保持）。

## 三、原语类型（冻结）

```
Dimension    = number | "Npx" | "Ndp"        # dp 默认（随 DPI 缩放）；px 不缩放（发丝线）
LightDark<T> = T | { light: T; dark: T }      # 亮暗原语：单值=共用；对象=分设
Color        = LightDark<ColorScalar>         # Go: Color = LightDark[string]
ColorScalar  = "#RRGGBB[AA]" | "${tokenName}" | "transparent"
ImageRef     = LightDark<string>              # resources 键 / 路径 / data:URI（Go: ResourceRef）
```

- **`LightDark<T>` 仅作用于表现层值**（color / gradient stop / image ref）；**几何与结构不随亮暗变化**（Dimension 不是 LightDark）。
- 命名诚实：仅表达亮暗二值轴，故命名 `LightDark` 而非泛化 `Variant`（YAGNI）。
- Go 实现 `LightDark[T]` 与 `ResourceRef` 同构（标量或 `{light,dark}` + 按 isDark 选取）。

## 四、colors（颜色 token 表，冻结）

`colors` 为**扁平 token 映射**，值为 `LightDark<color>`，可互引 `${token}`。Go：`PaletteSchema{ Meta, Primary string, Derive DeriveConfig, AutoDark bool, Tokens map[string]Color }`（自定义 `UnmarshalYAML` 抽出 meta/primary/derive/auto_dark，其余键逐值进 `Tokens`）。

**token 命名分三类（均扁平、均 LightDark）：**

| 类 | token | 说明 |
|---|---|---|
| 顶层语义 | `primary` `bg` `surface` `border` `text` `text_dim` `text_hint` `accent` `on_accent` `shadow` | 抽象语义优先 |
| 提升语义 | `selection` `selection_text` `hover` | 原 candidate_window 特有色（selected_bg/hover_bg）提升为顶层语义 |
| 功能前缀 | `menu_*`（bg/border/text/hover_bg/hover_text/disabled/separator）`tooltip_*`（bg/text）`status_*`（bg/text/border）`toast_*`（bg/text）`toolbar_*`（13 色） | 无抽象语义对应的窗口特有色（对「token 须抽象语义」的务实放宽，仍扁平） |

**两个正交 derive（均作用于 colors 表、使用点求值之前）：**
- `derive`（语义维度，`DeriveConfig{Enabled bool, Algorithm "hsl-shift"|"hct"|"none"}`）：仅给 `primary`，自动派生缺失语义色（`applyDeriveToTokens`；hct 当前等价 hsl-shift）。
- `auto_dark`（变体维度，`bool`，默认 false）：未显式给 `dark` 分支的 token 由其 `light` 派生（`applyAutoDarkToTokens`）。

**ResolvedPalette 形态**：顶层语义便捷字段（`Bg/Surface/Border/Text/TextDim/TextHint/Accent/OnAccent/Shadow/Primary`，从 Tokens 镜像）+ `Tokens map[string]color.Color`（全部解析后 token）+ `Toolbar ResolvedToolbarPalette`（从 `toolbar_*` token 填充，使 `viewbox_toolbar` 最小改动）。**已删** 5 个嵌套 `Resolved*Palette`（candidate/menu/tooltip/status/toast）。

## 五、resources（图片提供者，冻结）

```yaml
resources:
  panel: { light: "panel.png", dark: "panel-dark.png" }   # LightDark<ref>
  badge: "badge.png"                                       # 单值=共用
```

与 colors 同构（`resource_ref.go` 现成实现）；相对路径相对 theme.yaml 目录解析，`data:` URI 原样。

## 六、views（唯一结构树，冻结）

候选窗几何 + 引用全在 `views`；疏密档位由 **base 继承**实现（不再有 density 机制）。其它窗口几何随 P8 View 化后由 views 节点（status/tooltip/toast/menu）或 internal/ui 内置常量（toolbar 几何延后）承载。

### 属性归位（取消 metrics 杂物抽屉）

| 原 metrics 项 | 归位到 | ViewNode 字段 |
|---|---|---|
| `shadow`（窗口投影） | `window` 节点 | `Shadow *ViewShadowSpec{offset_x,offset_y,blur*,spread*,color}` |
| `item_spacing`（候选项间距） | `candidate_list` 节点 | `Gap *Dimension` |
| `band_gap`（band 间距） | `candidate_list` 节点 | `BandGap *Dimension` |
| `accent_bar` 几何 | `accent_bar` 节点 | `Enabled *bool` / `Width *Dimension` / `Offset *Dimension` / `HeightRatio *float64` |

### ViewNode 字段面（冻结）

```
margin / padding : ViewEdges{top,right,bottom,left : Dimension}
border           : ViewBorder{width *Dimension, color string(token), radius *Dimension}
background       : ViewFill{ color, shape(circle|none), gradient(linear/radial), image:ViewImage }
font_family / font_size(相对主字号有符号偏移) / font_weight / color(token)
labels[]         : 仅 index（序号槽位 ≤10）
layers[]         : ViewImage{ ref,mode(nine_slice|stretch|tile|center),slice,opacity,z,anchor,offset{x,y},size{w,h} }
shadow           : 仅 window（属性归位）
gap / band_gap   : 仅 candidate_list（属性归位）
enabled/width/offset/height_ratio : 仅 accent_bar（属性归位）
selected / hover / disabled : *ViewNode（递归 patch）
```

**具名 View 骨架（`Views`）**：候选窗 `window / preedit_bar / candidate_list / item / index / text / comment / accent_bar / footer_bar / mode_label`；其它窗口 `status / tooltip / toast`（单 `*ViewNode`）、`toolbar *ToolbarViews`、`menu *MenuViews`。

**状态 patch 递归**：消费层 `RVNode.Selected/Hover/Disabled *RVNode`（取代扁平 `RVState`）——未来 hover 要渐变/图层时零改结构。各字段零值/nil = 该属性不覆盖、沿用基态。

> 标 `*` 的字段（阴影 `blur`/`spread`、item 基底色/border、preedit border、footer align、disabled 运行时触发器）是**可补能力**：schema 占位、merge 保留、渲染 later。v3 只对齐 v2.6 现状渲染，不在结构改造里夹带能力补全（保回归判据纯粹）。
> **`gradient` 已于 P7-E 落地渲染**（linear/radial，`pkg/theme/gradient.go`），能力矩阵标 `supported`，不再是占位字段。

## 七、behavior（哲学Y：主题推荐默认 + 用户覆盖层，冻结）

```yaml
behavior:
  candidate:           # 当前 schema 扁平存于 Behavior（font_size/always_show_pager/show_page_number/vertical_max_width）
    font_size:          18
    vertical_max_width: 600
    always_show_pager:  false
    show_page_number:   true
```

Go：`Behavior{ FontSize, AlwaysShowPager, ShowPageNumber, VerticalMaxWidth *指针 }`（nil=走 `defaultBehavior` 基线）；解析后 `ResolvedBehavior`（plain 值）。

**双层覆盖模型（已落地）**：
```
最终值 = followTheme ? 主题 behavior 推荐默认 : 用户 config.UI 自定义值
```
- **主题层**：`theme.behavior.*` 是主题作者推荐默认。
- **用户层**：`config.UI` 为**每个** behavior 字段提供用户覆盖 + `*FollowTheme` 开关：
  - `FontSize` + `FontSizeFollowTheme`（既有）
  - `AlwaysShowPager` + `AlwaysShowPagerFollowTheme`（V3-D 补）
  - `ShowPageNumber` + `ShowPageNumberFollowTheme`（V3-D 补）
  - `VerticalMaxWidth` + `VerticalMaxWidthFollowTheme`（V3-D 补）
  - 所有 `*FollowTheme` 默认 `true`（新装跟随主题）；**不可加 omitempty**（默认 true 的 bool 会破坏 diff-save 闭环）。
- **应用点**：`renderer.applyBehaviorOverrides()`（pager/page_number，由 `SetTheme` + `SetBehaviorOverrides` 触发）+ `viewbox_render`（vertical_max_width 每帧据 follow 标志选源）。用户 `PagerDisplayMode`（never/auto/always）是更上层的独立强制覆盖，在 `applyPagerOverride` 注入。

> **判据**：behavior = "需独立用户覆盖、且覆盖跨主题保持"的设定集合（如 `vertical_max_width` 即便是几何也留 behavior——用户需按屏幕覆盖它）。

## 八、求值管线（冻结，权威语义见 theme-schema-v3.md「解析管线」）

```
1. 继承（先合并，作用于 raw）：merged = deepMerge(rawBase 链, rawSelf)   —— 纯 raw schema 合并，不求值
2. derive（作用于 merged.colors 表）：derive.enabled 由 primary 派生缺失语义色；auto_dark 逐值补 dark
3. 颜色递归求值（isDark 环境下对每个使用点求不动点）：
     LightDark → 取当前 isDark 分支后继续；${token} → 查表替换后继续（循环保护）；hex/transparent → 终止
4. 图片：node ref → 查 resources → PathFor(isDark)
5. 几何：Dimension × DPI scale
6. behavior：最终值 = followTheme ? 主题 behavior : config.UI 用户值
```

**求值铁律（关键）**：`resolved = resolve(deepMerge(rawBase, rawSelf))`——**先在 raw 上合并，再统一求值**。错误顺序 `deepMerge(resolve(base), self)` 会让"只换 primary 全套派生色跟着变"的期望落空（回归守护见 `TestInheritResolveOrder`）。

**加载期校验（fail fast，`validateViews`）**：一刀切只读 v3 无 legacy 兜底，故加载期须校验——
- 所有 `${token}` 引用可达（token 存在）且无环（循环引用报错）；
- 所有 `image.ref` 在 `resources` 中存在（或合法字面 path/data URI）；
- `base` 链可解析、无环。
- 校验失败 → 加载报错（附路径），不进入渲染（透明/黑屏根因前移到加载期）。

## 九、继承（base 单链，冻结）

```yaml
# 基主题 windy-blue：定义颜色（colors）
meta: { name: windy-blue }
colors: { primary: "#4285F4", derive: { enabled: true } }

# 派生主题：基于 windy-blue 改主色 + 局部 views
meta: { name: my-blue }
base: windy-blue
colors: { primary: "#0066CC" }                              # 只换主色——全套派生色跟着变（先合并后求值）
views:  { item: { border: { radius: 8 } } }                 # 局部覆盖
```

- 覆盖规则：override 非空字段覆盖、数组整替、对象递归（复用 `mergeViewNode`/`mergeViews`/`mergePaletteSchema`/`mergeBehaviorRaw`）。
- 疏密档位（原 density compact/cozy/comfortable）：改由作者写**只含 views 几何的 base 主题** + `base: <id>` 实现。本仓内置仅 compact 实际使用，故不预建 cozy/comfortable base（YAGNI）。
- 暂只做单 `base`，不做 import 多源。

## 十、验收证据（V3-D 冻结时）

- `go -C wind_input test ./pkg/theme/ -run TestV3GoldenSnapshot -count=1`：绿（重落基线，**已确认重落仅影响 layout 段**——逐字节 diff `110,112d109` 仅删 3 行 layout，palette/candidate views/other windows/behavior/resources 全段不变）。
- `go -C wind_input build ./... && vet ./... && test ./... -count=1`：全绿。
- `go -C wind_setting build ./... && vet ./...`：绿。
- config 新增三字段（always_show_pager/show_page_number/vertical_max_width + 各 FollowTheme）round-trip 单测绿（`behavior_override_test.go`）。

## 十一、变更规则

- **冻结字段语义/类型变更**须迁移（升版本 + 迁移器）。
- **新增可选字段**非破坏、无需迁移（如 gradient/blur 的渲染补全、behavior 新模块）。
- 改 `pkg/theme`/`pkg/config` 对外接口/结构须同步对应 `AGENTS.md`。

## 十二、能力声明 schema（Capability Manifest）

冻结的 schema 字段渲染消费不均：有的已渲染（真能力）、有的 schema 占位但渲染未实现（假字段，如 `shadow.blur`/`spread`）、有的对某 view 概念上无意义（如 `status` 无交互状态）。**能力声明 schema** 给出权威清单消解这种歧义，作前后端统一标准。

- **真相源**：`pkg/theme/capability.go` 的 `ThemeCapabilities`（Go 权威矩阵）→ `MarshalCapabilities()` 导出 `docs/design/theme-capabilities.json`（前端编辑器消费）。
- **三态**：`supported`（已渲染）/ `reserved`（schema 有、渲染未实现）/ `unsupported`（该 view 概念不支持）。编辑器：supported→正常控件、reserved→灰显标"未生效"、unsupported→隐藏；引擎：reserved/unsupported→忽略。
- **维度粒度**：按用户可感知的能力单元（`padding`/`background_color`/`state_*`/`line_spacing`/`accent_bar`…，白名单见 `capability.go`），非每个 schema 叶子字段。
- **维护纪律**：改渲染时同步对应格子；把格子转 `supported` 必须同时落地真实渲染（不得空转声明）。`capability_test.go` 守护 well-formed + JSON 不漂移。
- 完整设计见 `theme-capability-schema.md`。
