<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# pkg/control

## Purpose
控制管道通信协议定义和客户端实现。定义管道名称、命令常量、请求/响应格式，供服务端（`internal/control`）和客户端（设置应用、CLI 工具）共用。

## Key Files
| File | Description |
|------|-------------|
| `protocol.go` | 管道名称、命令常量、请求/响应结构体、格式化/解析函数、`ServiceStatus` |
| `client.go` | `Client`：连接控制管道、发送命令、解析响应的封装 |

## For AI Agents

### Working In This Directory
- 管道名称：`\\.\pipe\wind_input_control`（`PipeName` 常量）
- 协议格式（行文本）：
  - 请求：`COMMAND [JSON_ARGS]\n`
  - 响应：`OK\n` / `ERROR <message>\n` / `DATA <JSON>\n`
- 命令常量：`PING`、`RELOAD_CONFIG`、`RELOAD_PHRASES`、`RELOAD_SHADOW`、`RELOAD_USERDICT`、`RELOAD_ALL`、`GET_STATUS`
- `ServiceStatus` 包含：`running`、`engine_type`、`chinese_mode`、`full_width`、`chinese_punct`、`dict_entries`、`user_dict_count`、`phrase_count`、`shadow_count`
- 添加新命令时需同时在 `protocol.go`（常量）和 `internal/control/server.go`（处理逻辑）添加

### Testing Requirements
- 协议解析（`ParseRequest`/`ParseResponse`）可做纯函数单元测试
- 客户端集成测试需要服务进程运行

### Common Patterns
- `Client.SendCommand(cmd, args)` 封装了连接、发送、接收、解析的完整流程
- 设置应用通过此包与服务进程通信，无需了解底层管道细节

## Dependencies
### Internal
- 无

### External
- `github.com/Microsoft/go-winio` — Named Pipe 客户端连接（client.go）

<!-- MANUAL: -->
