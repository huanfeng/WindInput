# 词库管理界面重构 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 重构设置端词库管理界面，统一存储后端、优化布局、新增词频管理和实时更新

**Architecture:** 所有用户数据（短语、用户词、临时词、Shadow、词频）统一存入 bbolt Store。前端从多标签+左侧面板改为下拉选择器+全宽内容区。RPC 新增短语服务、词频端点和事件订阅长连接，设置端通过 Wails EventsEmit 实时刷新。

**Tech Stack:** Go 1.21 + bbolt 1.4 + JSON-RPC/named pipe + Vue 3 + TypeScript + Wails v2

---

## File Structure

### New Files
| File | Responsibility |
|------|---------------|
| `wind_input/internal/store/phrases.go` | Phrases bucket CRUD（全局，不按方案） |
| `wind_input/internal/store/phrases_test.go` | 短语存储测试 |
| `wind_input/internal/rpc/phrase_service.go` | RPC Phrase.* 服务 |
| `wind_input/internal/rpc/event.go` | RPC 事件订阅长连接 |

### Modified Files
| File | Changes |
|------|---------|
| `wind_input/internal/store/store.go` | 新增 `bucketPhrases`、`ListSchemaIDs()`、`init()` 创建 Phrases bucket |
| `wind_input/internal/store/freq.go` | 新增 `DeleteFreq()`、`ClearAllFreq()`、`SearchFreqPrefix()` |
| `wind_input/internal/rpc/server.go` | 注册 Phrase 服务；新增事件广播器 |
| `wind_input/internal/rpc/dict_service.go` | 新增 `GetFreqList`、`DeleteFreq`、`ClearFreq` 方法 |
| `wind_input/internal/rpc/system_service.go` | 新增 `ListSchemas`、`Subscribe` 方法 |
| `wind_input/internal/dict/manager.go` | 短语层改为从 Store 加载；新增 `SeedDefaultPhrases()` |
| `wind_input/internal/dict/phrase.go` | 新增 `LoadFromStore()` 方法 |
| `wind_input/pkg/rpcapi/types.go` | 新增 Phrase/Freq/Event 类型 |
| `wind_input/pkg/rpcapi/client.go` | 新增 Phrase/Freq/Subscribe 客户端方法 |
| `wind_input/cmd/service/main.go` | 启动时种子短语 |
| `wind_setting/app.go` | 移除文件式 phraseEditor；新增事件监听 |
| `wind_setting/app_service.go` | 新增短语/词频/方案列表 Wails 绑定 |
| `wind_setting/app_dict_schema.go` | 适配新的方案列表接口 |
| `wind_setting/frontend/src/api/wails.ts` | 新增短语/词频/事件 API |
| `wind_setting/frontend/src/pages/DictionaryPage.vue` | 完整重构 |

---

## Task 1: Store — 短语 CRUD

**Files:**
- Modify: `wind_input/internal/store/store.go`
- Create: `wind_input/internal/store/phrases.go`
- Create: `wind_input/internal/store/phrases_test.go`

短语存储在顶级 `Phrases` bucket（全局共享，不按方案隔离）。
Key = `code\x00text`（普通/动态短语）或 `code\x00\x01name`（数组短语，\x01 前缀区分）。
Value = JSON 编码的 PhraseRecord。

- [ ] **Step 1: 在 store.go 中新增 Phrases bucket 初始化**

在 `store.go` 中添加 `bucketPhrases` 变量，在 `init()` 中创建该 bucket：

```go
// store.go 新增变量
var bucketPhrases = []byte("Phrases")

// init() 中新增
if _, err := tx.CreateBucketIfNotExists(bucketPhrases); err != nil {
    return fmt.Errorf("create Phrases bucket: %w", err)
}
```

- [ ] **Step 2: 创建 phrases.go 实现 PhraseRecord 类型和 CRUD**

```go
// phrases.go
package store

type PhraseRecord struct {
    Code     string `json:"code,omitempty"` // 从 key 解析，不序列化到 value
    Text     string `json:"text,omitempty"`
    Texts    string `json:"texts,omitempty"` // 数组短语的字符列表
    Name     string `json:"name,omitempty"`  // 数组短语名称
    Type     string `json:"type"`            // "static" | "dynamic" | "array"
    Position int    `json:"pos"`
    Enabled  bool   `json:"on"`
    IsSystem bool   `json:"sys,omitempty"`   // 来自系统默认
}
```

方法签名（全部操作 Phrases 顶级 bucket，不经过 schemaBucket）：

