# 引导键物化（key_actions materialization）

`keys.key_actions` 从「每次加载都由 `trigger_keys` 折算出来的派生表」升级为**唯一真相源**，
出厂绑定在服务首次启动时一次性写进用户层。此后用户删掉一个绑定就是真的删掉了。

- 落点：`Config::materialize_key_actions()`（`wind-config/src/config.rs`），
  服务启动流程 `apps/service/src/main.rs` 的 D3 步调用。
- 标记：`keys.key_actions_materialized`（版本号，0 = 未物化）。
- 相关：[key-resolver-unification.md](key-resolver-unification.md)（五c 全局层收编）、
  [session-key-actions.md](session-key-actions.md) §6（姊妹表的同类教训）。

## 1. 报障现场

> 每次覆盖安装后，临时拼音触发键都默认勾选了反引号，但取消勾选后点「应用设置」，
> 又会提示「没有变更」。……在 A 电脑取消勾选并备份设置，B 电脑还原后两处又成默认状态。
> 而且用反引号可以触发临时拼音，但取消勾选，应用设置又显示没有变更。

三个现象出自同一根因，缺一不可地串成一条闭环。

## 2. 根因：用户层没有任何地方能表达「我不要这个出厂绑定」

| # | 环节 | 代码 |
|---|---|---|
| 1 | 出厂绑定住在 L2 | `data/config.toml` 的 `[input.temp_pinyin] trigger_keys = ["backtick"]` |
| 2 | 每次 `load()` 都折算进 `key_actions` | `migrate_trigger_keys_into_key_actions()`，由 `normalize()` 调用 |
| 3 | 折算的唯一守卫是 `contains_key` | 它区分不了「用户配过别的」与「**用户删掉了它**」 |
| 4 | 设置页只写 `key_actions`，从不写 `trigger_keys` | wind-setting `manifest.rs` 的 `build_dialog_button_key_action` |

于是：用户在设置页取消勾选 → 用户层 `key_actions` 里那条**真的被删了** → 下次 `load()` 时
用户层没写过 `trigger_keys`，深合并回落 L2 的 `["backtick"]` → 折算发现 `key_actions` 里
没有 `backtick` → **重新插回去**。

「没有变更」来自 wind-setting `state.rs` 的乐观更新：保存成功后 `base_config = current_config`。
设置页认为自己已经保存成「无 backtick」，而 core 实际生效的是折算复活后的值——**设置页的
本地状态与 core 的生效值发生认知分裂**，用户再点一次保存就看到「没有变更」。

覆盖安装、备份还原都带不走这个意图，因为**它从来没有被写进任何地方**。

### 放大器（会让情况更糟，但不是根因）

- `set_user_value` 的「等于默认则删键」（见 [config-layer 写回收口](../redesign/config-schema.md)）：
  `keys.key_actions` 的出厂默认是 `{}`。若 `backtick` 是用户唯一的一条绑定，删掉后整张表
  = `{}` = 默认 ⇒ 整个键被 `remove_nested` 抹掉，连「我有一张空表」都留不下。
- 该路径**不报 `skipped`**，设置页照常显示「已保存 1 项变更」——零落盘、满分回执。

## 3. 与 session-key-actions §6 的关系（★ 先读这一条）

姊妹表 `session_actions` 在 2026-08-11 踩过**同一个坑**并已修好，判据写在
[session-key-actions.md](session-key-actions.md) §6：

> **折算属于「怎么解释配置」，不属于「配置是什么」。** 把视图写回存储就丢掉了用户的原始意图，
> 而设置页读的正是存储。

那次的现象清单里就有「在高级表里删掉一条折算来的绑定，下次启动又被折算回来，**删不掉**」——
与本次报障一字不差。`session_actions` 的解法是把折算改成纯函数视图
（`KeysConfig::effective_session_actions()`），**存储层 `page_keys` 保持原样**。

### 为什么 `trigger_keys` 没有照搬那个解法

两族的分野只有一处：**设置页的写入口落在哪一侧**。

