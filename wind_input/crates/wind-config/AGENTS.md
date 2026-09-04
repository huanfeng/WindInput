<!-- Parent: ../../AGENTS.md -->
<!-- Updated: 2026-06-29 -->

# wind-config

## Purpose

配置系统的完整实现：TOML 三层合并加载、配置字段注册表（SSOT）、热键编译、运行时变体探测、运行时状态持久化、方案定义解析、应用兼容规则。无 Windows 平台依赖，可在任意主机编译测试。

## Key Files

| File | Description |
|------|-------------|
| `src/lib.rs` | 对外导出 `Config`、`RuntimeState`、`Schema` 及常用类型（`PinyinFuzzy`、`ModeIndicatorStyle`、`PreeditDisplay`、`CodeCommitConfig` 等） |
| `src/config.rs` | `Config` 结构体（七域：schema/input/keys/ui/stats/compat/debug）+ 三层合并加载（`Config::load`）+ 部分合并写入（`Config::set_user_value`，原子写）+ 路径辅助（`user_config_dir`/`local_dir`/`cache_dir`/`data_dir`） |
| `src/config_schema.rs` | **配置字段注册表 SSOT**：`REGISTRY` 静态数组声明所有叶子键的点分路径与类型（`FieldType`）；`validate` 校验键+值；`leaf_entries` 拍平 TOML 值；测试反向对照守护 struct ↔ registry ↔ data/config.toml 三不漂移 |
| `src/hotkey.rs` | `Compiler`：从 `Config` 编译 `CompiledHotkeys`（key_down/key_up 热键表，含 tsf_hash/match_hash/action）；VK 常量（`VK_LSHIFT` 等）与修饰位（`MOD_*`）模块内定义；`parse_hotkey` / `select_key_vks` / `select_char_vks` 供上游调用 |
| `src/variant.rs` | 运行时 dev/release/portable 判断：从 exe 文件名尾缀 `_dev` 判定（不依赖编译 profile）；`pipe_suffix` / `app_dir_name` / `is_portable` 供全仓使用；`WIND_VARIANT` 环境变量仅供开发覆盖。另含 `custom_userdata_dir`：读安装器写的 `datadir.conf` |
| `src/runtime_state.rs` | `RuntimeState`（state.toml）：上次中英模式、工具栏位置（按显示器 key）、候选框 pin 位置；原子写（tmp+rename） |
| `src/schema.rs` | `Schema` 方案定义（对应 `data/schemas/*.schema.toml`）：`SchemaInfo`/`EngineSpec`/`DictSpec` 等；与 config.toml 无关 |
| `src/app_compat.rs` | `AppCompat`：按进程名（不区分大小写）匹配兼容规则（compat.toml，系统层+用户层合并），`get_rule` 返回 `Option<&AppCompatRule>`；caret 宿主能力含 `caret_use_top` / `stale_probe_guard` / `composition_start_pair_guard`，后者是未写继承、显式 false 关闭的协议修正 |

## For AI Agents

### Working In This Directory