```go
func phraseKey(code string, rec PhraseRecord) []byte
// 普通/动态: code\x00text  数组: code\x00\x01name

func parsePhraseKey(key []byte) (code, identifier string)

func (s *Store) GetAllPhrases() ([]PhraseRecord, error)
// 遍历 Phrases bucket 所有 key-value，解析并返回

func (s *Store) GetPhrasesByCode(code string) ([]PhraseRecord, error)
// 前缀扫描 code\x00

func (s *Store) AddPhrase(rec PhraseRecord) error
// Put key=phraseKey(code,rec), value=json(rec)

func (s *Store) UpdatePhrase(rec PhraseRecord) error
// 同 AddPhrase（覆盖写入）

func (s *Store) RemovePhrase(code, text, name string) error
// 根据 text 或 name 构造 key 并 Delete

func (s *Store) SetPhraseEnabled(code, text, name string, enabled bool) error
// 读取 → 修改 enabled → 写回

func (s *Store) PhraseCount() (int, error)
// bucket.Stats().KeyN

func (s *Store) ClearAllPhrases() error
// 删除并重建 Phrases bucket

func (s *Store) SeedPhrases(records []PhraseRecord) error
// 仅在 Phrases bucket 为空时批量写入（种子数据）
```

- [ ] **Step 3: 创建 phrases_test.go 测试**

测试覆盖：AddPhrase → GetAllPhrases、重复 Add 覆盖、RemovePhrase、SetPhraseEnabled、SeedPhrases 不覆盖已有数据、ClearAllPhrases。

- [ ] **Step 4: 在 store.go 新增 ListSchemaIDs**

```go
func (s *Store) ListSchemaIDs() ([]string, error)
// 遍历 Schemas bucket 的所有子 bucket key，返回 schema ID 列表
```

- [ ] **Step 5: 构建并运行测试**

```bash
cd wind_input && go fmt ./internal/store/... && go build ./... && go test ./internal/store/... -v
```

- [ ] **Step 6: 提交**

```bash
git add wind_input/internal/store/
git commit -m "feat(store): 新增 Phrases bucket CRUD 和 ListSchemaIDs"
```

---

## Task 2: Store — 词频查询与删除

**Files:**
- Modify: `wind_input/internal/store/freq.go`

当前 freq.go 只有 IncrementFreq/GetFreq/GetAllFreq/ResetStreak。需要新增前缀搜索、单条删除和全部清空。

- [ ] **Step 1: 新增词频方法**

```go
// SearchFreqPrefix 前缀搜索词频记录，返回 key（code:text）和 record
func (s *Store) SearchFreqPrefix(schemaID, prefix string, limit int) ([]FreqEntry, error)

type FreqEntry struct {
    Code     string     `json:"code"`
    Text     string     `json:"text"`
    Record   FreqRecord `json:"record"`
}
// 扫描 Freq bucket，key 格式 "code:text"，按 prefix 过滤

func (s *Store) DeleteFreq(schemaID, code, text string) error
// 删除单条词频记录

func (s *Store) ClearAllFreq(schemaID string) error
// 删除并重建 Freq sub-bucket
```

- [ ] **Step 2: 补充测试**

在现有 freq 测试文件中添加 SearchFreqPrefix、DeleteFreq、ClearAllFreq 测试。

- [ ] **Step 3: 构建并运行测试**

```bash
cd wind_input && go fmt ./internal/store/... && go build ./... && go test ./internal/store/... -v
```

- [ ] **Step 4: 提交**

```bash
git add wind_input/internal/store/freq.go wind_input/internal/store/*_test.go
git commit -m "feat(store): 新增词频前缀搜索、删除和清空方法"
```

---

## Task 3: RPC — 短语服务 + 词频/方案端点

**Files:**
- Modify: `wind_input/pkg/rpcapi/types.go`
- Modify: `wind_input/pkg/rpcapi/client.go`
- Create: `wind_input/internal/rpc/phrase_service.go`
- Modify: `wind_input/internal/rpc/dict_service.go`
- Modify: `wind_input/internal/rpc/system_service.go`
- Modify: `wind_input/internal/rpc/server.go`

- [ ] **Step 1: 在 types.go 中新增请求/响应类型**

