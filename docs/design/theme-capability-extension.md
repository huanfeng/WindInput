# 主题能力扩展方案（强调条比例 / 窗口偏移 / 盒模型补齐 / 工具栏圆角 / Toast 边框预览）

跨仓：主仓 `WindInput` + 主题编辑器 `WindInputThemeEditor`。

## 实施状态：全部完成（待真机核对）

| 需求 | 主仓 | 编辑器 |
|---|---|---|
| 5 Toast 边框预览 | 无需改动（引擎本就正确） | `e1ad8f1` |
| 1 强调条比例 + 左缘偏移 | `8cd3e73` | `e1ad8f1` |
| 4 工具栏整条圆角 + 边框宽 | `389906b` | `fc6449a` |
| 3 四节点盒模型补齐 | `0419e5e` | `1a02850` |
| 2 候选窗位置偏移 | `2430942` | `a4f08cb` |

自动化验证：主仓 `wind-theme` 32 项 + `wind-ui` 95 项全绿，新增用例 9 个；
编辑器 365 项全绿（新增 3 个），`pnpm build`（含 `vue-tsc`）通过；
`place_window` 的 too-many-arguments clippy 告警已通过元组参数消除。

**真机待核对**（`scripts/dev.ps1 d1`）：候选窗横/竖排零回归、强调条比例与偏移、
工具栏圆角（含 `radius=0` 直角）、四节点背景边框、位置偏移的贴光标距离与上翻方向。

实施中发现方案未预见的两处，已一并修：
- 编辑器 `borderVal` 按 ViewNode 约定排除 radius，而 `dumpToolbar` 没有对应的节点级上提、
  引擎 `normalize_toolbar` 也不下沉散键 radius —— 工具栏圆角配了之后**一导出就丢**。
- 编辑器有两条解析路径，现行的 `theme3/resolve.ts` 里 `accentBarHRatio` 硬写 0
  且注释说明「刻意不读主题」，与 `theme/resolveViews.ts` 不同源。

## 0. 根因总表

调研结论（一手核验，附文件行号）。五项里有三项是同型病：**schema 有字段 → resolve 有透传 → 渲染层不消费**，字段"存在但是死的"。

| # | 需求 | 现状 | 根因位置 | 性质 |
|---|---|---|---|---|
| 1 | 强调条高度比可配 | schema/resolve 齐备，渲染硬编码 `0.6` | `wind-ui/src/view.rs:548` | 死字段，接线 |
| 2 | 候选窗位置偏移 | 无此字段 | `candidate_window.rs::place_window:1159` | 新增能力 |
| 3 | 组件圆角补齐 | preedit_bar/mode_label 借用 `item.border_radius`；footer_bar/candidate_list 连背景边框都没接 | `candidate_window.rs:1477 / 1517 / 1654` | 借用 + 缺失 |
| 4 | 工具栏整条圆角不生效 | 主题 `[toolbar] border.radius` 未提取到 `RvViews`，渲染硬编码 `高度 × 0.30` | `resolve.rs:480-491`、`toolbar.rs:347` | 死字段，接线 |
| 5 | Toast 边框色"不生效" | **引擎侧正常**（主题优先、未配才回退 accent），是编辑器预览固定画 accent 环 | `toast.rs:144-170` 正确；`otherWindows.ts:100` 有问题 | 编辑器预览缺陷 |

已确认的用户决策：
- 需求2 **主题级，仅作用于「跟随光标」定位路径**；
- 需求3 **补全四个节点的完整盒模型**；
- 需求1 `height_ratio` 与 `offset` **一起接**。

---

## 1. 强调条：高度比 + 左缘偏移

### 现状

