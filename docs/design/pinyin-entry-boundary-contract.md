# 拼音词条入库契约：合法性与音节边界

> 状态：**设计已定稿，未实施**。
> 起因 = 用户导入不带空格分隔的拼音词库后出现逻辑错误。
> 本文是 `pinyin-code-domains.md` §2.2「存储主键保持 flat」的补充：那里定的是 code 的
> **形态**（key 恒扁平、边界另存 bitmask），本文定的是入库前的**准入条件**。
> 边界的消费侧见 `pinyin-boundary-aware-lattice.md`，简拼索引见 `user-dict-abbrev-index.md`。

## 1. 病灶：三个被混为一谈的问题

用户的描述是「存储层要求拼音必须带空格，导入没做要求」。这个描述不准确，而不准确
正是它难修的原因——**存储层要的从来不是空格**。`user_words.rs` 的 key 恒是扁平的
（`enc_key(schema, "nihao", text)`），带空格反而写坏 key。空格只是**音节边界的传输载体**。

| 域 | 形态 | 闸口 |
|---|---|---|
| 交换域（文件 / RPC / 设置页展示） | `ni hao` | `wdict::join_code_by_boundary` |
| 存储域（redb） | key=`nihao` + `boundary=0b101` | `wdict::split_spaced_code` |
| 查询域（引擎） | 扁平、小写 ASCII、可切分 | — |

所以导入无空格词库**不会让 key 错，全拼输入照常可用**。坏掉的是另外三件事，它们的
失效机制完全不同，混在一起看就会开错药方：

### 1.1 边界缺失（`boundary = 0`）—— 静默降级

`user_words.rs:322 import_user_words` 的规则是「有空格就拆、没空格就 0」。代价：

| # | 落点 | 后果 | 性质 |
|---|---|---|---|
| 1 | `pinyin/mixed_abbrev.rs` `syllables_from_boundary` | `boundary==0` 返回 `None`，**不参与**混合简拼（`nhao`） | **硬失效** |
| 2 | `pinyin/mod.rs:886 abbrev_of_code` | 退回 DAG 猜切分 | 歧义码上必错 |
| 3 | `pinyin/mod.rs:1654 boundary_compatible` | `cand_boundary == 0` 即放行 | 双拼下**不设防** |
| 4 | `pinyin/mod.rs:1403` | `word_syls == 0` → `continue`，长词上浮跳过 | 长词排到 300 名外＝打不出来 |
| 5 | `pinyin/mod.rs:526 should_promote_user_completion` | 同上 | 同上 |
| 6 | `pinyin/lattice.rs:356` `mask_path` | 返回 `MaskCheck::NoInfo` → 降级放行 | 整句不设防 |
| 7 | `wind-store/src/abbrev_index.rs:67 group_of` | 落进 `\u{1}{首字母}` 兜底组 | **规模随导入量增长的线性扫描** |
| 8 | 码表自动造词沿用 0 | 污染扩散 | — |

第 7 条尤其值得注意：`abbrev_index` 这个模块存在的全部理由就是根除「逐键路径上的
无界扫描」（19 万词实测 172ms/次）。批量导入无边界词，等于把那个 bug 按比例喂回兜底组。

### 1.2 码字符集不合法 —— 词条彻底打不出来

`import_formats.rs:210 is_valid_code` 的全部把关只有一句：

```rust
fn is_valid_code(code: &str) -> bool {
    code.chars().all(|c| ('\x20'..='\x7e').contains(&c))  // 可打印 ASCII
}
```

放行并落库的脏码：

| 脏码 | 来源 | 后果 |
|---|---|---|
| `ni'hao` | Rime 的另一种音节分隔写法 | flat 域不变量是「剥除 `'` 后的串」，key 留着 `'` ⇒ **查询恒不命中**。全仓无一处处理 `'`（已 grep 确认） |
| `NiHao` | 部分第三方词库 | 查询侧全链路 `is_ascii_lowercase`（`lattice.rs:461/469/543`、`mixed_abbrev.rs:111`、`scorer.rs:31`），而 `search_user_words_prefix` 是**裸字节前缀匹配、无大小写归一** |
| `ni3hao3` | 带声调数字 | 同上 |
| `wgkq` | 五笔词库导进拼音方案 | 跨引擎校验（`webdata/lib.rs:632`）**只对 WindDict 格式生效**，Rime/TSV 路径注释写着「不拦截」 |

★ **判据建错了一侧。** `is_valid_code` 问的是「这串是不是可打印」（几乎无约束），
而拼音码的形态约束强得多。这与 `.dict.yaml` 列序那次 bug（见
`project_dict_column_layout_fix`）是同一个教训：**二选一时判据要建在约束更强的那一侧**。