- **新增配置字段**：在 `config.rs` 对应子结构体添加字段（值类型，带 `#[serde(default)]`）→ 在 `Default::default()` 或 `default_*()` 函数给出合理初值 → 在 `config_schema.rs` 的 `REGISTRY` 中以点分路径追加一条 `f(...)` 声明 → 在 `data/config.toml` 中显式列出并写一句说明。四处必须同步：漏注册表由 `registry_covers_every_config_key` 拦，漏预置文件由 `data_config_toml_covers_registry` 拦。
- **`data/config.toml` 必须列全**：它同时是「出厂默认值」与「全部可配置项的说明书」，注册表里每个键都要在其中显式出现。唯二豁免是 `schema.special_modes` / `schema.mix_modes`——结构体数组写进预置文件会把定义**冻结成快照**（数组是整体覆盖不是合并），日后改代码侧默认值会被静默遮蔽；豁免名单在 `config_schema.rs` 的 `ABSENT_FROM_DATA_CONFIG`，加条目须有「写进去会造成实际危害」这一级的理由。
- **预置文件的数组会整体替换代码默认值**：`data/config.toml` 里少写一个元素 = 成品里没有该元素。改 `chinese_pairs` / `english_pairs` / `url.prefixes` 这类键前，先与 `config.rs` 的 `default_*()` 比对（三者目前均与代码默认值不一致，见 `docs/deferred-config-features.md`）。
- **`KeysConfig` 必须手写 `Default`，禁止 derive**：字段的 `#[serde(default)]` 在整表缺失时不触发，全靠 `Default::default()` 给出有效热键默认值；若改为 derive，设置页"恢复默认"后热键清空。
- **REGISTRY 是全局真相源**：wind-rpc 的 `config.setItems` 用它校验写入，`system.capabilities` 由它派生，`config.getItem` 用它检查键合法性。**改键路径（罕见）须同步**：`REGISTRY` + `Config` struct 字段名（serde rename）+ `data/config.toml` + 所有调用 `Config::set_user_*` 的硬编码路径（`internal_setter_paths_are_registered` 测试守护）。
- **`set_user_value` 是部分合并写**：只改指定路径，保留用户文件其他已有项，用户层维持最小 diff；异步持久化需先 clone Config 再写，避免并发修改（见根 `AGENTS.md`）。
- **variant.rs 的 dev 判定与编译 profile 解耦**：产物一律叫 `wind_input.exe`；复制改名为 `wind_input_dev.exe` 即成 dev 变体。`is_dev()` 用 `OnceLock` 缓存，进程内不变，禁止在生产环境设 `WIND_VARIANT`。
- **`RuntimeState` 与 `Config` 分开存储**（state.toml vs config.toml）：用户手动编辑 config.toml 不会覆盖工具栏位置等本机状态；`Config::state_dir()` 与 `local_dir()` 返回同一路径（语义区分）。
- **`datadir.conf` 只重定向 `user_config_dir()`，`local_dir()` 系不跟随**：安装向导选定的自定义数据目录由安装器写入 `%LOCALAPPDATA%\WindInput[Dev]\datadir.conf`（跨仓约定，写端在 `wind-installer`）。优先级 **便携 > datadir.conf > 默认漫游**。cache/logs/state.toml 刻意留在 `%LOCALAPPDATA%`——与卸载器语义对齐（`cleanup.rs` 的 `user_data_dir()` 读该文件、`local_cache_dir()` 恒定不读），且让 C++ `FileLogger` 的硬编码日志路径无需改动、两份日志仍能按时间对齐。**新增读该配置的路径函数前先确认属于哪一侧**，两侧口径不一致会让卸载删错目录。
- **改 `user_config_dir()` 的分支必须同步 `probe_user_config()`**：后者是启动期「等漫游挂载」的探测，若只改前者，就会出现「配置已指向自定义目录、探测却仍盯着漫游根」的错配——白等一个完整超时后退回系统预置方案。`tests/datadir_conf.rs` 守护这条接线（该测试必须真调公开 API，纯解析单测证明不了接线）。
- **`schema.rs` ≠ 配置**：`Schema` 是方案 `.schema.toml` 的定义，由引擎直接加载，不经三层合并，不在 `REGISTRY` 中。

### Testing Requirements

- wind-config 无 Windows 平台依赖，可在任意主机运行 `cargo test -p wind-config`。
- `config_schema.rs` 的测试会读取仓库 `data/config.toml`（via `CARGO_MANIFEST_DIR/../../../data`），需仓库 data 目录存在。核心守护：`registry_covers_every_config_key`（struct ↔ registry 零漂移）、`data_config_toml_covers_registry`（预置文件**不许少**键）、`data_config_toml_has_no_orphan_keys`（预置文件**不许多**键）、`absent_allowlist_stays_accurate`（豁免名单不腐烂）、`data_config_toml_values_pass_validation`（类型/enum 合法）。
- `hotkey.rs` 的单测覆盖 toggle 模式键 hash（含具体修饰位回归）、数字模板展开、Compiler 端到端编译。

## Dependencies

### Internal

无（leaf crate，不依赖本仓其他 crate）

### External

- `serde` / `toml` — TOML 三层合并与序列化（直接 toml::Value 操作，无桥接层）
- `anyhow` / `thiserror` — 错误处理
- `dirs` — 用户配置目录（`%APPDATA%`/`%LOCALAPPDATA%`）跨平台获取
- `tracing` — 加载日志

## 全局约束

- VK 常量使用 `hotkey.rs` 内的模块级 `const VK_*`，禁止在调用方出现裸十六进制键码字面量（见根 `AGENTS.md` VK 红线）。
- 日志 INFO 级不得含用户输入/候选内容，见根 `AGENTS.md` 日志红线。
- `cargo fmt` 改完必跑。

<!-- MANUAL: 此行以下为人工补充区，重新生成时保留 -->
