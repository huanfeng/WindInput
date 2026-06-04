<!-- Parent: AGENTS.md -->
<!-- Generated: 2026-06-04 | Updated: 2026-06-04 -->

# 矢量图（SVG）+ 单色染色（tint）

## 背景与需求

主题图片管线原本只解 PNG/JPEG → `*image.RGBA`，paint 时由 gg 缩放。位图在高 DPI 放大会糊；
图标颜色写死在图里、不跟主题。本设计为主题图片接入**矢量图（SVG）**与**单色染色（tint）**，
首个落地消费方是翻页箭头（`views.footer_bar.prev_image`/`next_image`）。

## 方案（A+B）

- **A. SVG 栅格化**：纯 Go 库 `srwiley/oksvg` + `rasterx`（无 cgo），按**目标设备像素尺寸**现场栅格化，
  缓存键含尺寸 `(ref, w, h)` → 矢量在任意 DPI 都清晰，不走"先栅格再缩放"的糊路径。
- **B. 单色 tint**：把图（SVG/PNG）当 **alpha mask**、用主题色填充（保留 alpha、RGB 换成 tint 色，预乘）。
  让"自定义图标形状 + 颜色仍跟主题"合一——单色图标随亮暗/主色变化。

### 依赖决策（oksvg 停更评估）

`oksvg`/`rasterx` 多年低活跃，但成熟稳定、被 fyne 等广泛使用。我们的场景是**图标级矢量 + 主题文件
半可信输入**，停更风险低（≠ 失修；XML 炸弹类安全风险不适用于本地主题文件）。对边界清晰的图标栅格化，
一个"做完了"的成熟库比自写解析器更安全。**栅格化收口在 `theme.RasterizeSVG` 单一函数**，将来换库/
换自写实现只动这一处（隔离爆炸半径）。

## 实现链路

| 段 | 位置 |
|----|------|
| 栅格化 | `pkg/theme/svg.go`：`RasterizeSVG(pathOrDataURI, w, h)` / `IsSVGRef` |
| schema | `ViewImage.Tint`（ColorToken）；`ViewNode.PrevImage`/`NextImage`（仅 footer_bar）|
| resolve | `RVImage.TintColor`（`toRVImage(im, resolveColor)` 解析 tint token）；`RVNode.PrevImage`/`NextImage` |
| 取图 | `internal/ui` `imageResolver.resolveImage(ref, resources, w, h, tint)`：SVG 按 (w,h) 栅格化、位图走 ref；`tint` 非 nil 经 `tintMask` 染色；结果按复合键缓存 |
| 消费 | `fillFor`/`appendLayers`（背景/层，全窗口统一获得 SVG+tint）；`buildPager`（翻页箭头图，居中绘制，失败回退内置 chevron；禁用态=首/末页改用 `disabled_tint`，未配则不变化、无硬编码淡化）|

## 尺寸与缓存

- **已知尺寸**（layers 的 W/H、翻页箭头的图标尺寸）→ 按设备像素精确栅格化，清晰。
- **动态尺寸**（stretch 整窗背景，build 期不知最终 rect）→ SVG 兜底 64² 栅格化后由 gg 缩放（best-effort）。
  完整的 paint 期按 rect 栅格化（整窗矢量背景最优画质）**延后**。
- 缓存键：位图=`ref`；SVG=`ref@WxH`；带 tint 追加 `#RRGGBBAA`。失败结果也缓存（不每帧重试）。

## 用法示例

```yaml
views:
  footer_bar:
    color: "${footer}"                 # 页码 + 启用态箭头色
    prev_image: { ref: "arrow_left", tint: "${footer}" }   # SVG 箭头，跟随 footer 色
    next_image: { ref: "arrow_right", tint: "${footer}" }
resources:
  arrow_left: "icons/chevron_left.svg"
  arrow_right: "icons/chevron_right.svg"
```

## 未做（可延后）

- 整窗 SVG 背景的 paint 期按 rect 栅格化（当前 64² 兜底 + gg 缩放）。
- 矢量符号（grip/齿轮/全半角等内置 gg 矢量）改为可主题替换。
- 翻页箭头尺寸随图标/字号联动的更精细布局（当前按 chevron 视觉尺寸）。