### 1.3 同一份数据，两条路结果不同

单条加词（`webdata/lib.rs:536 normalize_add_code`）对无空格码有兜底：推导码 == 手输码
则借用推导边界（`:553 infer_boundary_for`）。**批量导入完全没有这一层**。同一条
`nihao / 你好`，设置页手动加＝有边界，走导入＝没边界。这是纯粹的路径不一致。

## 2. 契约

### 2.1 合法性判据：两条

```
拼音用户词条合法 ⟺
  ① text 每个字符都有读音（等价于：纯汉字）
  ② code 能切分成恰好 text.chars().count() 个合法音节
```

★ **② 蕴含了「码是小写 a-z」「码是拼音而非五笔码」「每个音节合法」**——非 a-z 字符
在 `SyllableTrie` 里根本没有边，求解直接无解。故不需要单独保留 `is_valid_code` 那种
弱判据，用 ② 替换它即可，代码量更少而约束更强。

实现上（`boundary_by_char_count`）① 只作标记（`no_reading`）、不拒收：`zuo ←`、`dengyu ＝`
这类拼音码 → 符号候选完全合法，符号没有单字读音，但边界由 ② 已能确定。拦截「码表词库误导入
拼音方案」靠的全是 ②（`wgkq`、`aaaa` 都切不出与字数相符的音节）。无读音时读音验证自然不计分，
落入「切分合法即采信、多解算歧义」的降级分支。

① 与加词路径的**现有守卫一致**（拼音方案下含非汉字的词拒绝取码，见
`handle_addword.rs:1357` 注释）。这不是新规矩，是把已有规矩推广到导入闸口。

**不合法的词条不进拼音词库**，改用短语（`(code, text)` 主键、code 任意 ASCII、
text 任意文本）。理由：拼音的处理流程（DAG 切分 / 简拼索引 / 双拼校验 / 整句 lattice）
对输入形态有强假设，非法词条会在这些环节产生干扰，而干扰是静默的。

⚠️ 提示文案必须讲明短语**不是等价替换**：短语触发靠打完整 code，不参与候选竞争、
前缀补全与整句；且存 `user_data.db`，与词库是两套导入入口。

★ 逃生口在 main 上已经具体化：`wind:p1` 短语文本导入（`web_phrase_import_text`）支持整段
粘贴分发，所以「改用短语」不再是一句无处落地的建议，用户有可操作的批量入口。
⚠️ 它写的是短语库（`import_phrases_appending`），**不经过本契约的闸口**——这是正确的：
短语 code 是任意 ASCII、本就无音节语义，套上边界不变量反而会把逃生口一起堵死。

### 2.2 边界不变量与作用域

```
不变量：data_schema_id == "pinyin" 的用户词 / 临时词记录，boundary != 0
```

⚠️ **作用域必须限定在拼音族**。码表 / 五笔的 `boundary = 0` 是**正确语义**
（码表词组码无音节概念，`handle_addword.rs:300` 那处硬编码 0 是对的），一刀切会把
它们全判成违规。

## 3. 边界求解链

### 3.1 有 code：四层，全部可判定

按可信度取，**任何一层命中即停**：

| 层 | 手段 | 落点 | 性质 |
|---|---|---|---|
| 1 | 文件自带空格 / `'` | `split_spaced_code` | 作者标注的真值 |
| 2 | 系统词典按 `(code, text)` 点查 | `manager.rs:2469 syllable_boundary_of` | 词典真值 |
| 3 | 推导码 == 导入码则借用 | `manager.rs:2504 generate_words_pinyin` | 与 `infer_boundary_for` 同款判据 |
| 4 | **字数约束 DAG 求解**（新增） | 见 §4 | 解方程，非猜测 |
| — | 四层皆无解 | — | **该行不合法，拒收** |

第 4 层是让「程序补充」变得可靠的关键。**它不是猜**：

★★ **`maximum_match` 之所以是猜，是因为它只看 code 不看 text。** 导入时手上有
`(code, text)` 一对，而汉字词的**音节数恒等于汉字数**。加上这条约束，切分从
「挑一条最像的路径」变成「解一个方程」：

| code | text | 恰好 N 音节的路径 | 结果 |
|---|---|---|---|
| `xian` | 西安(2) | `xi\|an` | 唯一 |
| `xian` | 先(1) | `xian` | 唯一 |
| `xianning` | 西安宁(3) | `xi\|an\|ning` | 唯一——`xian\|ning` 只有 2 音节，被约束排除 |
| `nanan` | 南安(2) | `nan\|an`、`na\|nan` | 2 解 → 逐字读音消歧 |
| `chongqing` | 重庆(2) | `chong\|qing` | 唯一 |
| `wgkq` | 工(1) | 无 | **无解 → 拒收** |

