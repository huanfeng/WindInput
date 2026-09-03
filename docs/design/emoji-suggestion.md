# Emoji 候选扩展（按候选文本追加，跨方案通用）

## 1. 背景

### 1.1 现状：emoji 绑死在方案的编码域上

今天的 emoji 是 `wubi86.schema.toml:48` 挂的一个扩展词库：

```toml
[[dictionaries]]
id = "wubi86_emoji"
path = "wubi86/wubi86_jidian_emoji.dict.yaml"   # 284 行，约 270 条
base_order = 2
```

来源是 rime-wubi86-jidian 的 extra，**码是上游手编的五笔码**。两个问题都是结构性的：

- **受方案编码限制**：条目是 `code → text`，码在五笔域里。拼音/双拼/混输各自的编码域完全
  不同，这套码搬不过去 ⇒ 换个方案就没有 emoji。
- **不通用**：270 条，且靠 `emoj` 这一个特殊码堆了 71 条常用表情，用 200/199/198… 递减权重
  表达展示顺序。这套编排很脆。

### 1.2 走过的第二条路：加扩展词库，干扰太大

原因在 `architecture/engine-candidate-pipeline.md` §8 里能直接读出来：**词库条目是一等候选**，
要参与去重、`filter_smart` 的 `(source, code)` 分组、词频重排、shadow。emoji 一旦是词库条目，
就会跟汉字抢同一个码位、抢首选、被调频顶上来。`schema.frequency.exclude_blocks = ["emoji"]`
（`bb1d05a9`）就是在给这个模型打补丁。

**本设计换一个模型**：emoji 不是词库条目，而是**对已成形候选的一次文本查表扩展**。它因此与
编码域无关 ⇒ 所有方案自动支持，不需要任何方案配置。

---

## 2. 机制：来自 rime-emoji 的原理

上游 `rime/rime-emoji` 用 opencc 的 `simplifier` 组件挂成 filter：

```yaml
'engine/filters/@before 0': simplifier@emoji_suggestion
emoji_suggestion:
  opencc_config: emoji.json
  option_name: emoji_suggestion
  tips: none
  inherit_comment: false
```

词典是纯文本，`opencc/emoji_word.txt`（4668 行）与 `opencc/emoji_category.txt`（165 行）：

```
一個人	一個人 👤️
ID	ID 🆔️ 🪪
動物	動物 🦆 🦅 🦉 …（90 多个）
```

格式契约由上游 `check.py` 强制：**键必须与值的第一项完全相同**（`\S+\t\S+( \S+)+`，且
`group(1) == group(2)`）。这是 opencc simplifier 的要求——值的首项是原文本身，rime 跳过它，
其余作为新候选插到该原候选之后。

我们移植时**丢弃值的首项**，只取其后的 emoji 列表。

---

## 3. 六条硬约束（实现前必读，均已核实）

### 3.1 ⚠️ 上游词表全繁体，不转换则命中率为零

实测 `emoji_word.txt` 的键：

```
国: 0    國: 28        爱: 0    愛: 16       这: 0    這: 6
门: 0    門: 18        无: 0    無: 23       华: 0    華: 4
```

**零简体条目。** 而本仓内部候选恒简体、只在出口转繁（`wind-transform/src/s2t.rs`，见
`architecture` 与 NOTICE 的 OpenCC 条目）。不做繁简归一的话不是「差一点」，是完全不工作。

归一**不必新引 T→S 方向的 octrie**：`.cache/opencc/dictionaries/STCharacters.txt` 反转即可
（一对多反转成多对一，取首个简体源）。实测覆盖足够，见 3.6 的撞键统计。

### 3.2 ⚠️ 必须钉在候选管线的最末端

`build_candidates()`（`wind-coordinator/src/handle_candidate.rs:623`）的装配顺序：

```
排序 → 按 text 去重(并入 merged_codes) → apply_filter → apply_freq_rerank
     → apply_shadow → 空码补全收口 → short_code_yield → 英文头部候选 → state.candidates
```

emoji 扩展必须在**最后一步之后**。前面每一步都会咬它一口：

