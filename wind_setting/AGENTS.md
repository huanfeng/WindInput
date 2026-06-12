<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-20 -->

# wind_setting

## Purpose
清风输入法（WindInput）的图形化设置界面。基于 Wails v2 构建，Go 后端负责读写配置文件、词库和输入方案，Vue 3 前端提供设置 UI。编译后嵌入为单一可执行文件（`wind_setting.exe`），由主程序托盘菜单启动，支持通过命令行参数 `--page=<name>` 直接跳转到指定页面，或 `--page=add-word` 以独立加词窗口模式启动。

## Key Files
| 文件 | 说明 |
|------|------|
| `main.go` | 程序入口：解析 `--page` / `windinput://` 协议参数；按 `resolveRunMode` 分流——`--web` 走 Web 形态（`runWebMode`），否则初始化 Wails App（GUI，含 `Mac.OnUrlOpen`）注册 Go 绑定 |
| `app.go` | `App` 结构体定义及生命周期（startup/shutdown）；含 `webMode`/`webEmit`（Web 形态标志与事件投递回调），初始化各编辑器和文件监控 |
| `run_mode.go` / `run_mode_test.go` | 运行形态决策与 Web 启动：`resolveRunMode(args)`（**仅显式 `--web` 进 Web 形态**，否则 GUI；缺 WebView2 交由 Wails 自带安装引导，不再自动降级）、`runWebMode`（起 HTTP 服务、开浏览器、心跳空闲自动退出、日志写 `%TEMP%/wind_setting/web.log`） |
| `web_server.go` / `web_server_test.go` | Web 形态 HTTP 服务（不依赖 WebView2）：`/api/call` 反射网关（`callReflect` 镜像 Wails 对 `*App` 的方法绑定）、`/api/events` SSE 事件桥 + `broadcast`（投递 rpcClient 事件与 App 直接 emit 的 `update:*`）、`/api/ping`·`/api/bye` 存活心跳、端口探测（18923+）启停 |
| `app_runtime_compat.go` | wailsRuntime 调用的 **Web 安全包装**：`a.logInfof`/`a.logErrorf`/`a.emitEvent`。Web 模式下降级为标准库 `log` / SSE 广播；桌面模式原样转发到 `wailsRuntime.*(a.ctx,...)`。**关键**：Web 形态 `a.ctx` 为占位 `context.Background()`，直接调 `wailsRuntime.*` 会触发 `log.Fatalf` 终止进程 |
| `app_config.go` | 配置读写 API：`GetConfig`、`SetConfigItems`（按 key 增量保存）、`ReloadConfig`、`CheckConfigModified` |
| `app_dict.go` | 词库管理 API：短语（Phrase）、用户词库（UserDict）、Shadow 规则（pin+delete 架构），含导入/导出 |
| `app_schema.go` | 输入方案管理 API：`GetAvailableSchemas`、`GetSchemaConfig`、`ExportSchema(s)`、`Preview/ConfirmImportSchema` 等。**方案文件双格式**：`SchemaConfig` 全树带 `yaml+toml` tag，`collectSchemaIDs`/`find*SchemaFile`/解析按 `.schema.toml` 优先、`.schema.yaml` 回退（`resolveSchemaFileIn`/`unmarshalSchemaFileData`/`isSchemaFileName`/`schemaIDFromFileName`）。**导出统一 TOML**（`toml.Marshal`→`id.schema.toml`），词库文件原样复制（内容保持其原格式）。**导出资源收集** `collectSchemaResourceFiles`：词库 + split 体(.dict.tsv) + 补丁(.dict.patch.yaml) + 词库头 `import_tables` 兄弟（`readDictImportTables` 自带最小头解析，因独立 module 不能引 internal/dictcache）+ 拆字 `engine.chaizi` 的 db_path/font_family（`SchemaConfigEngine.Chaizi` 字段，缺失会导致拆字资源随方案丢失）。导入解压逐文件原样写出，格式无关 |
| `app_service.go` | 服务控制 API：`CheckServiceRunning`、`NotifyReload`、主题管理、文件变化检测 |
| `app_tsf_log.go` | TSF 日志配置 API：`GetTSFLogConfig`、`SaveTSFLogConfig` |
| `protocol_url.go` | `windinput://` 链接解析：`ParseProtocolURL` → `ProtocolRequest{Kind,URL,Name}`，仅接受 https，kind ∈ theme/schema/dict/extdict |
| `protocol_handler.go` | 协议导入投递与注册开关 API：`handleProtocolURL`、`ConsumePendingProtocol`、`GetProtocolStatus`、`SetProtocolRegistered`；emit `protocol-import` 事件 + pending 冷启动缓存 |
| `protocol_register_windows.go` / `protocol_register_darwin.go` | 协议注册：Win 写/删/自愈 `HKCU\Software\Classes\windinput`（`RegisterProtocol`/`UnregisterProtocol`/`ProtocolStatus`/`SelfHealProtocol`）；darwin 声明式托管(no-op)，`protocolManagedBySystem` 区分 |
| `app_theme.go` | 主题导入 API：`ImportThemeFromFile/URL/Text`、`PreviewThemeFromURL`（下载解析 meta 不落盘，供 URL schema 确认框） |
| `singleton_windows.go` / `singleton_darwin.go` | 单实例 + 原生消息框 + 跨实例 IPC：Win 用互斥锁/窗口激活；darwin 靠 .app 天然单实例(no-op), 消息框走 osascript。`ensureSingleInstance(startPage, addWordParams, protocolURL)` 返回 `(release func(), ok bool)` 跨平台契约；IPC 消息支持 `add-word\|...` 与 `protocol\|<url>`；`startIPCListener(ctx, *App)`。**跨完整性级别**：mutex/event 经 `singletonSecurityAttributes()` 用低完整性标签创建(SDDL `D:(A;;GA;;;WD)S:(ML;;NW;;;LW)`)，使中/高完整性实例互相可见；`CreateMutex` 的 `ERROR_ACCESS_DENIED` 也按「已有实例」处理(兼容旧高标签对象)。改动同步对象创建处务必沿用同一 SA |
| `open_windows.go` / `open_darwin.go` | `shellOpen`(打开文件/URL)：Win 用 ShellExecuteW, darwin 用 `open` 命令 |
| `wails.json` | Wails 项目配置，前端包管理器为 pnpm; `frontend:build` 用 `pnpm exec vite build`(跳过 vue-tsc 严格门禁) |
| `go.mod` | Go 模块：`wind_setting`，依赖 `wind_input`（本地 replace）和 `wailsapp/wails/v2 v2.11.0` |