```
schema.rs:267-273   accent_bar.enabled / width / offset / height_ratio   ✔ 已定义
resolve.rs:421-426  rv.accent_bar_offset / accent_bar_height_ratio       ✔ 已透传
rvnode.rs:161-162   RvViews 字段                                          ✔ 已存在
────────────────────────────────────────────────────────────────────────── 断点
candidate_window.rs:1564  accent_bar = (color, width) 二元组             ✘ 丢掉 ratio/offset
view.rs:548               let bh = (r.h * 0.6).max(2.0);                 ✘ 硬编码
```

编辑器 `ViewsEditorV3.vue:860` 已明确注释"引擎不消费故控件撤下"，`candidateBox.ts:1138/1447` 预览也照 0.6 画。

### 改法

**`view.rs`** —— `left_bar` 从 `Option<([u8;4], f32)>` 扩为四元组：

```rust
// View 字段
pub left_bar: Option<([u8; 4], f32, f32, f32)>,  // (色, 宽, 高度比, 左缘偏移)

pub fn left_bar(mut self, color: [u8;4], width: f32, height_ratio: f32, offset: f32) -> Self

// paint（view.rs:547-551）
if let Some((color, bw, ratio, off)) = self.left_bar {
    let bh = (r.h * ratio).max(2.0);
    let by = r.y + (r.h - bh) * 0.5;
    fill_rounded(buf, buf_w, buf_h, r.x + off, by, bw, bh, color, bw * 0.5);
}
```

**`candidate_window.rs:1564`** —— 构造处补两个值：

```rust
let accent_bar = v.accent_bar_enabled.then(|| (
    col(v.accent_bar.bg_color, [66, 133, 244, 255]),
    dim(v.accent_bar_width, 3.0),
    if v.accent_bar_height_ratio > 0.0 { v.accent_bar_height_ratio } else { 0.6 },
    dim(v.accent_bar_offset, 0.0),
));
```

调用点 `:1787-1789` 同步解构四元组。

### 边界

- `height_ratio` 缺省 `0.6`（`RvViews` 默认是 `f32::default()=0.0`，须显式判零回退，否则**未配主题的强调条会消失**——这是 `project_z_key_action_mixed_engine_gap` 记的「结构体零值 ≠ 出厂默认」同型陷阱）。
- 建议 resolve 期就做钳制：`ratio ∈ (0.0, 1.0]`，越界告警回退 0.6。
- `offset` 是 `Dim`，走 `dim()` 求值随 DPI 缩放。
- 强调条以 `r.x` 为基准（item 矩形左缘），`offset` 正值右推。

---

## 2. 候选窗位置偏移

### 需求

主题边缘常有装饰设计（发光边、外描边、九宫格留白），可见内容边界比窗口矩形内缩，贴光标时观感过近。给主题一个偏移量把窗口整体推开。

### schema

```toml
[views.window]
position_offset = { x = 0, y = 4 }   # dp，随 DPI 缩放；默认 0,0 = 与现状一致
```

`ViewPoint` 类型已存在（`schema.rs:145-149`，x/y 各支持 `Dim`），直接复用。

> 不复用 `ViewNode.offset`——那是 `accent_bar` 专用单值 `Dim`，语义已占。
> 不放 `[behavior]`——behavior 是"用户可覆盖白名单"，而偏移是主题美术的一部分，用户不该在设置页调；放那儿还会牵动 `wind-setting` 仓的五道守门测试。

### 数据通路

```
schema.rs      ViewNode 新增 pub position_offset: Option<ViewPoint>
resolve.rs     rv.window_offset_x / window_offset_y: Option<Dim>（顶层 RvViews，
               与 shadow_* 同样是「从 window 节点提到列表级」的既有模式）
rvnode.rs      RvViews 加两个字段
normalize.rs   position_offset 是内联表，normalize_node 无需特殊处理；
               若要支持数组写法 [0, 4]，仿 expand_edges 加一个分支
```

### 注入点（关键）