| 若插得太早 | 后果 |
|---|---|
| 去重之前 | 参与 `merged_codes` 累积，造出假的同码关系（见 `Candidate::merged_codes` 的 ⚠️） |
| `apply_filter` 之前 | emoji 的 `is_common` 恒 false（不在 8105 字表内）⇒ 同码有常用字时**整批被静默滤掉** |
| `apply_freq_rerank` 之前 | 选一次 emoji ⇒ 码表侧 used-first **不衰减** ⇒ 永久顶到汉字前 |
| `apply_shadow` 之前 | 用户的置顶/移动规则被 emoji 打乱位置 |

手法与 `english_candidates.rs` 的头部候选同型（那份模块文档里写的是「钉在**所有加工之后**：
重排、shadow、出简让全、空码补全收口全都只作用于词库候选」），只是方向相反——英文钉在最前，
emoji 钉在最后。

**由此 emoji 候选不得带 `code`**（同短语、同英文头部候选）。将来若有人把它挪回过滤链，
上表整张会一起引爆。

### 3.3 ⚠️ 短语侧自动上屏会当场失效

`handle_candidate.rs:1166`：

```rust
let [c] = &state.candidates[..] else { return None; };
```

`phrase_auto_commit` 判的是**整个候选列表长度为 1**。插一条 emoji 进去这条恒假 ⇒ 表现为
「短语打全码不再自动上屏、得按空格」，且只在该短语的文本恰好命中 emoji 表时发生。

**引擎侧的 `decide_auto_commit` 天然免疫**：它按 `c.code == input` 筛码表子集判唯一
（`wind-engine/src/codetable/engine.rs`），而 emoji 的 `code` 为空（见 3.2）。所以只需补
短语这一处，并写测试钉住。

### 3.4 ⚠️ 不得全量载入内存

`wind-dict/src/commentdict.rs` 的模块文档已经论证过同一件事，且点名了 emoji：

> 注释库由用户自行挂载，容量**不可预知**——可能是几百条 emoji 名称，也可能是十万条级的英汉
> 词典或字义库。**全解析进内存意味着容量直接变成常驻内存**，而注释是个可选的展示功能，不该
> 为它付这份代价。故与 `.wdat` 同样走 mmap：页按需载入，**常驻内存与库大小基本无关**。

★ **判据不是「这次数据小」**。输入法是常驻进程，「这次只有 137KB」是每一个功能都能说的话；
真正的问题是一旦走内存表，就会长出一条平行的加载/失效/降级路径（指纹怎么算、转换失败怎么办、
换表后怎么清、多实例怎么共享），每条都要重答一遍，且迟早与词库那套分叉。

### 3.5 LGPL-3.0：原样分发，本机建缓存

rime-emoji 是 **LGPL-3.0**，与本仓 MIT 不同。三种做法的分发物不同：

| 做法 | 我们分发的是 | 义务 |
|---|---|---|
| 构建期转换 | 衍生作品 | 全套（标注修改、提供源、产物仍 LGPL） |
| 原样分发 + **本机**建缓存 | 逐字副本 | 轻，且缓存从不离开用户机器 ⇒ **不触发 conveying** |

取后者。同时：

- ⛔ **不得 `include_bytes!` 嵌进 exe**。一旦嵌入就构成 LGPL §4 Combined Work，需要提供机制
  让用户换成自己改的版本；保持独立数据文件则该节整节不适用。
- 发行版须随附 LGPL 全文与版权声明（GPL-3.0 §4「give all recipients a copy of this License」）。

⚠️ **现存缺口（与本功能独立，但同一条义务）**：`scripts/pack-installer.sh:118-128` 组装
staging 时只放 exe/dll/`data/`/uninstall.exe，`build_dev/data/` 顶层与 `dist/WindInput.app.toml`
均无任何 LICENSE/NOTICE。当前发行版已带着 GPL-3.0 的 rime-frost 产物与 LGPL-3.0 的 stroke
码表，却未随附许可证副本。本阶段一并补上。

### 3.6 同键必须合并，不得覆盖

同键有**两个来源，后果完全不同**，别当成一回事（这一条初稿写错过，实测后修正）：

| 来源 | 实测 | 覆盖的后果 |
|---|---|---|
| 繁→简归一撞键 | 4669 行 → 4657 键，**12 组** | **无害**——12 组全是异体字对（煙/菸、台/臺、機/昇）指向同一个 emoji，合并与覆盖结果相同 |
| 两张上游表同键 | `emoji_word` × `emoji_category` 交集 **14 键，内容全不同** | **整组丢数据** |

