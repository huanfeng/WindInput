<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-20 | Updated: 2026-04-20 -->

# pkg/rpcapi

## Purpose
JSON-RPC 协议的请求/响应类型定义及帧协议实现。供 `internal/rpc` 服务端和客户端（Wails 设置应用）共用。定义了 Dict、Shadow、Phrase、System 四个服务的方法参数和返回类型。

## Key Files
| File | Description |
|------|-------------|
| `protocol.go` | 帧协议实现：`Request`/`Response` 结构体；`ReadMessage`/`WriteMessage` 函数；length-prefix 编码（4 字节大端序整数 + JSON payload） |
| `types.go` | RPC 方法的参数和返回值类型定义：DictSearchArgs、DictAddArgs、ShadowPinArgs 等；EventMessage 事件类型 |
| `client.go` | RPC 客户端实现：Named Pipe 连接、请求发送、响应接收、超时控制 |
| `protocol_test.go` | 帧协议单元测试（读写往返、边界条件） |

## For AI Agents

### Working In This Directory
- **管道名称**：`\\.\pipe\wind_input{Suffix}_rpc`（Suffix 通过 `buildvariant.Suffix()` 获取，用于多版本共存）
- **事件管道**：`\\.\pipe\wind_input{Suffix}_events`（用于推送变化事件）
- **帧格式**：4 字节大端序长度 + JSON payload；长度不含 4 字节头本身
- **协议版本**：`ProtocolVersion` 常量，服务端和客户端需匹配
- **请求格式**：`{ "version": int, "id": string, "method": "Service.Method", "params": {...} }`
- **响应格式**：`{ "id": string, "result": {...}, "error": "..." }`（error 为空表示成功）
- **异步请求**：某些请求（如 `ReloadAll`）无需客户端等待响应

### Testing Requirements
- 运行：`go test ./pkg/rpcapi`
- `protocol_test.go` 覆盖帧编解码、大小端、边界情况
- 集成测试需要同时启动服务端和客户端

### Common Patterns
- 类型定义遵循 Go 风格：字段大写（导出）、JSON 标签小写 + snake_case
- 可选字段用 `omitempty`（如 `schema_id,omitempty`）

## Dependencies
### Internal
- `pkg/buildvariant` — Suffix() 获取版本后缀

### External
- `encoding/json` — 标准库
- `io` — 标准库（ReadWriter）

<!-- MANUAL: -->