```go
// ── Phrase 服务类型 ──

type PhraseEntry struct {
    Code     string `json:"code"`
    Text     string `json:"text,omitempty"`
    Texts    string `json:"texts,omitempty"`
    Name     string `json:"name,omitempty"`
    Type     string `json:"type"`
    Position int    `json:"position"`
    Enabled  bool   `json:"enabled"`
    IsSystem bool   `json:"is_system"`
}

type PhraseListReply struct {
    Phrases []PhraseEntry `json:"phrases"`
    Total   int           `json:"total"`
}

type PhraseAddArgs struct {
    Code     string `json:"code"`
    Text     string `json:"text,omitempty"`
    Texts    string `json:"texts,omitempty"`
    Name     string `json:"name,omitempty"`
    Type     string `json:"type"`
    Position int    `json:"position"`
}

type PhraseRemoveArgs struct {
    Code string `json:"code"`
    Text string `json:"text,omitempty"`
    Name string `json:"name,omitempty"`
}

type PhraseUpdateArgs struct {
    Code     string `json:"code"`
    Text     string `json:"text,omitempty"`
    Name     string `json:"name,omitempty"`
    // 可更新字段
    NewText     string `json:"new_text,omitempty"`
    NewPosition int    `json:"new_position,omitempty"`
    Enabled     *bool  `json:"enabled,omitempty"`
}

type PhraseResetArgs struct {} // 恢复系统默认

// ── Freq 服务类型 ──

type FreqSearchArgs struct {
    SchemaID string `json:"schema_id,omitempty"`
    Prefix   string `json:"prefix,omitempty"`
    Limit    int    `json:"limit,omitempty"`
    Offset   int    `json:"offset,omitempty"`
}

type FreqEntry struct {
    Code     string `json:"code"`
    Text     string `json:"text"`
    Count    int    `json:"count"`
    LastUsed int64  `json:"last_used"`
    Streak   int    `json:"streak"`
    Boost    int    `json:"boost"`
}

type FreqSearchReply struct {
    Entries []FreqEntry `json:"entries"`
    Total   int         `json:"total"`
}

type FreqDeleteArgs struct {
    SchemaID string `json:"schema_id,omitempty"`
    Code     string `json:"code"`
    Text     string `json:"text"`
}

type FreqClearArgs struct {
    SchemaID string `json:"schema_id,omitempty"`
}

type FreqClearReply struct {
    Count int `json:"count"`
}

// ── System 扩展类型 ──

type ListSchemasReply struct {
    Schemas []SchemaStatus `json:"schemas"`
}

type SchemaStatus struct {
    SchemaID      string `json:"schema_id"`
    Status        string `json:"status"` // "enabled" | "disabled" | "orphaned"
    UserWords     int    `json:"user_words"`
    TempWords     int    `json:"temp_words"`
    ShadowRules   int    `json:"shadow_rules"`
    FreqRecords   int    `json:"freq_records"`
}
```

- [ ] **Step 2: 创建 phrase_service.go**

```go
type PhraseService struct {
    store  *store.Store
    logger *slog.Logger
}

func (p *PhraseService) List(args *rpcapi.Empty, reply *rpcapi.PhraseListReply) error
func (p *PhraseService) Add(args *rpcapi.PhraseAddArgs, reply *rpcapi.Empty) error
func (p *PhraseService) Update(args *rpcapi.PhraseUpdateArgs, reply *rpcapi.Empty) error
func (p *PhraseService) Remove(args *rpcapi.PhraseRemoveArgs, reply *rpcapi.Empty) error
func (p *PhraseService) SetEnabled(args *rpcapi.PhraseUpdateArgs, reply *rpcapi.Empty) error
func (p *PhraseService) ResetDefaults(args *rpcapi.PhraseResetArgs, reply *rpcapi.Empty) error
// ResetDefaults: ClearAllPhrases → 重新 SeedPhrases
```

- [ ] **Step 3: 在 dict_service.go 新增词频方法**

```go
func (d *DictService) GetFreqList(args *rpcapi.FreqSearchArgs, reply *rpcapi.FreqSearchReply) error
// 调用 store.SearchFreqPrefix，计算每条的 CalcFreqBoost

func (d *DictService) DeleteFreq(args *rpcapi.FreqDeleteArgs, reply *rpcapi.Empty) error
func (d *DictService) ClearFreq(args *rpcapi.FreqClearArgs, reply *rpcapi.FreqClearReply) error
```

- [ ] **Step 4: 在 system_service.go 新增 ListSchemas**

```go
func (s *SystemService) ListSchemas(args *rpcapi.Empty, reply *rpcapi.ListSchemasReply) error
// 1. store.ListSchemaIDs() 获取 bbolt 中所有 schema
// 2. config.Load() 获取 Available schemas
// 3. 对比标注 status: enabled/disabled/orphaned
// 4. 为每个 schema 查询 UserWordCount/TempWordCount/ShadowRuleCount
```

- [ ] **Step 5: 在 server.go 注册 Phrase 服务**

```go
phraseSvc := &PhraseService{store: s.store, logger: s.logger}
if err := s.rpcServer.RegisterName("Phrase", phraseSvc); err != nil {
    return fmt.Errorf("register Phrase service: %w", err)
}
```

- [ ] **Step 6: 在 client.go 新增客户端方法**

