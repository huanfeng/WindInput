# 主题 Schema v3：结构归一与亮暗统一

> 状态：**已实现并冻结（2026-06-04）**。冻结契约（字段面 + 求值语义快照）见 `theme-v3-freeze-report.md`；本文为权威设计语义。V3-D 收尾完成：删 layout/density 死代码、config.UI 补全 behavior 三字段用户覆盖、重新冻结。本文已纳入架构评审的全部修正，是无内部矛盾的可执行 spec。
> 关键决策已全部敲定（见文末「已定决策」）：一刀切只读 v3（v2.5/v2.6 未正式发布，无兼容包袱）、删 density、删 metrics 杂物抽屉、`LightDark<T>` 亮暗原语、先合并后求值、颜色递归求值、states 消费层递归、behavior 哲学Y（主题推荐默认 + 用户覆盖层）、加载期校验。

## 背景与动机

v2.6 盒模型 View 引擎落地后，主题文件暴露出三处结构不一致（已在编辑器原型中验证并收敛）：

1. **几何被切成多套**：`views`（v2.6 候选窗盒模型）、`layout`（v2.5 遗留几何）、`density`（疏密基线档位）三套并行描述几何，分属新旧不同机制。
2. **颜色被切成两半且有冗余**：`palette` 既有顶层语义色，又有 `candidate_window` 等每窗口颜色组；而 `views` 节点又带 `${sem}` 颜色 token。实测默认主题里 `candidate_window` 12 个语义色中 **10 个是纯 `${顶层}` 转发**，只有 `selected_bg`/`hover_bg` 是独立值——中间层 ~83% 冗余。
3. **亮暗有两套机制**：颜色用顶层 `light`/`dark` 分块（选整块）；图片用 `{light: a.png, dark: b.png}` 逐键（选逐值）。颜色落后于图片。

v3 的目标：**几何归一（只留 views，覆盖所有窗口，吸收 layout 与 density）、颜色归一（扁平语义 token + 节点引用）、亮暗归一（单一 `LightDark<T>` 原语逐值贯穿颜色与图片）、命名按角色**。净效果是**删机制**——三套几何来源合一、二层颜色合一，代码净减。

> **本设计文档是唯一权威 spec**。编辑器（独立仓 `WindInputThemeEditor`）已实现一版 v3 数据层，**由用户单独维护并以本文档为准对齐**；后端**不依赖编辑器、不做 TS↔Go 对拍**。文末「参考实现索引」仅供参考。
> **回归基准（后端自比对）**：后端改造前，对 windyBlue/msime 两主题用现 v2.6 管线产出"解析后渲染消费形态"（`ResolvedViews` 的颜色/几何值）存为**黄金快照**；v3 各切片后用对应 v3 主题经新管线产出同结构、逐项比对（**v3 产物 == v2.6 产物**）。引擎正确性优先，观感靠主题拉回。

---

## 顶层结构

```yaml
theme:
  meta:      { name, version?, author? }
  base?:     <themeId>        # 单链继承（替代旧 palette/layout 外链）
  colors:    <ColorTokens>    # 提供者①：颜色语义 token
  resources: <ResourceMap>    # 提供者②：图片资源
  views:     <ViewsTree>      # 结构：几何 + 引用，所有窗口（吸收 layout + density）
  behavior:  <Behavior>       # 用户可覆盖的主题推荐默认（按模块）
```

四个顶层块**职责正交、按角色命名**；`layout`、`palette` 每窗口颜色组、`density` 三者**取消**。

> **块的受众二分**：`colors`/`resources`/`views` 是**设计师的画布**（主题作者的完整设计，换主题整套换）；`behavior` 是**用户的旋钮**（主题给推荐默认，用户可独立覆盖、覆盖跨主题保持）。这是 behavior 与 views 分块的根本理由——不是"行为 vs 外观"，而是**覆盖来源/生命周期不同**。

---

## 原语类型