多解时的消歧同样是确定的，不是打分：用 `generate.rs:28 CharPinyinIndex` 的
`readings(char)` 逐位验证（第 i 个音节 ∈ 第 i 个字的读音集）。`readings('南')` 不含
`na` ⇒ `na|nan` 直接出局。若验证后仍多解，取读音权重和最高者（`readings` 已按权重降序）。

### 3.1.1 ⚠️ 实现层序是 **2 → 4 → 3**，与上表的可信度排序不同

上表按**可信度**排列，实现按**成本**排列。`pinyin/mod.rs` 的 `resolve_boundary` 实际是：

```
层 2（词典点查，µs 级）→ 层 4（短码上建图，便宜）→ 仅当层 4 多解时才跑层 3
```

理由：层 3 的 `generate_word_pinyin` 要枚举读音笛卡尔积（`MAX_READING_COMBOS = 64`）
再回查词典，是三者里最贵的；而层 4 唯一解时同样确定。**绝大多数词条因此根本不触发层 3。**

★ 精度没有损失，有两条支撑：

1. **层 4 唯一解 ⇒ 无需层 3。** 约束已把答案锁死。
2. **层 4 无解 ⇒ 层 3 必然也命中不了**（可证，非偷懒）：若推导码 flat 后等于 `code`，
   其音节序列本身就是一条「音节数 == 字数」的合法路径，与层 4 无解矛盾。

⇒ 层 3 唯一还有价值的场合正是**层 4 多解**，实现也正是只在那时调用它。
实测确认它确实在起作用：`angan` +「安甘」两条路径都通过读音验证，层 4 只能报多解，
层 3 兜底出 `an gan`（flat 后恰等于 code）把结果拉回 `Derived`
（测试 `layer_three_rescues_layer_four_ambiguity`）。

### 3.2 无 code（纯词表）：**已合规，本次不动**

★★ 这条路径的编排整个在 **wind-setting 仓**（`src/pages/dict/state.rs` 的
`import_word_list` / `encode_word_batches`），主仓只提供 `dict.encodeWords` 一个 RPC。
其设计注释写明了这个取舍：

> 复用 TSV 而不新开一条导入协议，是因为出好码之后这就是标准的 `code<TAB>text<TAB>weight`
> ——那条路的解析、去重、merge/replace 语义全部现成，core 侧因此只需新增一个批量出码
> RPC，`wind-store` 一行都不用动。

**已验证的完整链路**（每一跳都保住了空格）：

```
dict.encodeWords → encode_texts（webdata/lib.rs:1333）
                     拼音 → generate_words_pinyin（词级消歧，多音字按词典权重）
                          → 无果回退 reverse.gen_pinyin（逐字取第一个读音）
                     产出**带空格**的音节码
  → wordlist.rs:115 to_tsv    原样 push code，不动空格
  → dict.import → parse_words_tsv → normalize_code 折叠为单空格
  → split_spaced_code 拆出 flat key + boundary   ✓ 不变量满足
```

★ 而且 `import_word_list` 丢弃「出不了码」的词（`unencodable` 计数），这**恰好就是
判据 ①**——text 含无读音字符 ⇒ `generate_word_pinyin` 返回 `None` ⇒ 空码 ⇒ 被丢弃。
判据 ② 对这条路不适用：码由引擎生成，必然可切分。

⇒ **两条判据都已隐式满足，本次改动不需要碰这条路径。**

⚠️ 遗留问题（已知，不属本文范围）：`gen_pinyin` 回退时多音字读音可能错，但**边界是对的**
（每字一音节）。所以它满足不变量，坏的是码本身。要修得从 §3.3 的方向想办法——
而纯词表恰恰没有 code 可依。

### 3.3 两条路互斥，不是主备

★★ **有没有 code 决定了多音字是不是问题：**

| 输入形态 | 读音的权威来源 | 多音字 |
|---|---|---|
| 带 code 的词库（`chongqing / 重庆`） | **词库作者写的 code** | **不存在**——作者已经把 `chong` 写在那了 |
| 纯词表（只有 `重庆`） | `encode_texts` 推导 | 真问题，只能按权重猜 |

⚠️ **有 code 时绝不调用 `encode_texts`**。用推导码覆盖作者的 code = 用更差的信息源
覆盖更好的。`gen_pinyin` 的缺陷是确证的：`wind-reverse/src/lib.rs:1042`，其测试名就叫
`test_gen_pinyin_uses_first_reading`——「重要」侥幸对，「重庆」必错。
`generate_word_pinyin` 的层 3 兜底同样是 `index.representative(r)`（权重最高读音），
新词走不到层 1 的词典验证时也会错。