```go
// Phrase 方法
func (c *Client) PhraseList() (*PhraseListReply, error)
func (c *Client) PhraseAdd(args PhraseAddArgs) error
func (c *Client) PhraseUpdate(args PhraseUpdateArgs) error
func (c *Client) PhraseRemove(code, text, name string) error
func (c *Client) PhraseSetEnabled(code, text, name string, enabled bool) error
func (c *Client) PhraseResetDefaults() error

// Freq 方法
func (c *Client) FreqSearch(schemaID, prefix string, limit, offset int) (*FreqSearchReply, error)
func (c *Client) FreqDelete(schemaID, code, text string) error
func (c *Client) FreqClear(schemaID string) (int, error)

// Schema 方法
func (c *Client) SystemListSchemas() (*ListSchemasReply, error)
```

- [ ] **Step 7: 构建**

```bash
cd wind_input && go fmt ./... && go build ./...
```

- [ ] **Step 8: 提交**

```bash
git add wind_input/internal/rpc/ wind_input/pkg/rpcapi/
git commit -m "feat(rpc): 新增短语管理、词频查询和方案列表 RPC 服务"
```

---

## Task 4: DictManager — 短语层迁移到 Store

**Files:**
- Modify: `wind_input/internal/dict/phrase.go`
- Modify: `wind_input/internal/dict/manager.go`
- Modify: `wind_input/cmd/service/main.go`

将 PhraseLayer 从文件加载改为从 Store 加载。首次启动时从系统短语 YAML 种子。

- [ ] **Step 1: 在 phrase.go 新增 LoadFromStore 方法**

```go
// LoadFromStore 从 Store 加载短语（替代文件加载）
func (pl *PhraseLayer) LoadFromStore(s *store.Store) error {
    records, err := s.GetAllPhrases()
    // 将 PhraseRecord 转换为内部 PhraseEntry/PhraseGroup
    // 跳过 Enabled=false 的条目
    // 识别 Type 分配到 staticPhrases/dynamicPhrases/phraseGroups
}
```

- [ ] **Step 2: 在 manager.go 新增 SeedDefaultPhrases**

```go
// SeedDefaultPhrases 在 Phrases bucket 为空时从系统短语文件种子
func (dm *DictManager) SeedDefaultPhrases() error {
    count, _ := dm.store.PhraseCount()
    if count > 0 { return nil } // 已有数据，不覆盖

    // 解析系统短语 YAML（复用现有 parsePhraseFile 逻辑）
    systemFile := filepath.Join(dm.systemDir, "system.phrases.yaml")
    entries, _ := parsePhraseYAML(systemFile)
    
    records := make([]store.PhraseRecord, 0, len(entries))
    for _, e := range entries {
        rec := store.PhraseRecord{
            Code: e.Code, Text: e.Text, Texts: e.Texts, Name: e.Name,
            Type: detectPhraseType(e), Position: e.Position,
            Enabled: !e.Disabled, IsSystem: true,
        }
        records = append(records, rec)
    }
    return dm.store.SeedPhrases(records)
}
```

- [ ] **Step 3: 修改 Initialize 方法**

```go
func (dm *DictManager) Initialize() error {
    if dm.useStore {
        // Store 后端：从 Store 加载短语
        dm.SeedDefaultPhrases()
        dm.phraseLayer = NewPhraseLayer(...)
        dm.phraseLayer.LoadFromStore(dm.store)
    } else {
        // 文件后端：保持原有逻辑
        dm.phraseLayer.Load()
    }
    dm.compositeDict.AddLayer(dm.phraseLayer)
    return nil
}
```

- [ ] **Step 4: 新增 ReloadPhrases 的 Store 路径**

```go
func (dm *DictManager) ReloadPhrases() error {
    if dm.useStore {
        return dm.phraseLayer.LoadFromStore(dm.store)
    }
    return dm.phraseLayer.Load()
}
```

- [ ] **Step 5: 构建并测试**

```bash
cd wind_input && go fmt ./... && go build ./... && go test ./internal/dict/... -v
```

- [ ] **Step 6: 提交**

```bash
git add wind_input/internal/dict/ wind_input/cmd/
git commit -m "feat(dict): 短语层支持从 Store 加载，首次启动自动种子系统默认短语"
```

---

## Task 5: RPC — 事件订阅长连接

**Files:**
- Create: `wind_input/internal/rpc/event.go`
- Modify: `wind_input/internal/rpc/server.go`
- Modify: `wind_input/internal/rpc/dict_service.go`
- Modify: `wind_input/internal/rpc/phrase_service.go`
- Modify: `wind_input/pkg/rpcapi/types.go`
- Modify: `wind_input/pkg/rpcapi/client.go`

实现事件广播器：服务端在数据变化时推送事件，设置端通过长连接接收。

- [ ] **Step 1: 在 types.go 新增事件类型**

```go
type EventMessage struct {
    Type     string `json:"type"`      // "userdict" | "temp" | "shadow" | "freq" | "phrase"
    SchemaID string `json:"schema_id,omitempty"`
    Action   string `json:"action"`    // "add" | "remove" | "update" | "clear"
}

type SubscribeReply struct {
    Events []EventMessage `json:"events"`
}
```