> **实施后修正（真机反馈）**：偏移**不能**预先加进锚点。`below_ok`/`above_ok` 是拿锚点
> 跟工作区边界比出来的，锚点含偏移 ⇒ `off_y` 越大两个条件越难成立 ⇒ 本该上翻的场景被
> 判成「上方也放不下」，落回下方分支再被钳到 `wa.bottom - hi`（贴屏幕底），窗口压住光标。
> 观感正是「下方正常、上方遮盖」。
>
> 正确顺序是 **净锚点 → 方位决策 → 施加偏移 → 边界钳制**：偏移只改变距离，不参与
> 「往上还是往下」。见 `caret_anchors`（净锚点）与 `apply_offset_y`（按方位施加）。
> 下面这段是最初的设计，保留以对照。

**必须注入在 `place_window` 内部的锚点计算处、屏幕边界钳制之前**：

```rust
// candidate_window.rs::place_window:1167-1172
let gap = 2;
let below_y = caret_y + gap + off_y;              // 下方：正值向下推离光标
let above_y = caret_y - caret_h.max(0) - hi - gap - off_y;  // 上方：正值向上推离光标
let (mut x, mut y) = (caret_x + off_x, below_y);
```

三条理由：
1. **在钳制之前** —— `place_window:1190+` 有 `rcWork` 越界兜底。若在函数外把结果加偏移，偏移会把窗口推出屏幕且不再被钳回。
2. **`above_y` 用减号** —— 上翻时窗口在光标上方，`off_y` 正值应继续「远离光标」（向上），语义才一致。若照抄加号，上翻时偏移反而把窗口压向光标。
3. **`below_ok`/`above_ok` 判据随之变化** —— 偏移后放不下就该翻面，这是自动发生的正确行为。

`place_window` 签名加两个参数 `off_x: i32, off_y: i32`；`render_frame:883` 调用处从 `self.theme.views.window_offset_*` 求值传入（`Dim::resolve(self.scale, 0.0)`）。

### 不叠加的路径（已定）

```
drag_pin (render_frame:863)   → 不叠加。用户显式拖动，再推开会「拖到哪跳到哪旁边」
fixed_pos (render_frame:867)  → 不叠加。同上，且拖动落盘的 window↔content 换算
                                 会被偏移破坏互逆性（见 project_candidate_fixed_position）
place_window (render_frame:883) → 叠加 ✔
```

### 波及面

- ⚠️ **`manager_macos.rs` 无需改**：偏移在 `render_frame` 内完成，macOS 走 IMKit 由系统定位。但按既有教训，`UiCommand` 若改字段须同步——本方案不改 IPC 协议，无此风险。
- **host-render 路径自动继承**：`try_host_render_candidates` 透传 `frame.screen_x/y`（`manager.rs:1399`），偏移已在其中。
- 首帧定位闸门（`project_candidate_first_show_modes`）不受影响，偏移是纯几何叠加。

---

## 3. 盒模型补齐（四个节点）

### 盘查结果

| 节点 | 背景色/图/渐变/层 | 边框色+宽 | 圆角 |
|---|---|---|---|
| window | ✔ `:1443-1464` | ✔ `:1444` | ✔ `:1448` |
| item | ✔ | ✔ `:1824` | ✔ `:1694` |
| index | ✔ 圆形底 | ✔ `:1724` | ✔ `:1724` |
| text | — | ✔ `:1752` | ✔ `:1752` |
| comment | — | ✔ `:1775` | ✔ `:1775` |
| **preedit_bar** | ✔ `:1476-1490` | ✔ `:1491-1493` | **✘ 借 `item.border_radius` `:1477`** |
| **mode_label** | ✔ 底色 `:1514` | **✘ 无** | **✘ 借 `item.border_radius` `:1517/1654`** |
| **footer_bar** | **✘** | **✘** | **✘** |
| **candidate_list** | **✘** | **✘** | **✘** |
| status / tooltip / toast | ✔ | ✔ | ✔ |
| menu.root / menu.item | ✔ | ✔ | ✔ |