两条路共用 `CharPinyinIndex` 但方向相反：纯词表是「字 → 码」（多音字必须猜），
带码词库是「码 → 切分」（读音已定，只需切分，而切分被字数约束锁死）。
**恰恰是最难的多音字，在有 code 的那条路上根本不构成问题。**

### 3.4 为什么不用 `maximum_match` 兜底

两个独立理由，任一成立即足够：

1. **不增加任何正确性。** `boundary == 0` 时消费端现场跑的就是同一个 `maximum_match`
   （`mod.rs:886` 的 `boundary == 0` 分支）。落库只是把同一次猜测提前，却丢掉了
   「这是猜的」这一位信息。
2. **会把「不设防」变成「误杀」。** §1.1 的 3/6 两条是靠 `boundary == 0` 豁免的。
   填一个错值，双拼下这批词会**一条都出不来**——`sp_mask` 是真精确的，假真值与它
   不符即拒。这正是 P2b 踩过的坑（wdat v5 让简拼白拿真值，豁免随之失效）。

★ 沉淀判据：**给一个原本恒为 0 的字段填上真值前，先查「谁在依赖它是 0」。**
在这套设计里，**「不知道」和「猜了一个」是两种不同的状态，不能合并。**

## 4. 新增能力

### 4.1 `SegGraph::paths_with_edges(p, q, n, limit)`

✅ **已实现**。落在 `SegGraph` 而非 `Dag` 上——同族的 `mask_path`（验证一条给定路径）
与 `any_path`（取边数最少的一条）都在那里，缺的正是「枚举恰好 n 条边的路径」。

复杂度：受 n 与每位置分支数约束，词条码通常 ≤ 12 字节。需设结果上限，超限即视为多解
交给读音验证。

### 4.2 `Engine::resolve_boundary(code, text) -> BoundaryResolution`

```rust
enum BoundaryResolution {
    Exact(u64),        // 层 1/2：词典真值
    Derived(u64),      // 层 3/4 唯一解
    Ambiguous(u64),    // 层 4 多解，已按读音权重择一
    NoInfo,            // 合法但边界无法表达 ⇒ boundary=0，**不拒收**
    Unresolvable,      // 不合法 ⇒ 拒收
}
```

返回枚举而非 `Option<u64>`，因为预览要按类型分别计数（§5）。

⚠️ **不要改 `syllable_boundary_of`**。它的契约是「点查取真值，不做推断」，
`wind-engine/tests/pinyin_abbrev_index.rs:111` 有专门的测试防止它被当成重复实现合并掉
（测试注释原文：「这是它与 `generate_word_pinyin` 的分水岭」）。新方法语义是「求解」，
两者必须并存。

★ **实现落点：`pinyin/generate.rs`**，与 `generate_word_pinyin` 同模块。
理由是硬约束——`CharPinyinIndex` 虽是 `pub struct`，但 `representative`（`:69`）与
`readings`（`:73`）都是**私有方法**，模块外拿不到。⚠️ 撞到这堵墙时不要顺手把它们改
`pub`：同模块放置既绕开可见性问题，又让 §3.3 的「两个方向共用同一份真值表」在代码上
相邻可见，注释能互相指认。

### 4.3 导入侧归一化策略

`import_formats.rs:205 normalize_code` 现在只折叠空白。需要：

- **`'` → 空格**。这不是清洗，是**信息升级**：Rime 的 `ni'hao` 里那个撇号和空格一样是
  作者标注的音节真值，转成空格后 `split_spaced_code` 就能吃到边界。既修 §1.2 又解 §1.1。
- **大写 → 小写**（仅拼音目标）。
- `is_valid_code` 由 §2.1 的判据 ② 取代。

⚠️ **落点约束**：`import_formats` 在 wind-store 层，按设计拿不到 `engine_mgr`
（落库规则须两类方案通用）。故不要把引擎判断塞进去，而是给 `parse_words_auto` 加一个
显式策略参数（如 `CodePolicy { lowercase, syllable_separators }`），由 webdata 按目标
引擎填。store 层仍然只认识「策略」，不认识「引擎」。

同理，§3 的求解链落在 **webdata**（`web_dict_import` / `web_dict_preview_import`），
不在 store —— 求解需要 `engine_mgr`。

### 4.4 ★★ 求解结果不能用 code 当载体传递（单音节陷阱）

第一版接线把求解出的边界写回 `code`（`join_code_by_boundary(flat, b)`），让落库端照常
`split_spaced_code` 拆出来。**这对单音节词是错的**：

```
join_code_by_boundary("xian", 0b1) == "xian"   // 单音节没有内部空格可插
split_spaced_code("xian")          == ("xian", 0)   // 读回来是 0
```

