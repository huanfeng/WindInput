<!-- Parent: ../../AGENTS.md -->
<!-- Updated: 2026-08-15 -->

# wind-aux-code

## Purpose

辅助码过滤：为拼音/双拼候选提供字形层面的二次筛选。用户在拼音码后追加辅助码
（偏旁/部首/笔画对应的按键串），本 crate 据此裁减掉候选字词中字形不匹配者。
- 单字：匹配辅助码前缀（任一码 `starts_with(aux_input)`）则保留。
- 词组：**逐字首码匹配**（固定语义，无模式选项）——顺序输入每字的第一个
  辅助码（第 i 位命中第 i 字任一码的首字符；辅助码**可短于** N 位 = 前缀态，词组保留、
  边打边缩；**超过** N 位或某位不中 → 过滤）。如小鹤双拼「魔法少女」→ `gdxv`
  （广氵小乙，乙=折），打 `gd` 时「魔法少女」仍在候选里。
  字符一律按表查询、**不做纯汉字判断**：表里有码的
  非汉字字符（如「多啦A梦」里 A→a）可参与匹配；表里无码则查表未收录、自然过滤。
- **词组长度上限**（`AuxCodeFilterOptions::max_phrase_len`，默认 0，0 = 不限）：字数 >
  上限的词组一律排除、不参与辅助码筛选——长词组（整词补全/组合词）首字辅助码前缀匹配
  会让它们大量残留、污染逐字词筛选；单字恒参与匹配。

纯逻辑 crate，不依赖 wind-config、不接触文件系统（路径解析职责上提给调用方），
无 Windows 平台依赖，任意主机可编译测试。

## Key Files

| File | Description |
|------|-------------|
| `src/lib.rs` | 模块文档 + 统一导出：`AuxCodeTable`、`AuxCodeLookup`、`AuxCodeFilterOptions`、`aux_code_matches`、`filter_by_aux_code`、`AuxCodeSession`、`load_from_file`、`load_merged`、`read_name` |
| `src/lookup.rs` | `AuxCodeLookup` trait：筛选取码的唯一接口（`any_code` 按优先级回调、`is_empty` 决定 passthrough，前缀匹配为默认方法）。`AuxCodeTable` 实现它；协调器的方案来源运行时（`aux_code_source.rs`，文件表 + 方案视图分层）也实现它 |
| `src/table.rs` | `AuxCodeTable`：三段式紧凑布局（`entries` + `code_ends` + `arena`，与 `wind-reverse::PinyinTable` 同构）；`from_rows` 单表构建、`merge`/`append` 多表坍缩、查询（`any_code_starts_with`/`any_code_starts_with_char`）、状态（`is_empty`/`char_count`）。`codes_of`/`first_code`/`code_count` 仅测试用（`#[cfg(test)]`）。**纯内存，不碰 `std::path`/文件** |
| `src/loader.rs` | `parse_str`（`pub(crate)`）/ `load_from_file` / `load_merged`：txt 格式 → 表。**`=` 分隔**（UTF-8，每行一条 `字=码`，同字多码分列多行，与 rime-lua-aux-code `aux_code` 目录一致；`=` 右侧也可按空白写多个码 `七=a p`（手心表）；非 UTF-8 告警「请转存为 UTF-8」并产出空表）；处理 BOM/注释/空行/非单字行；**只从第 1 行提取方法名**（`# name: 笔画` / `#name: 笔画`），`load_from_file` 空名回落文件主干名；`load_merged` = `merge(paths.iter().map(load_from_file))`（协调器合并相邻文件来源的路径）；`read_name` 只读首行取名（设置页列可选码表用，不整张读入） |
| `src/filter.rs` | `aux_code_matches`（单候选谓词，供 `CandidateStore::set_filter` 等组合使用）+ `filter_by_aux_code`（批量筛选，输出 `FilterOutcome`）。逐字首码匹配**零分配**（字符迭代器，勿退化成 `Vec<char>`/前缀串） |
| `src/direct.rs` | **直接辅助码**（双拼，不按引导键、末 1～2 位自动当辅码）的纯函数：`split_direct`（奇偶切分）、`is_direct_source`（前缀候选资格）、`direct_matches` + `DirectPhraseRule::Any`（单字按码开头；词组 1 位任一字、2 位「任一字码以 ab 开头或 i<j 分别以 a、b 开头」）、`direct_placement` + `merge_direct_hits`（并入排序；常量 `ODD_KEEP_TOP_MIN_SYLLABLES` = 3、`EVEN_KEEP_FULL_COVER` = 2）。它是「提」不是「筛」，不受 `filter_by_aux_code` 的子序列不变量约束；协调器侧在 `handle_direct_aux.rs` |
| `src/session.rs` | `AuxCodeSession`：辅助码**筛选会话状态机**——内部持 `CandidateStore`（原始候选快照 + 筛选视图）+ 辅助码缓冲；`apply`（通过 `CandidateStore::set_filter` 从快照重筛，**只返回命中者**）、`restore_original`（通过 `CandidateStore::clear_filter` 还原）。**不含显示态**：组合区（preedit）拼接/光标是协调器职责，分隔符前缀在协调器进入时拼一次。协调器 `State.aux_code` 只持它（含显示态一并打包在 `AuxCodeOverlay`），按键路由/UI 更新留在协调器 |