```
Dimension    = number | "Npx" | "Ndp"        # dp 默认（随 DPI 缩放）；px 不缩放（发丝线）
LightDark<T> = T | { light: T; dark: T }      # 亮暗原语：单值=共用；对象=分设
Color        = LightDark<ColorScalar>
ColorScalar  = "#RRGGBB[AA]" | "${tokenName}" | "transparent"
ImageRef     = LightDark<string>              # resources 键 / 路径 / data:URI
```

- **`LightDark<T>` 是亮暗统一的核心**：颜色与图片用**同一套规则**。单值即亮暗共用，只有差异项才写 `{light,dark}`。`resource_ref.go` 的 `ResourceRef`（标量或 `{light,dark}` + `PathFor(isDark)`）**已是此原语的现成实现**，颜色 Variant 化照抄其 Unmarshal/选取即可。
- **命名诚实**：本原语**只表达亮暗二值轴**，故命名 `LightDark` 而非泛化的 `Variant`。将来若出现 @2x / 高对比度 / 平台等其它变体轴，再抽象一层通用 `Conditional<T>` 处理，**现在不预先泛化（YAGNI）**。
- **作用边界**：`LightDark` **仅作用于表现层值**（color、gradient stop、image ref）；**几何与结构不随亮暗变化**（Dimension 不是 LightDark）。亮暗只改"颜色/图片"，不改"尺寸/布局"。
- Go 侧：`LightDark[T]` 用一个能 Unmarshal「标量或 {light,dark}」的泛型/包装类型（或 `light`/`dark` 双字段 + 单值 fallback），与 `ResourceRef` 同构。

---

## colors（颜色 token 提供者）

```yaml
colors:                       # 扁平、可互引 ${}、值为 LightDark<color> —— 语义层（唯一颜色真源）
  primary:   "#4285F4"
  accent:    "${primary}"
  on_accent: "#FFFFFF"
  bg:        { light: "#FFFFFF", dark: "#2D2D2D" }
  surface: ...; border: ...; text: ...; text_dim: ...; text_hint: ...
  selection:      { light: "#E6F0FF", dark: "#3D4A5C" }   # 原 selected_bg 提升为语义
  selection_text: "${text}"
  hover:          "${selection}"
  shadow:         "#0000001A"

  derive:    { enabled?: bool, algorithm?: "hsl-shift" | "hct" | "none" }   # 维度①：由 primary 派生语义色
  auto_dark: bool                                                            # 维度②：未显式给 dark 的值由 light 派生
```

- **核心约定**：颜色值**只能活在语义层**（它有 light/dark 两份）。原 `candidate_window` 每窗口颜色组取消——其中纯 `${顶层}` 转发的并入引用，唯一的组件特有色（selection/hover）**提升为顶层语义**。
- token 命名用**抽象语义**（primary/accent/bg/selection…），不放 `index_bg` 这类功能名（功能归 views 引用）。
- **两个正交的 derive**（不冲突，可叠加）：
  - `derive`（**语义维度**）：用户只给 `primary`，自动派生 bg/text/border… 全套语义色（现状 `deriveHSLShift`）。
  - `auto_dark`（**变体维度**）：逐值 LightDark 下，未显式写 `dark` 分支的颜色，由其 `light` 自动派生 dark。
  - 两者均作用于 **colors token 表**（而非使用点），见「解析管线」第 3 步。

---

### 颜色归一落地细则（后端实施，2026-06-04 据实测数据补）

实测：`candidate_window` 12 字段 ~10 个 `${顶层}` 纯转发、仅 `selected_bg`/`hover_bg` 特有；但 tooltip/status/toast/menu 有**带 light/dark 两份的特有色**（如 `tooltip.bg = {light:#3C3C3CF0, dark:#1E1E1EF0}`），无法语义化、也不能内联进单份 views 节点（views 不分变体）→ **特有色必须活在 colors 层**（只有 colors 有 LightDark）。故定：