既有测试 `wdict.rs` 里就记录着这条行为（「单音节边界不经文本往返」）。后果不是报错而是
**静默退化**：单字词的边界补齐失效，而 `filled` 计数照样 +1、预览照样告诉用户「已补上」。

⇒ `WordIo` 新增 `boundary: Option<u64>` 字段，求解结果走这条旁路，`None` 时才回退到
code 里的空格（旧路径逐字节不变）。`import_user_words` 与 `preview_import_user_words`
两侧都要读，判据必须一致，否则预览的 `willUpdate` 会与实际落库不符。

★ 一般化：**空格作为边界载体是有损的**——它能表达「音节之间的缝」，表达不了「整串
就是一个音节」。凡是把边界经文本往返的路径都有这个洞（导出→导入亦然，那是既有缺陷）。

## 5. 导入闸口：三档处置

| 档 | 判据 | 处置 |
|---|---|---|
| **合法** | 自带空格，或求解出唯一解 | 直接导入 |
| **可补** | 缺空格但求解有解 | 提示后由用户二选一 |
| **非法** | `Unresolvable` / text 含非汉字 | **一律不导入**，列样例 |

### 5.0 实测：求解率与正确率（2026-08-22）

夹具 `wind-engine/tests/pinyin_boundary_solve_rate.rs`（`#[ignore]` eval）。
样本 = 出厂合并词典 `rime_frost.merged.wdat` 里**全部**多字词条，把码拍平模拟「用户导入
无空格词表」，**绕开层 2** 只跑层 4 → 层 3：

| 结果 | 条数 | 占比 |
|---|---|---|
| 层 4 唯一解 | 602 048 | 100.00% |
| 层 3 救回 | 0 | 0% |
| 仍多解 Ambiguous | 6 | ~0% |
| 无解 Unresolvable | 7 | ~0% |
| **补齐正确率** | **602 046 / 602 048** | **99.9997%** |

★ **两条「补错」经查都是词库自身的坏数据，求解是对的**：

- `均订 junding` —— 源文件 `base.dict.yaml:375848` **码里没有空格**（应为 `jun ding`）。
  求解给出 `jun|ding` 正确，真值那条才是错的。**这正是本契约要防的词条，只是它已经
  躺在出厂词库里了。**
- `亚里士多德 li shi duo de` —— `corrections.dict.yaml` 的**错音纠正条目**，故意让码比
  字数少一个音节。⚠️ **这类条目按设计违反「音节数 == 汉字数」**，导入 corrections 风格的
  词库时会被本契约误判，是已知的能力边界。

★ 三条决策由此确定：

1. **默认「导入，由程序补充」**——正确率 99.9997%，且失败模式全部可归因，不是赌。
2. **不为 Ambiguous 单独做 UI**：60 万条里只有 6 条，且首选全部正确。合并进「可补」档即可。
3. **拒收提示必须给样例**：拒收率虽只有 0.001%，但落到具体用户身上就是「我的词没进去」。

⚠️ **实测的适用边界**：样本取自出厂词库，其单字读音索引正是由同一份词库建的 ⇒ 判据①
（每字都要有读音）天然满足。用户导入生僻字时失败率会高于此。7 条无解全部属于这两类：

- **`dia` 不在 `STANDARD_SYLLABLES`**（嗲/嗲声嗲气）——本契约之外的既有缺口，
  改音节表影响全局切分，**未在本次改动**。
- 扩展区生僻字（`三个𦉼`）在单字表里没有条目 ⇒ 无读音。

预览响应（`web_dict_preview_import`）新增字段：

```
needsBoundary: N   // 「可补」档行数
ambiguous:     K   // 其中多解已择一（N 的子集）
unresolvable:  M   // 「非法」档行数
```

对话框按 `M` 分两种文案：

- `M == 0` → 「N 条词缺少音节分隔」+ 两个选项，**默认「导入，由程序补充」**
  （求解是确定的，不是赌）
  1. 不导入，用户自行处理
  2. 导入，由程序补充
- `M > 0` → 追加「另有 M 条无法解析为拼音音节，可能是码表词库」+ 样例，
  这 M 条**两个选项都跳过**；如需保留请用短语（§2.1 的提醒）

⚠️ **不要提供第三个选项「导入但保持无分隔」**（即现行为）。留这个选项就是给不变量
开一个正门。

### 5.2 实际落地形态（2026-08-22，与上面的设想有一处偏差）

**二选一没有做成弹出对话框，而是导入对话框里的一行勾选**「由程序补充（推荐）」，
仅在预览报出可补齐行时出现。三点理由：

1. 弹窗会**打断**——用户点了「合并导入」之后才被拦下来问一句。勾选在点导入**之前**
   就在视野里，他可以连同「合并/替换」一起决定。