## Subdirectories
| 目录 | 说明 |
|------|------|
| `internal/` | Go 内部包：editor（编辑器）和 filesync（文件监控）(see `internal/AGENTS.md`) |
| `frontend/` | Vue 3 + TypeScript 前端 (see `frontend/AGENTS.md`) |
| `build/` | Wails 构建资源（图标、Windows manifest、安装包脚本） |

## For AI Agents
### Working In This Directory
- Go 后端方法自动绑定为 Wails JS API，前端通过 `wailsjs/go/main/App` 调用
- 所有绑定方法定义在 `app*.go` 中，方法名即为前端调用名（PascalCase）
- 支持双形态运行：**GUI 形态**（默认，Wails + WebView2）；**Web 形态**（`--web`，HTTP 服务 + 浏览器访问，无需 WebView2）。Web 形态用 `/api/call` 反射网关镜像同一套 `*App` 绑定，前端经 `webShim` 注入 `window.go`/`window.runtime` 透明走 HTTP/SSE，页面零改动
- **Web 形态铁律**：被前端调到的 `*App` 方法内若需日志/事件，**必须**走 `a.logInfof`/`a.logErrorf`/`a.emitEvent`，禁止直接 `wailsRuntime.Log*`/`EventsEmit(a.ctx,...)`（Web 下 `a.ctx` 无效会 `log.Fatalf` 杀进程）；文件对话框类（`*Dialog`）方法在 Web 无法工作，由前端 `webShim` 黑名单拦截提示
- 命令行参数格式：`wind_setting.exe --page=dictionary` 或 `--dictionary`
- 有效页面名：`general`、`input`、`hotkey`、`appearance`、`dictionary`、`advanced`、`about`、`add-word`
- `add-word` 页面为独立加词窗口模式：不显示主界面侧边栏，仅显示 `AddWordPage` 对话框，关闭后退出进程；也可通过 Wails 事件 `navigate-addword` 在已运行的实例中弹出加词对话框
- 保存配置后自动调用 `NotifyReload` 通知主程序热重载（goroutine 异步）
- Shadow 规则采用 **pin(position) + delete** 二元架构（非旧版 top/order/reweight）

### Testing Requirements
- Go 构建：`wails build` 或 `go build ./...`（在 wind_setting 目录下）
- 前端构建：`pnpm run build`（在 frontend 目录下）
- 开发模式：`wails dev`（同时启动 Go 和 Vite 开发服务器）
- 格式化：Go 修改后运行 `go fmt ./...`；前端修改后运行格式化
- 功能测试须在完整 Wails 环境中进行，确保 IPC 绑定正常

### Common Patterns
- 每次写入文件后调用 `a.fileWatcher.UpdateState(path)` 更新快照，防止误报外部修改
- 配置保存后异步 `go a.NotifyReload(target)` 通知主程序
- 注意：`webShim` 安装后 `window.go` 存在，前端 `isWailsEnv` 在 Web 形态恒为 true；判断「是否具备原生能力」一律用 `isWebMode()`（见 `frontend/src/lib/webEnv.ts`），不要再用 `isWailsEnv`

## Dependencies
### Internal
- `wind_setting/internal/editor` — 配置/词库文件编辑器
- `wind_setting/internal/filesync` — 文件变化监控
- `github.com/huanfeng/wind_input/pkg/config` — 配置加载/保存
- `github.com/huanfeng/wind_input/pkg/dictfile` — 词库文件格式
- `github.com/huanfeng/wind_input/pkg/control` — 控制管道客户端
- `github.com/huanfeng/wind_input/pkg/theme` — 主题管理

### External
- `github.com/wailsapp/wails/v2 v2.11.0` — 桌面应用框架
- Vue 3、TypeScript、Vite（前端）

<!-- MANUAL: -->
