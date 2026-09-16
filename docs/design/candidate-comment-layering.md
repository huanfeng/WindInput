# 候选注释的分层与收口

> 2026-08-26 设计。P0 实施于 main。
> 前身：`mode-candidate-layout.md`（模式级三态的形态来源）、`schema-scoped-behavior.md`
> （方案级 `[punct]`/`[candidate]`/`[phrases]` 三段，本设计是它的第四段）。

## 1 问题

候选注释（候选右侧灰字）的配置面散在 10 个落点，而**方案级层整个缺失**——
想给五笔配 `${code_hint}`、给拼音配 `${code|pinyin}`、给英文方案配 `${dict}`，
只能改全局那一份，切个方案就错。

| # | 落点 | 键 | 作用域 |
|---|---|---|---|
| 1 | config.toml | `ui.candidate.comment_template_vertical` / `_horizontal` | 全局 |
| 2 | config.toml | `ui.candidate.comment_max_chars` | 全局（横竖共用） |
| 3 | config.toml | `input.{temp_english,temp_pinyin,url}.comment_template_*` | 模式级三态 |
| 4 | config.toml | `schema.mix_modes[].comment_template_*` | 实例级三态 |
| 5 | 方案文件 | `[overlay].comment_template_*` | overlay **激活期间** |
| 6 | config.toml | `[[ui.comment_dicts]]`（含 `schemas`） | 全局表 + 方案过滤 |
| 7 | config.toml | `schema.pinyin.code_hint_source` | 全局四档，门控 `${code_rev}` / `${shuangpin}` 的**求值** |
| 8 | 方案文件 | `[engine.codetable].show_code_hint` | 方案级，门控 `${code_hint}` 的**生产** |
| 9 | 主题 | `[comment]` ViewNode | 主题级（样式） |
| 10 | config.toml | `ui.tooltip.*` | 全局，相邻但独立的悬停提示 |

★ **散的是配置面，不是代码面**：`comment.rs` 始终是模板的唯一消费点，`comment_for` 是唯一
渲染入口。这决定了本轮是**加一层**而不是拆重构。

### 1.1 四条真缺陷

1. **没有方案级层**。`[overlay]` 那两份填不了这个坑——它的语义是「本方案**被叠加激活期间**」，
   有进入/退出生命周期；「本方案作为**常驻 active 方案**期间」在方案文件里无处表达。
2. **`comment_max_chars` 横竖共用一份**。模板分横竖的全部理由是「两种排布可用横向空间差
   一个数量级」，那么长度预算更该分开。这是遗漏，不是取舍。
3. **注释库的方案过滤判据源错了**。`sync_comment_dicts` 用 `active_schema_id()`，
   临英背后是硬编码的 `english` 方案 ⇒ `schemas = ["english"]` 在五笔方案下不生效。
4. ~~**`show_code_hint` 同名双键**~~ ✅ **已消解**（GH#128 那轮）。7 改名为
   `schema.pinyin.code_hint_source`（四档枚举），8 保持原名——两者管的本就是两件事
   （一个是「显示哪种编码」，一个是「码表引擎要不要产剩余编码」），重名只是历史巧合。
   设置页两个都叫「显示编码提示」的重影也一并消掉了。

   ★ **P3 的方向被推翻了**，记下来免得日后有人照着做：原案是「让 `${code}` 只由模板
   决定、开关退役」。但 GH#128 证明这个开关有**模板表达不了**的职责——用户要的第三种
   编码（本方案击键）得先有人决定「允许它出现吗」，而模板只能决定「出现的话摆哪」。
   现在的分工是「开关管允许哪些来源求值、模板管按什么顺序和格式摆」，两层不重叠，
   谁也不是谁的重复。

### 1.2 三条「看着散但不要合并」

- **注释段 vs 悬停提示**（`ui.tooltip.*`）：已刻意解耦过一次。合并会重造已删除的
  「溢出转气泡」重复显示缺陷（气泡自己的逐字 `[拼音]` 段与追加的那份同时出现）。
- **内容 vs 样式**（config vs 主题 `[comment]`）：正交，合并会破坏主题可移植性。
- **`[overlay]` 那份 vs 新增的 `[candidate]` 那份**：R2「实例身份从哪来」——
  前者身份来自叠加生命周期，后者来自常驻方案。**身份不同，同名字段并存是对的**，
  一个方案可以两段都写、取值互不干扰（`[candidate].layout` 与 `[overlay].candidate_layout`
  已是先例）。