2. 这个对话框的类型选择本来就是勾选框，形态一致。
3. 实测（§5.0）之后，默认项的可信度足够高，勾着不动就是推荐路径，不值得为它加一层模态。

⚠️ **「不勾选」= 跳过这些行，不是「照原样导入」**。后端 `fillBoundary=false` 走
`unfilled_skipped` 计数、整行不入库。这一点在 UI 文案里必须说到（现文案：
「不勾选则跳过这些词，不会以缺分隔的形式入库」），否则用户会以为自己选的是
「保持原样」——那正是被否掉的第三个选项。

★ toast 把少掉的词**按原因分列**：`rejected`（程序判非法，要去查是不是拿错文件）与
`boundarySkipped`（用户自己刚做的选择）是两种不同的下一步，合并成一个「跳过 N 行」
讲不清。预览侧同理，并且**非法档一定要列样例**。

⚠️ 跨仓：`dict.import` / `dict.previewImport` 在 wind-setting 有手写 mock，本次已按
schemaId 分流出一个拼音样例——只留原来的码表样例的话，新增的那一行选择在 mock 模式下
**永远不显示，等于没法离线验证**。

★ 「非法」档给样例很重要：用户看到 `wgkq 工` 就知道自己选错了文件。现在这类行混在
`skipped` 里，用户看不出为什么少了词。

### 5.1 两仓分工

现状（实测）：

| 层 | 仓 | 内容 |
|---|---|---|
| 落库 + 格式解析 | 主仓 `wind-store` | `split_spaced_code`、`import_user_words`、`parse_words_*` |
| 求解 / 出码能力 + RPC | 主仓 `wind-webdata` | `dict.import` / `previewImport` / `encodeWords` |
| **编排 + UI** | **wind-setting** | 选文件、预览回显、纯词表出码 → 拼 TSV → 复用 `dict.import` |

本次改动的归属，**主要逻辑在主仓**：

| 改动 | 落点 | 量 |
|---|---|---|
| `SegGraph::paths_with_edges` + `resolve_boundary` | 主仓 `pinyin/{dag,generate,mod}.rs` | 重 ✅ |
| 求解链接入导入闸口 | 主仓 `webdata/lib.rs:632/696` | 中 |
| `normalize_code` 策略化 + `is_valid_code` 替换 | 主仓 `wind-store/import_formats.rs` | 中 |
| 运行时探测器 | 主仓 `user_words.rs` 等 | 轻 |
| 预览三档字段渲染 + 二选一对话框 | wind-setting `pages/dict/state.rs` | 轻 |

⚠️ **`dict.previewImport` 在 setting 仓有手写 mock**（`src/rpc.rs:1353`，非快照生成），
新增响应字段必须同步改它——否则 mock 模式下新分支离线验证不到。同理 `dict.import`
（`:1349`）与 `dict.encodeWords`（`:1333`）。

⚠️ 跨仓时序：setting 仓 `Cargo.toml` 用 path 依赖指向 `../WindInput/...`，故主仓改动
必须先落到**那份工作区**（不是 worktree / 别的分支）setting 侧才做得动。见
`reference_wind_setting_repo`。**未验证**：本次只改 RPC 响应字段、不加 config key，
预计不触发 `capabilities.snapshot.json` / `mockdata/config.json` 那两道快照守门
（它们从 `wind-config` + `data/config.toml` 现算），但动手前应确认。

## 6. 其余写入路径

只在导入处补，不变量当天就会破。已确认的 `boundary = 0` 入口：

| 落点 | 现状 | 判定 |
|---|---|---|
| `user_words.rs:252 on_word_selected` | **硬编码 0**，「隐性造词」路径 | ⚠️ **实测无生产调用点**，见 §6.1 |
| ~~`webdata/lib.rs:553 infer_boundary_for`~~ | 推导码 != 手输码 → 0 | ✅ **已收敛进求解链**，见 §6.2 |
| `handle_addword.rs:300` 码表自动造词 | → 0 | ✓ **正确**，码表码无音节语义 |
| `learn_temp_word` / `promote_temp_word` | 沿用旧值，旧值 0 则 0 | 取决于源头 |

★★ **执行方式在两类路径上必须不同：**

| 路径 | 违规时 |
|---|---|
| 导入 / 手动加词（有 UI、有人在看） | **拒绝 + 告知** |
| 自动造词等无 UI 路径（用户正在打字） | **探测 + 照写**，绝不拒绝 |

运行时拒写 = 用户正常打字时静默丢词，比 `boundary = 0` 降级严重得多。这些路径的 code
来自用户实际击键，**理论上必定合法**（模糊音候选的 code 是原码 `zongguo`，仍能切成
2 音节）——所以断言在这里是**探测器**，专门用来抓「理论上必定合法」这个假设何时不成立。
### 6.1 ⚠️ `on_word_selected` 当前是一条**走不到的路**（2026-08-21 实测）

