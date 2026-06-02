<!-- Generated: 2026-05-29 | Updated: 2026-05-29 -->

# 主题渲染架构：盒模型 View 树（v2.6 方向）

本文档定义清风输入法主题系统从"固定化渲染"演进到"统一 View 盒模型"的架构。它是一个跨多阶段的工程，本文是 **P0 设计层**，约束后续各阶段（每阶段单独 spec → 计划 → 实现）。

## ⛔ v2.6 schema 冻结声明（2026-06-02）

**候选窗口 schema 已冻结**为 v2.6 契约。冻结范围、契约与变更规则如下：

- **冻结范围**：候选窗口的盒模型 schema —— `views`（window/preedit_bar/candidate_list/item/index/text/comment/accent_bar/footer_bar + metrics）、`resources`、`palette`、`behavior`，及其涉及的 `ViewNode/ViewFill/ViewImage/ViewGradient/ViewShadowSpec/ResourceRef` 等类型。**主题 `meta.version` 用 `"2.6"`**。
- **契约基准**：完整字段面见 `theme-v26-freeze-report.md` 第四节，编辑器（P3）与未来扩展以此为准。
- **未冻结（P8 增量）**：工具栏 / 弹出菜单 / Tooltip / 状态泡 / Toast 的**几何** View 化（当前仅颜色 View 化）。这些是后续增量，按下方扩展政策非破坏地补。
- **变更规则**：冻结字段的**语义/类型变更**须走迁移（升版本 + 迁移器）；**新增可选字段**属非破坏扩展，无需迁移（见下）。

### 钦定的非破坏扩展（冻结后可直接加，无需迁移）

这些是已设计好、保证不破坏 v2.6 结构的扩展路径。编辑器与解析器**必须容忍**未知的此类字段，不得写死假设：

| 扩展 | 形状 | 现状 |
|---|---|---|
| **渐变** | `ViewFill.gradient {type,angle,stops:[{color,pos}]}` | 字段已在、merge 保留，渲染待补 |
| **阴影 blur/spread** | `ViewMetrics.shadow.{blur,spread}` | 字段已在，渲染仅 offset+color |
| **方向变体** | `ViewNode.vertical:`（与 selected/hover 同构的 patch，竖排叠加在 base 上）；字段**未加**，需要时增补即非破坏 | 仅政策声明（YAGNI） |
| **整窗/整 View 透明** | 用现有 `background.color: transparent`（全透）或 `#RRGGBBAA`（半透）+ `border.{radius,width}: 0` 组合；位图自带形状 | 字段已对；引擎需补"窗口透明时以位图 alpha 作遮罩"（blendOver alpha-gate，render-later） |

> 编辑器必须把**方向（横/竖）当作一个维度**、把**透明**当作 `background.color` 的常规取值，不得假设单方向或不透明。

## 一、背景与动机

### 现状（v2.5 固定化渲染）

- `pkg/theme` 把主题拆成 `LayoutSchema`（尺寸）+ `PaletteSchema`（颜色/背景图）两个可组合零件，经 density 基线补全为 `ResolvedV25`，再由 `adapter.go` 适配成 legacy `ResolvedTheme` 喂给 `internal/ui` 渲染器。
- 渲染器（`renderer_layout.go`）对每个具名元素（候选窗 / 预编辑条 / 候选项 / 序号 / 注释 / 强调条 / 页脚 / 翻页）写了**专属绘制代码**，夹杂大量 magic number（阴影偏移 2px、边框 1px、光标 1.5px、preedit 下距 4px、ModeLabel 间距 12px、index 圆半径公式、ItemHeight=max(32,字号·1.8) 等）。

### 痛点

1. **加任何外观能力都要改渲染器**：圆角、边框、渐变、每元素背景图、层级覆盖图都得在固定代码里逐处硬塞。
2. **magic number 长尾**（见历史审计的 A/B 两类）无法被主题表达。
3. **图片只有窗口一处**；复刻复古输入法 skin（选中高亮图、候选项底图、外框水印）无从表达。
4. **圆圈 / 强调条等"形状元素"是渲染器特例**，而非可配置的通用外观。

### 目标

