# 辅助码来源：方案引用 + 码表文件并存

状态：第一期已实施（分支 feat/aux-code-source，见 §8 分期表提交号）。
依赖：`docs/design/text-code-lookup.md`（按词查编码统一入口）。
前置：`docs/design/aux-code-settings-ui.md`（P1 / P3 已实施，本文取代其 P2 的「码表选择」一条）。
后续：直接辅助码另立设计，本文只为它备好数据层（见附录 A）。

## 1. 问题

辅助码的来源现在只有一种：`[engine.aux_code].files`，指向 `schemas/aux_code/` 下的 `字=码` 文本。

1. **不能拿一个输入方案当辅助码。** 「拼音 + 五笔前两码」是常见用法，五笔编码已经在 `wubi86`
   的 rime 词库里，却要另转一份 txt。用户反馈正是这一条。
2. **笔画表多转了一道。** `stroke.txt` 由 `gen_aux_code` 从 rime-stroke 的 `stroke.dict.yaml`
   剥出来；上游本来就是一个 rime 输入方案。
3. **设置端不能选。** 用哪张表只能手改方案文件。
4. **（现存 bug）改了来源不生效。** `schema.saveConfig` 不清辅助码表缓存
   （`refresh_schema_derived_config` 不调 `invalidate_aux_code_table`），要切一次方案才换表。

## 2. 决定

- 同一个键 `files` 列出全部来源，条目两种写法：`schema:<方案 id>` 引用码表方案，其余是码表文件路径。
- txt 保留给专用辅助码表（小鹤、自然码的 2 码首末形码：单独当输入方案无意义，且本身就是
  rime-lua-aux-code 的 txt 格式，零转换引入）。
- 引用方案时，**系统词库与用户词库是一个整体**：用户在被引用方案里造的单字，其编码同样可用于筛选。
  这件事做成通用的「按词查编码」入口（`docs/design/text-code-lookup.md`），辅助码是它的使用方之一。
- 筛选只通过一个「字 → 编码」查询接口取码，不再假定背后是一张静态表。
- 笔画改为码表方案（第 2 期），全拼默认引用它。

## 3. 配置形态

```toml
[engine.aux_code]
files = ["schema:stroke"]                            # 全拼出厂（第 2 期后）
# files = ["schema:wubi86", "aux_code/flypy_full.txt"] # 多来源：先出现 = 高优
enabled = true          # 不变，仍是总闸
max_phrase_len = 4      # 不变
```

- **一个键、整组替换。** `schema_overrides/{id}.toml` 里写 `files` 即整组替换方案文件的基线
  （`merge_toml` 对数组本来就整体替换）；不写 = 跟随方案。不存在「删了一个键、另一个键的基线漏进来」
  的问题，也不需要哨兵值。
- **键名保留 `files`**，免迁移；代价是名字不再精确，在 `AuxCodeSpec` 文档与文档站写明
  「条目可以是文件，也可以是 `schema:` 方案引用」。
- **解析**：以 `schema:` 开头的是方案引用，其余照旧按路径解析（`resolve_schema_resource`）。
  文件名本身不会以 `schema:` 开头（Windows 路径不允许 `:` 出现在那里），两种写法无歧义。
- **`has_aux_code_table` 判据不变**：`files` 非空。core 与设置端两处都不用改。
- **`files` 非空仍不代表开启**，总闸是 `enabled`（沿用 `AuxCodeSpec` 文档）。

### 3.1 可被引用的方案

- 引擎类型为 `codetable`；拼音、混输、英文不行（它们的「编码」不是字形码）；
- 不能引用自己；
- 不要求在 `schema.available`（切换列表）里，也不看 `hidden`。被引用 ≠ 被启用；
- 编码必须**只含字母**：辅助码模式只收字母键（`handle_aux_code_key`）。设置端不列出不合格的方案；
  手写进配置的，建表时跳过非字母编码并 warn 一次。

## 4. 字 → 编码：统一查询接口

`wind-aux-code` 的筛选（`filter_by_aux_code` / `AuxCodeSession::apply`）今天直接拿 `&AuxCodeTable`。
改为拿一个查询接口：

```rust
pub trait AuxCodeLookup {
    /// 该字的全部辅助码（顺序即优先级；无码返回空迭代器）。
    fn codes_of(&self, ch: char) -> impl Iterator<Item = &str>;
    /// 一个字都没有（未挂载）。筛选据此走「不过滤、原样放行」的防御语义。
    fn is_empty(&self) -> bool;
}
```