全仓 `grep on_word_selected` 13 处命中，**唯一的非测试引用是一句注释**
（`abbrev_index.rs:14`），生产代码零调用。上表原先把它列为头号嫌疑是照着 P2a 的记录
写的，但那条路径现在没人走——在它上面装探测器，装的是一个永不触发的空壳。

★ 教训与 `project_english_commit_space` 同型：**给守卫/开关找接线点前，先问「那条路
走得到吗」**。这条 grep 只花几秒，省下的是一段永不执行、却让人以为「已经防住了」的代码。

⚠️ 这也解开了 §7 记的那条耦合：既然探测器不装在 `on_word_selected` 上，
「存量词每次被选中都告警」的噪音问题**不复存在**。

⇒ 探测器实际落在 **`normalize_add_code`**（设置页手动加词）：那里既是活路径，又能拿到
完整的 `BoundaryResolution` 而不只是一个 0，且无需在 store 层硬编码 `"pinyin"` 这个
magic string（store 层生产代码里一处都没有）。用 `debug!` 而非 `info!`——该行含 code
与 text，属用户词库内容。

### 6.2 顺带收敛：`infer_boundary_for` 是求解链层 3 的重复实现

它做的「手输码 == 引擎推导码则借用其切分」正是层 3。收敛掉之后手输码额外获得层 2
（词典点查）与层 4（字数约束求解）：用户手打 `xianning` +「西安宁」以前因推导码对不上
而拿 0，现在能解出 `xi|an|ning`。

⚠️ **有意不在手动加词处拒收** `Unresolvable`：那是用户明确的意图，静默拒绝会变成
「点了保存没反应」。合法性拦截只放在导入闸口——那里有预览可以如实告知。

★ 前提已验证：`normalize_add_code` 收到的是折叠后的 `data_schema_id`（拼音族 → `pinyin`），
而 `data/schemas/pinyin.schema.toml` 确实存在 ⇒ `ensure_loaded("pinyin")` 可行。
**这个前提若不成立，求解会整批返回 `NoInfo`、功能静默失效而无任何报错。**

更强的守门（可选，未做）：把 `boundary` 参数换成不可默认构造的类型，强制每个调用点
交代来源。P2a 用过这招，正是那次逼问才翻出 `on_word_selected` 这条路径。

## 7. 存量数据：**已决定不迁移**（用户拍板，2026-08-21）

不变量只对**新入库**成立，已躺在库里的 `boundary = 0` 拼音词保持原样。

代价是明确且有限的：这些词继续享受不到简拼 / 双拼校验 / 长词上浮的改善——但那是
**维持现状而非回退**，它们本来就是这个样子。且有一条自然的修复路径：用户什么时候
重新导入词库，什么时候就被新闸口补齐。

★ **曾与 §6 的探测器耦合，现已解除。** 原本的顾虑是：存量无边界词每次被选中都要写一次
count，若在那条路径上无差别断言 `boundary != 0`，告警会全是存量噪音。后来查明
`on_word_selected` 根本没有生产调用点（§6.1），探测器改落在手动加词处，噪音问题随之消失。
⚠️ 但**若日后把 `on_word_selected` 接线启用，这条耦合会立刻复活**——届时探测器必须
收窄到 `is_new` 分支（真正的凭空造词），否则存量词会淹没日志。

## 8. ⚠️ 不可删的降级分支

有了不变量之后，容易产生「§1.1 那些 `boundary == 0` 分支可以删了」的想法。**不能删。**
引擎不区分数据来源，而以下四类 0 依然合法且长期存在：

1. 码表 / 五笔词库的 code —— 本就无音节语义（§2.2）
2. 系统词典中 code 超 64 字节的词条 —— `split_spaced_code` 整体降级为 0
3. 模糊变体候选 —— **设计上一律 0**（词典给的是变体码的切分，候选对外的 code 是原码，
   不同域，填值会错位误杀）
4. 尚未迁移的存量数据（§7）

★ **不变量是存储层的准入条件，不是引擎层的假设。** 两者的作用域不同。

## 9. 实施顺序

1. ~~**`SegGraph::paths_with_edges` + `Engine::resolve_boundary`**，配歧义样本测试。~~ ✅ **已完成**。
   `xian`（1/2 音节同码）、`xianning`（约束排除 2 音节解）、`nanan`（真多解，靠读音消歧）
   三个**必须**进测试。
   ⚠️ 写这类测试必须用**歧义切分码**：`cainiaoyizhan` / `lanshoubing` 这种
   `maximum_match` 恰好猜对的样本测不出任何东西（同 `pinyin-code-domains.md` 记的假绿模式）。