`footer_bar` 现只消费 padding/margin/text_color/font_*/prev_image/next_image/prev_char/next_char；`candidate_list` 只消费 gap/band_gap/row_gap。

### 改法

schema 侧**无需新增字段**——`ViewNode` 已含 `background`/`border`，四个节点全都是 `ViewNode`。改动全在 `resolve.rs` 建节点 + `candidate_window.rs` 消费。

**3a. preedit_bar 独立圆角**（`:1477`）

```rust
- .radius(dim(v.item.border_radius, 4.0))
+ .radius(dim(v.preedit_bar.border_radius.or(v.item.border_radius), 4.0))
```

用 `.or()` 保留回退链：未配 `preedit_bar.radius` 的老主题仍跟随 item，**零视觉回归**；配了就独立生效。

**3b. mode_label 独立圆角 + 边框**（`:1514-1519`、`:1651-1656` 两处，横排内嵌与竖排独立 chip）

```rust
if let Some(bg) = v.mode_label.bg_color {
    chip = chip.bg(bg)
        .radius(dim(v.mode_label.border_radius.or(v.item.border_radius), 4.0))
        .pad(edges_or(&v.mode_label.padding, [1.0, 6.0, 1.0, 6.0]));
}
// 新增：边框独立于底色（可做「只有边框的空心徽标」）
if let Some((bc, bw, br)) = eff_border(&v.mode_label, false, false) {
    chip = chip.border(bc, bw).radius(br);
}
```

⚠️ **两处必须同改**。横排 `:1514` 在预编辑栏内、竖排 `:1651` 是独立 chip，是同一功能的两条通路——这正是本仓反复栽跟头的形态（见 `project_mixed_overflow_vs_topcode`「否决开关必须三处都接」）。

⚠️ 现状「有底色才画徽标」的门控要保留：无底色时不应凭空出现边框盒子。建议门控放宽为 `bg_color.is_some() || border_color.is_some()`。

**3c. footer_bar 背景 + 边框 + 圆角**（`:1889-1895` 翻页容器）

按 preedit_bar 的成型模式装配：`bg` / `bg_image` / `bg_gradient` / `layers` / `border` / `radius`，全部 `Option`，未配 → 与现状逐像素一致。

**3d. candidate_list 背景 + 边框 + 圆角**

`candidate_list` 是列表容器（`:1555` 附近构造的 `list`）。同 3c 装配。这个节点补上后，「候选区与预编辑栏用不同底色/边框分区」这类设计才表达得出来。

### 零回归判据

四处改动**全部是 `Option` 新增消费点**，未配置时走原有默认值。回归网建议：对 `default` / `msime` / `_qingfeng` 三个内置主题跑几何+颜色指纹，断言改动前后完全一致。

---

## 4. 工具栏整条圆角

### 根因

```
schema.rs:293-296   ToolbarViews.background / border（含 radius）  ✔ 可解析
────────────────────────────────────────────────────────────────── 断点
resolve.rs:480-491  只提取 height/grip_width/button_width/button_padding/
                    button_radius/bg_color/border_color —— 没有 border.radius、
                    没有 border.width                              ✘
toolbar.rs:347      let radius = (h as f32 * 0.30) as u32;         ✘ 硬编码
```

编辑器 `ViewsEditorV3.vue:156` 是 `toolbar: { borderWidth: true, radius: true }` —— **两个控件都开放了，但两个都不生效**（`border.width` 同样没提取，`toolbar.rs:348` 的 `fill_rounded` 也不画边框线，只有分隔线用 `toolbar_border` 色）。

`button_radius` 是接线了的（`toolbar.rs:403`），所以"工具栏有圆角配置"这个印象来自它；不生效的是整条圆角。

### 改法