第 2 类的样子：

```
奖项:  word=🏅          category=🏅 🎖️ 🥇 🥈 🥉 🏆
帽:    word=🧢          category=👒 🧢 🎩 🎓 ⛑️ 🪖
寒冷:  word=🥶          category=❄️ 🌨️ ☃️ ⛄️ 🧊
```

⚠️ 第 1 类今天恰好无害，但那是上游数据的巧合而非保证——判据只能是「合并」，不能是
「反正现在一样」。⚠️ 也**不要拿繁简撞键去验这条测试**：它对覆盖式的错误实现没有判断力。

合并保序去重，发生在**建缓存那一刻**，不在查询热路径。

### 3.7 两张表的量级差 8 倍，配置默认值据此定

实测（归一后）：

| 表 | 键数 | 平均 emoji/键 | 最多 |
|---|---|---|---|
| `emoji_word.txt` | 4657 | **1.09** | 4 |
| `emoji_category.txt` | 166 | **9.0** | **90**（动物） |

⇒ **word 表基本是一对一，干扰远小于预期**，`max_per_word` 对它几乎不触发；那个闸门实际
是为 category 表设的。这也印证了「分类表单独开关、出厂关」的决定：干扰全部集中在那 166 条上。

---

## 4. 存储：`.wemj`

### 4.1 为什么另建格式而不直接用 `.wcmt`

`.wcmt` 的 Row 是 `text | comment | code` 三段，把 emoji 塞进 comment 段确实能跑。但仓里已有
一次同样的取舍并给出了答案——`reverseidx.rs` 的模块文档：

> 所以本格式的骨架取自 [`crate::commentdict`]（`.wcmt`：排序数组 + 二分），**数据语义**不同。

即：**骨架复用，格式独立**。`.wridx` 就是这么来的。emoji 表随本设计还会长出分类维度
（`emoji_category.txt`）与来源标记，挤在 comment 段里迟早要拆。

### 4.2 布局（照 `.wcmt` 骨架）

- Header (24B)：magic `WEMJ` + version u32 + entry_count u32 + index_off u32 + str_off u32 + reserved u32
- Entry[entry_count] (8B 每条，**按 text 升序稳定排序**)：off u32（相对 str_off）+ text_len u16 + emoji_len u16
- StringPool：每条连续存 `text | emojis`，`emojis` 为**空格分隔**的 UTF-8（与上游原始形态一致，
  少一层拆装）

查询是精确点查 + 二分，与注释库同一访问模式（每次只查当前页那 5~9 条候选），不需要 DAT。

### 4.3 整条通路全是现成的

| 组件 | 位置 | 解决 |
|---|---|---|
| mmap 零拷贝 + 排序数组二分 | `commentdict.rs`（367 行） | 常驻内存与库大小无关 |
| 内容指纹（**非 mtime**）+ 解析语义版本 + tag | `cache_fp.rs`（465 行） | 何时该重建 |
| 首次加载建缓存 / 不新鲜则重建 / **失败降级内存表** | `wind-reverse/src/lib.rs:669` | 首次运行转换 |
| `.tmp` → rename 原子写 | `write_comment_wcmt`（`commentdict.rs:167`） | 部署期竞态 |
| 进程级 reader 共享池 | `reader_pool.rs`（366 行） | 同路径不重复映射 |
| 挂载变更时清 `.wemj`/`.fp`/`.tmp` | `wind-reverse/src/lib.rs:729` | 换表不留垃圾 |

★ `cache_fp.rs` 明确要求：**不同种类的缓存各自持 tag**，免得共用一个语义版本号。`.wemj`
必须用自己的 tag。

---

## 5. 呈现形态（三档，`input.emoji.show_as`）

| 档 | 行为 | 列表影响 |
|---|---|---|
| `after` | 紧随宿主候选（`max_hosts` 控制扩几个，默认 1） | 序号会漂 |
| `tail` | 追加到列表末尾（复用 `is_scope_filtered` 的沉底约定） | 序号不漂，要翻页 |
| `comment` | 只在注释段灰字显示（`comment.rs` 的 `${emoji}` 变量） | 零影响，需另设上屏入口 |

