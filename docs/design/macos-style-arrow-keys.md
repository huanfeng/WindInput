# macOS 式方向键交互（← 编辑编码 / → 选词 / ↑↓ 展开多行）

> 来源：[GH#234](https://github.com/huanfeng/WindInput/issues/234)（看板 C2-97，Taswen：光标在末尾时 ← 进入编辑编码、→ 进入选词，↑↓ 展开多行）+
> 维护者要求（2026-10-25）：综合分析现状能力，给出**可供用户选择的策略**，不做唯一形态。

## 现状盘点

| 能力 | 现状 |
|---|---|
| 主路光标 | **基建完整**：`state.input_cursor_pos`（`input_buffer` 内字节偏移），读写走 `preedit_cursor::BufEdit`；光标不参与引擎查询，中间插入后按整串重算——对齐 Go 光标模型。`caret_utf16` / 显示串映射 / 大小写影子串都在 `preedit_cursor.rs` |
| 退格 | 主路与 mix 已按光标删（`BufEdit`），不是只会吞末尾 |
| ←→ 出厂语义 | `highlight_keys = ["arrows", "tab"]` → 移动高亮；展开态（网格）下 ←→ / Esc 被 `grid_key_action` 先行截走 |
| ↑↓ 展开 | `page_next` 首按展开网格（`try_expand_grid`），展开后 ↑↓ 变行移动（网格键接管） |
| 可绑定动词 | `session_actions` 现有：page_prev / page_next / highlight_up / highlight_down / cancel(clear) / select_candidate:N / single_char / aux_code / commit_highlighted / commit_code / command |
| 缺口 | **没有「光标移动」动词**——`← 编辑编码` 表达不出来，这是唯一需要写代码的点 |

结论：macOS 交互的**全部底层能力已在**，缺的只是两个动词 + 一份配方。

## 方案：两个新动词，不设「模式档」

`session_actions` 新增：

- **`caret_left`**：光标左移一字符。已在开头 = 吞键。无会话 = 放行（方向键还给宿主）。
- **`caret_right`**：光标右移一字符；**已在末尾 = 上屏高亮候选**（与空格选中同一函数，学习 / 调频副作用逐字一致）。这个边沿语义必须内建在动词里——「在末尾时改做选词」用静态绑定表达不出来，而这正是 macOS「→ 进入选词」的形态。

设计取舍：

1. **不做 `arrow_key_profile` 枚举档**。档位会变成第二份绑定真相源，与 `session_actions` 打架（档说 ←=编辑、表说 ←=高亮，谁赢？）；本仓按键哲学是**显式逐键绑定**（`key_actions` / `session_actions` 两张表的先例）。用户的选择通过**配方**表达，而不是开关。
2. **overlay 让位**：临拼 / 快捷输入等 overlay 各有自己的缓冲光标（`preedit_cursor::BufEdit` 的既有调用方）。`caret_left` / `caret_right` 只在主路生效——`state.active` 非 `None` 时该动词返回 `None` 让位，overlay 的 ←→ 不被绑走。展开态（网格）同样让位：`grid_key_action` 在 `apply_session_action` 最前面，天然先截。
3. **无会话 / 无候选**：光标移动只需「有会话」（对齐 `Cancel` 档的 `requires_candidates` 取值）——打了码还没出候选时也应能移动光标改码。

## 可选策略（写进文档的配方，出厂全部不变）

出厂维持现状（←→ = 移高亮）。想换的用户按下面配方改 `keys.session_actions`（先把 `highlight_keys` 里的 `arrows` 组摘掉，否则两处撞车——撞键裁决是「以词定字 > 翻页/高亮 > 二三候选」，绑定表写了的键优先，摘组是为了别留死配置）：

**B 全 macOS 档**（楼主描述的完整形态）：

```toml
highlight_keys = ["tab"]        # arrows 摘掉，tab 保留移高亮
[keys.session_actions]
left  = "caret_left"            # ← 从末尾进入编码编辑；再按继续左移
right = "caret_right"           # → 光标右移；到末尾 = 选词
down  = "page_next"             # ↓ 首按展开多行，展开后行移动（网格键接管）
up    = "page_prev"             # ↑ 对称
```

**C 半 macOS 档**（← 进编码，↑↓←→ 其余照旧——只有 ← 换语义时 ↑↓←→ 仍可保留高亮，见下）：

```toml
[keys.session_actions]
left = "caret_left"             # 仅 ← 换；→↑↓ 不写 = 回落 highlight_keys 的移高亮
```

⚠️ C 档下 `→` 仍是移高亮（不是选词）——`highlight_keys` 没摘。两档差异要在文档里写明白，避免「为什么 → 不选词」的追问。

**回退**：删掉对应绑定（或整段 session_actions 里这几行）即回出厂；动词没被引用时零开销。

## 分期

- **P1（本轮）**：两个动词 + 主路消费分支 + overlay 让位 + 测试 + 文档配方。
- **P2（另案）**：设置端「方向键布局」预设（一键写入上面配方到用户配置——落点是用户能清空的载体，符合出厂预置红线）；`key_conflict` 收录新动词的冲突提示。

## 测试计划

- 动词 parse：`caret_left` / `caret_right` 往返；未知值仍落 `None`。
- 主路：末尾按 ← 光标进编码 → 继续打字插入中间 → 候选按整串重算；`caret_right` 到末尾再按 = 上屏高亮候选（含选词副作用）；开头 `caret_left` 吞键。
- 让位：临拼 / 快捷输入里 ←→ 仍走各自光标臂；展开态 ←→ 仍是网格键。
- 配方端到端：按 B 档配置打 `hi` → `←` → `a`（缓冲变 `hai`，候选按整串重查出「海」）→ `→` ×2（右移到末尾、再按上屏「海」）——整条序列确定性钉住。

## 末尾动作可选：`caret_right[:commit|next|tip]`

macOS 的 → 在末尾是「切到下一个候选词」，本仓出厂语义是「上屏高亮候选」。末尾那一下现在可选：

| 写法 | 光标已在末尾时 → 的动作 |
|---|---|
| `caret_right` / `caret_right:commit` | 上屏高亮候选（原行为） |
| `caret_right:next` | 高亮下移一个候选（同 `highlight_down`），贴近 macOS |
| `caret_right:tip` | 开 / 关高亮项的悬停提示（t372），等价于 `toggle_tip` 绑在 → 上 |

光标没到末尾时三者都是右移光标。`toggle_tip` 直接绑在 → 上时的规则与 `caret_right:tip` 一致。
