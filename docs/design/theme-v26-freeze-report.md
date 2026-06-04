<!-- 生成: 2026-06-02 | 用途: v2.6 主题 schema 冻结评估 + 契约 -->

# WindInput v2.6 主题系统 — 评估报告 + 冻结契约

> ⚠️ **已解冻 → v3 取代（2026-06-04）**：v2.5/v2.6 均未正式发布，主题 schema 转入破坏性重构 **v3**（结构归一 + 亮暗统一），权威 spec 见 `theme-schema-v3.md`。本报告作为 **v2.6 历史快照 + v3 回归基准的语义参照**保留；v3 全部切片完成后产出 `theme-v3-freeze-report.md` 重新冻结。下方 v2.6 契约内容仅供 v3 迁移对照，不再是当前约束。
>
> 状态（历史）：**候选窗 schema 曾冻结（2026-06-02）**。冻结声明见 `theme-view-architecture.md`「⛔ v2.6 schema 冻结声明」；本文第四节为冻结契约字段面，第八节附扩展政策。
> 关联：`theme-view-architecture.md`（P0 设计 + 冻结声明）、`theme-view-p7-schema-completion.md`（P7 蓝图）、`pkg/theme/AGENTS.md`（实现现状）。

## 一、目标

| 目标 | 说明 |
|---|---|
| **声明式外观** | 主题用 YAML 描述"长什么样"，引擎只负责 measure/arrange/paint；加外观能力不再改渲染器硬编码。 |
| **盒模型统一** | 候选窗每个元素（窗口/预编辑/候选项/序号/文字/注释/强调条）都是统一的 `View`（margin/padding/border/background/layers/children），取代旧的逐元素 magic number。 |
| **三权分立** | `layout`（尺寸）/ `palette`（颜色）/ `views`（盒模型外观）解耦，"一套结构配多套配色"，支持明暗双变体。 |
| **复刻位图皮肤** | 能还原搜狗/极点那类位图皮肤：九宫格背景、装饰层、选中高亮位图、暗色变体。 |
| **冻结 + 开编辑器** | schema 定稳后冻结为契约，据此开 P3 Web 可视化编辑器（双渲染器 1:1）。 |

## 二、架构总览

```
theme.yaml
 ├─ meta {name,version,author,order}
 ├─ layout:  "compact-horizontal" | {内联}        ← 其它窗口尺寸(density 基线)
 ├─ palette: "windy-blue" | {内联}                ← 颜色 token 表(light/dark 双变体 + derive 派生)
 ├─ resources: { name → "path" | {light,dark} }   ← 图片注册表(P7-C/E)
 ├─ views:   { window/item/index/... }            ← 盒模型外观(核心)
 └─ behavior: {font_size, show_page_number…}       ← 行为(可被用户覆盖)
        │
        ▼ ResolveV25(isDark)
   ResolvedV25 { Palette(color.Color), Views, Resources(按 isDark 选路径), Behavior }
        │
        ▼ ResolveCandidateViews + 运行时字号回填   (每帧, 廉价)
   ResolvedViews(RVNode: plain 逻辑像素 + color.Color + 图片 spec)
        │
        ▼ buildHorizontal/VerticalCandidateTree    (盒模型 View 树)
        ▼ PaintTree(measure→arrange→paint)          ← 唯一渲染路径
```

**关键设计**

- **Raw / Resolved 两形态**：YAML 层距离/边框用 `*int`（nil=未写、含 0=显式值）；Resolved 层是 plain 值供渲染器直接读。
- **位图解码不在每帧**：`imageForRef` 按 ref 一次性解码缓存（换主题失效）；每帧只传廉价的 `RVImage` spec。
- **明暗对称**：palette 有 light/dark，resources 也有 `{light,dark}`，统一受 `isDark` 信号驱动。
- **双形态**：layout/palette 可外链（共享零件 ID，查 `themes/_layouts`、`_palettes`）或内联对象；两者产出等价 `ResolvedV25`。

## 三、完成度矩阵