三档互斥，是「emoji 以什么形态出现」的**单一决策点**。`${emoji}` 变量因此只在 `comment`
档求值——只看 `enabled` 的话，配了 `after` 又在模板里写 `${emoji}` 会两处都出，而两处都
「按配置办事」，没人觉得自己错了。

### 5.1 ⛔ `focus`（只扩当前高亮候选）已收回，不要再提

设计初稿有第四档 `focus`，理由是「只扩首选时，想打的词不在首位（拼音下很常见）⇒ 那个词
有 emoji 却永远够不着」。**问题是真的，这个解法不成立**：

- 它要在每次导航后移除旧 emoji、按新高亮重插，而**列表长度随之变化会与 `selected_index`
  的语义打架**——用户按下键时，「下一条」可能正是刚插进来的那个 emoji；
- 这个交互得先在真机上定，不能凭空实现；
- 而半实现（只在首次插入时按 index 0）的后果最坏：用户选了 `focus`，得到的却是 `after`
  的行为，**且无从察觉**。

「够不着」改由 `max_hosts` 放大到首页条数解决。将来若要重做，先在真机上把导航交互定下来。

★ **标记位模式**：emoji 候选带 `is_emoji_suggestion`，语义严格限定为「这条是扩展来的」这个
客观事实，**不编码任何排序或显示决策**——四个消费者各自表态（自动上屏跳过 / 词频排除 /
排序按 `show_as` / UI 标注）。手法与 `is_scope_filtered` 完全一致，那条注释里写明了理由。

---

## 6. 配置面

```toml
[input.emoji]                 # 全局基线；方案文件可用同名 [emoji] 段逐字段覆盖
enabled = false               # 出厂关（同 aux_code / short_code_yield）
scope = "exact"               # off | exact（只扩精确整词命中）| all
show_as = "after"             # after | tail | focus | comment
max_per_word = 3              # 一个词最多产出几个 emoji（挡住「動物 → 90 个」）
max_hosts = 1                 # 只对前 N 个候选扩展（show_as = focus 时忽略）
min_word_chars = 2            # 单字不触发（「一 → 1️⃣」噪音最大）
categories = false            # emoji_category.txt 单独开关，出厂关
learn_freq = false            # emoji 不参与调频
```

方案级覆盖走 `read_schema` 的 `merge_toml`，与 `[punct]`/`[candidate]` 同路
（`schema-scoped-behavior.md`）。

---

## 7. 实施阶段

规模参照：生僻字模式整分支为 21 文件 / +1488 行 / 跨 4 crate；本功能多出「新外部数据源 +
新二进制格式 + 繁简归一 + 跨仓设置页」，估 26–37 文件 / ~2400 行 / 跨 6 crate + 1 相邻仓。

**阶段 A（离线、纯函数、100% 可单测，不需部署一次）**

1. `gen-data` 下载 rime-emoji（照 `dev.ps1` 的 rime-stroke 那段）+ NOTICE 条目
2. `pack-installer.sh` 随附 licenses（补 3.5 的现存缺口）
3. 繁简归一（反转 `STCharacters`）
4. `.wemj` 格式 + 建缓存 + 指纹 tag + **撞键合并**

**阶段 B（已完成）**

5. 管线末端插入 + `is_emoji_suggestion` 标记位 + 四个消费点
6. `phrase_auto_commit` 排除（3.3）
7. 三种呈现形态 + 配置层 + 设置页（`../wind-setting`）

### 7.2 阶段 B 的实现落点

| 落点 | 内容 |
|---|---|
| `wind-candidate/candidate.rs` | `is_emoji_suggestion` 标记位（语义只表达「这条是扩展来的」，不编码排序决策） |
| `wind-config/config.rs` | `EmojiConfig`（serde 缺省与 `Default` 共用 `default_emoji_*` 函数） |
| `wind-config/config_schema.rs` | 8 个键登记 + 两个 `Enum` 值域常量 |
| `wind-coordinator/coordinator.rs` | `emoji_dict` / `emoji_spec` 字段、`sync_emoji_dict`、`load_emoji_dict`、值域告警 |
| `wind-coordinator/handle_candidate.rs` | `plan_emoji_insertions`（纯函数）、`apply_emoji_suggestions`、`sole_non_emoji`、`record_selection_cand` |
| `wind-coordinator/comment.rs` | `${emoji}` 变量（`emoji_comment_of`） |
| `wind-transform/s2t.rs` | `Dict::convert_once`（单表替换，不走转换链） |
| `../wind-setting` `settings_manifest.toml` | 8 项 UI 声明（label / hint / `enabled_when` 联动） |