筛选今天只用到三件事：`any_code_starts_with`、`any_code_starts_with_char`、`is_empty`
（`filter.rs`）。前两者由 trait 的默认方法从 `codes_of` 推出。`AuxCodeTable` 实现它，行为不变。

运行时的查询对象按 `files` 顺序拼接各来源：

```
AuxCodeSources = [来源 1, 来源 2, …]      // 按 files 顺序
来源 = 文件表(AuxCodeTable)
     | 方案(TextCodeView，取 SYSTEM + USER 层)
codes_of(ch) = 各来源依次给出的码（先出现的在前，去重）
```

方案来源不预先 `merge` 进表：它的用户层会频繁变，合成放在查询时做。候选一页只有几十个字，
成本可忽略。**相邻的文件来源仍 `merge` 成一层**（`wind-aux-code` 的「多表坍缩成单表」规矩照旧），
层数只随方案来源数增长；被方案来源隔开的文件段各自合并，清单顺序即优先级不变。

### 4.1 方案来源 = 「按词查编码」统一入口的一个使用方

方案来源**不自带数据结构**，直接用 `docs/design/text-code-lookup.md` 的 `TextCodeView`：

- 系统层就是该方案的反查索引（含词组，按单字查即可）；保留策略改为「在用集合」后，
  辅助码引用的方案进入在用集合，不会把主码表 / 联想方案的索引挤掉；
- 用户层是该方案的用户词文本索引（收单字、按方案分槽、按方案写代次），过期时本次照用旧的、
  后台重建；
- 取 `SYSTEM | USER`，**不取临时层**：临时词是未确认的自动造词，拿它改变字形筛选结果不合适；
- 用户隐藏（shadow）的系统单字首版不扣除。（已确认）

五笔恰好是主码表时，它的反查索引本来就为悬停编码建好了，辅助码零额外成本。
五笔简码是全码前缀，一并收进来不影响前缀匹配；「简码不是全码前缀」的方案同样成立（按任一码判）。

## 5. 加载与失效

今天的加载点是 `enter_aux_code` → `ensure_aux_code_table`，在按键线程上同步读 txt。方案来源不能照搬。

- **后台预热**：「在用」的方案来源 = 活跃方案 ∪ 临拼目标方案各自 `[engine.aux_code]` 里的
  `schema:` 条目（`EngineManager::aux_code_schemas_in_use`，辅助码关闭时为空；临拼目标只解析 id、
  不加载其引擎）。它既钉进反查索引的在用集合（`reverse_index_pins`），也是预热对象：启动预热
  （`prewarm_indexes`，阻塞在预热线程里）、全局配置重载（`reload_user_config` 的 schema_dirty
  分支同样走 `prewarm_indexes`——在用集合只随 `schema` 段与 `input.temp_pinyin` 变，二者都标脏）、
  切方案（`finish_user_schema_switch`）与 `schema.saveConfig` / `resetConfig`
  （`refresh_schema_derived_config`）后各派一次后台预热（`warm_aux_code_sources` →
  `spawn_index_warm`，自带去重与延迟提示）。用户层按统一入口的规则「用到时检查」。
- **按键线程只读不建**：`enter_aux_code` 发现方案来源的系统层未就绪时不进入、不吞键，
  派 `spawn_index_warm` 后台构建（构建超过延迟阈值才弹「正在建立词库索引…」，建好后重渲染）。
  文件来源仍可同步加载，行为不退化。
- **会话中途索引被清**（改方案设置、词库启用集变更、主码表重载都会整表清反查索引）：
  `refresh_aux_code_candidates` 发现任一方案层系统层未就绪时，本次原样放行（`restore_original`）
  并派后台重建，不拿仅剩的用户层去筛——那会把候选滤到几乎全空。
- **已知限制**：被引用方案的反查索引若被 `build_guard` 判为「反复死在构建中」而永久跳过，
  它永远不会就绪，辅助码也就一直进不去（只有日志里那条 build_guard 的记录）。与悬停编码等其它
  反查索引使用方同一处境，不单独处理。
- **缓存键**：缓存记住「由哪组来源建成」（`files` 原文 + 解析后的路径）。来源不变就不重建，
  来源变了才重建——不再只靠「切方案清空」，§1 第 4 条的 bug 随之消失。
- **被引用方案的词库变了**（改启用词库、`schema.invalidate` / `rebuildCache`）：反查索引已有的
  失效路径（`reverse_index.clear()`）照管，辅助码不另写。

