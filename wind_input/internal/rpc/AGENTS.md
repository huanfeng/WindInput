<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-20 | Updated: 2026-05-02 -->

# internal/rpc

## Purpose
轻量级 IPC 服务端实现，为 Wails 设置应用提供词库管理、Shadow 规则、短语、系统状态查询等功能。采用 length-prefix 帧协议替代 `net/rpc`，避免引入 `net/http` 等重量级依赖。通过命名管道（`\\.\pipe\wind_input_rpc`）接收 JSON 格式请求，返回 JSON 响应。

分四个子服务：Dict（词库查询、添加、删除）、Shadow（pin/delete 规则）、Phrase（短语管理）、System（配置重载、方案管理、数据库重置）。

## Key Files
| File | Description |
|------|-------------|
| `server.go` | `Server`：IPC 服务端主体；`Start()`/`Stop()` 生命周期；`StatusProvider`/`ConfigReloader`/`BatchEncoder`/`PinyinCodeGenerator` 接口定义；注册四个服务的所有 RPC 方法 |
| `router.go` | `Router`：方法注册和分发（`RegisterMethod`、`Dispatch`），支持同步和异步请求 |
| `event.go` | `EventMessage`：数据变化事件定义；`EventBroadcaster`：事件广播管理 |
| `dict_service.go` | `DictService`：词库 RPC 实现（Search、SearchByCode、Add、Remove、Update、BatchEncode、GetStats 等） |
| `shadow_service.go` | `ShadowService`：Shadow pin/delete 规则 RPC 实现 |
| `phrase_service.go` | `PhraseService`：短语管理 RPC 实现 |
| `system_service.go` | `SystemService`：系统操作 RPC 实现（Ping、GetStatus、ReloadConfig、ReloadShadow、ReloadUserDict、ListSchemas、DeleteSchema、DumpPerf、GetPerfStats 等） |
| `server_test.go` | Server 集成测试（协议、方法分发、事件广播） |

## For AI Agents

### Working In This Directory
- **命名管道配置**：`winio.ListenPipe` + `PIPE_TYPE_MESSAGE`（MESSAGE 模式）+ 64KB 缓冲
- **协议**：请求/响应使用 `rpcapi.ReadMessage`/`WriteMessage`（length-prefix 帧）；JSON 格式
- **四个核心服务**：
  1. **Dict**：词库增删查改，支持分页、统计、批量操作
  2. **Shadow**：管理 pin/delete 规则
  3. **Phrase**：用户定义短语
  4. **System**：系统重置、配置重载、方案管理
- **事件推送**：通过独立的 `EventPipeServer` 异步推送数据变化事件到监听的客户端
- **接口注入**：`SetStatusProvider`、`SetConfigReloader`、`SetBatchEncoder`、`SetPinyinCodeGenerator` 供 main.go 注入依赖
- **拼音编码生成**：`DictService.Add` 当 `code` 为空且注入了 `PinyinCodeGenerator` 时自动生成全拼编码；`Dict.GeneratePinyinCode` RPC 方法供前端查询
- **线程安全**：Server 持有 `sync.Mutex`，确保状态变更原子性

### Testing Requirements
- 运行：`go test ./internal/rpc`
- `server_test.go` 覆盖请求处理、错误分发、事件推送
- 集成测试可验证 RPC 方法的完整流程

### Common Patterns
- 所有服务实例化时注入 `logger`、`store`、`dictManager`、`broadcaster` 等依赖
- 错误响应：`Response.Error` 填充错误信息，`Response.Result` 为 nil
- 成功响应：`Response.Result` 填充 JSON 序列化结果，`Response.Error` 为空

### ⚠️ 新增 RPC 方法必须手动注册
本包**不使用反射自动注册**，每个方法必须在 `server.go` 的 `Start()` 内用 `RegisterMethod` 显式注册，否则运行时报 `method not found: Xxx.YyyMethod`。

```go
// server.go — 在对应服务的注册块内添加一行：
RegisterMethod(s.router, "System.DumpGoroutineProfile", systemSvc.DumpGoroutineProfile)
```

**检查清单**（新增方法时必做）：
1. 在 `*_service.go` 中实现方法（签名：`func (s *XxxService) Method(args *T, reply *R) error`）
2. 在 `server.go` `Start()` 对应服务块内调用 `RegisterMethod`
3. 在 `pkg/rpcapi/client.go` 中添加客户端调用包装
4. 如需在设置 UI 暴露：`wind_setting/app_*.go` 添加 Wails 方法 → `frontend/src/api/wails.ts` 添加包装 → Vue 组件调用

## Dependencies
### Internal
- `internal/dict` — DictManager（词库管理）
- `internal/store` — Store（持久化存储 bbolt）
- `pkg/rpcapi` — 协议类型定义

### External
- `github.com/Microsoft/go-winio` — Named Pipe
- `encoding/json` — 标准库
- `log/slog` — 日志

<!-- MANUAL: -->