- **colors = 扁平 token**：抽象语义优先（`primary/bg/surface/border/text/text_dim/text_hint/accent/on_accent/shadow/selection/selection_text/hover`）；无抽象语义对应的特有色用**功能性前缀 token**（对「token 须抽象语义」的务实放宽，仍扁平、仍 LightDark）：
  - menu：`menu_bg` / `menu_border` / `menu_text` / `menu_hover_bg` / `menu_hover_text` / `menu_disabled` / `menu_separator`（**实测** `popup_menu.border #C7C7C7` ≠ 顶层 `border #C8C8C8`，故 menu 用独立 token 而非转发顶层——保真优先）
  - tooltip：`tooltip_bg` / `tooltip_text`
  - status：`status_bg` / `status_text`（border 转发 `${accent}`）
  - toast：`toast_bg` / `toast_text`
- **删全部 6 组嵌套窗口组**：candidate_window / popup_menu / tooltip / status / toast / **toolbar** 取消，颜色**全部扁平进 colors token**（colors 形态纯扁平、无混合嵌套）。toolbar 的 13 色迁为 `toolbar_*` 前缀 token（如 `toolbar_grip`/`toolbar_mode_cn_bg`/`toolbar_settings_icon`…）；toolbar 的**几何/L1+L2 重构仍延后**，本次仅迁颜色来源。
- **ResolvedPalette 新形态**：保留顶层语义便捷字段（`Bg/Text/Accent…`）+ 新增 `Tokens map[string]color.Color`（全部解析后 token）；删 5 个 `Resolved*Palette` 组（candidate/menu/tooltip/status/toast），**保留 `ResolvedToolbarPalette`**（从 `Tokens["toolbar_*"]` 填充，使 `viewbox_toolbar` 消费最小改动）。token resolver（`candidate_views`/`other_views`）统一查 `Tokens`。
- **节点 token 引用**：候选窗转发色改引顶层语义（`${bg}/${accent}/${selection}/${hover}/${selection_text}…`）；其它窗口特有色引功能 token（`${tooltip_bg}`/`${status_bg}`/`${menu_hover_bg}`…）。
- **回归判据**：本切片**只改颜色来源、不改颜色值**——`TestV3GoldenSnapshot` 必须逐字节不变（候选窗 + status/tooltip/toast/menu 均在快照内；toolbar 不在快照，其颜色不变由编译 + `viewbox_toolbar_test` 守护）。

## resources（图片提供者，与 colors 同构）

```yaml
resources:
  panel: { light: "panel.png", dark: "panel-dark.png" }   # LightDark<ref>
  badge: "badge.png"                                       # 单值=共用
```

后端 `resource_ref.go` 已就绪，几乎不用改。

---

## views（唯一结构树，吸收 layout + density）

几何归一：`views` 覆盖所有窗口（候选窗 + toolbar/menu/tooltip/status/toast，**替代 layout**）。疏密档位（原 density 的 compact/cozy/comfortable）改由 **base 继承**实现——见「继承」。

### 核心原则：属性归位（取消 metrics 杂物抽屉）

**一个视觉元素的所有属性（颜色 + 几何 + 状态）都集中在它自己的节点里**，不设独立的几何口袋。原 `metrics` 块（item_spacing / band_gap / accent_bar 几何 / shadow）**整体取消**，各项归位：

| 原 metrics 项 | 归位到 | 理由 |
|---|---|---|
| `shadow`（窗口投影） | `window` 节点 | 投影长在窗口元素上（CSS `box-shadow`） |
| `item_spacing`（候选项间距） | `candidate_list` 容器节点的 `gap` | 项间距是容器属性（CSS `gap`） |
| `band_gap`（band 间距） | `candidate_list` 容器节点的 `band_gap` | 同上，容器排布属性 |
| `accent_bar` 几何（width/offset/enabled/height_ratio） | `accent_bar` 节点 | accent_bar 本就是具名节点，颜色已在其上；几何并入，消除"颜色在节点、几何在 metrics"的割裂 |