| 能力 | 状态 | 备注 |
|---|---|---|
| 盒模型引擎（Row/Column/Stack、margin/padding/border/radius） | ✅ 唯一渲染路径 | 旧固定化渲染器已退役 |
| 背景填充：纯色 / 九宫格 / stretch / tile / center | ✅ | 预乘合成 + 双线性缩放 + 圆角裁剪 |
| 装饰层 `layers[]`（z 序、anchor、offset、size、opacity） | ✅ | 水印类 |
| 选中/悬停态（item/index/comment 各自独立） | ✅ | |
| 禁用态 disabled | ⚠️ schema 解析通、无运行时触发器 | 候选项无禁用渲染标志，预留 |
| 选中=高亮位图 + 文字色/字重/边框 | ✅ | P7-D 头号能力 |
| 逐元素字号/字重/字体族 | ✅ | DirectWrite/GDI 按名解析，未知回退 |
| 序号样式（circle/none、自定义 labels） | ✅ | |
| 强调条（开关/宽/偏移/高比） | ✅ | |
| 暗色位图变体 `{light,dark}` | ✅ | P7-E，按 isDark 选路径 |
| 结构化阴影 offset_x/y + color | ✅ | |
| **渐变** `gradient` | 🟡 schema 冻结、**不渲染** | 占位防冻结，merge 保留 |
| **阴影 blur/spread** | 🟡 schema 冻结、**不渲染** | 仅 offset+color 生效 |
| **方向变体**（横/竖分别配） | 🟡 不加字段、声明为非破坏扩展 | D7 |
| 其它窗口（菜单/Tooltip/状态泡/Toast） | ✅ 几何+颜色+字体+背景图+layers View 化（P8 切片0-3/5/6 完成） | 工具栏仅颜色 View 化，几何延后（切片4 独立重构） |

**验收证据**：`go build`（主 + wind_setting）、`go test ./...` 全绿、`go vet` 干净。`jidian-classic`（testdata）端到端覆盖九宫格背景/水印层/选中高亮位图/暗色变体/结构化阴影/逐元素字体/序号样式。

## 四、冻结字段面（契约）

**顶层 `Theme`**：`meta` / `layout` / `palette` / `views` / `behavior` / `overrides` / `resources`

**`views.<node>`**（window / preedit_bar / candidate_list / item / index / text / comment / accent_bar / footer_bar 均为 `ViewNode`）：

```
margin/padding {top,right,bottom,left}      border {width,color,radius}
background: ViewFill{ color, shape(circle|none), image:ViewImage, gradient:ViewGradient* }
font_family / font_size / font_weight / color / labels[](仅 index)
layers: []ViewImage{ ref,mode,slice,opacity,z,anchor,offset{x,y},size{w,h} }
selected / hover / disabled : ViewNode(递归 patch)
```

**`ViewImage`** `{ ref, mode(nine_slice|stretch|tile|center), slice{t,r,b,l}, opacity, z, anchor, offset{x,y}, size{w,h} }`

**`views.metrics`**：`item_spacing / band_gap / shadow_offset(legacy) / shadow{offset_x,offset_y,blur*,spread*,color} / accent_bar{enabled,width,offset,height_ratio}`

**`views.toolbar / menu / status / tooltip`**：各自专用结构（toolbar button mode 中/英态 + settings 齿轮；menu hover/disabled 等）。

**`resources`**：`name → "path"` 或 `{light, dark}`（YAML/JSON 双写法，标量回退）。

**`palette`**（双变体）：语义色 `bg/surface/border/text/text_dim/text_hint/accent/on_accent/shadow` + 组件覆盖 `candidate_window/toolbar/popup_menu/tooltip/status/toast`；`${name}` 引用 + `derive`（hct / hsl-shift）派生。

**`behavior`**（可被用户覆盖）：`font_size / always_show_pager / show_page_number / vertical_max_width`。

> 🟡 标 `*` 的字段（`gradient`、阴影 `blur`/`spread`）是**预留**：解析 + merge 保留，但渲染不消费。冻结后补渲染**不破坏** schema。

## 五、主题示例

`jidian-classic`（位于 `pkg/theme/testdata/themes/`，**纯测试不发布**）端到端用到了：九宫格背景图 + 暗色变体、右下水印层、选中高亮位图 + 白加粗字、纯数字序号、强调条、结构化阴影。

```yaml
meta: {name: "极点经典(位图)", version: "2.6", order: 2}
layout: compact-horizontal      # 其它窗口尺寸
palette: windy-blue             # 颜色(明暗双变体)
resources:
  panel: {light: "panel.png", dark: "panel-dark.png"}   # 暗色自动切
  sel:   {light: "sel.png",   dark: "sel-dark.png"}
  mark:  "mark.png"
views:
  window:
    background: {color: "${background}", image: {ref: panel, mode: nine_slice, slice: {top: 8,right: 8,bottom: 8,left: 8}}}
    border:  {radius: 8, width: 1, color: "${border}"}   # 圆角由引擎裁,位图只填充
    layers:  [{ref: mark, z: 1, anchor: bottom-right, offset: {x: -8,y: -6}, size: {w: 20,h: 20}, opacity: 0.9}]
  item:
    border: {radius: 4}
    selected:                    # 选中=高亮位图+白加粗(被圆角裁剪)
      background: {color: "${selected_bg}", image: {ref: sel, mode: stretch}}
      color: "#FFFFFF"
      font_weight: 600
  index: {color: "${index_text}", background: {shape: none}}  # 纯数字序号
  metrics:
    accent_bar: {enabled: true, width: 3}
    shadow: {offset_x: 3, offset_y: 4, color: "${shadow}"}
```