```rust
// rvnode.rs：RvViews 新增
pub toolbar_border_radius: Option<Dim>,
pub toolbar_border_width: Option<Dim>,

// resolve.rs:485 后追加
rv.toolbar_border_radius = tb.border.radius;
rv.toolbar_border_width  = tb.border.width;

// toolbar.rs:347
let radius = self.tb_border_radius
    .map(|d| d.resolve(self.scale, 0.0))
    .unwrap_or(h as f32 * 0.30) as u32;
```

`unwrap_or` 保留原派生行为 → 未配主题零回归。

**边框宽一并接**：现在 `fill_rounded(buf, w, h, 0, 0, w, h, self.bg, radius)` 只填底不描边。若 `toolbar_border_width > 0` 且有 `border_color`，应补一圈描边（`view.rs` 已有圆角描边能力可复用）。否则编辑器那个"边框宽度"控件仍是死的。

---

## 5. Toast 边框（编辑器预览）

### 结论：引擎正确，只改编辑器

引擎 `toast.rs:144-170`：

```rust
self.border = node.border_color.map(|c| (c, node.border_width.map(...)));
...
let (border_color, border_width) = match self.border {
    Some((c, w)) => (c, w.unwrap_or(2.0 * s).max(1.0)),   // 主题优先 ✔
    None => (kind.accent(), (2.0 * s).max(1.0)),          // 未配才回退等级色
};
```

代码里还留着注释："边框此前是 accent + 2px 字面量，主题写什么都没用，现在未配才回退到那套"——说明这是**已修过的历史问题**。

编辑器 `src/lib/preview/otherWindows.ts:74-100` 停在修复前的模型：

```
// 渲染 Toast。镜像引擎 toast.rs 简化模型：**单行居中文本 + accent 色边框环**
...
// accent 色边框环（引擎 border(kind.accent(), (2s).max(1))）
```

### 改法

`otherWindows.ts` 的 toast 分支改为与 status/tooltip 同构（那两个已经在 `:56` / `:157` 正确读 `s.borderColor` / `s.borderWidth`）：

```ts
const borderColor = s.borderColor ?? INFO_ACCENT;
const borderWidth = s.borderWidth > 0 ? s.borderWidth : 2;
```

并把文件头注释里"简化模型 / accent 色边框环"的描述一并更正，避免下次又照它做判断。

---

## 6. 跨仓改动清单

### 主仓 `WindInput`

| 文件 | 改动 |
|---|---|
| `crates/wind-theme/src/schema.rs` | `ViewNode` 加 `position_offset: Option<ViewPoint>` |
| `crates/wind-theme/src/rvnode.rs` | `RvViews` 加 `window_offset_x/y`、`toolbar_border_radius/width` |
| `crates/wind-theme/src/resolve.rs` | 提取上述四项；`height_ratio` 加值域钳制 |
| `crates/wind-theme/src/normalize.rs` | （可选）`position_offset` 数组写法归一 |
| `crates/wind-ui/src/view.rs` | `left_bar` 四元组 |
| `crates/wind-ui/src/candidate_window.rs` | 强调条构造/消费；`place_window` 偏移；preedit_bar/mode_label/footer_bar/candidate_list 盒模型 |
| `crates/wind-ui/src/toolbar.rs` | 整条圆角 + 边框宽读主题 |
| `data/themes/_base/*.toml` | 新字段的默认值（保持与现有硬编码一致） |
| `crates/wind-theme/AGENTS.md`、`crates/wind-ui/AGENTS.md` | 同步字段表 |

### 编辑器 `WindInputThemeEditor`