把"最小渲染元素"抽象为 **View（盒模型节点）**，让渲染管线由"逐元素硬编码"变为"遍历一棵 View 树、按统一规则 measure/arrange/paint"。外观能力（背景色/图、边框、圆角、层级覆盖图、内外边距、状态样式）成为每个 View 的统一属性，**扩展一次、所有元素受益**。

### 关键洞察：输入法内容是封闭集合

输入法能显示的内容是固定的（预编辑串、候选文字、序号、注释、翻页、若干条）。因此**让主题定义任意结构的视图树**（完全动态 + data-binding DSL + 通用 flex 引擎）收益有限、代价极大；而**骨架固定、每个已知元素都是统一 View**能拿到约 90% 的好处且工作量有界、双渲染器（Go 引擎 + 编辑器 Canvas）可逐一对齐。

## 二、已锁定的核心决策

| # | 决策 | 取舍 |
|---|---|---|
| D1 | **统一 View、骨架固定** | 每个已知元素都是统一 View；树形状与横/竖流式由引擎定，主题只调各节点属性。不做动态任意树/data-binding/通用 flexbox。 |
| D2 | **View 引用可换 token，palette 独立保留** | View 外观字段存颜色/图片 **token 引用**；palette 仍是可独立替换的 token 表（含 derive、light/dark）。保住"一套结构配多套配色"，dark 图片走变体 token。 |
| D3 | **具名 View（非递归 YAML 树）** | 骨架固定 → 用具名 View（window/preedit_bar/candidate_list/item/index/text/comment/accent_bar/footer_bar 各一个 View），作者更好写、保留零件组合。 |
| D4 | **整数 z 层级，内容基准 z=0** | 覆盖图 `layers[]` 带 z 整数：z<0 在内容下、z>0 在内容上，同 z 按数组序。`background` 填充永远在最底。 |
| D5 | **通用 Image 对象 + 顶层资源注册表** | 唯一 `Image` 类型（ref/mode/slice/opacity/z/anchor/offset/size），顶层 `resources` 存 base64/路径、按 ref 引用。扩展 Image 一次，处处受益。 |
| D6 | **新盒模型渲染器，以视觉对齐验收** | P1 直接写新渲染器（这才是"自由管线"的意义），以"与现输出对齐"为验收，达标后删旧固定渲染；不长期维护双渲染路径。 |
| D7 | **方向变体延后** | 横/竖先用单图适配（nine_slice 自适应）。Image 是通用对象，日后加 `vertical` 覆盖为非破坏性扩展。 |

## 三、类型定义（P0 契约）

> 以下为语义契约，非最终 Go 代码；字段名以最终实现为准，但语义需 1:1 落地于 Go 引擎与编辑器。

```text
Edges        { top, right, bottom, left : *int }   # 沿用 v2.5 "显式 0" 指针语义

ColorToken   string   # "#RRGGBB[AA]" | "${semantic}" | "transparent"

Image {                # 唯一通用图片对象（D5）
  ref      : string    # → resources[ref]；否则按字面 path / data: URI 解析
  mode     : nine_slice | stretch | tile | center   # 注：fit 未实现，v2.6 不含
  slice    : Edges     # 仅 nine_slice
  opacity  : *float64  # nil=1.0
  z        : int       # 仅 layers[] 用；内容基准=0，<0 在下、>0 在上
  anchor   : top-left | top | ... | center | ... | bottom-right   # 仅覆盖图
  offset   : {x, y : int}                                          # 仅覆盖图
  size     : {w, h : int}   # 0=原尺寸                              # 仅覆盖图
}

Gradient {             # P4 才消费，P0 仅预留
  stops : [{ color: ColorToken, at: float }]
  angle : int          # 度
}

Fill {
  color    : ColorToken   # 底色
  image    : Image        # 可选，画在底色之上（背景填充，裁剪到圆角内）
  gradient : Gradient     # 可选（P4）
}

Border {
  width    : *int
  color    : ColorToken
  gradient : Gradient     # 可选（P4），与 color 互斥
  radius   : *int
}

View {                  # 最小渲染元素（盒模型）
  margin     : Edges
  padding    : Edges
  background : Fill
  border     : Border
  layers     : []Image            # z 层级覆盖图（D4）
  states     : { selected: ViewPatch, hover: ViewPatch, disabled: ViewPatch }
}

# Text 节点 = 一个 View + 排版属性（其 background/border/padding 等照常生效）
Text : View + {
  font_family : string
  font_size   : int
  font_weight : int        # 100..900，0=继承全局
  color       : ColorToken
}

# ViewPatch = View 的部分覆盖（仅写出的字段覆盖基态），用于 selected/hover/disabled
```