### 节点 ViewNode

```
ViewNode = {
  # 几何
  margin?:  Edges<Dimension>
  padding?: Edges<Dimension>
  border?:  { width?: Dimension; radius?: Dimension; color?: Color }
  # 颜色（命名对齐 CSS）
  background?: Fill          # 背景填充
  color?: Color             # 文本/前景色
  # 文本
  font?: { size?: number; weight?: number; family?: string }   # size=相对模块基准字号的有符号偏移
  # 仅特定节点
  align?: "left" | "center" | "right"   # 仅 footer_bar
  labels?: string[]                     # 仅 index 序号槽位
  layers?: ImageFill[]                  # z 层级装饰图
  shadow?: { offset_x?, offset_y?, blur?, spread?, color?: Color }   # 仅 window（投影归位）
  gap?: Dimension                       # 仅容器节点（candidate_list）：子项间距
  band_gap?: Dimension                  # 仅 candidate_list：band 间距
  enabled?: bool                        # 仅 accent_bar：是否绘制
  width?: Dimension                     # 仅 accent_bar：条宽
  offset?: Dimension                    # 仅 accent_bar：左缘偏移
  height_ratio?: number                 # 仅 accent_bar：条高 = ItemHeight × 此比例
  # 状态 patch（结构同 ViewNode，递归）
  states?: { selected?: ViewNode; hover?: ViewNode; disabled?: ViewNode }
}
```

**颜色字段命名（三类，对齐 CSS）：**

| 用途 | 字段 | 类型 |
|------|------|------|
| 背景色 | `background.color` | `Color`（`${token}` / `{light,dark}` / hex） |
| 文字/前景色 | `color` | `Color` |
| 边框色 | `border.color` | `Color` |

节点颜色**默认用 `${token}` 引用**（复用语义、亮暗正确）；**亦可内联** hex 或 `{light,dark}`（一次性值，已定允许）。

**状态 patch 是完整 ViewNode（递归）**：`states.selected/hover/disabled` 结构同 ViewNode，可携带自己的 background（含 gradient/image）、border、layers、font。**消费层（Go 的解析产物）对应也用递归的 `RVNode`，不再用扁平的 `RVState`**——确保未来 hover 要渐变/图层时零改结构。各字段零值/nil = 该属性不覆盖、沿用基态。

### Fill（背景填充：底色 → 渐变 → 位图 自下而上叠加）

```
Fill = {
  color?:    Color
  gradient?: { type?: "linear"|"radial"; angle?: number; stops: { color: Color; pos?: number }[] }
  image?:    { ref: string; mode?: "nine_slice"|"stretch"|"tile"|"center"; slice?: Edges<Dimension>; opacity?: number }
  shape?:    "circle" | "none"          # 裁形（序号圆）
}
```

- 渐变 `stop.color` 也是 `Color` → 渐变亮暗正确。
- **状态 patch 对 `background` 的合并是逐字段**（patch 只写 `image` 则保留基态 `color`），与 `mergeViewNode` 现有逐字段规则一致。
- 绘制栈（下→上）：`shadow → background.color → background.gradient → background.image → border → content → layers(z>0)`。

### 候选窗节点树（属性归位后）