| 文件 | 改动 |
|---|---|
| `src/lib/theme3/types.ts` | `ViewNodeV3` 加 `position_offset`；`ToolbarViewsV3` 圆角注释更正 |
| `src/lib/theme3/toml.ts` | 键序白名单（`:558-594`）加新字段，保 round-trip |
| `src/lib/theme3/defaultViews.ts` | 新字段默认值，与主仓 `_base` 对齐 |
| `src/components/form/ViewsEditorV3.vue` | ① `accent_bar` 恢复「左偏移/高度比」控件并删除 `:860` 的撤下注释；② `preedit_bar` 开 `radius`、删 `:99-101` 的"跟随候选项"注释与 note；③ `mode_label` 开 `radius` + `borderWidth`；④ `footer_bar` / `candidate_list` 开 `borderWidth` + `radius`；⑤ `window` 加「位置偏移」控件 |
| `src/components/form/StructureTreeV3.vue` | 若新增可选节点则同步 |
| `src/lib/preview/candidateBox.ts` | 强调条 `:1138/:1447` 改用 ratio/offset 并删「引擎忽略 height_ratio」注释；四节点盒模型镜像 |
| `src/lib/preview/otherWindows.ts` | Toast 边框读主题；工具栏整条圆角读主题 |
| `src/lib/theme3/tokenMeta.ts` | 如涉及新 token |

⚠️ **编辑器注释是本次的 gap 清单本身**。`ViewsEditorV3.vue` 与 `candidateBox.ts` 里每一条"引擎不消费/不生效"注释都对应本方案的一项；改完引擎必须逐条翻转，否则下次会有人照注释又把控件撤下去。

---

## 7. 验证

| 项 | 手段 |
|---|---|
| 零回归 | `default`/`msime`/`_qingfeng`/`amber`/`jade`/`violet` 六个内置主题跑几何+颜色指纹，断言改动前后一致 |
| 强调条 | 单测：`ratio=0.0`（未配）→ 0.6；`ratio=1.0` → 满高；`offset=4dp` → 右移 |
| 窗口偏移 | 纯函数测 `place_window`：下方 `+off_y`、上方 `-off_y`、偏移后越界仍被钳回工作区 |
| 盒模型 | 四节点各配一次背景/边框/圆角，指纹断言生效；未配时与旧指纹相同 |
| 工具栏 | `radius=0` → 直角（Switch 硬边缘风格的验收点）；未配 → 仍是 `h×0.30` |
| 真机 | `scripts/dev.ps1 d1` 核对候选窗横/竖排、工具栏、Toast；偏移需在带装饰边的主题上看贴光标距离 |
| 编辑器 | `pnpm build`（含 `vue-tsc`）+ `pnpm test`；预览与真机逐项对照 |

⚠️ 报"全量通过"前须确认 `build_dev/data` 存在——缺失时依赖词库的测试会静默跳过、计数照绿（见 `project_build_dev_data_missing`）。

## 8. 风险

1. **`left_bar` 签名变更**扩散到所有构造点，编译器会全部指出，低风险。
2. **`height_ratio` 零值陷阱**（§1 边界）——最容易翻车的一处，未显式判零会让所有未配主题的强调条消失。
3. **mode_label 两处通路**（§3b）——只改一处会导致横排生效、竖排不生效。
4. **主仓 schema 加字段无编译期跨仓约束**，编辑器不同步就会丢字段（`reference_windinput_tools_repo` 记的同型契约）。本方案的编辑器改动清单即是补偿。
5. **偏移与首帧闸门**：偏移是纯几何叠加，不改变 `coords_ready` 判据，但真机需确认首帧不会因偏移落到屏幕外触发钳制抖动。

## 9. 建议实施顺序

1. **需求5**（编辑器 Toast 预览）—— 单仓、独立、几行，先清掉。
2. **需求1**（强调条）—— 死字段接线，两仓改动小，验证闭环短。
3. **需求4**（工具栏圆角）—— 同上，且直接服务 Switch 硬边缘风格。
4. **需求3**（盒模型补齐）—— 改动面最大但全是 `Option` 新增消费点，风险可控；四个节点可拆四个提交。
5. **需求2**（窗口偏移）—— 唯一涉及定位逻辑的一项，放最后单独验证，避免与其它改动的视觉回归混淆。