| | 出厂值住哪 | 设置页写哪 | 结论 |
|---|---|---|---|
| `page_keys` / `highlight_keys` | `keys.page_keys` | **同一个键**（数组整体覆盖） | 健康：删除意图能表达 |
| 五处 `trigger_keys` | `input.*.trigger_keys` | **折算的下游** `keys.key_actions` | 病：上下游错配 |

⇒ 可以照搬 §6 的解法（把设置页改回直接写 `trigger_keys`、折算降为视图），但那等于**撤销
五c 收编**：五个控件各写各的字段，「同一个键在两处配」的冲突会重新出现，而收编的目的正是
让「这个键干什么」有单一答案。

本方案选择相反的方向——**把存储层搬到 `key_actions`，完成收编**。§6 判据的内核（存储层必须
能表达用户的原始意图）同样满足：物化之后折算不再发生，`key_actions` 就是存储层。

### ★ 通用判据（本次提炼，比上面两条都好用）

> 「折算」本身无害。有害的是**折算的下游成了 UI 的唯一写入口，而出厂值还留在上游**。
>
> 一问即知：**用户在 UI 上做的删除落到哪个键？那个键能否压制出厂值所在的那个键？**
> `page_keys` 答案是「能」（同一个键、数组整体覆盖）；`trigger_keys` 答案是「不能」
> （写下游 map、出厂值在上游数组，而 map 深合并无法表达删除）。

新增任何「A 折算进 B」的配置时，先回答这个问题。

## 4. 方案

服务启动时跑一次 `materialize_key_actions()`：

1. 取 `Config::load()` 的结果——它已跑完 `normalize()`，`keys.key_actions` 就是当前**生效**
   的那张表（三层合并 ⊕ 折算 ⊕「用户显式配过的不被覆盖」）。
   ⚠️ **绝不能自己再抄一遍折算规则**，抄一份就是第二个真相源，本次修的正是这类问题。
2. 把它整表写进用户层 `keys.key_actions`。
3. 摘掉用户层残留的五处 `trigger_keys`（L2 的那份**不动**）。
4. 写 `keys.key_actions_materialized = KEY_ACTIONS_MATERIALIZE_VERSION`。

此后 `migrate_trigger_keys_into_key_actions()` 见到标记即让位，只做一件事：清空内存态
`trigger_keys`，维持 `normalize()` 既有的后置条件「折算之后 trigger_keys 恒为空」。

### 对新用户也必须跑

只迁移「已有 config.toml 的老用户」会让方案**退化成没修**：新装机器的用户层里永远没有实体
条目，删除依然会被 L2 折算复活。故用户层文件不存在时按空表处理，照常物化。

### ★ 标记必须住在用户层 config.toml 里

不能改成独立标记文件（`mark_user_config_seen_if_present` 那种）。**判据直接来自本次报障场景**：
标记不随备份走的话，A 机删掉的绑定，在 B 机还原后会因「看起来没迁移过」被重新折算灌回去，
bug 原样复现。**备份能否带走「我删过它」这个意图，是本方案成立的前提。**

用版本号而非 bool：日后若要再物化一批键，递增即可重跑，bool 没有第二次机会。

### 两道安全闸（都退化为「什么都不做」）

1. **用户配置目录不可用**（漫游未挂载）→ 不动。此时用户层「看起来是空的」，照做会把出厂
   绑定物化成用户的全部绑定，抹掉他真实的自定义。同 `prune_user_config` 必须排在
   `wait_user_config_ready` 之后的理由。
2. **L2 `data/config.toml` 不在场** → 不动。折算结果依赖 L2 声明的出厂绑定，L2 缺席时折算出
   的是一张**残缺**的表，物化下去 = 用户永久丢失出厂绑定，而且标记一置位就再也不会补回来。
   **这是本函数最危险的一条路径。**

后来又补了两道，同样退化为「什么都不做」：