## 2 目标模型

```
注释模板 = 模式级(三态) → 方案级(三态) → 全局(必有值)
           input.* / mix_modes[] / [overlay]      [candidate].comment_template_*      ui.candidate.comment_template_*
```

三态语义与既有完全一致，不发明新写法（config-design-rules §R3 只允许两式）：

| 态 | 含义 |
|---|---|
| 键缺失 | 跟随**下一层** |
| 非空 | 本层覆盖 |
| 空串 | 本层起「不显示注释」 |

### ★ `Follow` 的语义是「跟随下一层」，不是「跟随全局」

加了方案层之后这两者不再等价。唯一能区分新旧实现的格子是
**「模式缺失 + 方案有意图 + 全局有值」**——测试必须钉住它（`vertical_for` 那轮的原话，照抄）。

### ★ 注释**不需要**标点那套基线备份

`punct` 的 `Follow` 曾退化成「保持上一个方案强加的值」，因为 `state.chinese_punct` 既是当前值
又是唯一存储，被覆盖后没有可回落的原值，于是加了 `punct_before_schema`。

注释没有这个问题：它是**每次渲染前重算**的派生值（声明式，与 `layout::intent_for` 同构），
不写运行时状态 ⇒ 退出模式、切走方案后自动回落，"恢复"不是一个需要被执行的动作。
⛔ **不要照着 `sync_schema_scope` 给它补一个恢复动作。**

## 3 P0-1 方案级模板

**落点：`[candidate]` 段扩字段**，不新建 `[comment]` 段。

```toml
# xxx.schema.toml
[candidate]
layout = "vertical"
comment_template_vertical   = "${code_hint}"
comment_template_horizontal = ""            # 本方案横排不显示注释
```

判据：本仓给方案文件加段时，**段名先去全局配置里找同名域**。全局是 `ui.candidate.comment_template_*`
⇒ 对应方案段就是 `[candidate]`。字段名与全局、与 `[overlay]` 那两份**逐字一致**，
让「全局 / 方案 / 模式」在用户眼里是同一件事的三个层级，而不是三套发明出来的键名。

### ★ 为什么用 `active_behavior()` 而不是 `State` 快照

`comment_template_for` 返回 `&'a str`（借 `cfg`，每次按键 × 每页候选调一次，不为它分配），
而方案级模板来自 `behavior_for()` 的临时 `Arc<SchemaBehavior>`，借不出去。`[overlay]` 当年撞的
就是这堵墙，解法是把 `[overlay]` 段快照进 `State`。

**但注释不能照抄那个解法**：`State` 快照要有失效点，而 `schema_generation`
**不随 `invalidate_schema` 递增**（设置页改 `schema_overrides` 不 bump 代际）⇒ 用户在设置页
改完方案级注释模板，代际没变、快照不刷新，表现正是本仓反复栽的「设置了不生效、重启后生效」。

改为在调用点（`notify_ui_update`）**循环外取一次** `active_behavior()` 存局部变量，
模板借用它：`Arc` 活到函数结束，生命周期成立，且 `behavior_cache` 已在 `invalidate_schema`
里被清 ⇒ 设置页一保存就生效。开销是每次按键一次 `Arc::clone` + 一次哈希查找。

⇒ ★ 可复用判据：**给某层配置做运行时快照前，先问「这一层的失效点是什么，那个信号真的会动吗」**。
代际是"活跃方案变了"，不是"方案内容变了"，两者不可互换。

### ★ 方案层的归属取 `active`，不取 `effective_data_schema`

`[phrases]` 取 effective（数据类归属，临英归 `english` 桶），`[candidate].layout` 取 active
（呈现类）。注释模板归**呈现类**，取 active，理由是：临英/临拼/mix 的注释需求**已经由模式层
表达**，方案层再按 effective 解析一次就成了两层说同一件事，且两者可以互相矛盾。

（注释**库**的过滤是另一回事，那是数据类，见 §5。）

### 实施要点

- `CandidateSpec` 加两个 `CommentTemplateOverride` 字段 ⇒ **失去 `Copy`**，调用点顺带核一遍。
- `SchemaBehavior` 随之加两个字段（三段仍合一个缓存条目：同源、同批失效）。
- `template_for(cfg, overlay, schema, active, vertical)` —— 保持纯函数，测试直接造结构体，
  不必构造 `EngineManager`。

## 4 P0-2 `comment_max_chars` 拆横竖