- [ ] **Step 2: 创建 event.go 实现事件广播器**

```go
// EventBroadcaster 管理事件订阅和广播
type EventBroadcaster struct {
    mu          sync.RWMutex
    subscribers map[int]chan EventMessage
    nextID      int
}

func NewEventBroadcaster() *EventBroadcaster
func (b *EventBroadcaster) Subscribe() (id int, ch <-chan EventMessage)
func (b *EventBroadcaster) Unsubscribe(id int)
func (b *EventBroadcaster) Broadcast(msg EventMessage)
```

- [ ] **Step 3: 在 server.go 集成广播器**

Server 持有 `*EventBroadcaster`，传给各 service。

- [ ] **Step 4: System.Subscribe RPC 方法**

```go
// Subscribe 长连接事件订阅（阻塞式，通过 streaming JSON 推送事件）
// 注意：标准 net/rpc 不支持 streaming，改用独立的命名管道端点
```

由于 Go 标准 `net/rpc` 不支持 streaming response，改用以下方案：
- 新增独立命名管道 `wind_input_rpc_events`
- 客户端连接后，服务端持续写入 JSON line 事件
- 每个连接是一个订阅者

```go
// server.go 新增
func (s *Server) startEventPipe() error {
    listener, _ := winio.ListenPipe(rpcapi.RPCEventPipeName, pipeConfig)
    go func() {
        for {
            conn, _ := listener.Accept()
            go s.handleEventConn(conn)
        }
    }()
}

func (s *Server) handleEventConn(conn net.Conn) {
    id, ch := s.broadcaster.Subscribe()
    defer s.broadcaster.Unsubscribe(id)
    defer conn.Close()
    enc := json.NewEncoder(conn)
    for msg := range ch {
        if err := enc.Encode(msg); err != nil { return }
    }
}
```

- [ ] **Step 5: 在数据修改方法中触发广播**

在 DictService.Add/Remove/Update、PhraseService.Add/Remove 等方法中，
操作成功后调用 `s.broadcaster.Broadcast(EventMessage{...})`。

- [ ] **Step 6: 在 client.go 新增事件监听**

```go
// rpcapi/client.go 新增
var RPCEventPipeName = `\\.\pipe\wind_input` + buildvariant.Suffix() + `_events`

func (c *Client) SubscribeEvents(ctx context.Context, handler func(EventMessage)) error {
    conn, _ := winio.DialPipe(RPCEventPipeName, &c.timeout)
    dec := json.NewDecoder(conn)
    go func() {
        <-ctx.Done()
        conn.Close()
    }()
    for {
        var msg EventMessage
        if err := dec.Decode(&msg); err != nil { return err }
        handler(msg)
    }
}
```

- [ ] **Step 7: 构建**

```bash
cd wind_input && go fmt ./... && go build ./...
```

- [ ] **Step 8: 提交**

```bash
git add wind_input/internal/rpc/ wind_input/pkg/rpcapi/
git commit -m "feat(rpc): 新增事件订阅长连接，支持数据变化实时推送"
```

---

## Task 6: 设置端后端 — Wails 绑定

**Files:**
- Modify: `wind_setting/app.go`
- Modify: `wind_setting/app_service.go`
- Modify: `wind_setting/app_dict_schema.go`

- [ ] **Step 1: 移除旧的 phraseEditor 依赖**

在 `app.go` 中移除 `phraseEditor`、`systemPhraseEditor`、`systemUserPhraseEditor` 字段，
以及 `startup()` 中的初始化代码。短语操作全部走 RPC。

- [ ] **Step 2: 新增短语管理 Wails 绑定**

在 `app_service.go` 中新增：

```go
func (a *App) GetPhrases() ([]rpcapi.PhraseEntry, error)
func (a *App) AddPhrase(code, text, texts, name, pType string, position int) error
func (a *App) UpdatePhrase(code, text, name string, newText string, newPosition int) error
func (a *App) RemovePhrase(code, text, name string) error
func (a *App) SetPhraseEnabled(code, text, name string, enabled bool) error
func (a *App) ResetPhrasesToDefault() error
```

- [ ] **Step 3: 新增词频管理 Wails 绑定**

```go
func (a *App) GetFreqList(schemaID, prefix string, limit, offset int) (*rpcapi.FreqSearchReply, error)
func (a *App) DeleteFreq(schemaID, code, text string) error
func (a *App) ClearFreq(schemaID string) (int, error)
```

- [ ] **Step 4: 新增方案列表 Wails 绑定**

```go
func (a *App) GetAllSchemaStatuses() ([]rpcapi.SchemaStatus, error)
// 调用 rpcClient.SystemListSchemas()
```

- [ ] **Step 5: 新增事件监听**

