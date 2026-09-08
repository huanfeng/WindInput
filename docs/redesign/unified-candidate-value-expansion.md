# 统一候选值展开与提交派发

## 背景与问题

词库候选 value 可内嵌特殊语法（`$CC` 命令 / `$AA`·`$SS` 组 / `$Y`/`$M`/`$D` 模板 / `{..}` 插值）。
现状：这套「值 → 候选」的展开逻辑，以及「候选 → 动作」的 `$CC` 执行派发，**只接在正常候选路径**
（`handle_candidate.rs::update_candidates`，主方案 codetable/pinyin/mixed 均经此），其余 4 条 overlay/特殊
路径各自绕过：

| 路径 | 生成函数 | 展开 | 提交侧 $CC 执行 |
|---|---|---|---|
| 正常（主方案，含五笔拼音混输） | `update_candidates` / `build_candidates` | ✅ | ✅ `commit_selected` |
| 特殊模式 / 快符 | `update_special_candidates` | ❌ | ❌ |
| overlay 快捷混输（`[[schema.mix_modes]]`） | `update_mix_candidates` | ❌ | ❌ |
| 临时拼音 | `update_temp_pinyin_candidates` | ❌ | ❌ |
| 临时英文 | `update_temp_english_candidates` | ❌ | ❌ |

**症状**：快符表里 `arrx  $AA("箭头","←↑→↓")` 在特殊模式下原样显示字面量，不炸开。根因是特殊模式
候选路径缺失展开 pass（Go 版曾接 `dict.ValueExpander.ExpandToCandidates`，Rust 移植未带过来）。

**目标**：所有输入方案的候选，**生成时统一展开、提交时统一派发**，`$` 全量语法（含 `$CC` 选中即执行）
在全部路径生效；并从结构上消灭「新增路径漏接」的分叉类 bug。

## 架构：双汇聚点

展开器 `wind_phrase::expand_dict_value(text, input, now, recent, clip) -> DictExpansion{None|Single|Many}`
已存在且干净，无需改动。问题在于调用它的 ~60 行后处理 pass 被**内联**在路径 1，未抽共享件。

引入**两个唯一汇聚点**（均在 `handle_candidate.rs`，`impl Coordinator`）：

### ① 生成侧 `finalize_candidates`

```rust
pub(crate) fn finalize_candidates(
    &self,
    state: &State,          // 只读：取 recent_commits / clipboard 上下文
    raw: Vec<Candidate>,    // 引擎/成员输出的原始候选（已去重/排序）
    input: &str,            // 当前编码缓冲
) -> Vec<Candidate>
```

内部 = 把 `update_candidates` 现有 228–280 行展开 pass **原样搬入**（廉价预检 `contains('$')||'{'`
→ `expand_dict_value` → `None` 保留 / `Single` 替换 display（带 `command_src` 标 `is_command`）/
`Many` 就地炸开）。逻辑零变更，仅「内联 → 共享」。

**约定**：各 `update_*_candidates` 不再直接写 `state.candidates`，而是
`state.candidates = self.finalize_candidates(state, raw, buf)`。次序恒为：
引擎取候选 → 各路径自有的去重/排序 → **finalize 展开** → 写入。

**`$AA`/`$SS` 组的前缀折叠 / 精确展开**（与短语前缀分组一致）：`expand_dict_value` 对数组
返回 `DictExpansion::Group { name, items }`（携组名）。`finalize_candidates` 按**候选自身码
`cand.code` 相对当前输入 `input`** 决定呈现：

- **精确码**（`cand.code == input`，或引擎未给码 `cand.code` 为空）→ 逐成员炸开（← ↑ → ↓）。
- **前缀**（`cand.code` 比 `input` 长，即输入是其真前缀）→ 折叠为**单个组名候选**：
  `is_group = true`、`group_code = cand.code`（完整码）、`text = name`（组名）。

选中折叠组候选时，经 `complete_to_group_code`（正常路径）/ 各 overlay 的「设缓冲=`group_code`
+ 重查」补全输入到完整码 → 此时 `code == input` → 精确展开成员（二级选择）。`cand.code` 由码表
引擎填充（`engine.rs` 内即以 `c.code == input` 判精确），信号现成。