顶层 theme 结构（D2/D3）：

```yaml
resources:                 # 变体无关，共享字节
  paper:  "data:image/png;base64,..."
  glow:   "data:..."

palette:                   # 独立可换的 token 表（沿用 v2.5：primary/derive/light/dark/语义色）
  primary: "#4285F4"
  light: { bg: "...", accent: "...", ... }
  dark:  { ... }

views:                     # 具名 View 集合（取代 v2.5 的 layout 零件；外观字段引用 token）
  window:         View
  preedit_bar:    View(Text)
  candidate_list: View
  item:           View          # 含 states.selected / states.hover
  index:          View(Text)    # "圆圈"= 此 View 的圆形 background/border
  text:           View(Text)
  comment:        View(Text)
  accent_bar:     View          # 强调条 = 一个细 View
  footer_bar:     View(Text)
```

## 四、固定骨架（候选窗）

引擎内置的树形状与流式方向（横/竖为运行时参数，非主题字段，沿用 D7/已删的 direction）：

```text
window (View)
├─ preedit_bar (View → Text)
├─ candidate_list (View；横排=行流式 / 竖排=列流式，item 间距由 item.margin 表达)
│   ├─ item (View，按候选重复；states: selected / hover)
│   │   ├─ index   (View+Text)   # 圆圈 = index.background 圆形 + index.border.radius
│   │   ├─ text    (Text)
│   │   └─ comment (Text)
│   └─ accent_bar (View)         # 选中项左侧强调条
└─ footer_bar (View → Text)      # 翻页/页码
```

历史包袱的消解映射：

| 旧特例 | 新表达 |
|---|---|
| index 圆圈样式 | `index.background`（圆形填充）+ `index.border.radius` |
| accent_bar 强调条 | 一个细 `accent_bar` View 的 `background` |
| 各种 magic number（间距/边框/阴影/光标） | 各 View 的 `margin/padding/border` |
| #1 背景图 / 层级覆盖图 | View 的 `background.image` / `layers[]` |
| selected/hover 高亮 | `item.states.selected/hover` 的 ViewPatch |
| dark_image 特例 | dark 变体 token 覆盖 image ref（D2） |

## 五、渲染契约（measure / arrange / paint）

1. **measure**：Text 内在尺寸由字体度量得出；View 尺寸 = 内容 + padding + border；参与外层流式时再计 margin。
2. **arrange**：window 内 band 垂直堆叠（window.padding 控制留白）；candidate_list 按方向行/列流式排 item（item.margin 控制间距）；item 内部 [index, text, comment] 行内排列；accent_bar 定位到 item 左缘。
3. **paint（每个 View，自底向上）**：
   ```
   ① background.color  → ② background.image / gradient
   → ③ layers[] 中 z<0（按 z、再数组序）
   → ④ 子节点 / 文本内容（内容基准 z=0）
   → ⑤ layers[] 中 z>0
   → ⑥ border（描边，裁剪到 radius）
   ```
   所有绘制裁剪到本 View 的圆角矩形。
4. **states**：item 处于 selected/hover/disabled 时，绘制前以对应 ViewPatch 部分覆盖基态。
5. **token 解析**：颜色 `${name}` 对 palette 变体解析（复用现有 `expandPaletteRefs` + derive）；图片 ref 对 `resources` 解析，变体可覆盖 ref 以实现 dark 图。
6. **方向**：运行时参数；list 流式方向据此选择；单图自适应（D7）。

绘制原语复用现成 `bgimage.go` 的 `DrawBackground(dst, rect, src, mode, slice, opacity)` —— 它已能往任意矩形画 nine_slice/stretch/tile/center，可直接服务任意 View 的 background/layers。

## 六、与 v2.5 的关系与迁移