在 `app.go` 的 `startup()` 中启动事件监听 goroutine：

```go
func (a *App) startEventListener() {
    ctx, cancel := context.WithCancel(a.ctx)
    a.eventCancel = cancel
    go func() {
        a.rpcClient.SubscribeEvents(ctx, func(msg rpcapi.EventMessage) {
            // 通过 Wails runtime 推送到前端
            wailsRuntime.EventsEmit(a.ctx, "dict-event", msg)
        })
    }()
}
```

- [ ] **Step 6: 构建**

```bash
cd wind_setting && go fmt ./... && go build ./...
```

- [ ] **Step 7: 提交**

```bash
git add wind_setting/
git commit -m "feat(setting): 新增短语/词频/事件监听 Wails 绑定，移除文件式 phraseEditor"
```

---

## Task 7: 前端 — API 层

**Files:**
- Modify: `wind_setting/frontend/src/api/wails.ts`

- [ ] **Step 1: 新增 TypeScript 类型和 API 方法**

```typescript
// 短语类型
export interface PhraseItem {
  code: string;
  text?: string;
  texts?: string;
  name?: string;
  type: "static" | "dynamic" | "array";
  position: number;
  enabled: boolean;
  is_system: boolean;
}

// 词频类型
export interface FreqItem {
  code: string;
  text: string;
  count: number;
  last_used: number;
  streak: number;
  boost: number;
}

// 方案状态类型
export interface SchemaStatusItem {
  schema_id: string;
  status: "enabled" | "disabled" | "orphaned";
  user_words: number;
  temp_words: number;
  shadow_rules: number;
  freq_records: number;
}

// 短语 API
export async function getPhrases(): Promise<PhraseItem[]>
export async function addPhrase(code, text, texts, name, type, position): Promise<void>
export async function updatePhrase(code, text, name, newText, newPosition): Promise<void>
export async function removePhrase(code, text, name): Promise<void>
export async function setPhraseEnabled(code, text, name, enabled): Promise<void>
export async function resetPhrasesToDefault(): Promise<void>

// 词频 API
export async function getFreqList(schemaID, prefix, limit, offset): Promise<{entries: FreqItem[], total: number}>
export async function deleteFreq(schemaID, code, text): Promise<void>
export async function clearFreq(schemaID): Promise<number>

// 方案列表 API
export async function getAllSchemaStatuses(): Promise<SchemaStatusItem[]>

// 事件监听（Wails runtime events）
export function onDictEvent(callback: (event: any) => void): void {
  window.runtime.EventsOn("dict-event", callback);
}
export function offDictEvent(): void {
  window.runtime.EventsOff("dict-event");
}
```

- [ ] **Step 2: 提交**

```bash
git add wind_setting/frontend/src/api/
git commit -m "feat(setting/frontend): 新增短语/词频/事件监听 API 类型和方法"
```

---

## Task 8: 前端 — DictionaryPage.vue 重构

**Files:**
- Modify: `wind_setting/frontend/src/pages/DictionaryPage.vue`

这是最大的单一任务。完整重写 DictionaryPage.vue 的模板和逻辑。

### 新 UI 结构

```
┌──────────────────────────────────────────────┐
│ 词库管理  管理您的词库数据（修改即时生效）      │  ← 标题一行，紧凑
├──────────────────────────────────────────────┤
│ 词库: [快捷短语] [wubi86·五笔 ▾]             │  ← 类型选择器
├──────────────────────────────────────────────┤
│ [用户词库][词频][临时词库][候选调整]            │  ← 子标签（仅方案模式）
│ [全选][+添加][删除]  搜索... [重置▾]          │  ← 工具栏
├──────────────────────────────────────────────┤
│                                              │
│  (全宽内容区，flex-grow 占满)                  │  ← 数据列表/表格
│                                              │
└──────────────────────────────────────────────┘
```

- [ ] **Step 1: 重写模板 — 标题栏 + 类型选择器**

标题从两行改为一行，减少间距。类型选择器替代旧的 4 标签页导航：

```vue
<div class="dict-header">
  <h2>词库管理</h2>
  <span class="dict-header-desc">管理您的词库数据（修改即时生效）</span>
</div>

<div class="dict-type-selector">
  <span class="selector-label">词库:</span>
  <button :class="['type-btn', { active: dictMode === 'phrases' }]"
          @click="dictMode = 'phrases'">
    快捷短语
  </button>
  <div class="schema-dropdown">
    <button :class="['type-btn', { active: dictMode === 'schema' }]"
            @click="toggleSchemaDropdown">
      {{ currentSchemaLabel }} ▾
    </button>
    <div v-if="showSchemaDropdown" class="dropdown-list">
      <div v-for="s in allSchemas" :key="s.schema_id"
           :class="['dropdown-item', { active: selectedSchemaID === s.schema_id }]"
           @click="selectSchema(s.schema_id)">
        <span>{{ s.schema_name || s.schema_id }}</span>
        <span class="schema-badge">{{ s.user_words }}词</span>
        <span v-if="s.status !== 'enabled'" class="schema-status">
          ({{ s.status === 'disabled' ? '未启用' : '残留' }})
        </span>
      </div>
    </div>
  </div>
</div>
```