2. ~~拿实测求解率决定 §5 的默认选项与文案。~~ ✅ **已完成**，见 §5.0：
   补齐正确率 99.9997%，默认选项定为「导入，由程序补充」。
3. ~~`normalize_code` 策略参数化（§4.3）。~~ ✅ **已完成**（`CodePolicy`）。
   ⚠️ `is_valid_code` **有意保留**：store 层拿不到引擎，判据② 只能在 webdata 层做。
   分工变成：store 只做**归一化** + 拦乱码，webdata 做**准入**。
4. ~~导入闸口接求解链 + 预览三档字段（§5）。~~ ✅ **已完成**，Rime/TSV 与
   **WindDict 多段**两条路径都已接（后者含 `words` 与 `temp_words` 两段）。
5. ~~wind-setting UI 对话框。~~ ✅ **已完成**（wind-setting `919a69c`），落地形态见 §5.2。
   后端配套：`dict.import` 新增 `fillBoundary`（缺省 true）与 `boundarySkipped` 计数。
6. ~~其余写入路径装探测器（§6）。~~ ✅ **已完成**，但落点与原计划不同：
   原定的 `on_word_selected` 实测无生产调用点，探测器改落 `normalize_add_code`，见 §6.1。
7. ~~存量迁移动作（§7）。~~ ⛔ **已决定不做**，见 §7。

## 10. 未验证项与风险

- **性能是唯一的真实风险点。** 分层顺序即成本顺序，但第 3 层最重
  （`generate_word_pinyin` 要枚举 410 音节的笛卡尔积，`MAX_READING_COMBOS` 封顶）。
  第 4 层的 DAG 只在单行的短码上建，**很可能比第 3 层便宜，把 4 提到 3 前面大概率更快**。
  需在 19 万词量级实测后定序。
- ~~本次接线只覆盖 Rime/TSV 路径~~ ✅ **WindDict 多段路径已接**（2026-08-22）。
  闸口以 `&mut dyn FnMut(DictSection, Vec<WordIo>) -> Vec<WordIo>` **注入** `wind-store`——
  求解链要引擎，store 拿不到 `engine_mgr`（同 `CodePolicy` 的理由）。store 只负责
  「少了几行 → 进 `skipped`」，不关心为什么少；分类计数由 webdata 逐段并进响应。
  ⚠️ 闸口是注入的 ⇒ **漏接一段不会有任何编译错误或运行时报错**，表现是「导入成功，
  但那批词悄悄进去了」。`dict_export.rs` 的
  `contract_gates_both_word_sections_and_is_counted_as_skipped` 专门锁这一点。
- ~~`import_temp_words` 与 freq 段是否同款问题未读代码~~ ✅ **已查**：
  - **临时词是同款问题，且更隐蔽**——它会随 `promote_temp_word` **带着 boundary 一起**
    晋升成用户词，是绕过不变量的一条现成后门。已一并过闸。
  - ★★ 顺带修掉一个静默失效：`import_temp_word_rows` / `import_temp_words_wdict` 原本
    **只认 code 里的空格、无视 `WordIo::boundary`**。空格载体表达不了单音节
    （`join_code_by_boundary("xian", 0b1)` 仍是 `xian`），闸口求解出的边界会被丢掉，
    而闸口那边照样记成「已补齐」。测试 `temp_word_import_honors_explicit_boundary`
    用单音节样本 + 对照组锁住，已做变异验证（撤掉修复即红）。
  - **freq 段有意不过闸**：词频表按既定决策不带 boundary 字段，且它的 code 来自用户
    既有词条、不是新入库的词。`shadow` 段不是词条。
- `backup.rs` 的整机还原（`import_user_words_wdict` / `import_temp_words_wdict`）走 store
  直连、**绕过 webdata 因而绕过闸口——这是正确的**：还原读的是自家导出文件，边界随空格
  流出，且还原的语义是「恢复原样」而不是「重新裁决一遍」。
  ⚠️ 上面那条 `WordIo::boundary` 修复**对该路径无影响**：`parse_words_wdict` 产出的行
  `boundary` 恒为 `None`，`unwrap_or(spaced_b)` 取的仍是空格推断值，逐字节等价于改动前。
  ⇒ **单音节用户词经「导出→还原」一轮仍会丢掉 `0b1`**（`join_code_by_boundary` 表达不了
  单音节，`wdict.rs::single_syllable_boundary_is_lossy_by_design` 记着这是既定取舍）。
  要修得给 wdict 文件格式加一列显式边界，代价远大于收益，**本次不做**。
- 求解率本身未实测。若「可补」档的实际比例很低（多数词库本就带空格），§5 的 UI
  投入可以相应缩减。