`ui.candidate.comment_max_chars` → `comment_max_chars_vertical` / `_horizontal`，
命名对齐既有的 `min_window_width_vertical` / `_horizontal`。

**存量迁移**（`migrate_comment_max_chars_value`，须在反序列化前跑）：旧键有值且新键缺失时，
把旧值抄进两个新键（**旧键留在用户文件里，不退役**，理由见下）。不迁移的后果是配了非 0 值的用户升级后注释
突然不再截断——R4 变更纪律。

方案级**不做** max_chars：截断是**排布的显示预算**（横排全部候选共享一行），与方案无关。

### ★★ 旧键**不进** `RETIRED_KEYS`（实施时才发现）

本想顺手登记退役，写「顺序不可颠倒」那句注释时才发现顺序根本不可控：
`prune_user_config` 是服务启动 D2 步在**用户文件**上删键，而值迁移只改 load() 的**内存**、
从不落盘 ⇒ 登记进去的时序必然是「先把文件里的旧键删掉，下次启动再也迁不到」，
用户配的截断值静默归 0。

⇒ ★ 可复用判据：**一个键只要还有值迁移在读它，就不能进 `RETIRED_KEYS`**。
那份清单的前提是「删掉不改变任何生效值」，被迁移读取的键不满足这个前提。
测试 `retired_keys_excludes_keys_still_read_by_migration` 钉住这个「顺手补上去就出事」的改动。

## 5 P0-3 注释库过滤下移到查询层

### 问题

单纯把 `sync_comment_dicts` 的判据源从 `active_schema_id()` 换成 `effective_data_schema`
**是错的**：那样进/出临英就要重挂 mmap，而 `reload_comments` 的热路径成本几乎全是
「读整份源文件算内容指纹」（十万条 ~2.7ms，百万条 ~39ms），临英是 Shift 一按就进的高频操作,
这笔账会落在按键线程上。

### 解法

**挂载去方案化，过滤下移到查询点**：

- `sync_comment_dicts` 挂载**全部 enabled 的库**，不再按方案筛选；
- `schemas` 白名单改由 `ReverseLookup::comment_of(text, code, schema)` 在查询时求值。

成立的理由：
1. **mmap 的常驻内存与库大小无关**（已实测），挂载全集的代价只是启动时每库一次指纹校验，
   一次性、不在按键路径；
2. 消除「切方案要重挂 mmap」的抖动——**那笔成本现在就在付**（`finish_user_schema_switch`
   里那次调用）；
3. `schemas` 语义变纯：只管「查不查」，于是它天然按**查询发生时**的语境求值，
   缺陷 3 自动消失，且将来 overlay/模式语境也一并正确。

⇒ ★ 与既有判据同构：**过滤要尽量靠近消费点**。模板里不写 `${dict}` 就根本不调用 `comment_of`
（零开销）——任何挂载层的过滤都做不到这一点，这本就说明过滤属于消费侧。

### 实施要点

- `CommentSource` 带上 `schemas: Vec<String>`；`reload_comments` 收 `(path, schemas)` 对
  （复用旧条目时**要更新 schemas**——只按 path 匹配复用会让改了 schemas 的库仍按旧白名单查）。
- `comment_of` 三处遍历（code 精确 / 首条 / 大小写回退）**统一加同一个过滤**，
  且**两遍扫描的语义不变**：先跨全部**适用**库找 code 精确命中，再按挂载顺序取首条。
- 切方案相关的三处 `sync_comment_dicts` 调用（`finish_user_schema_switch`、
  `webdata` 的 `schema.setActive`、`reload_user_config` 的 `schema_dirty` 分支内那次）
  随之删除并改注释：挂载只跟**配置**走。保留构造期与配置热重载两处。

## 6 测试判据

| 判据 | 为什么必须是这一条 |
|---|---|
| 「模式缺失 + 方案有 + 全局有」取方案值 | 唯一能区分三层与两层实现的格子 |
| 方案层空串 = 不显示（≠ 跟随） | 三态的第三态，漏了会被当成跟随 |
| 方案层横竖各自独立三态 | 只覆盖竖排、横排跟随是合法且常见的配置 |
| 端到端**收 UI 通道**（`new_headless_with_ui`） | 注释在**发往 UI 的路径上**算、不回写 `state.candidates`；只测 `template_for` 的纯函数用例在"半接线"下全绿 |
| 端到端模板**至少含一个非空变量** | 纯字面量模板会被「变量全空则整段消失」渲染成空串，看着像没生效其实是用例写错 |
| headless 须预置 `last_valid_caret` + `composition_start` | 否则首帧被 `first_show` 闸门拦下，压根不发候选 |
| 注释库：`schemas` 限定的库在**临英**下查得到 | 缺陷 3 的直接判据，改判据源前它必红 |
| 注释库：改 `schemas` 后不重启即生效 | 钉住「复用旧条目时更新 schemas」那一步 |
| max_chars 迁移：旧键非 0 → 两个新键都拿到该值 | R4，防升级后静默不截断 |