```yaml
views:
  candidate:
    window:         { padding, border:{width,radius,color:"${border}"}, background:{color:"${bg}"}, shadow:{offset_x,offset_y,color:"${shadow}"} }
    candidate_list: { gap, band_gap }                              # 容器：子项/band 间距（原 metrics.item_spacing/band_gap）
    preedit_bar:    { padding, border:{radius}, background:{color:"${surface}"}, color:"${text_dim}" }
    item:           { padding, border:{radius}, states:{ selected:{background:{color:"${selection}"},color:"${selection_text}"}, hover:{background:{color:"${hover}"}} } }
    index:          { background:{color:"${accent}", shape:circle}, color:"${on_accent}", font:{size:-4} }
    text:           { margin:{left}, color:"${text}" }
    comment:        { margin:{left}, color:"${text_hint}", font:{size:-4} }
    accent_bar:     { enabled, width, offset, height_ratio, background:{color:"${accent}"} }   # 几何+颜色合一
    footer_bar:     { font:{size:-4}, align }
    mode_label:     { font:{size:-4}, padding, color:"${text_hint}" }
  toolbar / menu / tooltip / status / toast: { ... }   # 同骨架，几何全归此处（替代 layout）
```

**渲染器对各节点真正消费的面（编辑器 Inspector 已按此声明，后端实现须一致）：**

| 节点 | 生效几何 | 备注 |
|------|---------|------|
| window | padding / border 宽·色·圆角 / **shadow** | 窗口盒；shadow 归位至此 |
| candidate_list | **gap / band_gap** | 容器排布；间距归位至此 |
| preedit_bar | padding(上下→条高、左右→带内边距) / 圆角 | border 宽/色当前未渲染（可补能力，延后） |
| item | padding(上下→行高、左右→候选内边距) / 圆角 | 基底底色/边框**仅经 states 渲染**；item 基底 bg/border/字重当前不渲染（可补能力，延后） |
| index | padding(上下仅参与圆形序号直径·居中不位移；左右→文本序号列宽) | |
| text / comment | **仅 margin.left** | 其余 padding/margin 不生效 |
| accent_bar | **enabled / width / offset / height_ratio**（几何归位）+ 颜色 | |
| footer_bar | padding（配合 align 定位页码）| **align 当前引擎未实现**（可补能力，延后） |
| mode_label | padding（左右=徽标内边距）| 真机已渲染 |

> 标注"未渲染/未实现"的项（item 基底色/border、preedit border 宽色、footer align）是**可补能力**。**已定：v3 只对齐现状渲染行为，不在结构改造里夹带能力补全**——保持回归判据纯粹（v3 产物 == v2.6 产物）。能力补全作为独立后续。

---

## behavior（哲学Y：主题推荐默认 + 用户覆盖层）

```yaml
behavior:
  candidate:
    font_size:          18      # 主字号基准（节点 font.size 是相对它的有符号偏移）
    vertical_max_width: 600     # 竖排最大宽度
    always_show_pager:  false
    show_page_number:   true
  # toolbar: {...}   # 留位
```

**定位（这是 behavior 的"用意"）**：behavior = **"需要独立用户覆盖层、且覆盖跨主题保持"的设定集合**。判据不是"是不是行为"，而是"**用户是否需要独立覆盖它**"——故 `vertical_max_width` 即便是几何也留在 behavior（用户需要按屏幕覆盖它），而不进 views。

**双层覆盖模型（哲学Y，已定）：**

```
最终值 = followTheme ? 主题 behavior 推荐默认 : 用户 config.UI 自定义值
```

- **主题层**：`theme.behavior.candidate.*` 是**主题作者的推荐默认**（紧凑主题可建议小字号/不显页码）。
- **用户层**：`config.UI` 为**每个** behavior 字段提供用户覆盖 + `*FollowTheme` 开关（沿用 `font_size` 已实现的 `FontSizeFollowTheme` 模式）。
- **现状缺口（v3 须补全）**：当前只有 `font_size` 兑现了用户覆盖（`config.UI.FontSize` + `FontSizeFollowTheme`，`config.go:182-188`）；`always_show_pager`/`show_page_number`/`vertical_max_width` **缺 `config.UI` 覆盖通道**，v3 须为它们补上 config 字段 + FollowTheme 开关，让"behavior = 可被用户覆盖白名单"的承诺真正落地（原 `behavior.go:3-4` 注释里"阶段3 用户 override 层"的 TODO）。