3. **本次 `load()` 的 `keys` 段被段级降级** → 不动。降级把有毒的 `keys` 段换成 L1 默认，
   `bindings` 只剩出厂绑定，而写回是整表覆盖、还会摘掉 `input.*.trigger_keys` 并置版本标记，
   用户的自定义绑定会永久丢失。段级降级出现前 `load()` 直接返回 `Err`，`?` 即是保护；
   降级把它变成「成功但内容残缺」，所以要在这里补回。
4. **用户 config.toml 语法不合法** → 不动。语法错误发生在四层合并之前，闸三拦不到。
   「本次加载不生效」只是内存态，拿空表当种子写回才真正删掉文件内容（真机发生过：
   重复写了一个字段，重启后整份配置只剩 key_actions）；且空表恒判为未物化，会每次启动重跑。
   种子只用严格解析，容错解析跳过的行写回后会永久消失。

### 为什么 `trigger_keys` 在 L2 里保留

它降级为**出厂声明处**：设置页的「恢复默认」按钮读的是 `config.getDefaults`
（`system_preset_value` = L1 ⊕ L2），那条路**不跑 `normalize()`**，所以出厂绑定只在被折算的
那一侧看得见（wind-setting `manifest.rs::key_action_defaults` 的文档已载明）。删掉 L2 那份，
「恢复默认」会变成「全部取消勾选」——而清空 `trigger_keys` 正是 core 认定的「禁用该功能」。

## 5. 范围

| 键 | 处置 |
|---|---|
| `input.temp_pinyin.trigger_keys` | ✅ 物化 |
| `input.temp_english.trigger_keys` | ✅ 物化 |
| `schema.mix_modes[].trigger_keys` | ✅ 物化（逐元素摘字段，元素本身保留） |
| `keys.page_keys` / `highlight_keys` / `select_key_groups` / `select_char_keys` | ❌ 不动，见 §3——那一族是健康的 |
| `schema.codetable.z_key_action` | ❌ 不动，它有自己的家（独立配置项） |

### Android

`wind-mobile` 不走 `apps/service/src/main.rs`，**不跑物化**，标记恒为 0 ⇒ 折算照旧，行为与
本次改动前完全一致。移动端没有「设置页删触发键」这个入口，暴露面小；要补钩子是独立话题。
守门测试 `unmaterialized_config_still_folds_trigger_keys` 锁住这条路径不被误删。

## 6. 递增版本号的语义（⚠️ 破坏性）

`KEY_ACTIONS_MATERIALIZE_VERSION` 递增会让所有用户**再物化一次**，即**用当前折算结果覆盖
用户对这批键的修改**。只有在「出厂绑定本身要变、且必须送达存量用户」时才够格递增——
加一个新绑定不够格（那属于新增配置项，走正常的 L2 新增即可）。

## 7. 守门测试

均在 `wind-config/src/config.rs` 的 `mod tests`：

| 测试 | 锁住什么 |
|---|---|
| `materialized_config_stops_reviving_deleted_binding` | ★★ 行为闸：已物化时折算不得复活删掉的绑定（把屏蔽的 `>=` 改成 `>` 即精确变红） |
| `unmaterialized_config_still_folds_trigger_keys` | 反事实对照：未物化时折算照旧（Android 依赖） |
| `prune_keeps_materialize_marker` | ★ 标记不被 `prune_user_config` 清掉——标记一丢，本修复整个失效且现象与修复前一模一样 |
| `materialize_into_writes_bindings_and_drops_legacy_fields` | 写入 + 摘旧字段 + 同段其它键不受牵连 + mix 元素本身保留 |
| `already_materialized_only_trusts_explicit_version` | 幂等判据只认显式版本号，不做「看起来像迁过了」的推断 |

⚠️ `materialize_key_actions()` 本身**不可直接单测**：它依赖 `user_config_dir()`，直接测会写
用户真实的 `%APPDATA%\WindInput\config.toml`（本仓已有前科：`cargo test -p wind-coordinator`
曾真写 `schema.active`）。**本函数会删键，代价比那次更大。** 故判定抽成纯函数
`materialize_into` / `already_materialized`，IO 留在外层——与 `prune_redundant` 同一策略。
