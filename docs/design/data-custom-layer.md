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
  降级只是兜底。日志必须 WARN 级且在设置页可见，否则会掩盖真实的迁移缺失。
- 降级信息要能被 RPC 读到（设置页/关于页显示「keys 段配置解析失败，已回落出厂值」）。

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

`resolve_overridable`（config.rs:5367）由「两级」改为「遍历 `resource_layers()`」。
`resolve_data_file` / `resolve_schema_resource` / `resolve_schema_file` 自动继承，
这三个函数覆盖了绝大部分单文件读取——**这是本计划改动量小的原因**。

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
| `coordinator.rs:1737` `join("opencc")` | 简繁数据目录 |
| `coordinator.rs:1807` `join("themes")` | 主题根 |
| `wind-mobile/lib.rs:528` `read_dir(join("schemas"))` | 移动端方案枚举 |

**守门测试**：一条 grep 式测试钉住「除 `resource_layers()` 外不得新增裸
`join("schemas")` / `join("themes")`」，否则下一个功能又会漏接一处。

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
| 6 | 段级降级必须 WARN 且可见 | 掩盖真实的迁移缺失，故障静默化 |

**已核实为安全、不必额外处理的**：

- 词库缓存指纹（`cache_fp.rs` 的 `fingerprint`）按**文件内容**哈希，不是路径/mtime，
  custom 层替换词库后缓存自然失效。**但** `wind-reverse/lib.rs:648` 提示过
  「同一时刻 `resolve_data_file` 只解析出一个路径」，缓存文件**命名**若含路径派生，
  两层可能撞名——实施 P1d 时核一遍。
- 单层 TOML 语法错误已隔离（`read_toml_value` 返回 None，只跳过那一层）。

## 5. 明确不做的

- **`schema_overrides/` 的 custom 层**：方案覆盖本就是设置页写给用户的，
  定制者要改方案基线应直接提供 `.schema.toml`，不需要再叠一层 override。
- **`data_custom` 的自动清理/迁移**：违反契约 3。旧键只做「读时忽略 + 日志告警 + CLI 校验」。
- **用户在 GUI 里生成 data_custom**：定制版是打包分发的产物，不是运行时功能。