> 与 views/colors 一致按模块组织（`candidate.*`）。`candidate.font_size` 是候选窗主字号基准。

---

## 继承（base 单链）

```yaml
# 基主题 windy-blue
meta: { name: windy-blue }
colors: { primary: "#4285F4", derive: { enabled: true } }
views:  { candidate: { item: { border: { radius: 4 } } } }

# 派生主题：基于 windy-blue 改两处
meta: { name: my-blue }
base: windy-blue
colors: { primary: "#0066CC" }                              # 只换主色——期望全套派生色跟着变
views:  { candidate: { item: { border: { radius: 8 } } } }  # 局部覆盖
```

- **🔴 求值顺序（关键修正）**：`resolved = resolve(deepMerge(rawBase链, rawSelf))` —— **先在原始未求值 schema 上合并，再统一求值（derive + token 展开 + 变体选取）**。
  - **错误顺序** `deepMerge(resolve(base), self)` 会导致上例 bug：`resolve(windy-blue)` 已把语义色基于旧主色 `#4285F4` 派生固化，self 只覆盖 `primary` 字段，派生色仍是旧主色的——"只换主色全套跟着变"的期望落空。
  - 正确做法：merge 作用于 raw → 合并后 `primary=#0066CC` 进入 raw colors → derive 基于新主色重跑。**继承必须发生在求值之前**。
- 覆盖规则：override 非空字段覆盖、数组整替、对象递归（复用现有 `mergeViewNode`/`mergeViews`/`mergeEdges`）。支持 base 链（自底向上）。
- **吸收 density**：原 `compact`/`cozy`/`comfortable` 三档疏密 → 改为三个**只含 views 几何的 base 主题**，作者写 `base: cozy` 即可。`density.go` 整套机制（`densityBaseline`/`mergeWithDensityBaseline`）删除，基线值搬进三个 base 主题文件。
- 替代旧 `palette/layout: string|object` 外链 + `Overrides`——"复用别人配色" = base 一个只定义 colors 的基主题再加 views。
- 暂只做单 `base`，不做 import 多源。

---

## 解析管线（修正后）

```
1. 继承（先合并，作用于 raw）：merged = deepMerge(resolve_raw(base链), rawSelf)
                               —— 纯 raw schema 合并，不求值、不 derive、不展开
2. derive（作用于 merged.colors 表）：
     a. derive.enabled → 由 primary 派生缺失的语义色字段
     b. auto_dark      → 为未显式给 dark 的颜色逐值补 dark 分支
3. 颜色递归求值（在 isDark 环境下，对每个使用点的 Color 求不动点）：
     resolveColor(v, isDark):
       若 v 是 LightDark  → 取当前 isDark 分支，对结果继续 resolveColor
       若 v 是 "${token}" → 查 colors 表替换，对结果继续 resolveColor（带循环保护）
       若 v 是 hex/transparent → 返回标量（终止）
     —— isDark 是贯穿求值的环境参数，非"最后一步"；LightDark 与 ${token} 可任意交错嵌套
4. 图片：node 的 ref → 查 resources → PathFor(isDark) 选当前变体
5. 几何：Dimension × DPI scale
6. behavior：最终值 = followTheme ? 主题 behavior : config.UI 用户值
```

> **view 颜色字段 = `ColorRef`（=`LightDark[string]`）**：上述求值的「v 是 LightDark」分支落到字段类型上——
> 所有 view 颜色字段（`ViewFill/ViewNode/ViewBorder.Color`、`tint`/`disabled_tint`、`shadow.color`、gradient stop）
> 既可写**标量** token/hex（明暗共用），也可写**内联 `{light,dark}`**（各分支仍可 `${token}`）。亮暗值**集中定义**
> 仍推荐 `colors:` token；内联是一次性颜色的便捷写法。后端实现见 `pkg/theme` 的 `ColorRef` 别名 + 各 resolve 闭包 `Select(isDark)`。

