# 用户覆盖程序自带数据文件

程序自带的数据文件（方案、词库、常用字表、系统短语……）随安装目录发布，装在
`Program Files` 这类**只读**位置。用户要改这些内容，唯一的途径是在**用户配置目录**放一份
同名文件，运行时由用户那份胜出。本文档定义这套机制的语义、覆盖范围与新增数据文件时的约定。

两个根目录：

| 角色 | 路径 | 谁写 |
|---|---|---|
| 安装目录（自带） | `<exe目录>/data/`，即 `Config::data_dir()` | 随程序发布，升级时整体替换 |
| 用户目录（覆盖） | `Config::user_config_dir()`，正常模式为 `%APPDATA%\WindInput[Dev]`；便携模式与 `datadir.conf` 会改写它，见 `wind-config/src/variant.rs` | 用户手工放置、设置页写入、方案包导入 |

---

## 1. 三种覆盖模型

新增任何数据文件时，必须**明确选一种**并接上对应的解析函数。三者不可混用。

### 模型 A：整体替换（默认，绝大多数数据文件）

用户目录存在同名文件 → 整份文件胜出，安装目录那份完全不参与。**不做任何键级合并**——
用户拿到的是一份完整文件的所有权，语义简单、可预测，也便于用户直接拷贝自带文件后改。

解析入口（都实现「用户优先，回落安装目录」）：

| 函数 | 根 | 用途 |
|---|---|---|
| `Config::resolve_data_file(data_dir, rel)` | 数据根 `data/` | 数据根下的自带文件 |
| `Config::resolve_schema_resource(data_dir, rel)` | `data/schemas/` | 方案附属资源（拆字库、字根字体、常用字表） |
| `EngineManager::resolve_schema_file(rel, data_dir)` | `data/schemas/` | 方案文件 `*.schema.toml` |
| `EngineManager::resolve_dict_file(rel, schemas_dir)` | `data/schemas/` | 词库，**四级**优先级（见下） |

词库那条多两级，因为支持 **wdat-only 分发**（用户可只投放编译好的 `.wdat` 而不带源 yaml）：

```
用户 yaml → 安装 yaml → 用户 wdat → 安装 wdat → 兜底安装路径（由调用方报加载失败）
```

必须按 wdat 再探一轮而不能只探 yaml 后兜底：兜底恒指向安装目录，而用户投放的 wdat 在用户
目录，只探 yaml 会把路径定位到错误的目录上。

### 模型 B：键级合并（仅两处，不扩大）

只有配置类文件走合并——用户改的是**其中几个键**，不该因此冻结整份文件的其余部分。

| 文件 | 层次 | 实现 |
|---|---|---|
| `config.toml` | 代码默认 → `data/config.toml` → 用户 `config.toml` | `Config::load` |
| `compat.toml` | `data/compat.toml` → 用户 `compat.toml`，按进程名整条覆盖；协议修正 `composition_start_pair_guard` 未写时继承 | `app_compat.rs::load` |

`composition_start_pair_guard` 的例外是为了升级安全：菜单会为某应用写只含被修改字段的
稀疏用户规则，这不应该静默抹掉后续版本给该宿主新增的协议级正确性修正。该字段是
`Option<bool>`：未写继承低层，显式 `false` 可覆盖关闭；其余 `compat.toml` 字段仍保持整条
覆盖语义。

附带一个只存在于用户侧、无安装目录对应物的合并层：`schema_overrides/{id}.toml`
（设置页对方案参数的调整，深合并到方案文件之上）。它是**程序写、程序读**的，不是给用户
手工编辑的覆盖入口；其中 `dictionaries` 特判为「按 id 匹配、只接受 `enabled` 字段」的稀疏
合并——方案文件始终是词库结构的唯一权威，见 `manager.rs::merge_dict_overrides`。

> 为什么 `config.toml` 不能改成整体替换：用户层由程序增量写入（设置页改一项就写一项）。
> 整体替换意味着用户层必须是一份完整配置，那么出厂默认值的任何升级都对老用户永久失效。

### 模型 C：不用文件覆盖（数据进数据库）

用户短语、用户词、词频、shadow 置顶/删词全部存 `userdata.redb` / `user_data.db`，
**没有**用户目录文件覆盖入口。`data/system.phrases.toml` 是系统短语的**种子**：启动时按
内容哈希同步进库，之后一切读写都以库为准。

注意区分：`system.phrases.toml` 这个文件本身**支持模型 A 覆盖**（用户可以整份替换掉自带的
系统短语种子），但用户对单条短语的增删改仍然只走数据库，不回写任何 TOML。

---

## 2. 资源矩阵

`kind` 是覆盖日志里的类别标签（见 §3）。相对路径均相对于所在根。