⚠️ **内置方案从 `5fd995b` 起一项都不声明** ⇒ 方案级路径**出厂状态下走不到**，
本机试不出。夹具必须自造方案（`zz_ct` 那种），不能拿 wubi86 当现成的。

## 7 后续（不在 P0）

### P1 设置页收口（✅ 已实施 2026-08-26）

落地形态与原设计有两处出入，都是实施时按既有结构调整的：

1. **方案级三态没有单开面板**，而是并进方案对话框既有的「本方案行为」节
   （方案 → 选中方案 → 方案自定义），与 `[punct]` / `[candidate].layout` / `[phrases]` 并列。
   那里才是「这个方案怎么表现」的语境；放进外观页的话，用户看不出当前配的是哪个方案。
   ★ 三态用**两个控件**表达：勾选框「自定义」回答要不要覆盖，输入框只装内容
   （勾上留空 = 不显示注释）。做成三档下拉的话「自定义 + 留空」与「不显示」会是同一状态的
   两种走法——`config-design-rules` §R3 反对的重复态。
   ⚠️ **「取消覆盖」回传 `null` 而不是空串**：core 侧 `json_to_toml` 跳过 null +
   `saveConfig` 拿方案文件基线做 diff，两条既有性质合起来才等于「这一项不写进 override」。
   写空串会让用户取消勾选后得到「不显示注释」——与他要的「跟随全局」恰好相反，
   而两者在界面上都是空输入框，看不出来。
2. **注释库列表编辑器**（`dialog_button_comment_dicts`，外观 → 候选窗口 → 注释词库）：
   拖拽排序（顺序即优先级）+ 每行启用开关 + 增删改表单。`ui.comment_dicts` 已从
   `UNCOVERED_BY_DESIGN` 移除。★ 写回**在原对象上 insert 而不是重建**，core 将来给
   `CommentDictSpec` 加字段时不会静默抹掉用户手写的值。
   ⏸ **未做「浏览文件」**：设置端不直接写数据目录（词库导入一律走 RPC 交给 core 落盘），
   要支持「选个文件自动复制进 `schemas/comments/`」得先在 core 加一个导入 RPC。
   当前是填相对路径 —— 与 core 既有约定一致，文件放置本就是另一件事。

### ⚠️ 实施中踩到的两个坑