★ **设置页的类型、值域与默认值是自动的**：`wind-rpc/capabilities.rs` 从
`config_schema::REGISTRY` + 系统预置配置动态生成 capability 清单，登记即下发。
`settings_manifest.toml` 只补 core 不该管的东西——中文 label、hint、分区与联动。

### 7.3 跨仓 worktree 的编译

`wind-setting` 的 `Cargo.toml` 里 `wind-config = { path = "../WindInput/..." }` 指向的是
WindInput **主工作区**，故在 worktree 里开发时设置页看不到新加的配置字段。用 Cargo 的
`paths` 覆盖解决（`wind-setting-*/.cargo/config.toml`，加进该仓的 `info/exclude` 不入库）：

```toml
paths = ["D:/.../WindInput/.claude/worktrees/<名>/wind_input/crates/wind-config"]
```

`cargo metadata` 可验证是否生效（看 `wind-config` 解析到哪个 `manifest_path`）。

### 7.1 阶段 A 的实现落点（已完成，供阶段 B 接手）

| 落点 | 内容 |
|---|---|
| `scripts/dev.{ps1,sh}` | gen-data 下载 rime-emoji → `.cache/rime-emoji/`；assemble **原样复制**到 `data/emoji/`（含 LICENSE） |
| `scripts/pack-installer.sh` | staging 新增 `licenses/`（本仓 LICENSE + NOTICE.md） |
| `.gitignore` / `NOTICE.md` | `data/emoji/` 不入库；NOTICE 记明「不加工、逐字副本」与未补全项 |
| `gen_opencc.rs` | `compile_t2s_octrie`：反转 `STCharacters` 得 `TSCharactersDerived.octrie`（繁→简，繁简同形不入表，首次出现胜出） |
| `wind-dict/src/emojidict.rs` | `.wemj` 格式（`EmojiReader` / `write_emoji_wemj`）、`parse_upstream`、`load_or_build` |
| `wind-dict/src/cache_fp.rs` | `EMOJI_TAG`（★ 归一表须一并进指纹） |
| `wind-dict/src/reader_pool.rs` | `open_emoji` + `EMOJI_POOL` |

阶段 B 的入口只有一个：

```rust
wind_dict::emojidict::load_or_build(
    &[data/emoji/emoji_word.txt, data/emoji/emoji_category.txt],  // 后者按 categories 开关决定是否传
    &[data/opencc/TSCharactersDerived.octrie],                     // 只进指纹，不解析
    <cache>/emoji/emoji.wemj,
    |s| /* 用上面那张 octrie 做繁→简 */,
) -> Option<Arc<EmojiReader>>
```

★ `normalize` 用闭包注入而非直接依赖：`wind-dict` 不依赖 `wind-transform`，且测试可传恒等闭包。
★ 失败返回 `None` = **功能不可用**，刻意不降级内存表（理由见 §3.4 与函数文档）。

★ 阶段划分的依据是**对工作树的诉求相反**：A 纯离线，隔离收益大、真机需求为零；B 高频部署
验证，而本仓 worktree 的 `build_dev` 存在指向主仓的符号链接（`candidate-font` 即是），
在 worktree 里部署会静默覆盖主仓产物。按功能开 worktree 会把两半绑在一起、两边缺点都吃到。

---

## 8. 已知坑

- **`reader_pool` 的 `Close` 是配额语义、非幂等**：emoji 表随配置开关动态挂卸时，重复调用会
  吃掉别人的引用计数。
- **worktree 的 `.cache`/`build_dev` junction**：删 worktree 前须先解 junction，否则删穿词库。
  本分支刻意不建这两个链接，测试自带 fixture。
- **`emoji_category.txt` 的干扰量级**：单条最多 90 余个 emoji。默认关，且受 `max_per_word` 约束。
- **与 `wubi86_emoji` 词库的关系**：两者不互斥也不合并。旧库是「打 `emoj` 直接检索」的入口型
  能力，新机制是「打出词之后追加」，替代不了。旧库保留原样，不再往里加东西。
