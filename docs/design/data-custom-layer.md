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

**主拦截点：`read_schema`（manager.rs:3190）对被 hide 的方案直接返回 `None`。**
釜底抽薪——`schema_supported`、`build_engine`、mix members 解析、special_modes
全部自动失效，不必逐处补过滤。

**上层列表过滤（双保险）**：`available_schemas`（1577）、`installed_schemas`（1599）。

**必须一并处理的连带情形**：

1. `schema.active` 指向被 hide 的方案（原版用户升级到定制版必然发生）
   → 降级到第一个可用方案，WARN 落日志，**不改写用户配置**
   （改写会让用户切回原版时丢失设置）。
2. mix `members` / `special_modes` 引用被 hide 的方案 → 该成员静默跳过 + WARN。
   这是定制者的配置错误，CLI 校验（P3）要能提前报出来。
3. **`is_user_schema` / `delete_user_schema`（manager.rs:1995）的判据要扩**。
   现在是「存在于用户目录 = 可删」。`data_custom` 里的方案属于**定制版内置，
   不可删**——否则用户删掉后 `data/` 里的原版又冒出来，现象是
   「我删了五笔，五笔自己回来了」。
4. themes 的 hide 同理落在 `theme_search_dirs` 的消费侧 + 设置页列表。

**守门测试**：

- hide 掉的方案不出现在 `installed_schemas` / `available_schemas` / 设置页列表
- `schema.active` = 被 hide 方案 → 降级成功且用户配置文件未被改写
- `data_custom` 方案的 `is_user_schema() == false`、`delete_user_schema()` 报错

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
