<!-- Parent: ../../AGENTS.md -->
<!-- Updated: 2026-06-29 -->

# wind-rpc

## Purpose

为 wind_input core 提供本地 JSON-RPC IPC 服务，架起 coordinator（服务进程）与设置端（wind_setting_native）之间的双向通信：控制通道（请求-响应）和事件通道（单向广播）。去掉了旧版 wind-webapi 的 axum/CORS/PNA/token/端口发现，本地授权靠 OS ACL。

## Key Files

| File | Description |
|------|-------------|
| `src/lib.rs` | 对外导出 `CoreRpc`、`DispatchState`、`dispatch`、`EventSink`、`RpcServer`、`ctrl_endpoint`、`events_endpoint` |
| `src/dispatch.rs` | 传输无关的 JSON-RPC 分发：`system.*` / `config.*` 本地处理，其余转发 `CoreRpc::data_rpc`；含 `FakeCore` stub 单测 |
| `src/server.rs` | `RpcServer` 句柄：`start()` 启动 ctrl + events 两个后台线程；`ctrl_endpoint` / `events_endpoint` 管道/套接字路径生成 |
| `src/events.rs` | `EventSink`：可 Clone 的广播句柄，`emit_config_changed` / `emit_dict_changed` / `emit_compat_changed` / `emit_schema_config_changed` / `emit_needs_restart` |
| `src/client.rs` | 最小同步客户端（单次请求-响应），供 `wind_input config` CLI 向运行中 core 触发热重载 |
| `src/capabilities.rs` | 从 `config_schema::REGISTRY` + L1⊕L2 系统预置配置动态生成 `system.capabilities`，替代退役的 manifest.toml |

## For AI Agents

### Working In This Directory

- **双通道架构**：ctrl 通道（请求-响应，每连接一线程）+ events 通道（单向广播，writer 线程消费 mpsc 队列写线路）。同步线程模型，**不引入 tokio**。
- **CoreRpc trait 是唯一扩展点**：宿主（coordinator）注入 `Arc<dyn CoreRpc>` 实现 `is_chinese_mode`、`active_schema_id`、`apply_config`、`data_rpc`、`fonts`。`data_rpc` 负责转发 schema/dict/freq/shadow/stats/theme/phrase 等数据类 RPC；dispatch 层本身只处理 `system.*` / `config.*`。
- **新增 RPC 方法**：在 `dispatch.rs` 的 `handle()` match 新增一个 arm 即可，**无需手动注册**（与 Go 版的 RegisterMethod 不同）；数据类方法只需在宿主的 `data_rpc` 里加 arm。
- **capabilities 缓存**：`DispatchState` 构造时调 `capabilities::generate()` 一次性生成并缓存，变更 `config_schema::REGISTRY` 或 `Config::default()` 后需重启才刷新。
- **EventSink 使用**：core 在 dict 写操作后调 `event_sink().emit_dict_changed(...)`, `config.setItems` 和 `config.reload` 会自动广播 `"config.changed"`，宿主无需手动触发。
- **`compat.*`（应用兼容规则）**：dispatch 把 `compat.` 前缀整体转发给宿主 `data_rpc`（实现在 `wind-webdata` → `wind_config::compat_admin::rpc`），方法：`list` / `schema` / `upsert` / `resetField` / `setDisabled` / `reset` / `resetAll` / `export` / `import`。写方法成功后 dispatch 自动广播 `"compat.changed"`（`{method}`），**只读方法（list/schema/export）与 `dryRun` 导入不广播**，失败也不广播——见 `compat_method_writes`。宿主不要自己再发一次。新增 `compat.*` 方法时，若它会改用户层文件，无需改这里（默认按写处理）；若是只读，必须加进 `compat_method_writes` 的只读名单，否则设置页每次读取都会触发一次刷新循环。
- **`schema.configChanged`**：`schema.saveConfig` / `resetConfig` / `setDictEnabled` / `delete`、`userDicts.setEnabled`、`userdata.purgeSchema` 成功后 dispatch 自动广播（`{id, method}`）；`scheme.importPackage` / `importText` 与 `backup.restore` 可能动多个方案，载荷不带 `id`，订阅方按全部刷新处理。见 `schema_config_write_id`。新增改方案配置的写方法要加进去，否则开着的设置页不会刷新。
- **平台传输**：Windows 管道路径由 `ctrl_endpoint(suffix)` / `events_endpoint(suffix)` 生成（`suffix` 来自 `variant::pipe_suffix()`）；传输实现代码以 `#[path = "transport_*.rs"]` 按平台切换，dispatch/协议层平台无关。
- **client.rs 失败语义**：连不上（core 未运行）立即返回 `Err`，调用方应回退到离线直写配置文件，不得重试。

### Testing Requirements

- wind-rpc 在 `[target.'cfg(windows)'].dependencies]` 中依赖 `windows` crate，非 Windows 主机（Linux CI）无法编译 `transport_windows.rs`，不能在非 Windows 环境跑测试。
- 在 Windows 开发主机上可运行 `cargo test -p wind-rpc`：`dispatch.rs` 的单测使用 `FakeCore` stub，不需要真实管道；`capabilities.rs` 的单测需要仓库 `data/` 目录存在（`CARGO_MANIFEST_DIR/../../../data`）。
- 新增 dispatch 逻辑需在 `dispatch.rs` 的 `tests` 模块补用例（参考已有 `FakeCore` 模式）。

## Dependencies

### Internal

- `wind-ipc` — JSON-RPC 协议类型（`Request`/`Response`/`EventMessage`）+ 4 字节大端长度前缀帧（`encode_message`）
- `wind-config` — `Config::load`/`set_user_value`/`data_dir`、`config_schema::validate`/`registry`/`leaf_entries`

### External

- `serde` / `serde_json` — JSON 序列化
- `toml` — config.setItems 落盘时 JSON → toml::Value 转换
- `anyhow` — 错误传播
- `tracing` — 结构化日志
- `windows`（仅 Windows 目标）— 命名管道传输实现

## 全局约束

- 日志 INFO 级不得含用户输入/候选内容，见根 `AGENTS.md` 日志红线。
- `cargo fmt` 改完必跑。

<!-- MANUAL: 此行以下为人工补充区，重新生成时保留 -->