发布主题：`default`（windy-blue，纯色）、`msime`（微软风，纯色）——简洁、无位图。

## 六、注意点 / 架构铁律（编辑器与主题作者必读）

1. **圆角 = View 裁剪，位图 = 填充**：位图**不能**自带硬边/硬角（会被 View 圆角裁出缺角）。要圆角让 `border.radius` 画，位图只管渐变/纹理。
2. **预乘 alpha 全程**：`image.RGBA` 是预乘的；授图 PNG 用 NRGBA（straight），解码时预乘一次，此后缩放/合成/OS 面全预乘——否则半透明边缘发暗/起毛边。
3. **offset 是屏幕平移**（+x 右 / +y 下）：`bottom-right` 锚点要内缩须用**负**偏移。
4. **`transparent` 让背景透出**：预编辑条设 `background.color: transparent` 才能让窗口背景图整窗透出。
5. **显式 0 语义**：距离/边框写 `0` 是"显式关闭"会被保留；不写才回退基线。
6. **states 按元素独立**：`item.selected` 只管候选文字 + 项背景；序号/注释要变得各配 `index.selected` / `comment.selected`（默认不随选中变）。
7. **测试/验收主题别放 `themes/`**：会被 `build_all.ps1` 打包发布；放 `pkg/theme/testdata/`。

## 七、已知限制 / 遗留

- 渐变、阴影 blur/spread：schema 在、渲染未实现（冻结后可补，不破坏 schema）。
- 候选项 disabled 态：无运行时触发器（`Candidate` 无禁用渲染标志），目前是 schema 预留。
- ~~其它窗口几何仍 hardcode~~ **已于 P8 完成**：菜单/Tooltip/状态泡/Toast 几何+背景图+layers View 化（切片0-3/5/6，2026-06-03）。**仅工具栏几何延后**（切片4，因尺寸/布局动态化 + 与 HitTest 强耦合，单列独立重构）。
- 双渲染器一致性（Go vs P3 编辑器 Canvas）未验证——P3 阶段需逐场景比对。
- 既有小瑕疵（待单独修）：GDI getMetrics 缓存 key 缺 symbol 字段；accent glow `sc(2.5*scale)` double-scaling。

## 八、冻结决定（已生效 2026-06-02）

**范围**：**冻结候选窗 schema**（views 候选窗节点 + metrics + resources + palette + behavior）。其它窗口（工具栏/菜单/Tooltip/状态泡/Toast）**几何 View 化为 P8 增量**，按下方扩展政策非破坏地补。

**理由**：候选窗是主战场，表达力已足以复刻位图皮肤；渲染管线（预乘/双线性/圆角）正确；字段面清晰、预留位齐全、死字段已清。等其它窗口会拖太久，且有了候选窗经验后它们风险更小。

### 钦定的非破坏扩展（冻结后可直接加，无需迁移）

| 扩展 | 形状 | 现状 |
|---|---|---|
| **渐变** | `ViewFill.gradient {type,angle,stops:[{color,pos}]}` | 字段已在、merge 保留，渲染待补 |
| **阴影 blur/spread** | `ViewMetrics.shadow.{blur,spread}` | 字段已在，渲染仅 offset+color |
| **方向变体** | `ViewNode.vertical:`（与 selected/hover 同构的 patch，竖排叠加在 base 上）；字段**未加**，需要时增补即非破坏 | 仅政策声明（YAGNI） |
| **整窗/整 View 透明** | 现有 `background.color: transparent`（全透）/ `#RRGGBBAA`（半透）+ `border.{radius,width}: 0` 组合，位图自带形状 | 字段已对；引擎需补"窗口透明时以位图 alpha 作遮罩"（render-later） |
| **其它窗口几何 View 化** | 工具栏/菜单/Tooltip/状态泡/Toast 的 margin/padding/border 等 | ✅ P8 完成（菜单/Tooltip/状态泡/Toast，切片0-3/5/6）；工具栏延后 |

> 编辑器（P3）与解析器**必须**：把方向（横/竖）当作一个维度、把透明当作 `background.color` 的常规取值、容忍未知的上述扩展字段——**不得写死单方向或不透明假设**。

**契约基准**：本文第四节字段面 + `theme-view-architecture.md`「⛔ v2.6 schema 冻结声明」。**变更规则**：冻结字段语义/类型变更须迁移（升版本 + 迁移器）；新增可选字段非破坏、无需迁移。