- `LayoutSchema`（尺寸）→ 演进为 `views`（结构+尺寸+引用式外观）。
- `PaletteSchema` 基本**保留**为 token 表（D2），新增 `resources` 顶层。
- `adapter.go`（ResolvedV25→legacy ResolvedTheme）在新渲染器落地后**退役**：渲染器直接消费解析后的 View 树（D6）。
- 现有 `bgimage.go` 原语保留复用。
- 种子主题（default/compact-horizontal、msime/msime-tight 等）在 P2 迁移为 views 形态，以视觉对齐为验收。

## 七、分阶段

| 阶段 | 范围 | 验收 |
|---|---|---|
| **P0 设计** | 本文档：类型/骨架/契约/迁移/分期 | 用户 review 通过 |
| **P1 引擎核心（Go）** | 盒模型 measure/arrange/paint 引擎；候选窗全元素走 View 树渲染；token/Image/layers/states 落地；新渲染器替换 `renderer_layout.go` 候选窗路径 | 与现候选窗输出**视觉对齐**（横/竖、选中/悬停、preedit/页脚、index 圆圈/数字、强调条、背景图） |
| **P2 schema + 迁移** | `views` schema、density 基线→View 默认、resolver/merge、种子主题迁移、测试；`adapter.go` 退役 | 种子主题解析+渲染与 P1 一致；测试绿 |
| **P3 编辑器** | 编辑器 Canvas 镜像 View 引擎；UI 改为按 View 的属性面板（盒模型/背景/边框/层级/状态） | 编辑器预览与引擎 1:1；导入导出 views |
| **P4 丰富化** | 渐变、各元素 layers/images、其它窗（工具栏/菜单/Tooltip/状态条）View 化、方向变体 | 增量交付 |

## 八、非目标（明确不做 / 延后）

- ❌ 主题自定义任意结构的视图树、data-binding DSL、通用 flexbox（D1）。
- ❌ 跨 surface 的全局 z 排序（z 仅在单个 surface 内、相对其内容）。
- ⏸ 渐变（`ViewFill.Gradient` 字段形状已于 P7-E 冻结：`{type,angle,stops:[{color,pos}]}`，渲染 later）。
- ⏸ 阴影 blur/spread（`ViewMetrics.Shadow` 结构已于 P7-E 冻结：`{offset_x,offset_y,blur,spread,color}`，仅 offset+color 已实现，blur/spread later）。
- ⏸ 图片方向变体（**D7 冻结声明**：横/竖排共用一套 views；日后为 `Views` 或 `ViewImage` 增加 `vertical` 覆盖块属**非破坏性扩展**——v2.6 不含该字段，编辑器与解析器不得写死"仅横排/仅竖排"假设，遇未知 `vertical` 键应容忍）。
- ✅ 暗色位图（P7-E 已实现）：`resources` 值支持 `{light,dark}`，按 palette 变体（isDark）选路径；与 palette light/dark 对称。
- ⏸ 工具栏/弹出菜单/Tooltip/状态条/Toast 的 View 化（P4）。

## 九、风险与待解决

1. **双渲染器一致性**：Go（gg/自绘）与编辑器（Canvas 2D）的文本度量、nine_slice 缩放、圆角抗锯齿存在天然差异，需以"视觉对齐"而非"像素一致"为标准，并在 P1/P3 用 chrome 插件逐场景比对。
2. **文本度量**：measure 依赖字体度量；Go 侧（DirectWrite/GDI）与 Canvas 的换行/省略号策略需对齐到可接受范围。
3. **性能**：候选窗小、每帧重绘，measure/arrange 开销可忽略；但 layers/nine_slice 缩放需保留 P1 的缓存策略（按 src 指针+尺寸失效）。
4. **迁移**：种子主题与既有用户主题（v2.5）需要兼容或一次性迁移策略——P2 决策（兼容读旧 schema vs 提供迁移器）。
5. **states 覆盖语义**：ViewPatch 的部分覆盖与 token 引用叠加顺序需在 P1 明确（先并 patch 再解析 token）。

## 十、关联文档

- `schema-layers.md` — 方案配置三层叠加（与主题无关，但同属"叠加/合并"心智）
- `../../wind_input/pkg/theme/AGENTS.md` — 主题包对外接口（P2 需同步）
- `enum-constraint.md` — 全局枚举约束（mode/anchor 等新增枚举需登记）