## 6. 设置端

辅助码卡片（`schema_manager.rs` 方案设置）在「本方案启用辅助码」之后加一行 **「辅助码来源」**，
R8.1 勾选行：

- 不勾：只读显示「跟随方案（笔画）」——方案文件声明的来源，多个用「+」连接；
  声明的来源不存在时加「（未安装）」。写回：`files` 写 `null`（删键）。
- 勾上：下拉分两组——「输入方案」（§3.1 的可选方案，显示方案名）、「辅助码表」
  （`schemas/aux_code/*.txt`，显示 `# name:` 头，缺头用文件名）。选中后写
  `files = ["schema:wubi86"]` 或 `files = ["aux_code/flypy_full.txt"]`。
  首版只选一个来源；多来源仍可手写（写在方案覆盖 `schema_overrides/<id>.toml` 里），
  设置端遇到多条（或单条但不在下拉里）时只读显示「手写的多个来源：…」，悬停提示
  「取消勾选即恢复跟随方案」——取消勾选保存即写 `null`；重新勾上仍回只读、不改写。
- 用勾选行而不是 R8.2 下拉：选项会超过 6 项；也与卡片上另两行形态一致。
- 「全部恢复跟随」一并清这一行；卡片摘要「已自定义 N 项」计入它。

新 RPC **`schema.auxCodeSources`**（主仓 wind-webdata）：

```json
{
  "schemas": [{ "id": "wubi86", "name": "五笔" }, { "id": "stroke", "name": "笔画" }],
  "files":   [{ "path": "aux_code/flypy_full.txt", "label": "小鹤" }]
}
```

`files` 用 `list_schema_resource_dir(data_dir, "aux_code", ".txt")`，与注释词库来源
（`web_comment_sources`）同一做法；`schemas` 取全部已安装方案（含 hidden、不看 available）中
符合 §3.1 的，排除参数 `id` 指定的请求方自己。设置端 mock（`rpc.rs`）同步补上。

## 7. 笔画改为码表方案（第 2 期）

- `gen_aux_code --schema-out <schemas>` 从 `.cache/aux-code/` 的 rime-stroke 原档另外产出
  `schemas/stroke/stroke.dict.yaml`，与 `stroke.txt` 出自同一次解析（字与码序逐一相同）。
  **字集裁剪规则不变**（GB 18030-2000 ∪ 通用规范汉字表 ∪ 〇；上游全量约 11 万字，对默认关闭的
  功能过重。字集必须由脚本定义、可复现：最初入库的手工裁剪版 14738 字无法从 hanzi-chars 的任何
  字表或其并集逆向复原，现行字集是它的超集）。许可同今天：词库构建期下载、
  不入库（NOTICE.md 已更新措辞）。方案文件 `data/schemas/stroke.schema.toml`（`type = "codetable"`、
  名称「笔画」）不含上游数据，**入库**，不由工具生成。
- **权重**：上游不带权重，全 0 时既排不出序、wdat 前缀检索也无从剪枝（打 `h` 走完整棵子树）。
  取 rime-frost 单字表 `8105` / `41448` 的字频（同字多音取最大），按对数压进约定值域
  `0~10000`——原始值最大 1537 万，直接写入每次加载都会触发越界告警。无字频的字为 0。
- 方案段两项结构参数：`max_code_length = 64`（笔画码 1~52 码不定长，取大于最长码的值，
  顶码 / 满码上屏 / 满码空码清空都不会触发）；`z_key_repeat = false`（z 是「折」，全局出厂的
  z 键重复上屏会占掉它的首选）。
- 不进出厂 `schema.available`：方案管理里能看到、能启用，默认不在切换列表。
- 全拼 `pinyin.schema.toml` 改为 `files = ["schema:stroke"]`。
- **兼容**：用户 override 里已写的 `files = ["aux_code/stroke.txt"]` 不能失效——继续生成
  `stroke.txt`，不做迁移。

## 8. 分期