- **manifest 的 hint 里写了反斜杠**（`schemas\comments\`）：经过多层引号后只剩一个，
  TOML 基本字符串里 `\c` 是非法转义 ⇒ **整份 manifest 解析失败**，而失败被
  `unwrap_or_default()` 吞掉 ⇒ 表现是「205 个配置键突然未覆盖」+ 29 个测试失败，
  报错完全指不到根因。⇒ ★ 面向用户的文案里**不要出现反斜杠**，用正斜杠或改写措辞。
- **新建的 `.rs` 文件默认是 LF**，而本仓源文件是 CRLF：多行文本替换会全部匹配不上
  （单行的照样成功，于是像是"改了一半"）。新建文件后先统一行尾。

### P1 原设计（保留备查）

新建**「候选注释」独立面板**，一屏容纳全局两份模板 + 两份 max_chars + 方案级三态行 +
注释库列表编辑器（销掉 `capabilities.rs` 里 `ui.comment_dicts` 的 `UNCOVERED_BY_DESIGN` ⑮
——那条明写着「**待接入**，不是设计上不暴露」）。

方案级三态行直接复用方案级码表配置那屏已验证的形态：逐项勾选框 + 来源三色
（跟随灰 / 方案自带琥珀 / 已自定义蓝），**跟随态不置灰**（否则整屏发灰）。

三条硬约束：
1. 读侧旁路字段（`effectiveComment` / `followedComment` / `commentOverride`）**必须登记
   `READONLY_SIDECAR_FIELDS`**，否则用户打开一次设置页就把该方案的注释行为冻结在那一刻；
2. `saveConfig` 整份 diff 后全量重写 override ⇒ 本面板改动必须与其它方案级改动**写进同一份
   cfg、只入队一次**（`SideCommitter`，禁止新增独立保存按钮）；
3. 新增设置节要手工加进 `SettingsSection::ALL`（那份防 panic 测试遍历的是**手写列表**），
   且改完必须 `--screenshot` 自查——设置页控件有两处分派，漏一处静默降级成 `[类型名?]`
   占位符而测试全绿。

### P2 文档站

`settings/appearance/candidate-comment.mdx` + `guides/config/{ui,schema}.mdx` 补三层优先级表。
★ 除 grep 新键名外，还要反着问「这次改动让哪些**既有陈述**失效了」——凡写「注释模板是全局的」
「模式级覆盖全局」的句子都变成半对，而这类句子不含任何新键名，grep 找不出来。

### ~~P3 消解 `show_code_hint` 双真相源~~ ✅ 已办，但走的是相反的路

原案是「让 `${code}` 只由模板决定、全局开关退役」，判据是「模板里不写 `${code}` 本来
就等于关掉，而且是零开销的关法」。

**GH#128 推翻了这个判据。** 用户要的是第三种编码——双拼编码（`${shuangpin}`），
它既不在码表词库里、也不是「关掉」能表达的。于是「显示哪一类编码」成了一个模板表达不了
的问题：模板只能回答「出现的话摆在哪、和什么拼在一起」。

最终改成四档枚举 `schema.pinyin.code_hint_source`（`off` / `codetable` / `schema` /
`auto`），分工是**开关管允许哪些来源求值、模板管按什么顺序和格式摆**，两层不重叠。
迁移代价比原案小得多：`true → codetable`、`false → off`，存量用户所见分毫不变，
不必改写任何人的模板。

顺带把 §1.1 缺陷 4 的同名双键消掉了——改名之后 7 与 8 不再重名。

## 8 跨仓 checklist

| 仓 | 事项 | 状态 |
|---|---|---|
| WindInput | Config 结构体 + default + REGISTRY + `data/config.toml`(L2) | ✅ P0 |
| WindInput | 方案文件 `[candidate]` 两字段 + `SchemaBehavior` | ✅ P0 |
| WindInput | `comment_of` 加 schema 过滤 + 挂载去方案化 | ✅ P0 |
| wind-setting | `comment_max_chars` 改名的**被动同步**：manifest 拆成两项、`capabilities.snapshot.json`、`mockdata/config.json`（769 测试过，含快照对账） | ✅ P0 |
| WindInputDocs | `comment_max_chars` 改名 + 方案级两键（`guides/config/ui.mdx`、`guides/schemas.mdx`） | ✅ P0 |

### GH#128（`${shuangpin}` / `code_hint_source`）

| 仓 | 事项 | 状态 |
|---|---|---|
| WindInput | `ShuangpinReverse` 反向表（枚举 `convert_pair` 反演） | ✅ |
| WindInput | `EngineManager::schema_keys_of` + 单槽缓存 + 布局解析收口 | ✅ |
| WindInput | `${shuangpin}` 新变量；`${code}`→`${code_rev}` 改名 + 永久别名 | ✅ |
| WindInput | `show_code_hint`(bool) → `code_hint_source`(四档) + 值迁移 | ✅ |
| WindInput | 出厂模板 `${code_hint|code_rev|shuangpin}`（`data/config.toml` 与 `default_comment_template()` 两处） | ✅ |
| wind-setting | manifest 改 select、`mockdata/config.json` | ✅ |
| wind-setting | `capabilities.snapshot.json` + `mockdata/config.json` 由生成器重出 | ✅ 编译机独立槽位，全量 1038 passed |
| WindInput | `docs/config-key-migration.md` 键名映射表 | ✅ |
| WindInputDocs | `candidate-comment.mdx` / `config/schema.mdx` / `config/ui.mdx` / `special-modes.mdx` | ✅ |
| 真机 | 双拼方案下候选注释显示双拼码；升级迁移后老用户所见不变 | ⏸ **待验** |

⚠️ wind-setting 在 Linux 上构建不了（`rfd` 缺 gtk3/xdg-portal 后端），两个生成器
（`regenerate_capabilities_snapshot` / `regenerate_mock_config`）只能上编译机跑，用独立
槽位 `C:/build-<tag>/` 避开共享的主构建树，用完删掉（一份 target 约 2.6 GB）。
手改那两份产物是 `capabilities.rs` 明令禁止的。

★ 上编译机前**先 `git rebase main`**：跨仓对账测试（`uncovered_capability_keys_match_allowlist`、
`mock_config_matches_core_system_preset`）会因为 core worktree 落后于 main 而红，
那不是自己的改动坏了。这次就撞上了一次（并发会话新增的 `input.commit_newline`）。

### 命名与范围各错了一次，都是同一个来源

**第一次：变量曾叫 `${code_schema}`、界面曾叫「本方案击键」（均已改名）。** 当时刻意避开 `shuangpin`
这个名字，理由是「不把实现绑进名字」，改用「本方案 vs 他方案」作语义轴。那条轴站不住：
**本方案是全拼时这一档恒空**，用户在全拼下选中它什么也不会发生——名字承诺了一件它做
不到的事。真正的语义从来只是「双拼编码」，也就是 GH#128 标题里提问者自己的词。

**第二次：范围只认活跃方案。** 全拼下恒空，理由写的是「全拼的击键就是拼音本身，显示是
冗余」。那句话对「本方案击键」成立，对「双拼编码」不成立。

两次错的根子是同一个：**把 issue 读成了「双拼用户要看自己的码」**。但原话是「有时忘记
了还能看下」——正在用双拼打字的人刚敲完码不会忘；会忘并且想看一眼的，多半是用全拼打字、
正往双拼迁移的人。issue 里没写作者用什么方案，这个信息缺口当时没被当回事，于是拿一个
未经检验的读法定了名字和范围。

现在：变量 `${shuangpin}`、配置值 `shuangpin`、界面「双拼编码」三处同名；布局来源走
「活跃方案 → `primary_pinyin` → `available` 首个双拼」的回退链，与
`resolve_primary_codetable` 同构，不新增配置键。

★ 下次遇到「用户要 X」这类需求，先问**他在什么场景下要 X**。这次如果一开始就问「他用
什么方案打字」，名字和范围都不会错。

### 这轮踩到的坑：值迁移的判据不能问合并结果

`Config::load` 的 L1 是 `toml::Value::try_from(Config::default())` —— **凡是仍在结构体里
的字段，合并结果里必然都有**。于是任何「新键还不存在才写」的迁移，放在 `load` 末尾那批
合并后迁移里，`contains_key` 永远命中，**一次都不会执行**。

这不是理论风险，两条都中招了：

- `migrate_comment_max_chars_value` —— 早已静默失效（配过 `comment_max_chars` 的用户升级
  后两个新键仍是 0）。本轮的 `migrate_show_code_hint_value` 照抄了它的模式，同样失效。
- 两条现已移进 `Config::migrate_user_layer_value`，在各层合并**之前**就地跑。只有在单独
  一层上问 `contains_key`，答案才真正是「这一层的作者写过吗」。

⚠️ **判据不能简单反转成「旧键优先」**：设置页写回时只写它管的那个键、不会顺手删掉旧键，
于是新旧两键在用户配置里长期并存。旧键优先的话，用户在设置页的每次修改都会在下次启动
被打回。

**为什么原有那批测试没抓住**：它们喂的是「只有旧键的裸 `toml::Value`」，那个形态在生产
中一次都不会出现。迁移即便完全不执行，测试照样全绿。新增的
`migrate_show_code_hint_works_through_the_real_layering` 按真实链路走「用户层 ⊕ L1 默认」。

### ⏸ 顺带发现（**未处理**，不属本轮范围）

`migrate_force_vertical_value` 是**相反方向**的同类缺陷：它对 `mix_modes[].candidate_layout`
**无条件 `insert`**，没有任何「用户是否显式设过」的判据。用户配置里若还留着旧的
`schema.quick_input.force_vertical`，他在设置页给 quick_mix 设的候选排布会在每次启动被
那个旧键打回。`migrate_enable_english_value` 的写入形态相同，需一并核。

修法现成——挪进 `migrate_user_layer_value` 并补 `contains_key` 守卫即可——但那会改变已
发布的行为，该单独排期、单独测。
| wind-setting | 方案级三态行（并入「本方案行为」节）+ 注释库列表编辑器 + 销 UNCOVERED ⑮（781 测试过，含截图自查） | ✅ P1 |
| WindInputDocs | 三层优先级表（`settings/appearance/candidate-comment`）+ 反向审查既有陈述 | P2 |
| WindInputTools | **不涉及**：工具站的方案模型只覆盖码表/编码字段，`[punct]`/`[candidate]`/`[phrases]` 这三段行为段本就不在其中（已核 `src/lib/schema/model.ts`） | — |