### ② 提交侧 `select_candidate` + `ModeCommit`

```rust
struct ModeCommit {
    source: CommitSource,      // Candidate/SpecialMode/TempPinyin/TempEnglish
    supports_partial: bool,    // 是否有 consumed_length 分段消费语义
    promote_temp_word: bool,   // 是否推进临时词晋升（仅正常路径）
}

pub(crate) fn select_candidate(
    &self, state: &mut State, cand: &Candidate, pos: i32, mode: ModeKind,
) -> KeyAction
```

统一顺序：
1. `is_group`（仅正常路径的短语前缀导航会产生）→ `complete_to_group_code`。
2. `is_command` → `commit_command`（执行动作）。**全量 $CC：所有模式都经此。**
3. 文本提交：`record_selection` + `record_commit(source)`；
   - `supports_partial` 且 `consumed<total` → push seg、推进对应缓冲、按 `ModeKind` 重查、`UpdateComposition`；
   - full → `learn_phrase` →（`promote_temp_word` 时晋升临时词）→ `maybe_convert` → 按 `ModeKind` 退出
     （`reset_pinyin_composition`/`exit_special_mode`/`exit_mix`/`exit_temp_pinyin`/`exit_temp_english`）→ `commit_action`。

**退出/重查按 `ModeKind` 在函数内 `match` 分派，不放闭包进描述符**——闭包借 `self` 会与 `&mut state`
冲突，集中 match 最顺借用检查。

各模式实例：

| ModeKind | source | partial | 晋升 | 退出 |
|---|---|---|---|---|
| Normal | Candidate | ✓ | ✓ | reset_pinyin_composition |
| Special(_) | SpecialMode | ✗（单发） | ✗ | exit_special_mode |
| Mix(_) | 按成员 | 视情 | ✗ | exit_mix |
| TempPinyin | TempPinyin | ✓ | ✗ | exit_temp_pinyin |
| TempEnglish | TempEnglish | ✗ | ✗ | exit_temp_english |

各路径原本散落的 `commit_action(text)` 直接上屏点，统一收口到 `select_candidate`。
`top_commit_command_guard` 已在多路径调用，覆盖顶码 `$CC`，保持不动。

## 范围边界

- ✅ 覆盖：dict value 内 `$CC/$AA/$SS/$Y/{..}` 在全部 5 条路径的展开 + `$CC` 选中/顶码执行。
- ❌ 不含：overlay 路径的**短语前缀枚举**（打 `zz` 列出所有 zz 短语的 `is_group` 二级导航）——独立特性，
  不由 dict value 触发，本次不铺到 overlay（后续可选）。

## 边界情况

1. **数字 lens 的 `$CC`**（混输计算模式）：数字候选无 `$`，预检零开销跳过；文本模式正常展开。
2. **`$CC` 执行线程与 overlay 退出时序**：`commit_command → spawn_command` 独立线程跑动作（不持锁），
   overlay 退出须在 `select_candidate` 返回前于主线程完成（现有 `commit_command` 时序，保持）。
3. **特殊模式多选中入口**（空格选高亮 / 1-9 / 二三候选键 / 标点顶屏）全部收口 `select_candidate`。
4. **回归防线**：路径 1 抽取后行为须逐字节等价，靠既有测试守。

## 测试

- 单元：`finalize_candidates` 对 `$AA→Many`、`$CC→is_command`、`$Y→Single`、普通词→原样各一例。
- 集成：每条 overlay 路径喂含 `$AA` 候选断言炸开；喂 `$CC` 断言 `is_command` 且选中触发 `commit_command`。
- 回归：`wind-coordinator` 全测，确保路径 1 / 临拼 partial 分段 / 特殊模式单发退出不破。

## 实施顺序

1. 抽 `finalize_candidates`，路径 1 改用之（回归基线，行为不变）。
2. 接入 4 条 overlay 生成路径。→ 此步即修复 `$AA` 不展开报告 bug。
3. 抽 `select_candidate` + `ModeCommit`，迁移 4 条 overlay 提交侧（`$CC` 全量执行）。
4. 补测 + 全量 cargo test。