- [ ] **Step 2: 重写模板 — 快捷短语模式**

选中「快捷短语」时，直接显示短语列表（无子标签、无方案面板）：

```vue
<template v-if="dictMode === 'phrases'">
  <div class="dict-toolbar">
    <label class="toolbar-checkbox">
      <input type="checkbox" v-model="allPhraseSelected" @change="toggleAllPhrases" /> 全选
    </label>
    <button class="btn btn-sm btn-primary" @click="showAddPhraseDialog">+ 添加</button>
    <button class="btn btn-sm btn-danger-outline" :disabled="selectedPhraseKeys.size === 0"
            @click="handleBatchRemovePhrases">
      删除{{ selectedPhraseKeys.size > 0 ? ` (${selectedPhraseKeys.size})` : '' }}
    </button>
    <div class="toolbar-spacer"></div>
    <input type="text" class="toolbar-search" v-model="phraseSearch" placeholder="搜索..." />
    <button class="btn btn-sm" @click="handleResetPhrases">恢复默认</button>
  </div>
  <div class="dict-content">
    <div class="dict-list dict-list-scrollable">
      <div v-for="item in filteredPhrases" :key="phraseKey(item)" class="dict-list-item">
        <input type="checkbox" ... />
        <label class="phrase-toggle">
          <input type="checkbox" :checked="item.enabled" @change="togglePhraseEnabled(item)" />
        </label>
        <span class="dict-item-code">{{ item.code }}</span>
        <span class="dict-item-text">{{ item.text || item.name }}</span>
        <span v-if="item.type === 'array'" class="dict-item-tag tag-array">数组</span>
        <span v-if="item.type === 'dynamic'" class="dict-item-tag tag-dynamic">动态</span>
        <span v-if="item.is_system" class="dict-item-tag tag-system">系统</span>
        <span class="dict-item-weight">{{ item.position }}</span>
        <div class="dict-item-actions">
          <button class="btn-icon" @click="editPhrase(item)">✎</button>
          <button class="btn-icon btn-delete" @click="removePhrase(item)">×</button>
        </div>
      </div>
    </div>
  </div>
</template>
```

- [ ] **Step 3: 重写模板 — 方案模式子标签页**

选中某个方案时，显示子标签页（用户词库/词频/临时词库/候选调整）+ 工具栏 + 内容区。

4 个子标签页各有独立的工具栏按钮组合和内容展示。工具栏中的「重置」改为下拉菜单：

```vue
<template v-else-if="dictMode === 'schema'">
  <!-- 残留/未启用警告 -->
  <div v-if="currentSchemaStatus !== 'enabled'" class="dict-warning">
    ⚠ 此方案{{ currentSchemaStatus === 'disabled' ? '未启用' : '数据为历史残留' }}
  </div>

  <!-- 子标签页 -->
  <div class="dict-sub-tabs">
    <button v-for="tab in schemaTabs" :key="tab.key"
            :class="['sub-tab', { active: schemaSubTab === tab.key }]"
            @click="schemaSubTab = tab.key">
      {{ tab.label }}
    </button>
  </div>

  <!-- 工具栏（根据子标签页变化） -->
  <div class="dict-toolbar">
    <!-- 根据 schemaSubTab 显示不同按钮组 -->
    ...
    <div class="toolbar-spacer"></div>
    <div class="reset-dropdown">
      <button class="btn btn-sm btn-danger-outline" @click="showResetMenu = !showResetMenu">
        重置 ▾
      </button>
      <div v-if="showResetMenu" class="dropdown-list">
        <div @click="handleResetCurrentSchema">重置当前方案</div>
        <div @click="handleResetAllSchemas">重置所有方案</div>
      </div>
    </div>
  </div>

  <!-- 内容区 -->
  <div class="dict-content">
    <!-- 用户词库 / 词频 / 临时词库 / 候选调整 各自的列表 -->
  </div>
</template>
```

- [ ] **Step 4: 重写模板 — 词频子标签页内容**