## For AI Agents

### Working In This Directory

- **核心不变量（不可打破）**：`filter_by_aux_code` 输出的 `kept` 必须是输入候选的
  **子序列**（不凭空加/丢后重排），被保留候选的相对顺序与原列表完全一致（不额外按权重重排）。
  守护测试 `filter::tests::kept_preserves_original_relative_order`。
  ⚠️ 主排序首要键是消费长度（`by_consumed`，librime 对齐），会让低词频长子短语排在短单字
  前（如 `没时间` 池的 `没试` w=30 在 `没` w=60230 前）——**这是主排序的有意行为**，
  本 crate 不纠正、也不在辅助码侧叠按词频的排序。
- **直接辅助码与引导键模式的防御语义相反**：`direct_matches` 空辅码 / 空表 / 超过 2 位一律
  **不命中**——命中的后果是被提前，防御态应当什么都不做；而引导键筛选的防御态是 passthrough。
  **2 位命中集 ⊆ 其首字母 1 位命中集**（守护测试 `two_letter_hits_are_subset_of_first_letter_hits`）：
  协调器据此按首字母给引擎下推准入，改匹配规则时这条必须仍成立。
- **空输入 / 空表 = 不过滤（passthrough）**：`aux_input` 为空或表未挂载时原样放行
  全部候选。这是防御语义（辅助码模式由触发键进入，正常不会空手筛选），**禁止**改回
  旧行为「全部滤掉」——那会把候选窗整个滤光。
- **词组固定逐字首码匹配**：**无模式选项**（`PhraseFilterMode` 已移除——只保留
  `PerCharPrefix` 语义，配置项 `[engine.aux_code].phrase_mode` 一并删除，勿重新引入）。
  逐字匹配走 `table.any_code_starts_with_char`（**零分配**，别退化成逐字构造 String 前缀）。
  **前缀态语义**：辅助码短于词字数 = 前缀匹配（输入尚未打满，
  词组保留、边打边缩，如「时间」打 `o` 保留因为 时=oc 以 o 开头）；只有**超过**词字数
  （字全部对齐后仍有剩余）或某位不中才过滤——多出的位没有字可对应，否则 2 字词会被
  3 位辅助码静默保留、挤掉真正匹配的 3 字词。
  单字与词组匹配**都不做字集判断**：字符一律按表查询，非汉字有码则参与匹配
  （如「多啦A梦」A→a、单字 `A`→`a` 同样命中）、无码则自然过滤。字集（简繁/字符集等）
  由输入法自身的相关选项在上游决定，此处不拦截。
- **多表挂载一律 `merge`/`append` 坍缩成单表**：不要引入「多表 Vec + flat_map/HashSet」
  的查询路径。`merge` 迭代序 = 挂载优先级（先出现 = 高优），跨表 first-seen 去重，
  同字异码并存、同码只留高优首次出现。查询阶段零额外开销。
  **方案来源（`schema:<id>`）例外**：它的码随用户层（造词）变化，只能查询时取视图，
  不能预先坍缩；协调器的 `aux_code_source.rs` 按层顺序查询，但**相邻文件来源仍 `merge`
  成一层**，层数只随方案来源数增长。筛选经 `AuxCodeLookup`（`lookup.rs`）取码，不假定
  背后是一张静态表。
