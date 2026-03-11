<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# internal/control

## Purpose
控制管道服务端实现。监听 `\\.\pipe\wind_input_control`，供设置应用（或 CLI 工具）发送管理命令：热重载配置、重载词库、查询服务状态等。

## Key Files
| File | Description |
|------|-------------|
| `server.go` | 控制管道服务端（`Server`）、请求路由、各命令处理方法 |

## For AI Agents

### Working In This Directory
- 使用 `github.com/Microsoft/go-winio` 创建 Named Pipe 监听器（字节流模式，非 MESSAGE 模式）
- 协议为行格式文本（`\n` 分隔），由 `pkg/control` 定义
- 支持的命令：`PING`、`RELOAD_CONFIG`、`RELOAD_PHRASES`、`RELOAD_SHADOW`、`RELOAD_USERDICT`、`RELOAD_ALL`、`GET_STATUS`
- `ReloadHandler` 接口由 `cmd/service` 的 `reloadHandlerImpl` 实现，连接到 `coordinator`
- `RELOAD_ALL` 会依次重载配置、短语、Shadow、用户词库，收集所有错误后返回

### Testing Requirements
- 需要 Windows Named Pipe 环境测试
- 可通过 `pkg/control.Client` 发送命令进行集成测试

### Common Patterns
- 每个连接在独立 goroutine 处理，连接关闭后即销毁
- 通过 `sync.WaitGroup` 跟踪活跃连接，`Stop()` 等待所有连接处理完毕

## Dependencies
### Internal
- `internal/dict` — DictManager（重载词库）
- `pkg/config` — 配置加载
- `pkg/control` — 协议定义（命令常量、请求/响应格式）

### External
- `github.com/Microsoft/go-winio` — Named Pipe 监听器

<!-- MANUAL: -->