**加载期校验（lint，已定须入文档与实现）**：一刀切只读 v3 后无 legacy 兜底，一个 `${typo}` 会静默变 transparent → 透明/黑屏难排查。故加载期须校验并 **fail fast**：

- 所有 `${token}` 引用**可达**（token 存在）且**无环**（循环引用报错，非静默）。
- 所有 `image.ref` 在 `resources` 中**存在**（或为合法字面 path/data URI）。
- `base` 链可解析、无环。
- 校验失败 → 加载报错（附 token/节点路径），不进入渲染。编辑器已有 lint 可参考对齐。

> **本节是权威求值语义**，后端实现以本节为准；回归用顶部「回归基准」的 v2.6 黄金快照自比对。编辑器由用户单独对齐本节（先合并后求值、颜色递归求值）。

---

## v2.6 → v3 迁移映射（后端 pkg/theme 改造要点）

> **已定：一刀切只读 v3**（v2.5/v2.6 未发布）。后端**只保留 v3 读取层**，删除全部 v2.6 读取路径；内置主题全重写为 v3；用户旧主题失效（迁移工具作为可选后续，非本次硬约束）。

| v2.6（删除） | v3 | 说明 |
|------|----|----|
| `layout` 块（其它窗口几何）+ `density` 机制 | 并入 `views`（盒模型树，所有窗口）；疏密档位转 base 主题 | 删 `layout.go` 几何块 + `density.go` |
| `palette.{light,dark}` 顶层分块 | `colors`（扁平 token，值 `LightDark`） | 顶层语义保留，改逐值 LightDark 表示 |
| `palette.*.candidate_window` 等 6 个窗口颜色组 | **取消**；纯转发并入引用，特有色（selected_bg/hover_bg）提升为顶层 `selection`/`hover` | 消除 ~83% 冗余；删 6 个 `Resolved*Palette` |
| `views` 节点 `${candidate_window 语义}` token | 节点 `${顶层语义}` token | `resolveCandidateViewColor` + `other_views.go` 改解析顶层语义 token |
| 颜色顶层 light/dark 分块（选整块） | 逐值 `LightDark<T>`（选逐值） | 颜色与图片同构；`finalizePalette`「选块」→「逐值选变体」 |
| `metrics` 块（item_spacing/band_gap/accent_bar 几何/shadow） | 取消；各项归位到 window/candidate_list/accent_bar 节点 | 见「属性归位」 |
| `RVState`（扁平状态 patch） | 递归 `RVNode` | states 消费层统一 |
| `behavior` 扁平 | `behavior.candidate.*` + `config.UI` 补全各字段用户覆盖 | 哲学Y 双层 |
| `palette/layout: string` 外链 + `Overrides` | `base` 单链继承 | |

**后端工作量集中在**：`pkg/theme` 的 schema 结构体 + Unmarshal（`LightDark`、`Fill`、base 合并）、求值管线（先合并后求值、颜色递归求值、derive 双维、加载期校验）、`resolveCandidateViewColor` + `other_views.go`（token 命名空间换顶层语义）、删 `layout.go`/`density.go`、`config.UI` 补 behavior 用户覆盖、defaultViews/各 theme.yaml 重写、渲染器对节点面消费保持一致（见消费面表）。

---

## 切片计划（基于全部已定决策）

每切片独立 commit、可独立验收。**回归判据（贯穿全程）**：每切片后，v3 主题渲染产物**逐项等于**对应 v2.6 主题（用 windyBlue/msime 两预设做黄金基准）；引擎正确性优先，观感靠主题拉回。