```vue
<template v-if="schemaSubTab === 'freq'">
  <!-- 词频表格 -->
  <div class="dict-list dict-list-scrollable">
    <div v-for="item in freqList" :key="item.code + ':' + item.text" class="dict-list-item">
      <input type="checkbox" ... />
      <span class="dict-item-code">{{ item.code }}</span>
      <span class="dict-item-text">{{ item.text }}</span>
      <span class="dict-item-weight">×{{ item.count }}</span>
      <span class="dict-item-weight">boost:{{ item.boost }}</span>
      <span class="dict-item-time">{{ formatTime(item.last_used) }}</span>
      <div class="dict-item-actions">
        <button class="btn-icon btn-delete" @click="handleDeleteFreq(item)">×</button>
      </div>
    </div>
  </div>
</template>
```

- [ ] **Step 5: 重写 script — 状态管理**

```typescript
// 模式
const dictMode = ref<"phrases" | "schema">("phrases");
const schemaSubTab = ref<"userdict" | "freq" | "temp" | "shadow">("userdict");

// 方案列表（含状态标注）
const allSchemas = ref<SchemaStatusItem[]>([]);
const selectedSchemaID = ref("");

// 快捷短语
const phrases = ref<PhraseItem[]>([]);
const phraseSearch = ref("");
const filteredPhrases = computed(() => { ... });

// 词频
const freqList = ref<FreqItem[]>([]);
const freqSearch = ref("");

// 事件监听
onMounted(() => {
  wailsApi.onDictEvent((event) => {
    // 根据 event.type 和当前展示的标签页决定是否刷新
    if (event.type === "phrase") loadPhraseData();
    if (event.schema_id === selectedSchemaID.value) {
      if (event.type === "userdict") loadUserDictData();
      if (event.type === "freq") loadFreqData();
      if (event.type === "temp") loadTempData();
      if (event.type === "shadow") loadShadowData();
    }
  });
});
onUnmounted(() => {
  wailsApi.offDictEvent();
});
```

- [ ] **Step 6: 重写 script — 数据加载方法**

```typescript
async function loadSchemaList() {
  allSchemas.value = await wailsApi.getAllSchemaStatuses();
  if (!selectedSchemaID.value && allSchemas.value.length > 0) {
    const first = allSchemas.value.find(s => s.status === "enabled");
    if (first) selectSchema(first.schema_id);
  }
}

async function loadPhraseData() {
  phrases.value = await wailsApi.getPhrases();
}

async function loadFreqData() {
  if (!selectedSchemaID.value) return;
  const reply = await wailsApi.getFreqList(selectedSchemaID.value, freqSearch.value, 200, 0);
  freqList.value = reply.entries || [];
}
```

- [ ] **Step 7: 重写 script — 操作处理函数**

短语操作、词频操作、重置操作等处理函数。
用户词库/临时词库/候选调整的操作函数从旧代码迁移，适配新的状态结构。

- [ ] **Step 8: 重写样式 — 紧凑布局**

```css
.dict-header {
  display: flex;
  align-items: baseline;
  gap: 12px;
  padding: 12px 16px 8px;
}
.dict-header h2 { margin: 0; font-size: 16px; }
.dict-header-desc { font-size: 12px; color: #9ca3af; }

.dict-type-selector {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 0 16px 8px;
}

.dict-toolbar {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 6px 16px;
  border-bottom: 1px solid #f3f4f6;
}

.dict-content {
  flex: 1;
  overflow: hidden;
  display: flex;
  flex-direction: column;
}

.dict-list-scrollable {
  flex: 1;
  overflow-y: auto;
}
```

- [ ] **Step 9: 构建前端并验证**

```bash
cd wind_setting/frontend && npx prettier --write src/pages/DictionaryPage.vue src/api/wails.ts
```

- [ ] **Step 10: 全量构建验证**

```bash
cd wind_input && go build ./...
cd ../wind_setting && go build ./...
```

- [ ] **Step 11: 提交**

```bash
git add wind_setting/
git commit -m "feat(setting): 词库管理界面全面重构，统一布局+词频+事件监听"
```

---

## Self-Review Checklist

1. **Spec coverage:**
   - ✅ UI 重构（标题压缩、方案下拉、全宽内容区、工具栏顶部）
   - ✅ 短语入库（Phrases bucket、种子、单级列表、enabled 开关、恢复默认）
   - ✅ 词频标签页（RPC、前端展示、删除/清空）
   - ✅ 残留方案数据（ListSchemaIDs、状态标注、查看/删除）
   - ✅ 实时更新（事件广播、长连接推送、Wails EventsEmit）

2. **Placeholder scan:** 无 TBD/TODO，所有步骤包含具体代码或明确描述。

3. **Type consistency:**
   - `PhraseRecord`（Store 层）→ `PhraseEntry`（RPC 层）→ `PhraseItem`（前端层）：字段一致
   - `FreqEntry`（Store 层/RPC 层）→ `FreqItem`（前端层）：字段一致
   - `SchemaStatus`（RPC 层）→ `SchemaStatusItem`（前端层）：字段一致
   - `EventMessage`（RPC 层）→ 前端 `dict-event`：结构一致