| 资源 | 相对路径 | 模型 | kind | 解析点 |
|---|---|---|---|---|
| 方案文件 | `schemas/{id}.schema.toml` | A | `schema` | `EngineManager::resolve_schema_file` |
| 词库 | `schemas/{方案}/*.dict.yaml` \| `*.wdat` | A（四级） | `dict` | `EngineManager::resolve_dict_file_in` |
| 拆字库 | `schemas/` 下，由 `[engine.chaizi].db_path` 指定 | A | `resource` | `coordinator.rs` → `resolve_schema_resource` |
| 字根字体 | `schemas/` 下，由 `[engine.chaizi].font_path` 指定 | A | `resource` | 同上 |
| 常用字表 | `schemas/common_chars.txt` | A | `resource` | `coordinator.rs` → `resolve_schema_resource` |
| 双拼布局 | `schemas/shuangpin/{layout}.toml` | A | `shuangpin` | `EngineManager::scan_shuangpin_layouts` |
| 主题 | `themes/{id}/theme.toml` | A | `theme` | `wind_theme::theme::find_theme_dir` |
| 系统短语种子 | `system.phrases.toml` | A | `data` | `coordinator.rs` → `resolve_data_file` |
| 快捷输入格式表 | `system.quick.toml` | A | `data` | `coordinator.rs` → `resolve_data_file` |
| 拼音读音表 | `pinyin_map.txt` | A | `data` | `coordinator.rs` → `resolve_data_file` |
| 主配置 | `config.toml` | B | — | `Config::load` |
| 应用兼容规则 | `compat.toml` | B | — | `app_compat.rs::load` |
| 简繁转换表 | `opencc/*.octrie` | **不支持** | — | `coordinator.rs` 直接拼 `data_dir` |

`opencc` 是**有意不支持**的：转换表由 OpenCC 上游数据编译而来，用户没有替换它的实际场景，
而支持覆盖就得同时处理「一组文件里只覆盖其中一个」的半覆盖状态。需要时再补。

---

## 3. 覆盖日志（排查线索）

「同一版程序，这台机器行为和出厂不一致」是这套机制最典型的故障形态，而覆盖本身没有任何
界面痕迹。因此**所有模型 A 的解析点在命中用户层时必须打日志**，统一经
`Config::log_user_override(kind, rel, path, shadowed)`：

```
用户覆盖生效[data]: system.phrases.toml → C:\Users\x\AppData\Roaming\WindInput\system.phrases.toml
```

一条 `grep 用户覆盖生效` 即可列出当前生效的全部覆盖。

`shadowed` 参数区分命中用户层的两种情形，**不可省**：

- `true`（安装目录也有同名文件）= 真的覆盖了自带数据 → `info`，这是排查目标；
- `false`（安装目录没有）= 第三方方案自带资源本就只在用户目录 → `debug`。

两者都记 info 的话，一个第三方方案的几十个词库会把真正的覆盖淹没掉。

双拼布局的打点位置特殊：放在**被遮蔽方**（扫到安装目录同名文件时），因为命中用户目录那一刻
还不知道安装目录有没有同名；日志里给的路径仍是胜出的那份。

---

## 4. 生效时机

**没有文件监视器。** 手工放置覆盖文件后需要：

- 方案/词库/双拼布局/主题：重启服务，或从设置页触发 `schema.invalidate` / `schema.rebuildCache`；
- `system.phrases.toml` / `pinyin_map.txt` / `common_chars.txt`：**必须重启服务**。
  这三处的路径在 `Coordinator::new` 时一次解析定死，后续（如系统短语的重读）沿用同一路径。
  即启动时若用户目录没有覆盖文件，运行期间新放一份不会被发现。

词库缓存（`.wdat`）按**源文件内容指纹**校验（`wind-dict/src/cache_fp.rs`），指纹含内容不含
路径，因此用户覆盖的同名词库与自带词库内容不同 → 指纹不同 → 正确重建。副作用是二者共用
同一缓存落点，来回切换会互相踩、反复重建；是性能问题，不影响正确性。

---

## 5. 新增自带数据文件时的约定

1. **选一种模型**：新数据文件默认走模型 A。要走 B 必须有「用户只改其中几个键」的实际理由。
2. **不要直接拼 `data_dir.join(...)`**——这是本机制历史上全部缺陷的唯一形态。
   解析函数是对的，错的总是调用方绕过了它。`common_chars.txt`、`pinyin_map.txt`、
   `unigram_path` 三处都栽在这上面，且失败是静默的（找不到就退化，不报错）。
3. **两处均不存在时告警**：解析返回 `None` 说明连自带文件都缺了，属于部署损坏，必须 `warn`。
4. **接线要有端到端测试**：纯函数单测证明不了接线。`crates/wind-config/tests/user_override.rs`
   用 `WIND_DATADIR_CONF` 把用户目录重定向到临时目录，真调公开 API。
   ⚠️ 该测试文件只能有一个 `#[test]`——`user_config_dir()` 经 OnceLock 缓存，同一进程内
   多个测试会争抢环境变量，先跑的把用户目录定死，后跑的静默测到错误的目标。

---

## 6. 已知缺口（未处理）

- **覆盖内置方案 id 会被误判为「用户方案」**。`EngineManager::is_user_schema` 的判据是
  「用户目录存在同名 `.schema.toml`」，没有排除「安装目录也有」的情形。后果：用户目录里放一份
  `wubi86.schema.toml` 覆盖内置方案后，设置页把它标为可删除（`builtin: false`），点删除会走
  `web_schema_delete` —— 方案本身会回落到内置（`delete_package` 不删系统目录文件），但**该方案的
  用户词、临时词、词频、shadow 会被一并清空**。用户本意只是撤销覆盖。
  正确的判据应是「用户目录有 ∧ 安装目录没有」才算用户方案；覆盖内置的情形应提供
  「恢复默认」而非「删除」。
- **主题与方案对同一件事的口径相反**：覆盖内置主题时 `list_themes_full` 仍标为内置
  （以程序目录为枚举主干），覆盖内置方案却标为用户方案。两处应统一。
- **用户目录不预建骨架**：`schemas/` 只在设置页 RPC 走到时才创建。想手工放覆盖文件的用户
  得自己知道该建什么目录、用什么文件名——目前没有 GUI 入口（只有方案包导入），也没有面向
  用户的文档页。
