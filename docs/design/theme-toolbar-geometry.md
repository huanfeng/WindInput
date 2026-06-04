<!-- Parent: AGENTS.md -->
<!-- Generated: 2026-06-04 | Updated: 2026-06-04 -->

# 工具栏几何重构（L1 几何单一真相源 + L2 盒模型化）

## 背景

输入法浮动工具栏（`internal/ui` 的 toolbar 系列，仅 Windows）当前几何还停留在
"线性公式手算按钮坐标 + 几何与鼠标命中各算一套"的状态。候选窗早已迁到盒模型 View
引擎（`viewbox.go` 的 `Layout`/measure/arrange），工具栏渲染虽也接了 `Layout`/`PaintTree`，
但用 `FixedW/FixedH` 把 measure 架空，且命中/边界查询仍走独立公式。

本次按颜色已迁 `toolbar_*` token（v3）之后的延续工作，分两步：**L1 几何单一真相源**
（解耦，零视觉变化）、**L2 盒模型化**（几何进 schema，measure 真正生效）。

## 问题：按钮矩形被算了三遍（+窗口尺寸第四处）

| # | 位置 | 用途 | 形式 |
|---|------|------|------|
| 1 | `viewbox_toolbar.go buildToolbarTree` | 渲染布局 | View `FixedW + Margin` 隐式编码 x |
| 2 | `toolbar_renderer.go HitTest` | 鼠标命中 | 独立线性累加 `gripW + n×buttonW` |
| 3 | `toolbar_renderer.go GetButtonBounds` | tooltip/菜单定位 | 独立线性累加 + padding |
| 4 | `toolbar_window.go:219/547` | Win32 窗口尺寸 | `ScaleIntForDPI(116/30)` 字面量 |

改任一常量（如 `buttonWidth`）需同步改 1/2/3 三处——这是耦合。#4 用的是不同缩放源
（`ScaleIntForDPI` vs 渲染的 `GetDPIScale`），属另一议题，L1 不动缩放基准。

## L1：几何单一真相源（已实现，零视觉变化）

**核心**：一切按钮矩形从同一次 `buildToolbarTree + Layout` 派生，删除 #2/#3 的线性公式。

仿候选窗 `renderTree → RenderResult.Rects → hitRects` 范式，但工具栏只 5 个 View、
几何与状态无关（mode 文字变化不影响布局，因 `FixedW` 固定），故采用**无状态按需布局**，
不引入缓存（无缓存失效 / DPI 过期风险）：

```
computeGeometry() → 用零 state/零色 buildToolbarTree + Layout，提取：
  - size   = root.Rect().Size()                    （GetToolbarSize）
  - bounds = 各按钮 View 的 Rect()（content 矩形）  （GetButtonBounds）
  - hits   = 各按钮 View 的 margin 盒（content 外扩 Margin）（HitTest）
```

**等价性证明（faithful 关键）**：
- 旧 `HitTest` 命中语义 = 按 x 分段、忽略 y、按钮间无间隙 = 每个子 View 的 **margin 盒**。
  `LayoutRow` 使 margin 盒首尾相接 → 平铺整条、满高。逐按钮核对：grip `[0,10)`、
  mode `[10,36)`、width `[36,62)`、punct `[62,88)`、settings `[88,114)`（scale=1），与旧带界一致。
- 旧 `GetButtonBounds` = content 矩形 = View `Rect()`（如 settings `Min.X=90`、宽 `buttonW-2pad=22`）。
- 旧 `GetToolbarSize` = `(116,30)×scale` = root `Rect().Size()`（root `FixedW/FixedH` 即此值）。

`viewOuterRect(v)` = `v.Rect()` 外扩 `v.Margin`（Margin 是 View 自带数据，非重算公式）。

**附带**：`toolbar_window.go` 的窗口尺寸字面量 `116/30` 换成包内常量
`toolbarBaseWidth/toolbarBaseHeight`（消魔数，不改 `ScaleIntForDPI` 缩放源）。

**不变**：渲染（`Render`）与矢量符号后处理已用 `tt.X.Rect()`，本就单源，无需改。

## L2：几何进 schema（已实现）

L1 解耦后几何收口到 `buildToolbarTree`，L2 把硬编码几何提升为主题可覆盖字段：

1. **schema 几何字段**（`ToolbarViews`，`*Dimension`，nil=内置默认零回归）：
   `height`(30) / `grip_width`(10) / `button_width`(26 槽位含 padding) / `button_padding`(2) / `button_radius`(4)。
   `resolveToolbarGeom(rv, scale)`（internal/ui）按 scale 换算为设备像素、缺省回退默认；
   `buildToolbarTree` 改吃 `toolbarGeom`。
2. **保持固定统一按钮 + 预留内容驱动**：按钮仍 `FixedW`（统一固定，工具栏的正确模型）；
   `button_width` 取 `*Dimension` 形态即为内容驱动预留——未来 nil 可改走"内容 measure + padding"，
   届时只改 `resolveToolbarGeom`/`buildToolbarTree` 一处，命中/尺寸经 L1 自动跟随。
3. **总宽 116→114**：`root` 去掉 `FixedW`，由 measure 汇总 = `grip + 4×button 槽位` = 114，
   消除旧 116 的尾部 2px 死区。
4. **间距用按钮 margin（非引擎 Gap）**：margin 盒首尾相接 → 命中带无缝（`viewOuterRect` 平铺），
   避免 `Gap` 在按钮间留 2px 死区。引擎通用 `View.Gap` 已具备（候选列表在用），
   工具栏出于命中无缝考量用 margin；`button_padding` 即间距控制。
5. **窗口尺寸收口（#4）**：`toolbar_window.go` 创建/DPI 变化处改用 `renderer.GetToolbarSize()`
   （与渲染同源；`GetDPIScale` 与 `ScaleIntForDPI` 本就同一 provider，仅四舍五入 vs 截断之差）。
   实际显示尺寸由 `UpdateLayeredWindow` 取渲染图像 bounds，`GetToolbarSize` 使三者一致。
   `computeGeometry` 用 `zeroMeasurer`（几何全 FixedW，免依赖文本后端，窗口创建早期可安全调）。

**未做（可延后）**：补 `other_views.go` 的 `ResolveToolbarViews` 统一走 `resolveViewNode`（颜色解析现仍在
`viewbox_toolbar.go` 自实现，工作正常）；矢量符号尺寸仍按 scale（不随 button_width 变），属 L3 内容动态化范畴。

## L3 愿景（远期，不在本次）

按钮内容动态化：每个状态指定开/关对应显示效果（文字 / 符号 / 图片），支持悬停特效。
当前无视觉 hover（仅记录用于 tooltip）。

## 测试策略

- `TestBuildToolbarTree_Geometry`（既有）：整条 116×30、按钮框高 26、mode 选色、settings `Min.X=90`——L1 后仍逐项绿（几何数值不变）。
- 新增 `TestToolbarHitTest_SingleSource`：① 各按钮带中心点 `HitTest` 返回对应 kind；② `GetButtonBounds` == `buildToolbarTree+Layout` 的对应 `Rect()`；③ `GetToolbarSize` == root 尺寸；④ 命中带平铺无缝（相邻带界相接）。
- `TestResolveToolbarViews_BaseAndMode`（既有）：颜色不变。

## 风险与回滚

- L1 行为零变化由等价性证明 + 测试守护；如真机命中异常，回滚仅涉及 `toolbar_renderer.go` 三方法。
- 缩放源差异（`GetDPIScale`/`ScaleIntForDPI`）刻意留到 L2，避免 L1 夹带 DPI 回归。