| 期 | 内容 | 仓 | 提交（分支 feat/aux-code-source） |
|---|---|---|---|
| 0 | 前置：「按词查编码」统一入口第一期（`text-code-lookup.md`） | 主仓 | `8b99d7a7`、`d5fa0499`、`d000b8e6` |
| 1 | `AuxCodeLookup` 接口与来源拼接；`schema:` 条目解析；方案来源接统一入口；后台预热与缓存键（修 saveConfig 不失效）；`schema.auxCodeSources` | 主仓 | `9ab878f4`、`31cd04c8`、`09e1459d`、`51595c94`；终审修复 `12406518`、`d8a8b03d`（预热落地、会话中失效放行、文件来源合层） |
| 2 | 笔画码表方案、全拼默认改引用、NOTICE | 主仓（工具 + 数据） | `ad350315`、`2a9193ba`、`951a6b5c`（直接在 main） |
| 3 | 「辅助码来源」勾选行、mock、渲染与写回测试 | wind-setting + 主仓 | wind-setting `26a2c60`、`b7cd3bc`、`c2c4198`；主仓 `ec3e1c8b` |
| 4 | 文档：`[engine.aux_code].files` 的 `schema:` 写法；「用五笔 / 笔画方案作拼音辅助码」 | 文档站 | WindInputDocs `b9f066d`（另补笔画码取码规则） |

触发键与分隔符冲突警示（原 P2 第 6 条）不在本文范围。

## 9. 验收

- 全拼 `files = ["schema:wubi86"]`：`gong` + 触发键 + `a`，「工」「攻」保留、「公」滤掉。
- 用户层：在五笔里给某个系统没有的码造一个单字，下一次进入辅助码即可用该码筛到它
  （用户层本身的验收在统一入口设计里）。
- 引用不存在的方案：不崩、warn 一次、不进入；系统层未就绪：不进入且不吞键。
- `schema.saveConfig` 改来源后，下一次进入即用新来源（先写这条，确认它在修复前失败）。
- 设置端：勾选 / 取消 / 在方案与码表间切换三种写回；跟随档显示方案基线与「（未安装）」；
  「全部恢复跟随」清这一行。
- 第 2 期：`stroke` 方案可单独启用打字；全拼默认辅助码行为与今天逐字一致。

---

## 附录 A：直接辅助码调研（下一份设计的输入）

**来源**：[GH#129](https://github.com/huanfeng/WindInput/issues/129)（wmjordan，含取码规则、用例、
手心输入法选项截图）、论坛 [t181](https://forum.windinput.com/topic/181)（双拼下要「不依赖引导键的直接辅助码」，
点名参考万象）。本仓尚未实现。

**诉求**：不按引导键，末尾字母自动判为辅助码。双拼 `uidu` + `c` = `uiduc` → 「释读」。
判不准时，筛过的字在前、未打完的词在后。

**万象的两种实现**（读 `wanxiang_algebra.yaml` 的「直接辅助」段与 `lua/wanxiang/super_lookup.lua`）：

| | A. 编码进词库（Pro 版） | B. 候选后处理（基础版 `wanxiang_lookup/enable_direct`） |
|---|---|---|
| 数据 | 词库每个音节带辅码：`双拼[声调];辅码1辅码2,第二组…` | 独立的反查库 |
| 机制 | 拼写运算为每个音节派生「双拼 + 1/2 位辅码」拼写，**完全走正常输入流程** | 输入 X 首选为两字词时缓存候选；输入变成 X + 1～2 个字母时，查缓存候选的辅码，命中者插到最前，其余照常 |
| 覆盖 | 词组任意音节、整句里都能带辅码 | 仅两字词：任一字单辅，或两字各一码 |
| 代价 | 辅码与词库绑死；切分组合暴涨 | 覆盖窄，要维护缓存 |

**双拼是主战场**：每个音节固定两键，奇数长度时末位基本确定是辅码；全拼 `lim` 既可能是 `li`+`m`，
也可能是「厘米」的简拼，歧义大得多。自然码、手心、万象都是双拼形态。

**映射到本仓的倾向**：B 思路、比万象宽——协调器候选阶段把输入拆成「前缀 + 末尾 1～2 字母」，
前缀候选（取上一次按键的候选，免重算）经辅码筛选后插到最前，其余去重接后。插入点必须在
协调器按消费长度重排**之后**。首版只做双拼。A（辅码并进音节切分）能力最强，但要改拼音引擎切分核心。

**待定规则**：词组里辅码匹配哪个字——自然码「任意字」、手心「任意字 / 首字 / 末字」可选、
万象「两字任一单辅或各一码」；本仓引导键模式是「逐字首码」。两种模式可各有配置。

**与本文的关系**：直接辅助码只是一个新的触发方式，取码走 §4 的 `AuxCodeLookup`，用户层（统一入口）
的「用到时检查、后台重建」对逐键检查同样成立。
