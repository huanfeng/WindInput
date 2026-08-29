# 定制版数据层（data_custom）设计与实施计划

> 状态：**设计已定，未实施**。P0 可独立发布并先行。
> 起因：第三方作者二次定制（换虎码/小鹤音形等）时直接改发布包的 `data/` 目录，
> 每次版本更新都要手工与内置文件 diff，冲突处理极其痛苦。
> 关联：`docs/redesign/config-schema.md`（配置总览）、`docs/redesign/schema-config-layering.md`
> （方案配置分层）、`wind-config/src/{config,config_schema,app_compat}.rs`、
> `wind-engine/src/manager.rs`、`wind-coordinator/src/{coordinator,handle_mode}.rs`、
> 跨仓 `wind-installer`。

## 0. 目标与非目标

**目标**：让定制者把全部定制内容放进与 `data/` 同级的 `data_custom/`，
不碰 `data/` 一个字节，从而使主程序升级对定制版**零冲突**。

**非目标**：不做「用户自己在 GUI 里做定制版」；不替代已有的用户层
（`%APPDATA%\WindInput\`），后者仍是终端用户的个人覆盖层。

## 1. 已定的设计契约

1. **层序固定为 `data < data_custom < %APPDATA%`。**
   `data_custom` 在安装目录下（Program Files），普通用户无写权限，只承担
   「随安装包分发的定制」职责；用户个人调整仍走 `%APPDATA%`。

2. **判据是 `data_custom/custom.toml` 存在，不是目录存在。**
   隐式契约无编译期约束——`datadir.conf` 曾整段断链（写端完整、读端一行不读、
   骗了用户半年）就是前车之鉴。manifest 同时充当：定制版身份（关于页/日志/报障）、
   减法清单、版本兼容判据。

3. **`data_custom` 对程序只读。** 程序绝不改写它（同「GUI 绝不回写
   `system.quick.toml`」原则）。退役键清理、配置剪枝一律只作用于用户层。

4. **覆盖粒度按资源的语义单元，不按文件系统。** 见 §2。

5. **`custom.schemas.hide` 与 `[schema].hidden` 是两个正交的轴，不得合并。**
   `hidden` = 「不列进方案切换列表」（english/快符仍可用、仍被 mix 引用）；
   `hide` = 「这个方案在本定制版里不存在」。拿 `hidden` 实现减法会让被隐藏的方案
   继续被 mix / special_modes / `schema.active` 引用到。

6. **定制者只写差异键，不整份复制 `config.toml`。** 机制上支持（深合并），
   文档上引导，CLI 上校验。差异越小跨版本存活率越高。

## 2. 资源语义分类

| 资源 | 现有语义 | data_custom 语义 | 落点 |
|---|---|---|---|
| `config.toml` | L1→L2→L3 深合并 | **深合并，插为 L2.5** | `Config::load`、`system_preset_value` |
| `compat.toml` | 按进程名条目合并 | 按条目合并（三层） | `app_compat.rs` 加载器 |
| `pinyin_map.txt`、`system.*.toml`、`schemas/common_chars.txt` | 整表替换 | 整表替换 | `resolve_data_file` |
| `schemas/*.schema.toml`、`schemas/<id>/**` | 逐文件覆盖 + 枚举合并 | 同左 + hide 清单 | `resolve_schema_file`、`installed_schemas` |
| `themes/`、`opencc/` | 搜索链按名覆盖 | 同左 + hide 清单 | `theme_search_dirs`、`join("opencc")` |
| `schema_overrides/{id}.toml` | 用户层方案覆盖 | **本期不支持** | 见 §5 |

### manifest 格式

```toml
[custom]
id = "huma-edition"          # 稳定标识，日志/关于页/报障用
name = "虎码定制版"
version = "1.2"
base_version = "0.9.30"      # 基于哪个主程序版本定制

[schemas]
hide = ["wubi86", "wubi86_pinyin"]

[themes]
hide = ["msime"]
```

加法与替换**不需要声明**：直接把文件放进 `data_custom/` 对应位置即可。
manifest 只负责「减法」与「身份」。

---

## 3. 实施阶段

### P0 — 配置合并失败的段级降级（独立价值，先行）

**问题**：`Config::load` 是多层合并后**一次性** `try_into`，任何一层里一个类型
不匹配的键都让整份 `Config` 返回 `Err`，而调用方几乎都是 `unwrap_or_default()`
（`construct.rs:33`、`apps/repl`、`wind-mobile`）⇒ **用户方案/词库/按键/主题一起
回落出厂值**，只留一行日志。

加了 `data_custom` 之后风险被放大：**定制者的一个陈旧键会把终端用户的全部配置
打回出厂**，而这个故障在没有 `data_custom` 的开发机上永远复现不出来。

**改动**：`try_into` 失败后，按**顶层段**逐段反序列化，只把失败的段替换为 L1 默认。

```
try_into(merged) 失败
  └→ 逐段 try_into：input / keys / schema / ui / compat / engine …
       成功的段保留，失败的段用 L1 默认填回 + WARN 落日志
```

选段而非选层的理由：**层是错误的隔离单元**。剥掉 L3 丢用户全部设置、剥掉 L2.5
丢定制版全部身份，代价都太大；段的边界恰好对应功能边界，用户感知到的是
「按键设置回默认了」而不是「一切归零」。

**注意**：

- 段级降级**不是** `migrate_*_value` 的替代品。已发布字段改类型仍必须加迁移，
  降级只是兜底。日志必须 WARN 级，否则会掩盖真实的迁移缺失。
- **降级信息的可见性拆成两半**：P0 只做日志 + 内部消费者（见下「连带闸门」）；
  RPC / 设置页 / 关于页暴露归 P3，与「定制版身份暴露」一并做。
  在 P3 落地之前，不变量 6 的「可见」只对读日志的人成立。

**★ 连带闸门（P0 必须一起做，否则降级本身就是数据丢失）**

「`load()` 从 Err 变成 Ok-但某段是默认值」会让原先靠 `?` 保护的下游拿到**残缺表**
并当成真实配置去写盘。已识别两处，都必须加闸：

| 落点 | 不加闸的后果 |
|---|---|
| `materialize_key_actions`（config.rs） | 拿只剩出厂绑定的残表**整表覆盖**用户 `keys.key_actions`，并打一次性版本标记 ⇒ 用户自定义按键绑定从磁盘永久消失、**不会自愈**；毒若恰在该段还会被自己覆盖掉，事后无从归因 |
| `cmd_export`（apps/service/src/config_cli.rs） | 静默导出一份「坏段已被出厂值替换」的 TOML，用户拿它做备份或回写 ⇒ **把丢失固化** |

判据统一为「相关段降级或整份降级 ⇒ 什么都不做」，与
`preset_for_pruning` 取不到就退化为不清理同构。⚠️ 段名是**点分路径**（见下），
判前缀而非精确相等。

| `patch::writes`（wind-config，经 `config.applyPatch`） | Map 键（`custom_mappings` / `key_actions` 等）以**当前生效配置为种子**拼整表再覆盖用户层。降级时种子是出厂空表 ⇒ 用户真实的自定义标点映射被整表抹掉。P1 之后这条**可由第三方触发**：定制者在 `data_custom/config.toml` 把该键写成错类型，该定制版每个用户每次 load 都降级 |

★ **这是同一形状的第三条**（P0 只找到前两条）。判据要固化成规矩而不是逐条打地鼠：
**凡是「拿 `Config::load()` 的结果当种子、再整表写回用户层或导出给用户」的路径，
降级时都会把用户数据抹掉**。新增此类路径必须过闸，并有守门测试。

未加闸但已确认安全的：`prune_user_config` / `set_user_value`（`system_preset_value`
走生 `toml::Value`，根本不反序列化，不变量 1 不破）；`keys_overview`
（wind-webdata）只影响显示、不写盘，留待 P3 一并处理。

### 定制层里不建议声明 `[keys] key_actions`（P3 的 CLI 要检出）

`materialize_key_actions` 会把 L1⊕L2⊕L2.5 折算后的绑定**一次性物化**进用户层并打死
版本标记。定制者第一版包里写的 `key_actions` 会被固化到存量用户的 `%APPDATA%`，
**此后定制包再改这些绑定对存量用户永远不生效，且无任何日志**。

这与 L2 的 `trigger_keys` 同构、不是 data_custom 引入的，但对第三方完全不可发现。
P3 的 `config check --custom` 要把它列为「定制层里不建议声明」的键，制作指南写死。

**★ 降级粒度：顶层段 + 再递归一层子表**

`Config` 只有 7 个顶层段，键数极不均衡：ui 99、schema 88、input 67、keys 25、
stats/mobile/debug 各 3。只按顶层段降级的话，`ui.font.scripts` 一个坏值就让候选窗
尺寸、字体、主题、工具栏、注释模板…99 项全回出厂——离「一切归零」并不远，
与本功能「缩小爆炸半径」的目的相悖。

故对**判定为坏的顶层段**再用同样的探针探它的直接子表，**只递归这一层**
（无限递归收益递减且复杂度上升）。坏值直接落在该段标量键上时退回整段降级。
`degradation.sections` 因此是点分路径（如 `ui.font`）。

**守门测试**（`wind-config`）：

- 构造一份含类型不匹配键的用户层 → `load()` 成功，坏段回落默认，**其余段的用户值保留**
- 幂等：连续 load 两次结果一致
- 反事实：摘掉降级逻辑后该用例精确变红（不是「恰好绿」）

**⚠️ 已知的隐蔽触发路径**（写进测试用例）：Map 类型字段是「未知键免疫」的例外。
`input.punct.custom_mappings`（`HashMap<String, Vec<String>>`）、`keys.key_actions`、
`keys.session_actions`、`scripts` 这几个字段对 serde 而言**任何键都是已知的**，
旧版残留项不会被忽略，会被当真数据反序列化。全仓无 `deny_unknown_fields`、
无 `flatten`、无 `untagged`，所以**普通 struct 字段被删除时的残留键是零风险的**——
排查时不要往那个方向找。

### P1 — data_custom 层接线

#### P1a 层解析收口

新增 `wind-config`：

- `custom_manifest() -> Option<CustomManifest>`（OnceLock 缓存，判据 = `custom.toml` 可解析）
- `custom_data_dir() -> Option<PathBuf>`（manifest 在场时才返回 Some）
- `resource_layers() -> Vec<PathBuf>`：`[user?, custom?, data?]` 有序列表

`resolve_overridable` 由「两级」改为「遍历 `resource_layers()`」。
`resolve_data_file` / `resolve_schema_resource` 是它的两个薄包装，自动继承。

> ⚠️ **原文曾把 `resolve_schema_file` 也列进「自动继承」，这是错的**（P1 实施时证伪）。
> 它在 `wind-engine/src/manager.rs`，是**另一份独立的两层实现**，自己拼
> `user_config_dir()/schemas/` 再回落 `data_dir/schemas/`，不经过 `resolve_overridable`。
> 详见 P1d。

#### P1b config.toml 四层合并（★ 本计划最危险的一步）

`Config::load` 在 L2 与 L3 之间插入 L2.5。同时——**这一条不能漏**——
`system_preset_value` 的「出厂默认」定义必须同步扩成 **L1⊕L2⊕L2.5**。

理由：`preset_for_pruning`（config.rs:4692）与 `materialize_key_actions`
拿这个值去**删用户层的键**。定制层不进入这个计算的话：

- 用户在定制版里把开关点到「定制默认」位 → 被判定为「与默认不同」→ 永久钉死，
  此后不跟随定制层的任何更新；
- 反向算错则**静默删掉用户真实设置**。这颗雷本仓已引爆过一次（实测一份真机配置
  105 键中 62 键冗余，`auto_commit_block_on_pinyin` 已经中招）。

`preset_for_pruning` 现有的闸门是「`data/config.toml` 在场」，加层后闸门语义变成
「data 层在场」——`data_custom` 单独在场而 `data/config.toml` 缺失时**必须返回 None**
（退化为不清理），不能拿残缺的 preset 去删键。

**守门测试**：

- 四层合并结果逐键断言；`prune` 前后 `load()` 结果**逐键完全相同**（沿用现有不变量）
- `data_custom` 在场时 `system_preset_value` 含 L2.5 值
- `data/config.toml` 缺失 + `data_custom` 在场 → `preset_for_pruning` 返回 None

#### P1c compat.toml 三层

`app_compat.rs` 现为「系统层 + 用户层，同名进程整条覆盖」，插入 custom 层为中间层。
⚠️ `merge_rules` 与 `[[apps]]` 是**两段独立合并**（刻意不合并成一个字段），加层时
两段各自加，不要顺手合并。

#### P1d 枚举点接线（**漏一处 = 静默降级**）

单文件读取靠 `resolve_*` 自动继承，但**目录枚举各有一份自己的列表**，必须逐个改用
`resource_layers()`：

| 落点 | 说明 |
|---|---|
| `manager.rs:1611` `scan_dirs`（`installed_schemas`） | 方案枚举，两目录合并去重 |
| `manager.rs:1719` `shuangpin_layouts` | 目录列表，靠前同名 stem 胜出 |
| `handle_mode.rs:756` `theme_search_dirs` | 主题搜索链 |
| `webdata/lib.rs:3035` `theme_dirs` | 设置页主题列表（与上一条**是两份**） |
| `coordinator.rs:1737` `join("opencc")` | 简繁数据目录（实施时改为**按名逐文件**解析，见下） |
| `coordinator.rs:1807` `join("themes")` | 主题根 |
| `wind-mobile/lib.rs:528` `read_dir(join("schemas"))` | 移动端方案枚举 |

**守门测试**：一条 grep 式测试钉住「除 `resource_layers()` 外不得新增裸
`join("schemas")` / `join("themes")`」，否则下一个功能又会漏接一处。

#### ★ P1 实施时发现的两处独立实现（不在上表里，且都不会自动继承）

**(a) `resolve_schema_file`（`wind-engine/src/manager.rs`）——最要紧的一处**

方案文件的**唯一入口**：`read_schema`（`{id}.schema.toml`）与双拼布局
（`shuangpin/{id}.toml`）都走它。它自己拼两层路径，不经过 `resolve_overridable`。

⇒ **不改它，`data_custom/schemas/*.schema.toml` 完全不可见**，而方案正是定制者要
替换的头号资源。现象是「我把虎码方案放进 data_custom 了，程序当没看见」——
静默降级，无任何日志。

改法：**就地改成遍历层序**，不要换成 `resolve_schema_resource`。两者契约不同——
前者返回 `PathBuf`（找不到时返回安装目录路径，由调用方报加载失败），后者返回
`Option<PathBuf>`。换过去会改变「找不到时返回什么」。

**(b) 词库路径解析（`wind-engine/src/manager.rs`）——不能套单趟循环**

它不是「逐层找同一个文件」，而是**两趟**：第 1 趟在各层找 `.yaml`，第 2 趟才在各层
找「有同名 `.wdat` 兄弟」的。遮蔽判定也不同（安装侧只投 wdat 时，用户的 yaml
仍算覆盖）。

⚠️ 天真地改成一趟 `for base in resource_layers()` 会**反转语义**。反例：用户层只有
`.wdat`、data 层有 `.yaml` ⇒ 现行正确结果是 **data 层的 yaml 胜出**（第 1 趟就命中），
单趟逐层则 user 的 wdat 先命中。加 custom 层后这类组合只会更多——定制者只投
`.wdat` 是很自然的做法，那正是他们预编译词库的产物。

改法：**保持两趟结构，每趟内部各自遍历层序**。

⚠️ 该函数收的是已经到 `.../schemas` 那一级的路径，不是数据根。要么在调用方把层序
转成 schemas 级列表传进来，要么改签名收数据根。**不要**在函数内部拿 `parent()` 反推
数据根——那是把「层的兄弟关系」埋进一个反推里，正是本仓反复栽过的隐式契约。

**(c) API 缺口**：`resource_layers_with()` 只返回路径，而日志需要层名
（`log_layer_override` 要 user/custom/data）。P1d 需要一个带层名的公开形态
（如 `ResourceLayer { name, path }`），否则每个枚举点都要自己猜层名。

#### ★ P1d 实施结论

**层名 API 落成 `ResourceLayer { name: &'static str, path: PathBuf }`**（含 `sub()` /
`is_user()`），`Config::resource_layers_named[_with]()` 公开返回它；
`resource_layers[_with]()` 保持返回纯路径，给不关心层名的调用方。层名不只喂日志——
主题列表的 builtin 标记、P2 的「方案可不可删」都要靠它，靠路径前缀猜会在便携版
/自定义 data 目录下猜错。

**上表七处之外，另有四处同类落点**（本次一并接线，否则同样是静默降级）：

| 落点 | 不改的后果 |
|---|---|
| `manager.rs` `load_sentence_freq_dict` | 直接 `schemas_dir.join("pinyin/rime_frost.dict.yaml")`。定制版换了拼音词库，码表整句的**词频来源**仍读 data 层，现象只是「排序有点不对」，无日志 |
| `handle_mode.rs` `list_themes_full` 的「程序目录」扫描 | 只扫 data 层 ⇒ 定制版主题进得了搜索链却**不进设置页/右键列表**，现象是「主题在包里，界面上没有」 |
| `wind-webdata` `system_schemas_dir()` → **`system_schemas_dirs()`** | 它喂给 `wind_transfer::scheme::{export_package, delete_package}` 当「系统目录」。只传 data 层时，只存在于 `data_custom` 的方案被 `locate` 判成 `Missing` ⇒ 导出成功但包是空的/全进 missing，**用户拿它装不回去**。改为传「非 user 的每一层」：定制层与出厂层同类——导出一并打包（自包含），删除永不触碰（判成 `User` 就等于允许程序删 `data_custom`，破不变量 3）。`scheme.rs` 的 `system_dir: Option<&Path>` 随之改为 `system_dirs: &[PathBuf]` |
| `apps/service/src/dict_cli.rs` `cmd_weight_check` | 单目录 `read_dir` + `schemas_dir.join(词库)` ⇒ `dict weight-check` 看不见定制层自带的方案，而它恰恰是给定制者查权重用的工具。**带 `--data` 时仍只看指定目录**（那个标志的语义是「体检这个目录」，混进 `%APPDATA%`/`data_custom` 会让结果对不上用户所指的那份数据） |

顺带把 `wind-webdata` 的 `theme_dirs()` 改为直接复用 `theme_search_dirs()`——那两份
本就逐字重复，留着就是下一次分叉的种子。`WebDataHost::themes_dir()` 随之删除
（它唯一的用途就是让人再拼一份两层链）。

**`opencc` 按名逐文件覆盖**（§2 表格写的「搜索链按名覆盖」是对的）。实现走
`Converter::load_variant_resolved(variant, resolve)`：链里每本 octrie 各自经
`resolve_data_file(data_dir, "opencc/<名>.octrie")` 逐层解析。

> ⚠️ 这里曾一度实现成「逐层探测、首个能建出链的目录整份胜出」，理由写的是「半份定制
> 半份出厂会拼出一条没人验证过的链」。**那个理由不成立，且已实测出静默失效**：定制者
> 只想改几个词组的繁体写法，往 `data_custom/opencc/` 放一本 `STPhrases.octrie`，输入
> 「简体字转换测试」**一个字都不转**。机理是 `load_variant` 的组内判据只要求「至少一本
> 加载成功」（`if !group.is_empty()`），于是残链被当成胜利，日志只有一行「命中定制层」，
> 看不出链是残的；`STVariants.octrie`（以词定字的繁体变体）也随之整份消失。而且那一版
> **把风险新引入了用户层**——改动前 opencc 只看 data 层，整份胜出之后 `%APPDATA%\WindInput\opencc\`
> 里一个残留目录就能顶掉出厂链。
>
> 每本 `.octrie` 由 `gen_opencc` 各自独立生成，`Converter` 的组合方式（组内最长匹配、
> 组间串行）就是 OpenCC 自己的组合模型，跨层按名取文件正是它设计上支持的事。
> 「整份胜出 + 完整性闸门（缺一本就跳过该层并 WARN）」是治标：定制者放半套时仍然不
> 工作，只是多一行 WARN——而按名覆盖下，放半套本来就该正常工作。
>
> 守门：`wind-transform` 的 `resolved_chain_falls_back_per_file_not_per_directory`
> 用自造的 octrie 夹具（不依赖 `build_dev`）复现这个组合，并对照断言「只看定制层的残链
> 一个字都不转」。
>
> ⚠️ 顺带修掉一个**长期静默跳过**：`s2t.rs` 测试模块的 `opencc_dir()` 写的是**两级**
> `../../build_dev/...`（= 不存在的 `wind_input/build_dev/`），于是那几个依赖真实 opencc
> 数据的用例一直走「跳过」分支、计数照常绿。改成三级（仓库根）后它们真的在跑了——
> 同款坑 `wind-engine/tests/engine_manager.rs` 早已修正并在注释里记过，这里是漏网的一处。

**词库两趟结构**（(b) 那条）已按「每趟内部各自遍历层序」实现，并配了直接的行为测试
（`resolve_dict_file_three_layers_keeps_two_pass_semantics`），钉住那个反例组合。
`resolve_dict_file` 的 data 层取调用方传进来的 `schemas_dir`，user/custom 走
`resource_layers_named_with(None)`，**没有从 `schemas_dir` 反推数据根**。

**守门测试**：`wind-config/tests/resource_layer_gates.rs` 用「文件 → 出现次数 + 判定
理由」的清单钉住 `join("schemas")` / `join("themes")` / `join("opencc")`（opencc 的清单
**刻意为空**：改按名解析后全仓不该再有把 opencc 当目录拼的写法）。它钉不住的：只认这
三个字面量（换成常量或 `push` 即绕过）；不看调用上下文；跳过测试模块的判据是精确字面量
`#[cfg(test)]`，`#[cfg(all(test, windows))]` 一族**不跳过**（在那种块里写夹具会假红，
遇到假红要扩判据、别改数字）；闭合判据是缩进精确相等的 `}`，缩进对不齐会从
`#[cfg(test)]` 一路吞到文件末尾。

⚠️ 这里曾写过「写在测试模块之后的生产代码同样不计」——**已实测推翻**（跳过测试模块后
扫描回主循环继续，`log_rotate.rs` 里写在测试模块之后的函数照样计入）。一条假的「已知
盲区」比没有更糟：它让人相信一块其实有覆盖的区域没覆盖。

### P2 — 减法（hide）

**主拦截点：`read_schema`（`wind-engine/src/manager.rs`）对被 hide 的方案直接返回 `None`。**
釜底抽薪——`schema_supported`、`build_engine`、mix members 解析、special_modes
全部自动失效，不必逐处补过滤。

**上层列表过滤（双保险）**：`available_schemas`、`installed_schemas`。

**必须一并处理的连带情形**：

1. `schema.active` 指向被 hide 的方案（原版用户升级到定制版必然发生）
   → 降级到第一个可用方案，WARN 落日志，**不改写用户配置**
   （改写会让用户切回原版时丢失设置；`data_custom` 本就是可卸载的）。
2. mix `members` / `special_modes` 引用被 hide 的方案 → 该成员静默跳过 + WARN。
   这是定制者的配置错误，CLI 校验（P3）要能提前报出来。
3. ~~`is_user_schema` / `delete_user_schema` 的判据要扩~~ —— **已推翻，无事可做**。
   两者**刻意只查用户目录**（`resource_layer_gates.rs` 的清单已如此登记），故
   `data_custom` 里的方案本来就已经是 `is_user_schema() == false`、
   `delete_user_schema()` 直接 bail「内置方案不可删除」。
4. themes 的 hide 同理落在**主题搜索链的消费侧** + 设置页列表。

#### ★ P2 实施结论

**hide 是绝对的：被 hide 的 id 在任何层都不存在**，包括用户自己在
`%APPDATA%\WindInput\schemas\`（`themes\`）里放的同名文件。判据落在 `id` 上，
与「命中的是哪一层」无关（`Config::custom_hides_schema` / `custom_hides_theme`）。

理由是契约 5 的措辞「这个方案在本定制版里不存在」：若 hide 只对安装层生效，用户层放一个
同名文件就能让被删的方案复活，定制者意图落空，判定还得多带一个「哪一层」的分支。
**代价（不留白）**：用户无法用被 hide 的 id 给自己的方案/主题命名——他放的
`wubi86.schema.toml` 会连同被删的内置方案一起消失，现象与「文件没放对」难以区分。
故定制者应当只 hide 自己确实想删掉的**内置** id。这条取舍同时写在两个判据函数的文档注释里。

**落点**：

| 落点 | 角色 |
|---|---|
| `manager.rs` `read_schema` | 主拦截点。`schema_supported` / `build_engine` / mix 成员的 `ensure_schema` 门卫 / `overlay_modes`（特殊模式）全部自动失效 |
| `manager.rs` `available_schemas` | 双保险。**唯一独立生效的场合**是「构造/reload 的 `retain` 无条件保留活跃方案」那条旁路 —— 降级也找不到可用方案时，被 hide 的 active 会留在内部列表里 |
| `manager.rs` `installed_schemas` → `scan_layer_schema_ids` | **不是**正确性闸门（`schema_supported` 已经拦住了），作用是**降噪**：不过滤的话每个被删的方案都会在目录扫描时触发一次 `warn_hidden_schema_once`，把「定制者配错了」的告警变成「定制版正常工作」的噪音 |
| `manager.rs` `resolve_active_schema_id` | 活跃方案降级（构造与 `reload_from_config` 同源），只在内存里。★ 落点的挑法见下 |
| `wind-mobile` `scan_installed_schemas` | 移动端方案列表，与桌面同判据 |
| `handle_mode.rs` `list_themes_full` | 主题列表（随包分发那一段 + 用户层独有那一段，**两段各滤一次**） |
| `handle_mode.rs` `theme_id_honoring_hide` | 主题解析侧的统一裁决，被 `load_theme_with_fallback`（桌面 `push_theme`）与 `theme_query.rs` 的 `theme_palette`（移动端拉取面）共用 |
| `manager.rs` `overlay_index_of` | **只补 WARN，不改行为**。特殊模式是唯一不把 id 喂给 `read_schema` 的引用路径（注册表建自 `installed_schemas`，扫描侧已滤掉），于是分发点零日志。行为本身已经对：`BoundAction::Special` 拿到 `None` 后**不吞键、落普通输入**（引导键多是 `;` `/`，按下去就正常出符号），缺的只是一行能归因的日志 |

**告警节流**：`read_schema` 在热路径上，故「被 hide 的方案仍被引用」按 id 每进程只 WARN
一次（`warn_hidden_schema_once`）。这条信息是定制者的一次性配置错误，提前报出来归 P3 的
`config check --custom`。

#### ★ 降级落点怎么挑（审查退回过一版，别再简化成「第一个受支持的」）

第一版是「候选表过一遍 `is_supported()` 取第一个」，而候选表当时按字典序排。**实测落点是
`english`**（`'e' < 'h'`，英文方案抢在定制版自带的 `huma` 前面）：五笔用户装上虎码定制版，
首启工具栏显「英」、一个汉字都打不出。触发条件一点都不刁钻——`schema.available` 只有一项
的单方案用户很常见，而被 hide 的恰好就是他那一项。

⚠️ **「加一道 `[schema] hidden` 过滤」挡不住它**：`build_dev/data/schemas/*.schema.toml` 里
一个 `hidden` 都没有，英文方案也没标。判据必须是排序 + 类型。

现在的挑法：

1. 候选源 = `schema.available`（用户自己排过的顺序）+ 各层 `schemas/` 扫描结果，去重；
2. 扫描结果**按层序**（`user > custom > data`，层内按文件名序）——定制者自带的方案才是他
   意图中的替代品。为此 `scan_layer_schema_ids` 从 `ids.sort()` 改成层序去重，它原先那句
   「扫描顺序无关紧要」只对 `installed_schemas` 成立（那里自己再排一次）；
3. 按 `active_fallback_rank` **稳定**排序取第一个：`[overlay]` 方案直接出局（特殊模式是
   引导键瞬态进入、上屏即退出的小符号表，当常驻活跃方案等于打不了字），`english` 排最后
   （它不出汉字；排最后而不是排除——万一盘上真只剩它，能打英文仍好过没有）。稳定排序保证
   同档位内第 1、2 条的顺序原样保留。

**降级目标必须进 `available`**（`ensure_fallback_listed`，构造与 reload 共用）：单方案用户
那个场景里，`retain` 的两条判据——「等于活跃方案」（已降级成别的 id）与「受支持」（被 hide
的读不出来）——双双落空，`available` 会变成**空表**，方案菜单空白、循环切换键毫无反应。
只在确实降级过时补，否则会顺手改掉「用户的 active 本来就不在 available 里」这个既有合法
状态（`cycle_schema` 专门处理过它）。

**与本节原文的两处出入**（实施时证伪，据实记下）：

- 原文（及任务书）说 themes 列表是**两份**独立实现。**已不成立**：P1d 已把
  `wind-webdata` 的 `theme_dirs()` 改为复用 `theme_search_dirs()`、`web_theme_list` 改为复用
  `list_themes_full`。现在列表只有一份实现，设置页与右键菜单共用。
- 反过来多出一处原文没点到的搜索链消费者：`Coordinator::theme_palette`
  （`theme_query.rs`，移动端拉取式色表）**刻意不走** `push_theme` 那条链。不接它的现象是
  「桌面上换掉了、Android 上还是那个本该被删的主题」。已一并接线。

**守门测试**（四个独立测试二进制——`custom_manifest()` 是 OnceLock，每个进程只能有一种层状态）：

- `wind-engine/tests/custom_layer_hide.rs`：`ensure_schema` 拦下 / `installed_schemas` /
  `available_schemas`（含上表那个「唯一独立生效」的角落）/ `overlay_index_of`（特殊模式
  注册表）/ 用户层同名文件仍不可见 / **降级落点与 `available` 非空**（上一节那个故障）/
  **真读盘比对 `config.toml` 字节未变**
- `wind-coordinator/tests/custom_layer_hide_themes.rs`：列表两段 + 解析侧色表 + 用户层同名主题
- `wind-coordinator/tests/custom_layer_hide_mix.rs`：mix 成员跳过且其余成员照常
  （经 `debug_mix_members` 直通生产函数；被跳过的成员**在候选面上不可观察**）
- `wind-webdata/tests/custom_layer_hide_theme_import.rs`：被 hide 的 slug 导入被拒、不落盘

⚠️ **降级落点那几条判据互相兜底，夹具 id 是刻意挑的**：层序 / overlay 排除 / english 排最后
三条里任意一条单独失效，落点往往仍然正确，用例察觉不到。故 `zz_huma` 排在所有 data 层 id
之后（只有层序能让它赢）、`aa_ov_kept` 排在 custom 层最前（只有 overlay 排除能挡它）、
english 由「用户 available 里排第一」那个子场景负责。把 `zz_huma` 改回 `huma` 会让层序那条
判据静默失去覆盖。

⚠️ 上列第一条与第三条**依赖 `build_dev/data` 的真实词库**：正向对照要求没被 hide
的方案**真的**能构建出引擎——自造的空词库方案 `ensure_schema` 恒 false，两侧同因异果，
摘掉闸门用例照样绿（实施时先写成那样，反事实当场证伪）。⚠️ 这两条的跳过**不能靠耗时判**：
`.wdat` 缓存热时整条用例只跑 0.0x 秒，与跳过分支无从区分（本仓词库测试族的惯用判据在这里
不成立）。第二条（主题）夹具全自造，不受此限。

**已知不过滤的两处**：

- `dict weight-check`（`apps/service/src/dict_cli.rs`）直接扫目录，定制版里被 hide 的方案
  仍会被它体检到。这是给**定制者自己**用的诊断工具，「盘上有什么就体检什么」正是它该有的
  语义（同 `--data` 那个标志的取舍），且它不影响输入法任何行为。
- **主题的 `base` 继承链**：`wind_theme::load_merged_dirs` 解析 `base = "msime"` 时直接按
  目录找，不过 hide。定制者 hide 掉 `msime`、而某个保留主题写着 `base = "msime"` 时，
  msime 的色值照样经继承生效。**刻意不改**：当前行为更宽容（不连累那个无辜的派生主题——
  拦掉的话它会整个加载失败），代价只是与「在任何层都不存在」的措辞不严格一致。真要收紧
  得先想清楚「基底缺失时派生主题怎么办」，那不是减法要解决的问题。

**用户唯一能主动撞上被 hide 的 id 的入口**是主题导入 RPC（`web_theme_import_text`），
那里**已拒掉**并给出「换个 id」的提示——放行的话文件写下去了、回执是 `ok: true`，
但它永远不进列表、选它也会被 `push_theme` 兜底掉，用户只看到「导入成功了却哪儿都找不到」。
（方案包导入是同一形状，归 P3。）

### P3 — 可观测性与定制者工具

1. **日志**：`log_user_override`（config.rs:5357）现在只分「覆盖生效/未生效」，
   加层后必须打出**命中的是哪一层**；启动时打一行 manifest 摘要
   （`定制版 huma-edition 1.2（基于 0.9.30）`）。
   ⚠️ 遵守日志隐私约束：INFO 及以下不得含用户数据，路径按需降级到 DEBUG。

2. **身份暴露**：`capabilities::generate`（wind-rpc）或独立 RPC 方法暴露定制版
   `id/name/version/base_version`，设置页「关于」显示。**没有这个，定制版用户报障时
   连他装的是不是定制版都判断不出来。**

3. **校验 CLI**（本阶段性价比最高的一项）：

   ```
   wind_input config check --custom <data_custom 目录>
   ```

   落点 `apps/service/src/config_cli.rs`（`config` 子命令有**离线降级**，
   定制者不必装服务即可跑）。输出三类：

   - **已移除**：当前版本不再存在的键（warn，可安全删）
   - **类型不符**：会触发段级降级的键（error，附期望类型）
   - **值域非法**：枚举值不在合法集合内（error，附合法值域）

   判定依据用现成的 `config_schema::registry()` / `is_known_key` / `FieldType`。
   ⚠️ **遇 `Map` / `StructList` 类型的键就地停住**，不要继续下钻——自定义标点映射的
   子路径是**伪键**，整个 map 才是一个配置项，下钻会把用户的映射表逐条报成未知键。

   同时校验 manifest：hide 掉的方案是否仍被 mix members / special_modes 引用；
   `base_version` 与当前版本的差距。

4. **文档与跨仓契约**：

   - 文档站 `WindInputDocs` 新增「定制版制作指南」：层序、manifest 格式、
     「不要改 data，只写 data_custom」、「只写差异键」
   - `wind-installer`：确认覆盖式解包不会删 `data_custom`（当前只有 legacy 迁移会删东西，
     应当安全，但需显式测试）；卸载时 `cleanup.rs` 的清理范围要覆盖它
   - ⚠️ 这是**跨仓单向契约、无编译期约束**，两侧各写各的测试证明不了接线。
     必须有一个**真的建出 `data_custom` 再跑完整解析**的端到端用例。

**★ 跨仓核实结论**（只读核过 `../wind-installer`，未改那个仓一个字节）：

`data_custom` 这个名字在安装器里**零命中**——它是纯新增、未接线的。而两条契约**都已经
成立**，靠的正是安装器两条路径都与名字无关：

| 契约 | 结论 | 证据 |
|---|---|---|
| 升级/覆盖安装不删 `data_custom/` | ✅ 成立，且比原文的说法更强 | `installer/plan.rs:16-88` 是安装计划的唯一真相；解包前四个前置步骤里只有 `CleanupLegacy` 碰文件系统，而它是**逐条点名的黑名单**（`installer/legacy.rs:14-42`），没有递归扫描或通配。更进一步：本仓 `config/app.toml:29-30` 的 `legacy_files`/`legacy_dirs` **都是空的**，`plan.rs:40` 的门控使 `CleanupLegacy` 当前根本不入计划。`PrepareArchive`（`steps.rs:153-157`）只 `create_dir_all`、无前置清空；`ExtractFiles`（`steps.rs:170-188`）只正向遍历**包内条目**，从不 `read_dir(install_dir)` 反查差集 |
| 卸载清得掉 `data_custom/` | ✅ 成立，不残留 | `uninstaller/cleanup.rs:242-246` 是**整目录递归删**（`remove_dir_all(install_dir)`），不是按清单逐项。前面那段逐项删（含显式点名的 `data/`，`cleanup.rs:215-218`）只是为这一刀解锁的预处理。GUI 路径还有第二道：卸载器把自己复制到 `%TEMP%` 后回头再删一次（`uninstaller/selfdelete.rs:48-53`） |

⚠️ **一个既有缺口（不是 `data_custom` 引入的，知晓即可）**：`cleanup.rs:242` 的
`remove_dir_all` 失败时兜底是 `schedule_delete_on_reboot(install_dir)`，而 `MoveFileExW`
**对非空目录无效**——安装器自己在 `legacy.rs:45-47` 的注释里写明了这一点，并为此写了递归
排队的 `schedule_dir_on_reboot()`，但那个函数**只在 legacy 清理里用了，`delete_install_files`
没有用**。所以若 `data_custom/` 下有文件被占用导致整目录删失败，重启删除的兜底对它无效。
同样的缺口对 `data/` 也存在（`cleanup.rs:216` 失败仅 `eprintln!`），故不是本功能的新问题；
且 `data_custom` 按定义是主程序只读的内容，被独占锁的概率很低。
改法是现成的（把兜底换成已有的 `schedule_dir_on_reboot()`），但那影响**所有**卸载路径，
值得单独一轮验证，**刻意不在本计划里顺手做**。

⚠️⚠️ **由此产生一条跨仓约束，写在这里因为它约束的是本仓的人**：
`wind-installer` 的 `config/app.toml` 里，`legacy_dirs` **永远不能出现 `data_custom`**。
那是唯一能让升级删掉定制层的入口。它与 `datadir.conf` 同类——**跨仓单向契约，无编译期
约束**，两侧各写各的测试都证明不了它。

#### ★ P3-3 实施结论：`config check`

**形态**（`apps/service/src/config_cli.rs` 的 `cmd_check` + 子模块
`apps/service/src/config_cli/custom_check.rs`）：

```
wind_input config check [--custom <data_custom 目录>] [--data <data 目录>]
```

两个目录都可省略（回落到本机安装的那一份），但**打包前自查的正式用法是显式给两个**——
定制包那时还没装到任何机器上。`config` 有离线降级这一条已核实**仍然成立**：CLI 在
`main.rs` 的单例检查**之前**被拦截，`check` 全程不连 core、不读用户层、一个字节都不写盘。
子模块而不是塞进 `config_cli.rs`：新增 `mod` 要改 `main.rs`，而 `config_cli.rs` 自己
`mod custom_check;` 就能落在 `config_cli/` 目录下。

**退出码**：0 = 无错误（只有警告也是 0）／1 = 检出错误／2 = 用法错误。与 `cmd_export`
同一套（2 留给用法错误，1 是「用法没问题但拒绝/不通过」）。警告刻意不影响退出码——
它们说的是「现在能用、下次升级会坏」，拿它卡住打包流程弊大于利。

**核心是纯函数** `check_layer(custom_dir, data_dir: Option<&Path>, app_version) -> Report`：
不读环境变量、不读 `%APPDATA%`、不碰 `Config::load()` / `custom_manifest()`（后者是
OnceLock + 安装根）。理由不只是可测试性——**体检的对象是「这个包发出去之后会怎样」，
定制者本机的个人设置不该影响结论**，否则同一个包在两台机器上会体检出两种结果。
这条不变量由 `check_never_touches_the_user_layer` 的禁用词清单钉住。

##### 检出的类目

| # | 类目 | 级别 | 判据 / 出处 |
|---|---|---|---|
| 1 | 目录不存在 | error | `is_dir()` |
| 2 | 缺 `custom.toml` | error | 判据是**文件在场**，不是目录在场（契约 2）。整层被忽略且**无任何日志** |
| 3 | 清单语法错误 / 字段类型错 | error | 两条都是「整个定制层不启用」，最该第一时间报 |
| 4 | 清单未知段 / 未知键 | warn | 清单刻意无 `deny_unknown_fields`，`[schema] hide`（少个 s）这类拼写错误只能在这里报 |
| 5 | 身份缺失（`id` / `version`） | warn | 没有它，报障时分辨不出用户装的是哪个包 |
| 6 | `base_version` 与当前版本差距 | warn | 只比**主.次**两段，见下 |
| 7a | 定制层 `config.toml` 语法错误 | error | 整份被跳过，本层配置差异一条都不生效 |
| 7b | 定制层 `config.toml` **存在但读不出来** | error | 编码不是 UTF-8（记事本另存为 ANSI/GBK）、权限、被占用。后果同 7a，而运行时只有一行 INFO |
| 8 | 已移除 / 未登记的键 | warn | `field(key)` 未命中 |
| 9 | 类型不符 | error | `validate` 的 `TypeMismatch`。**后果最重**：段级降级会让这份包的每个用户每次启动都丢掉那一整段 |
| 10 | 值域非法 | error | `validate` 的 `EnumOutOfRange`，附合法值域 |
| 11 | 配置段被写成标量 | error | 前缀底下有已登记的键、值却不是表 ⇒ 同样整段降级 |
| 12 | Map 的键名越出值域 | warn | 仅对 `FieldType::Map(非空)`（如 `ui.font.scripts`），越界键被静默丢弃 |
| 13 | 定制层声明了 `key_actions` / `trigger_keys` | warn | 静默陷阱之一，见下 |
| 14 | 与出厂值相同的冗余键 | warn | 「只写差异键」（契约 6）落成可执行判据，见下 |
| 15 | hide 的目标在盘上不存在 | warn | 拼错的 id 完全无害地什么都不做，最难发现 |
| 16 | hide 的方案/主题仍被引用 | error | 见下「引用面」 |
| 17 | `opencc/` 里名字对不上的 `.octrie` | warn | 分**完全对不上**与**只差大小写**两种后果，见下 |
| 18 | ★ **真加载一遍**：Map / StructList 内部的坏值、越界的整数 | error | 注册表看不见的那一半，见下 |

##### 几处判据的取舍（都在实施时定下，别再回摆）

**`base_version` 只比主.次两段。** 补丁号差异是常态，每次小版本更新都告警一次，只会让人
把整个命令的输出当噪音略过。主/次版本变了才意味着「配置键可能改名或退役，值得复核一遍」。

**⚠️ 「已移除的键」不能断言「它会被忽略」。** 少数旧键仍被 `migrate_*_value` 一族读着
（`ui.candidate.comment_max_chars` 就是活的，`RETIRED_KEYS` 的文档注释里写明了「一个键
只要还有值迁移在读它，就不能进退役清单」）。措辞据此改成「多半是旧版本遗留，也可能是
拼错了；少数旧键还被兼容迁移读着，那是过渡措施，不保证长期有效」——报一个**方向**，
不替用户断言结果。

**冗余键检查的闸门 = `data/config.toml` 在场。** 拿不到出厂对照就**不做**这项检查，
而不是退而用 L1 默认凑合——L1 与 L2 差异很大，用 L1 当对照会把 data 层调过的键统统报成
「与出厂不同」，那是纯噪音。判据与 `preset_for_pruning`（取不到 preset 就退化为不清理）
同构。这条检查的价值在于把契约 6 从一句文档建议变成可执行判据：与出厂相同的键**现在**
不起任何作用，但主程序将来调整那个默认值时，它会把新默认顶住，现象是「新版本的改进在
定制版上没生效」，极难归因。

**⚠️ `Map` / `StructList` 就地停住，两道判据，关系是 ① ⟹ ②（子集）而**不是**等价。**

- ① `is_opaque_leaf(prefix)`：这个键登记为 `Map` / `StructList` ⇒ 整张表 / 整个数组就是
  一个配置项，里面是定制者的**数据**（标点符号、按键名、方案条目），不是配置项。
- ② 这个前缀底下一个已登记的键都没有。①的每个键都满足②，但②在①不成立的地方也会触发
  （未登记的整段 `[ui.oldsection]`），故②覆盖面更大——它顺带解决了「整段不认识的配置
  只报一次，而不是把段里每一行各报一遍」。

⇒ **行为上只有②被钉住**（摘掉①，全部用例仍绿；摘掉②，10 条红）。原文那句「两者恒同真、
摘掉任意一道都不会红」只对了一半，已订正；`collect_leaves_stops_at_map_typed_keys` 钉的是
合取，而**合取断言防不住两道判据分离**——分离恰恰发生在「①命中而②不命中」的键上，而那种
键当前一个都不存在。真正钉住②的是 `removed_section_is_reported_once`。

★ **①的判据必须是 `Map | StructList`，不能放宽成「登记过」。** ①排在②前面，将来注册表若
同时登记 `ui.font` 与 `ui.font.family`，「登记过」会让①先赢、把整张表当叶子吞掉，里面的
`family` 从此不再被校验——那正是错的那道。收窄之后语义才与「它是一个不透明叶子」相符，
排序在嵌套未来里也是对的（同 `config_schema::collect_leaf_keys` 的判据）。①被抽成具名谓词
`is_opaque_leaf`，由 `is_opaque_leaf_only_matches_opaque_types` **直接钉住这条收窄**——那是
①唯一钉得住的性质（放宽回 `field(prefix).is_some()` 时该用例精确变红，已反事实验证）。

**引用面（第 16 类）跑在 L1⊕L2⊕L2.5 上，刻意不含用户层。** 只看定制层自己的
`config.toml` 会漏掉绝大多数真实情况——`schema.available` 里那个被 hide 的方案通常来自
出厂 `data/config.toml`。当前覆盖的引用点：

- `schema.active` / `schema.available[i]` / `schema.primary_pinyin` / `schema.primary_codetable`
- `schema.mix_modes[i].members`（**`$primary_pinyin` 占位符要先解析**，否则漏掉一整类）
- `keys.key_actions.*` 与 `schema.codetable.z_key_action` 里的
  `special:` / `toggle_schema:` / `switch_schema:`（经 `BoundAction::parse`，不自己抄一份前缀解析）
- `ui.theme.name`（主题）

每条结论都算出**引用点住在哪一层**（定制层 / 出厂 / 内置默认），据此给不同的改法：来自
出厂的那些，改法是「在 `data_custom/config.toml` 里写这个键把出厂值盖掉——**别去改
`data/`**」，而不是让定制者去动出厂文件。**已知不覆盖**：方案级 `[key_actions]`
（`.schema.toml` 里那份）。

**opencc：半套目录不报错，名字对不上才报——而「对不上」有两种，后果不同。**

P1d 改成按名逐文件覆盖之后，只放一两本 `.octrie` 正是被支持的用法，报它就是报错了对的做法。
判据是「出厂 `data/opencc/` 里有没有同名文件」，不引入 `wind-transform` 依赖去问 `chain_for`。

⚠️ **原文把后果写反了一半，已订正。** 检测本身两种都抓得到（`list_file_names` 走 `read_dir`
拿磁盘真实文件名，`BTreeSet<String>` 精确比较），差别在措辞：

- **完全对不上**（`STPhrase` / `MyDict`）：链认的是固定的几个名字，链里不认识它 ⇒ 在**任何**
  平台上都永远取不到，程序照常用出厂那几本工作，现象是「换了词表却一个字都没变」。
- **只差大小写**（`stphrases` vs `STPhrases`）：加载侧 `resolve_overridable` 用 `p.is_file()`
  判定，`p` 是按出厂拼法 `opencc/STPhrases.octrie` 拼出来的——**在大小写不敏感的卷上它会被
  命中并加载**。本项目两个发行平台（Windows NTFS、macOS APFS 默认）都不敏感，所以
  「永远不会被加载」这句话对这一种是**假的**，只在 Linux 成立。措辞改成「现在能被取到，
  但那是在赌文件系统的行为；换到区分大小写的地方就取不到了」，并直接给出正确拼法。

**★ 第二道类型判据：把合并结果真的反序列化一次（第 18 类）。**

⚠️ **这是审查揪出的一个洞，正中本命令存在的理由。** `validate()` 对 `Map` / `StructList`
只做**一层形状判定**（`config_schema.rs`：`Map(_) => is_table()`、`StructList => is_array()`），
而 `collect_leaves` 又在这两类键上就地停住 ⇒ 表里的**值**再没有任何一处被检。运行时 serde
逐个值反序列化，一失败就是段级降级。实测的三种漏网：

| 定制层写法 | 注册表 `validate` | 运行时 serde |
|---|---|---|
| `[input.punct.custom_mappings]` `"," = "，"` | `Ok(())` | `invalid type: string, expected a sequence in \`input.punct.custom_mappings.,\`` |
| `[ui.font.scripts]` `latin = "Consolas"` | `Ok(())` | 同上 |
| `[ui.candidate]` `per_page = -1` | `Ok(())` | `invalid value: integer -1, expected usize` |

第一行就是最现实的失败场景：定制者写自定义标点，用最自然的写法 `"," = "，"`（值其实必须是
数组）。`config check` 打印「✓ 没有发现问题」，包发出去，之后**每个用户、每次启动**的
`input.punct` 整段回落出厂默认。

修法是 `check_deserialization`：把 `L1⊕L2⊕L2.5` 真的 `try_into::<Config>()` 一次。判据与运行时
完全一致，因为它**就是**运行时那条路径。三条必要的免责：

1. **先拿 `L1⊕L2` 单独试一次作对照。** 出厂 `data/config.toml` 自己就反序列化不了时，合并
   结果当然也不行——那不是定制者的错，把它栽给定制层是最坏的一种误报。对照失败就跳过本项
   并声明。没有出厂对照（`--data` 拿不到）时同样不做，理由与冗余键那条同构。
2. **前一道判据已经点名过的键不重复报。** 「已点过名」直接从 `rep` 里取（本文件、error 级、
   带键名的那些），**不手动维护一个集合**——手动维护的那种一定会在下一处新增检查时漏掉，
   实测就漏过 `collect_leaves` 报的「段被写成标量」，于是同一个键报了两遍。
3. ⚠️ **本函数跑的是裸 `try_into`，没跑 `Config::load` 的那批 `migrate_*_value`**（它们是
   wind-config 的私有函数，CLI 调不到）。其中**两条会就地改写已注册的键**：
   `migrate_index_labels_value` 把 `ui.candidate.index_labels` 从字符串改写成数组、
   `migrate_empty_code_behavior_value` 把非法枚举值改写成 `commit`。所以这两个键上
   「裸 try_into 失败、而实际 load 成功」是可能的。**但它们不会从这里漏成误报**：两者都是
   普通标量键，注册表那条（TypeMismatch / EnumOutOfRange）先一步点了名，免责 2 让本函数闭嘴。
   ⛔ **不要在 CLI 里复刻一份迁移名单**——那是第二个真相源，本仓反复栽过。

**已知局限（如实记下）**：靠迁移活着的旧格式（`index_labels = "1234"`、非法的
`input.*_behavior` 值）会被注册表那条报成 **error**，而实际 load 能救回来，级别偏严。措辞
本身没错（那两种确实都该改），只是重了一档。修它需要一份「谁还有迁移在读」的名单，
代价大于收益，暂不做。

**★ 「config.toml 存在但读不出来」必须与「不存在」分开（第 7b 类）。**

`read_toml` 原先是 `let Ok(text) = read_to_string(path) else { return Ok(None) }`——**任何**
读失败（非 UTF-8、权限、被占用）都退化成「文件不存在」，而调用方对「不存在」什么都不做。
现实的失败场景：中文定制者用记事本编辑 `data_custom/config.toml`（注释里有中文），另存为
ANSI/GBK ⇒ 不是合法 UTF-8 ⇒ 定制层的配置差异**一条都不生效**，运行时只有一行 INFO，而
`config check` 打印「✓ 没有发现问题」。清单那条路径一直是分开处理的，config.toml 这条漏了。
现改为 `TomlRead { Absent, Parsed, Unreadable, BadSyntax }` 四态；`Absent` 仍然静默——只做
减法的定制包本就不必有 config.toml。

**★ `--custom` 给了而 `--data` 省略时，绝不回落到本机安装的 data 目录。**

那会拿这台机器的出厂数据去对照别人的包：冗余键、hide 目标是否存在、opencc 文件名比对
三项检查全都会得出**与那个包不符**的结论，而抬头里印的出厂目录与 `--custom` 八竿子打不着，
人一眼看不出结论是错的。现改为在 `--custom` 的**同级**找 `data/`（`data/` 与 `data_custom/`
必须同级是本功能的硬契约，`variant::install_root` 刻意不拆成两个注入点正是这个理由），
找不到就是 `None` ⇒ 需要出厂对照的检查跳过并在抬头声明。`--custom` 省略时（体检本机装的
那份）才回落 `Config::data_dir()`。

##### 两张清单守门测试的登记（不登记 = 全仓测试红）

- `wind-config/tests/resource_layer_gates.rs`：新增文件出现了 `join("schemas")` /
  `join("themes")` / `join("opencc")`。判定是**刻意只看命令行给的那两个目录**，不走
  `resource_layers`——同 `dict weight-check --data` 的取舍。`OPENCC_SITES` 那张「刻意为空」
  的表因此破了例，注释里写明了理由：那句「空表」约束的是**加载**侧，本处是**诊断**侧，
  只列目录名做比对、不建 `Converter`，成立的前提恰恰就是「链按文件名跨层取」。
- `wind-config/tests/write_back_gates.rs`：`check_never_touches_the_user_layer` 里的禁用词
  字面量 `"Config::load("` 被那张清单的 grep 数成了调用点。登记为「**不是调用点**」而不是
  换个写法绕开——绕开正是那份文档警告过的事，而登记把这个假阳性记在了下一个人会看的地方。

##### 守门测试

`apps/service/src/config_cli/custom_check.rs` 的 `mod tests`，42 条，全部自造夹具
（`tempfile::TempDir` 建 `root/data` + `root/data_custom`），**不依赖 `build_dev/data`**：
本命令的判据只有「注册表 + 真加载一遍 + 盘上的文件名」，没有一条需要真实词库。

**反事实已逐类验证**（摘掉某类检查 ⇒ 对应用例精确变红，其余仍绿）：类型不符、值域非法、
已移除键、Map 不下钻、hide 引用、hide 目标不存在、`key_actions` 陷阱、冗余键、清单未知段、
`base_version`、opencc 名字（含大小写那一支）、段写成标量、Map 键名值域、清单缺失、
清单语法错误、清单字段类型、目录不存在、**反序列化探针整条**、**探针的免责 1（出厂对照）**、
**探针的免责 2（不重复报）**、**错误路径抽取认换行**、**config.toml 读失败分支**、
**判据①的收窄**——全部变红且指向正确的用例。

⚠️ **判据①（`is_opaque_leaf`）在行为上钉不住**（① ⟹ ②，摘掉①一条用例都不红）。能钉的是
它的**收窄**：`is_opaque_leaf_only_matches_opaque_types` 直接断言谓词只认 `Map`/`StructList`，
放宽回 `field(prefix).is_some()` 时该用例精确变红。这是审查退回过的一处，别再简化。

⚠️ **验证过程中揪出两条「摘掉也不红」的假绿**，都是「只数错误条数、不看措辞」造成的，
已改成断言具体措辞：

- `manifest_syntax_error_*` 原来只断言「有错误且提到『整个定制层不启用』」——而清单读不出来
  时还有一条「字段类型不对」的出口，措辞里同样有那句话，于是语法这一路被摘掉用例照样绿。
  现在必须点到「TOML 语法错误」这几个字。
- `missing_directory_is_error` 原来只断言 `errors() == 1`——摘掉目录判据后会落到「缺少
  custom.toml」那条上，条数不变、照样绿。现在必须点到「这个目录不存在」。

---

### 定制版制作指南（要点）

> 完整版归文档站 `WindInputDocs` 的「定制版制作指南」（另仓，本次未动）。这里先把**判据性**
> 的几条记下来，免得指南写出来之前它们只存在于代码注释里。

1. **层序是 `data < data_custom < %APPDATA%`。** 定制层能盖掉出厂值，但永远盖不过终端
   用户自己的设置——这是有意的：定制版是「换一套出厂配置」，不是「替用户做决定」。

2. **不要改 `data/`，只写 `data_custom/`。** 目录结构与 `data/` 同构，加法与整表替换
   **不需要在清单里声明**，把文件放进对应位置即可。这样主程序升级时 `data/` 整个被覆盖，
   而你的定制内容毫发无伤。

3. **`data_custom/custom.toml` 必须在场且能解析**，否则整个定制层被完全忽略（**不是
   「少了 hide 清单」，是连方案、主题、配置一起回落原版**），而且程序一切正常、日志里
   连 WARN 都没有。格式：

   ```toml
   [custom]
   id = "huma-edition"       # 稳定标识，日志/关于页/报障用
   name = "虎码定制版"
   version = "1.2"           # 定制包自身的版本
   base_version = "0.9.30"   # 基于哪个主程序版本定制

   [schemas]
   hide = ["wubi86", "wubi86_pinyin"]

   [themes]
   hide = ["msime"]
   ```

   清单只负责**减法**（`hide`）与**身份**。段名 `[schemas]` / `[themes]` 都是**复数**：
   写成 `[schema]` 解析得过、一个字都不起作用。

4. **`hide` 是绝对的**：被 hide 的 id 在**任何层**都不存在，包括用户自己在
   `%APPDATA%\WindInput\schemas\` 里放的同名文件。所以只 hide 你确实想删掉的**内置** id。

5. **只写差异键，不要整份复制 `config.toml`。** 差异越小，跨版本存活率越高。与出厂值
   相同的键现在不起作用，但主程序将来调整那个默认值时，你这份旧值会把新默认顶住。

6. **⚠️ 静默陷阱一：定制层里不要声明 `[keys] key_actions`**（`input.temp_pinyin.trigger_keys`
   / `input.temp_english.trigger_keys` 同理）。首次启动时程序会把折算后的按键绑定**一次性
   物化**进终端用户的个人配置并打上完成标记；此后你在定制包里再改这些绑定，对**已经装过
   旧版包的用户永远不生效**，且没有任何日志——只有全新安装的用户才拿得到新绑定。

7. **⚠️ 静默陷阱二：`data_custom/opencc/` 里的文件名必须与出厂目录逐字相同**（含大小写）。
   简繁转换链按**文件名**跨层取：只放一两本是正常用法（没放的那几本自动用出厂的），但
   名字对不上的那本会出问题，而且两种「对不上」后果不同——**完全对不上**（`MyDict`）在任何
   平台上都永远取不到；**只差大小写**（`stphrases`）在 Windows / macOS 上现在能取到，但那是
   在赌文件系统的大小写不敏感，换到区分大小写的地方（Linux、开了大小写敏感的 NTFS 目录、
   某些打包解包链路）就失效，届时现象是「同一个包在这台机器上换了词表、在那台上一个字都
   没变」。两种程序都照常工作，不会报错。

8. **⚠️ 映射表的值是数组，不是单个值。** `[input.punct.custom_mappings]` 里要写
   `"," = ["，"]` 而不是 `"," = "，"`；`[ui.font.scripts]` 同理（`latin = ["Consolas"]`）。
   写错的后果是那一整段配置在加载时被丢掉换成出厂默认，**每个用户、每次启动**都会踩到。

9. **打包前跑一次 `wind_input config check --custom <目录> --data <目录>`。** 上面这几条
   连同配置键的类型/值域、以及「把你的定制层真的加载一遍」的结果，它都会当场报出来，
   并给出该改哪个文件的哪个键。`--data` 省略时会自动找 `--custom` 同级的 `data/`。

---

## 4. 不变量与风险

| # | 不变量 | 破坏后的现象 |
|---|---|---|
| 1 | `prune` 前后 `load()` 结果逐键完全相同 | 用户设置被静默改写 |
| 2 | `data/config.toml` 缺失时 `preset_for_pruning` 返回 None | 拿残缺 preset 删用户键 |
| 3 | 程序永不写 `data_custom` | 定制者资产被改坏，且他无从察觉 |
| 4 | 被值迁移读取的键不进 `RETIRED_KEYS` | 先删键、下次启动迁不到，用户配的值归默认 |
| 5 | `hide` 与 `[schema].hidden` 保持正交 | 被隐藏方案仍被 mix/active 引用到 |
| 6 | 段级降级必须 WARN（P0）且在 UI 可见（P3） | 掩盖真实的迁移缺失，故障静默化 |
| 7 | 降级后所有「整表覆盖 / 导出」下游必须闸掉 | 降级本身变成磁盘上的永久数据丢失 |
| 8 | 折算型数据的降级判据须覆盖**全部来源路径** | 把出厂默认当成用户的真实绑定展示 |

### 两条实施期确立、只记在这里的判断

**① 不为「INFO 级定制版摘要有且只有一处」加 grep 守门测试。**
本仓现有的两条 grep 守门（`write_back_gates` / `resource_layer_gates`）守的都是**会丢用户
数据**的东西，威慑力正来自「红了就意味着你可能在毁数据」。为「日志多打一行」再加一条同形
测试会稀释这个信号——下一个人看到 gates 类变红时，第一反应会从警觉滑向不耐烦。日志重复的
最坏后果只是读日志的人误以为加载了两次，`config.rs` 与 `main.rs` 两处注释已经拦着。
⇒ 这条改动**没有守门测试兜着**，是知情的取舍，不是疏漏。

**② 不变量 8 的来源。** `keys_overview` 的 session 表是**折算**产物，来源有五个字段
（`session_actions` / `page_keys` / `select_char_keys` / `highlight_keys` /
`select_key_groups`）。实施时判据只问了同名子表，理由写的是「标量键探针定位不到子表、会
整段记 `keys`」——**恰好反了**：`narrow_bad_section` 对坏段的每个直接子键都做探针，不区分
子表与标量。实测 `[keys] page_keys = 5` ⇒ `sections=["keys.page_keys"]`，同名子表判据
够不着。⇒ 用户写 `[keys] page_keys = "brackets"`（组名列表长得像单值，这是最自然的手误）
就会看到一张**折算自出厂组名**的按键表，且标记为 `null`。
该判据的前提（「降级粒度对段内标量/列表键同样成立」）住在另一个 crate，已单独钉成测试。

**已核实为安全、不必额外处理的**：

- 词库缓存指纹（`cache_fp.rs` 的 `fingerprint`）按**文件内容**哈希，不是路径/mtime，
  custom 层替换词库后缓存自然失效。**缓存撞名已于 P1d 核实：安全**，三条各自成立——
  ① 缓存名 = `<父目录名>/<文件干>.<ext>`（`manager::cache_path`、
  `wind-reverse::comment_cache_path` 同构），**层前缀不进名字**，故三层共用同一个缓存名；
  而同一时刻 `resolve_dict_file` 只解析出**一个**路径，共用者至多一个，撞不上；
  ② 新鲜度判据是内容指纹（+ 解析语义版本 + tag），换层即内容变即重建；`.wridx` 的
  `derived_cache_is_fresh` 更把**源文件全路径**编进摘要，字节完全相同的换层也会失效；
  ③ wdat-only 的层（定制者最可能的分发形态）直接 mmap 层内的 sidecar，根本不经过缓存根。
  唯一残留的撞名是**与分层无关的老问题**：两个不同 rel 若父目录名与文件干都相同
  （`a/wubi/x.dict.yaml` 与 `b/wubi/x.dict.yaml`），缓存名相同——`data_custom` 不新增此类组合。
- 单层 TOML 语法错误已隔离（`read_toml_value` 返回 None，只跳过那一层）。

## 5. 明确不做的

- **`schema_overrides/` 的 custom 层**：方案覆盖本就是设置页写给用户的，
  定制者要改方案基线应直接提供 `.schema.toml`，不需要再叠一层 override。
- **`data_custom` 的自动清理/迁移**：违反契约 3。旧键只做「读时忽略 + 日志告警 + CLI 校验」。
- **用户在 GUI 里生成 data_custom**：定制版是打包分发的产物，不是运行时功能。
