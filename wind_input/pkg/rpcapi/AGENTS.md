<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-20 | Updated: 2026-05-17 -->

# pkg/rpcapi

## Purpose
JSON-RPC 协议的请求/响应类型定义及帧协议实现。供 `internal/rpc` 服务端和客户端（Wails 设置应用）共用。定义了 Dict、Shadow、Phrase、System 四个服务的方法参数和返回类型。

## Key Files
| File | Description |
|------|-------------|
| `protocol.go` | 帧协议实现: `Request`/`Response` 结构体; `ReadMessage`/`WriteMessage`; length-prefix 编码 (4 字节大端序 + JSON payload) |
| `types.go` | RPC 方法参数与返回值类型 (DictSearchArgs / DictAddArgs / ShadowPinArgs 等), `EventMessage` 事件类型 |
| `client.go` | 跨平台 RPC 客户端: `connect()` 走 `dialEndpoint` helper (平台分支), 短连接每次发送+接收+关闭 |
| `endpoint_windows.go` (`//go:build windows`) | `RPCPipeName` / `RPCEventPipeName` Named Pipe 路径常量 + `dialEndpoint` 走 `winio.DialPipe` |
| `endpoint_darwin.go` (`//go:build darwin`) | UDS 路径常量 (`rpc.sock` / `rpc_events.sock`); `dialEndpoint` 走 `net.Dial("unix", ...)`; 支持 `WIND_INPUT_RUNTIME_DIR` 环境变量覆盖 |
| `protocol_test.go` | 帧协议单元测试 (读写 roundtrip、边界条件) |

## For AI Agents

### EventType / EventAction / WailsEvent 枚举（SSOT）

所有事件枚举在 `types.go` 定义，前端 `enums.ts` 的 `WailsEvent` 是镜像，两边必须同步。

| EventType | 含义 | WailsEvent（前端） |
|-----------|------|--------------------|
| `config` | 配置变更 | `WailsEvent.Config` / `config-event` |
| `userdict` / `temp` / `shadow` / `freq` / `phrase` | 词库类变更 | `WailsEvent.Dict` / `dict-event` |
| `stats` | 统计数据变化（节流心跳 5s + 手动 Clear/Prune 立即推送） | `WailsEvent.Stats` / `stats-event` |
| `system` | 服务状态变化（Pause/Resume） | `WailsEvent.System` / `system-event` |

| EventAction | 含义 |
|-------------|------|
| `add` `remove` `update` `clear` `reset` | 标准 CRUD |
| `batch_put` `batch_add` `batch_set` | 批量操作 |
| `updated` | 聚合"有数据更新"信令（stats 心跳） |
| `paused` `resumed` | 服务暂停/恢复 |

### Shadow schema (2026-05-17 R2 CandID + 方案桶设计)

`ShadowPinArgs`/`ShadowDeleteArgs`/`PinnedEntry`/`ShadowPinItem`/`ShadowDelItem` 均新增
`CandID string (json:"cand_id,omitempty")` 字段。
- `CandID` 非空时按候选稳定 id 精准匹配（动态短语场景，Text 每次展开不同）；
- `CandID` 空时按 `Word` 匹配 `cand.Text`（兼容旧手输文本规则）。
- 客户端调用：`ShadowPin(schemaID, code, word, candID, position)`，`ShadowDelete/RemoveRule` 类同。

**方案桶设计**:
- `Shadow.Delete` / `Shadow.RemoveRule` 接受 `SchemaID` 字段, pin / delete 都写**方案桶** (`Schemas/{schemaID}/Shadow`)。
- 短语候选的"删除"已分流到 `PhraseRecord.Enabled = false` (跨方案的"禁用"语义), 因此 Shadow 不需要全局桶。
- `ShadowRulesReply.Deleted` 和 `ShadowCodeRules.Deleted` 从 `[]string` 升级为 `[]ShadowDeletedEntry{Word, CandID}` (替代 R2 当时遗留的 TODO), UI 端可按 id 删除短语 delete 规则。

#### ShadowDeletedEntry
```go
type ShadowDeletedEntry struct {
    Word   string `json:"word"`
    CandID string `json:"cand_id,omitempty"`
}
```

### Phrase schema (2026-05-16 简化)

`PhraseEntry` / `PhraseAddArgs` / `PhraseUpdateArgs` / `PhraseRemoveArgs` 字段统一为
`(code, text, weight, position, enabled, is_system)`, 删除 `Type` / `Texts` / `Name` 派生字段。
短语分类完全由 `text` 内容自描述 (`$AA(...)` 字符组 / `$SS(...)` 字符串数组 / `$CC(...)` 命令),
后端 PhraseLayer 在 LoadFromStore 时解析推断。`PhraseRemove` 客户端方法签名
`PhraseRemove(code, text)` (无 name)。

### Working In This Directory
- **端点路径** (平台分支):
  - Win: `\\.\pipe\wind_input{Suffix}_rpc` / `_events` (Suffix 通过 `buildvariant.Suffix()` 获取)
  - darwin: `~/Library/Application Support/WindInput{Suffix}/rpc.sock` / `rpc_events.sock` (可由 `WIND_INPUT_RUNTIME_DIR` 覆盖)
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
- Win: `github.com/Microsoft/go-winio` (overlapped Named Pipe)
- darwin: 仅标准库 `net` (Dial unix)
- `encoding/json` (跨平台)

## 全局约束
- macOS 移植: 见 [`/docs/design/macos-port.md`](../../../docs/design/macos-port.md)

<!-- MANUAL: -->
