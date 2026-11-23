# 展开网格的主题样式（整行 / 行间分隔线 / 格内对齐）

> 关联：`candidate-grid-expand.md`（展开网格本体，§6.2 当时写明「P1 不加主题字段」）。
> 起因：参照微信输入法亚克力皮肤——展开后每行有整行底色、行与行之间有发丝分隔线、格内文字居中。
> 系统背景模糊（亚克力）不做：平台相关，主题不应依赖单一平台特性。

## 1 问题

展开态里「一行」只是装配时临时包的一个无身份的 `Row`，主题无法给它配任何东西：

- 整行底色 / 边框 / 内外边距：没有节点；
- 高亮行（数字键作用的那一行，协议里的 `CandidateGrid.highlight_row`）：只用来决定序号显示，不参与样式；
- 行与行之间的分隔线：只有编码栏下面那一条 `divider`；
- 格内文字：永远左对齐，列宽等于该列最宽候选。

## 2 新增节点

```toml
[views.grid_row]            # 展开网格的整行容器
background = "#..."         # 基态
border = { color = "#..", width = 1 }
border.radius = 8
margin / padding = [..]
align = "center"            # 格内内容对齐：left（默认）| center

[views.grid_row.selected]   # 高亮行（= highlight_row）
background = "#..."
[views.grid_row.hover]      # 鼠标所在行
background = "#..."

[views.grid_divider]        # 行间分隔线（写法同 divider）
background = "#..."         # 必须显式配，才画
height = 1                  # dp，缺省 1
margin = [0, 12, 0, 12]     # 左右缩进
enabled = false             # 强制不画
```

状态优先级与 `item` 一致：高亮行优先于悬停行。悬停行 = 鼠标所在格所在的那一行。
行底色画在格子底色之下——「高亮行淡底 + 高亮格实心块」两层可以同时存在。

## 3 决策

- **零回归**：三个新节点缺省全部透明 / 不画 / 左对齐，没配的主题渲染树与此前逐字节一致
  （golden 未动）。
- **分隔线不回退调色板 `separator`**：与 `divider` 不同。默认主题有 `separator` 色，
  回退会让所有现有主题的网格凭空多出线、窗口高 1px。代价是要显式配色，可接受。
- **分隔线夹在行间，占位行也占位**：末屏行数不足时补的占位行与真实行同高，分隔线也按
  `rows` 计数，窗口高度与满屏一致（同 `candidate-grid-expand.md` §6.3 的「尺寸要稳定」）。
  整行的 `padding` / `margin` 同样加在占位行外，理由相同。
- **居中靠两侧弹性占位**：格宽已被抬到列宽（`min_w`），富余由两个 `spacer().grow()` 平分。
  收尾占位加在注释装配之前；带「上方注释条」的格子里条在 `stack_above` 之内，不随居中。
- **行容器撑满列宽只在有装饰或居中时才做**，避免改动未配主题的布局树。
- **整行不设默认选中色**：缺省取 `item.selected` 的色会叠在格子自己的底色上。

## 4 预留（本期不做）

- `grid_scrollbar`：当前按页翻转，没有滚动语义。要做时 `CandidateGrid` 需要补
  `total_rows` / `first_row` 两个字段（协议三端同步），主题侧是 `width` / `margin` /
  `radius` / `track` / `thumb`，缺省不画。节点名先占住，别另作它用。
- 展开按钮（∨ / ∧）：现用翻页按钮替代，暂不做。

## 5 落点

| 层 | 位置 |
|---|---|
| schema | `wind-theme/src/schema.rs`：`Views.grid_row` / `grid_divider`、`ViewNode.align` |
| 归一化 | `wind-theme/src/normalize.rs`：`VIEW_NODE_KEYS` |
| 解析 | `wind-theme/src/resolve.rs`、`rvnode.rs`：`RvViews.grid_row` / `grid_row_align` / `grid_divider*` |
| 渲染 | `wind-scene/src/candidate/mod.rs`：`NodeKey::GridRow` / `GridDivider`，网格装配段 |
| 测试 | `wind-ui/src/candidate_window/render_golden.rs`：`grid_row_*`、`grid_divider_*` |

跨仓待办：主题编辑器（WindInputThemeEditor）的字段清单与预览要同步，`pnpm check:engine`
会对不上；文档站的主题参考页补三个节点。