- **数据文件格式与 rime-lua-aux-code `aux_code` 目录一致用 `=` 分隔**（`字=码`，一行
   一条，UTF-8）：新增解析逻辑放 `loader.rs`，
   保持 `parse_str` 纯函数（可测）+ `load_from_file` 薄封装
   （读文件 + 告警 + 空表）。**路径由调用方经覆盖解析函数定位**（用户目录同名文件
   优先），本 crate 禁止 `data_dir.join(...)`。
- **⛔ 码表文件不入版本库**：`data/schemas/aux_code/` 已在 `.gitignore`，内容是
   `wind-tools/gen_aux_code` 的构建产物（`.cache/aux-code/` ← 上游下载）。
   笔画表上游 rime-stroke 是 **LGPL-3.0**，与本仓 GPL-3.0 不同——按 `NOTICE.md` 的既定政策
   （同 GPL-3.0 的 rime-frost）只下载不入库。要改码表内容就改 `gen_aux_code`，
   **别把生成物手工编辑后提交**：PR #68 最初那版 `stroke.txt` 正是手工产物，
   其裁剪字集比对 hanzi-chars 全部 81 张表也复原不出来，上游一更新就再没人能重做。
   同一次解析还产出「笔画」码表方案的词库 `schemas/stroke/stroke.dict.yaml`（`--schema-out`），
   全拼出厂引用的是这个方案（`schema:stroke`）而非 `stroke.txt`；`stroke.txt` 照旧生成，
   供 override 里写死旧路径的用户。两份的字与码必须逐一相同，别只改其中一条输出路径。
- **例外：`shouyou*.txt`（首右形码）随仓库入库**（作者许可，见 `NOTICE.md`），`.gitignore` 对
   `data/schemas/aux_code/` 用 `*` + `!shouyou*.txt` 放行；`gen_aux_code` 不碰它们。
- **名称只从第 1 行解析**（`# name: 笔画` / `#name: 笔画`）：`parse_str` 填
   `AuxCodeTable.name`，空则 `load_from_file` 回落文件主干名；`merge` 取首个非空
   （先出现 = 高优）。version/source 一律当注释，不解析。
- **懒加载由调用方（协调器）触发**：本 crate 不持加载状态/锁/路径。协调器
  `ensure_aux_code_runtime`（`aux_code_source.rs`）首次进入时对每段相邻文件来源调
  `load_merged(paths)`（内部 = `merge` `load_from_file`，先出现 = 高优），空表经
  `AuxCodeLookup::is_empty()` 走 passthrough。
- **会话筛选状态聚合在本 crate**：`AuxCodeSession`（`session.rs`）持有原始候选快照 +
   缓冲，`apply` 通过 `CandidateStore::set_filter` 重筛、`restore_original` 清除筛选视图。
   协调器只做按键路由与 UI 更新，**不要把重筛逻辑搬回 `handle_aux_code.rs`**。组合区
   （preedit）拼接是**协调器**职责（显示态与会话打包在 `AuxCodeOverlay`），会话不含显示态。
- **存储布局**：三段式（entries 按字升序 + code_ends 结束偏移 + arena 拼接区），
  码长任意（笔画/拆字 1~4+ 码混排）。`from_rows` 稳定排序 + 字内 first-seen 去重，
  **别换成 `HashMap<char, Vec<String>>`**——省数倍内存，二分+early-return 已够快。

### Testing Requirements

- 纯逻辑 crate，无 Windows 依赖，任意主机直接 `cargo test -p wind-aux-code`。
- 关键守护测试：`kept_preserves_original_relative_order`（子序列不变量）、
  `empty_aux_input_no_filter` / `empty_store_no_filter`（passthrough 语义）、
  `load_and_merge_multiple_files`（load+merge 组合，即协调器懒加载路径）、
  `session::tests::apply_always_reesifts_from_snapshot`（会话从快照重筛语义）。

## Dependencies

### Internal

- `wind-candidate`（`Candidate` / `CandidateStore` / `FilterOutcome`）

### External

- `tracing`（构建/过滤日志，`debug`/`warn` 级）

## 全局约束

- 空输入/空表 passthrough 语义（见上），勿回退为「全滤」。
- 数据文件格式与 rime-lua-aux-code `aux_code` 目录一致，路径解析职责上提。
- 日志 INFO 级不得含用户输入/候选内容，见根 `AGENTS.md` 日志红线。
- `cargo fmt` 改完必跑。

<!-- MANUAL: 此行以下为人工补充区，重新生成时保留 -->