| 阶段 | 内容 | 验收 |
|---|---|---|
| **V3-0** | `LightDark[T]` Go 原语（照抄 `resource_ref.go`）+ 解冻声明 + v2.6 黄金快照基准脚手架 | 原语单测；windyBlue/msime 的 v2.6 解析产物快照已落盘 |
| **V3-1** | 颜色亮暗 LightDark 化（palette「选块」→逐值）+ derive 双维 + 先合并后求值 + 颜色递归求值 | v3 产物 == v2.6 产物（两预设黄金对比） |
| **V3-2** | 颜色归一（删 6 窗口色组）+ token 改顶层语义（`resolveCandidateViewColor` + `other_views.go`） | 候选窗 + 5 窗口观感不变 |
| **V3-3** | `base` 单链继承替代外链 + `Overrides`；共享零件 + density 三档转 base 主题；删 `density.go` | jidian-classic 改 base 后正常；疏密档位可用 |
| **V3-4** | 删 layout 块/`ResolvedLayout`（P8 已铺路）；metrics 归位；states→RVNode；加载期校验；`config.UI` 补 behavior 覆盖；内置主题全重写 v3；重新冻结 | 全量 build+vet+test 绿；`dev.ps1` 全窗口实测 |

---

## 已定决策

1. **向后兼容 = 一刀切只读 v3**（v2.5/v2.6 未发布，无包袱）。删全部 v2.6 读取路径，内置主题重写，旧主题失效。
2. **亮暗机制 = 逐值 `LightDark<T>`**：颜色与图片 / `resources` 完全同构，不再保留 palette 顶层 light/dark 分块。命名 `LightDark`（仅亮暗轴，将来需其它轴再抽象）。
3. **`LightDark` 仅作用于表现值**（color/gradient/image），几何与结构不随亮暗变化。
4. **derive 双维正交**：`derive`（primary→语义）+ `auto_dark`（light→dark），均作用于 colors 表、在使用点求值之前。
5. **求值顺序 = 先合并后求值**：`resolve(deepMerge(rawBase, rawSelf))`，继承作用于 raw schema。
6. **颜色解析 = 变体环境下的递归不动点求值**（isDark 贯穿，LightDark 与 ${token} 任意交错）。
7. **删 metrics 杂物抽屉**：属性归位——shadow→window，item_spacing/band_gap→candidate_list，accent_bar 几何→accent_bar 节点。
8. **删 density**：疏密档位转三个 base 几何主题。
9. **states 消费层用递归 `RVNode`**（取代扁平 `RVState`）。
10. **节点颜色允许内联** hex/`{light,dark}`（以 `${token}` 引用为主、内联兜底）。
11. **加载期校验 fail fast**：${token} 可达且无环、image.ref 存在、base 链无环；失败报错不渲染。
12. **behavior = 哲学Y**：主题推荐默认 + `config.UI` 用户覆盖层（补全 pager/page_number/vertical_max_width 的 FollowTheme 覆盖）。判据 = "是否需独立用户覆盖、跨主题保持"。
13. **可补能力延后**：v3 只对齐现状渲染（item 基底色/border、preedit border、footer align、gradient 渲染均 schema 占位、渲染later），不在结构改造里夹带。
14. **冻结治理**：`archive/theme-v26-freeze-report.md` 顶部标注"v3 起取代"；v3 全部切片完成后产出 `theme-v3-freeze-report.md` 重新冻结。

---

## 参考实现索引（编辑器仓 `WindInputThemeEditor`，仅供参考）

> 编辑器由**用户单独维护**，以本文档为权威对齐；后端不依赖以下文件。仅在需要直觉参照时查阅。

- 类型：`src/lib/theme3/types.ts`
- 合并/继承：`src/lib/theme3/merge.ts`
- 解析：`src/lib/theme3/resolve.ts`（`resolveColor` 递归不动点 + 变体选取 + 多跳 token + 循环保护）
- 预设：`src/lib/theme3/presets/{windyBlue,msime}.ts`
- 可编辑 store：`src/stores/theme3.ts`
- Inspector：`src/components/form/ViewsEditorV3.vue` + `NodeColorField/VariantColorField/...`
- 渲染接入：`src/lib/preview/candidateBox.ts`（`renderCandidateBoxRV`）
