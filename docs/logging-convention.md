# 日志规范（Logging Convention）

WindInput 是高频事件系统：每次按键、每条 IPC 命令、每次坐标更新都可能触发代码路径。
默认日志级别为 `info`（见 `apps/service/src/main.rs::init_logger`），因此 **`info` 必须低频、对运维有意义**，否则单次输入会刷出几十行日志、淹没日志文件。

## 分级标准

| 级别 | 用途 | 触发频率 | 示例 |
|------|------|----------|------|
| `error!` | 进程级、不可恢复的失败 | 极低 | 服务启动失败、单例冲突、RPC 初始化失败 |
| `warn!`  | 可恢复异常 / 降级 / 协议错误 / 配置回退 | 低，每条都代表"有点不对但能继续" | 解析 header 失败、词库缺失回退、写响应失败 |
| `info!`  | 进程生命周期里程碑 | **低频** | 启动/就绪/重启、切方案、词库加载完成、配置热重载、切主题 |
| `debug!` | 开发诊断（连接 / 会话级） | 可高频（默认不输出） | 客户端连接/断开、按键处理细节、坐标更新、菜单命令 |
| `trace!` | 逐命令 / 字节级 / 协议细节 | 最高频 | 逐 IPC 命令收发（`Received command` / `Sending response`）、payload 内容、协议字段（仍禁止隐私明文） |

## 硬性约束

1. **`info` 及以上禁止出现在每次按键 / 每条 IPC 命令 / 每次坐标更新 / 每次连接读写的路径上。**
   这类逐事件日志一律用 `debug!`（需要时通过 `RUST_LOG=debug` 或配置 `[debug] log_level` 打开）。
2. **隐私红线**：`info` 及以上禁止包含用户输入内容、词库词条、组合串（preedit）明文。
3. **不打预期分支**：如消息模式 `ReadFile` 返回 `ERROR_MORE_DATA` 但已读到完整 header，是协议预期行为，**不记日志**。
4. **不打冗余日志**：一次操作只在一个有信息量的点记录，避免 "Sending response" + "Response sent" 这类成对重复。

## 配置与开启方式

- 默认级别：`info`。
- 临时调高：环境变量 `RUST_LOG=debug`（最优先）。
- 持久配置：配置文件 `[debug] log_level = "debug"`（见 `DebugConfig`）。
- 可按模块过滤：`RUST_LOG=wind_bridge=debug,info`（只让 bridge 输出 debug，其余 info）。
- 日志落盘：`%LOCALAPPDATA%\WindInput[Dev]\logs\wind_input.1.log`，按 `log_max_size_mb` / `log_max_files` 滚动。
- **分段规则**：服务每次启动强制滚动一次，`wind_input.1.log` 恒为「当前这次运行」，上一次运行在 `wind_input.2.log`，依次类推（默认保留 10 份历史）。故排查时不必在混着多次重启的大文件里找分界点。
  **规则只有一句：数字越小越新，`.1` 是最新**，与设置程序的 `wind_setting.N.log` 一致。早先当前那份叫 `wind_input.log`（无序号），收日志时用户认不出它才是最新的——按名字排序它还排在 `.1` 后面，发来的包里常常只有 `.1.log`。
  序号在扩展名**之前**，滚动后仍是 `.log`，编辑器认得、按 `*.log` 也搜得到（实现见 `apps/service/src/log_rotate.rs`）。两代老命名（`wind_input.log.N` 与无序号的 `wind_input.log`）都会在启动时自动迁移。
  注意序号不严格等于「一次启动」：本次运行写满 `log_max_size_mb` 也会滚动，此时 `.2` 是本次运行的前半段。
  另：`wind_input.1.log` 被服务的滚动器常驻句柄持有，**不要在服务运行时从外部删除**——句柄仍指向已摘名的文件，后续日志会写进看不见的地方。需要干净的日志重启服务即可。

## 已知合规说明

- `wind-engine` 词库加载日志（"Loading dictionary" / "Dictionary loaded: N entries" / 合并缓存写入）属于**启动 / 切方案时一次性**的里程碑，保留在 `info`。其中逐子文件的 "Merging N entries from ..." 细节属诊断性质，可在噪音敏感时降为 `debug`。
- `wind-coordinator` 的按键处理路径（`handle_candidate` / `handle_key` 等）已统一使用 `debug!`，`info!` 仅保留重启、切方案、切主题等低频里程碑。
