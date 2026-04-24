# JSON-RPC 控制管道实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在现有控制管道基础上新增 JSON-RPC 服务，提供用户词库的分页查询、增删改、Shadow 规则管理和系统状态查询能力，供 Wails 设置端调用。

**Architecture:** 新建 `internal/rpc` 包实现 RPC 服务端，通过独立的命名管道 `\\.\pipe\windinput_rpc` 提供 JSON-RPC 协议。RPC 服务注册 `Dict`、`Shadow`、`System` 三个服务对象，每个连接由 `jsonrpc.ServeConn` 处理。共享类型定义在 `pkg/rpcapi` 包中，供服务端和客户端（Wails）共用。现有控制管道保持不变（兼容旧客户端）。

**Tech Stack:** Go 标准库 `net/rpc` + `net/rpc/jsonrpc`，`go-winio` 命名管道

---

## 文件结构

### 新建文件

| 文件 | 职责 |
|------|------|
| `pkg/rpcapi/types.go` | RPC 请求/响应共享类型定义 |
| `internal/rpc/server.go` | RPC 服务端：管道监听、连接管理、服务注册 |
| `internal/rpc/server_test.go` | 服务端集成测试 |
| `internal/rpc/dict_service.go` | Dict 服务：用户词库 CRUD + 分页查询 |
| `internal/rpc/shadow_service.go` | Shadow 服务：Pin/Delete/Remove/GetRules |
| `internal/rpc/system_service.go` | System 服务：Ping/GetStatus/Reload |

### 修改文件

| 文件 | 修改内容 |
|------|----------|
| `cmd/service/main.go` | 创建并启动 RPC 服务端 |

---

## Task 1: 定义 RPC 共享类型

**Files:**
- Create: `wind_input/pkg/rpcapi/types.go`

- [ ] **Step 1: 创建类型定义文件**

```go
// Package rpcapi 定义 JSON-RPC 的请求/响应类型
// 供服务端和客户端（Wails 设置端）共用
package rpcapi

import "github.com/huanfeng/wind_input/pkg/buildvariant"

// RPC 管道名称
var RPCPipeName = `\\.\pipe\wind_input` + buildvariant.Suffix() + `_rpc`

// ── Dict 服务类型 ──

// DictSearchArgs 词库搜索请求
type DictSearchArgs struct {
	SchemaID string `json:"schema_id,omitempty"` // 方案 ID（空=当前活跃方案）
	Prefix   string `json:"prefix"`              // 编码前缀
	Limit    int    `json:"limit,omitempty"`      // 每页数量（默认 50）
	Offset   int    `json:"offset,omitempty"`     // 偏移量
}

// DictSearchReply 词库搜索响应
type DictSearchReply struct {
	Words []WordEntry `json:"words"`
	Total int         `json:"total"` // 总数（用于分页）
}

// WordEntry 词条
type WordEntry struct {
	Code      string `json:"code"`
	Text      string `json:"text"`
	Weight    int    `json:"weight"`
	Count     int    `json:"count,omitempty"`
	CreatedAt int64  `json:"created_at,omitempty"`
}

// DictAddArgs 添加词条请求
type DictAddArgs struct {
	SchemaID string `json:"schema_id,omitempty"`
	Code     string `json:"code"`
	Text     string `json:"text"`
	Weight   int    `json:"weight"`
}

// DictRemoveArgs 删除词条请求
type DictRemoveArgs struct {
	SchemaID string `json:"schema_id,omitempty"`
	Code     string `json:"code"`
	Text     string `json:"text"`
}

// DictUpdateArgs 更新词条权重请求
type DictUpdateArgs struct {
	SchemaID  string `json:"schema_id,omitempty"`
	Code      string `json:"code"`
	Text      string `json:"text"`
	NewWeight int    `json:"new_weight"`
}

// DictStatsReply 词库统计响应
type DictStatsReply struct {
	Stats map[string]int `json:"stats"`
}

// ── Shadow 服务类型 ──

// ShadowPinArgs 置顶请求
type ShadowPinArgs struct {
	SchemaID string `json:"schema_id,omitempty"`
	Code     string `json:"code"`
	Word     string `json:"word"`
	Position int    `json:"position"`
}

// ShadowDeleteArgs 隐藏请求
type ShadowDeleteArgs struct {
	SchemaID string `json:"schema_id,omitempty"`
	Code     string `json:"code"`
	Word     string `json:"word"`
}

// ShadowGetRulesArgs 获取规则请求
type ShadowGetRulesArgs struct {
	SchemaID string `json:"schema_id,omitempty"`
	Code     string `json:"code"`
}

// ShadowRulesReply 规则响应
type ShadowRulesReply struct {
	Pinned  []PinnedEntry `json:"pinned,omitempty"`
	Deleted []string      `json:"deleted,omitempty"`
}

// PinnedEntry 置顶条目
type PinnedEntry struct {
	Word     string `json:"word"`
	Position int    `json:"position"`
}

// ── System 服务类型 ──

// Empty 空参数/响应
type Empty struct{}

// SystemStatusReply 系统状态响应
type SystemStatusReply struct {
	Running       bool   `json:"running"`
	SchemaID      string `json:"schema_id"`
	EngineType    string `json:"engine_type"`
	ChineseMode   bool   `json:"chinese_mode"`
	StoreEnabled  bool   `json:"store_enabled"`
	UserWords     int    `json:"user_words"`
	TempWords     int    `json:"temp_words"`
	Phrases       int    `json:"phrases"`
	ShadowRules   int    `json:"shadow_rules"`
}
```

- [ ] **Step 2: 编译验证**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go build ./pkg/rpcapi/
```

Expected: 编译成功

---

## Task 2: 实现 RPC 服务端

**Files:**
- Create: `wind_input/internal/rpc/server.go`

- [ ] **Step 1: 实现 RPC 服务端**

```go
// Package rpc 提供 JSON-RPC 服务端实现
package rpc

import (
	"fmt"
	"log/slog"
	"net"
	"net/rpc"
	"net/rpc/jsonrpc"
	"sync"

	"github.com/Microsoft/go-winio"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/store"
	"github.com/huanfeng/wind_input/pkg/rpcapi"
)

// Server JSON-RPC 服务端
type Server struct {
	logger      *slog.Logger
	dictManager *dict.DictManager
	store       *store.Store
	rpcServer   *rpc.Server

	listener net.Listener
	wg       sync.WaitGroup
	stopCh   chan struct{}
	mu       sync.Mutex
	running  bool

	// 可选：系统状态提供者
	statusProvider StatusProvider
}

// StatusProvider 系统状态提供者接口
type StatusProvider interface {
	GetSchemaID() string
	GetEngineType() string
	IsChineseMode() bool
}

// NewServer 创建 RPC 服务端
func NewServer(logger *slog.Logger, dm *dict.DictManager, s *store.Store) *Server {
	return &Server{
		logger:      logger,
		dictManager: dm,
		store:       s,
		rpcServer:   rpc.NewServer(),
		stopCh:      make(chan struct{}),
	}
}

// SetStatusProvider 设置系统状态提供者
func (s *Server) SetStatusProvider(provider StatusProvider) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.statusProvider = provider
}

// Start 启动 RPC 服务
func (s *Server) Start() error {
	s.mu.Lock()
	if s.running {
		s.mu.Unlock()
		return fmt.Errorf("rpc server already running")
	}
	s.mu.Unlock()

	// 注册服务
	dictSvc := &DictService{store: s.store, dm: s.dictManager, logger: s.logger}
	shadowSvc := &ShadowService{store: s.store, dm: s.dictManager, logger: s.logger}
	systemSvc := &SystemService{dm: s.dictManager, server: s, logger: s.logger}

	if err := s.rpcServer.RegisterName("Dict", dictSvc); err != nil {
		return fmt.Errorf("register Dict service: %w", err)
	}
	if err := s.rpcServer.RegisterName("Shadow", shadowSvc); err != nil {
		return fmt.Errorf("register Shadow service: %w", err)
	}
	if err := s.rpcServer.RegisterName("System", systemSvc); err != nil {
		return fmt.Errorf("register System service: %w", err)
	}

	// 创建命名管道监听器
	pipeConfig := &winio.PipeConfig{
		SecurityDescriptor: "",
		InputBufferSize:    65536,
		OutputBufferSize:   65536,
	}
	listener, err := winio.ListenPipe(rpcapi.RPCPipeName, pipeConfig)
	if err != nil {
		return fmt.Errorf("listen rpc pipe: %w", err)
	}
	s.listener = listener

	s.mu.Lock()
	s.running = true
	s.mu.Unlock()

	s.logger.Info("RPC server started", "pipe", rpcapi.RPCPipeName)

	s.wg.Add(1)
	go s.acceptLoop()

	return nil
}

// StartAsync 异步启动
func (s *Server) StartAsync() {
	go func() {
		if err := s.Start(); err != nil {
			s.logger.Error("Failed to start RPC server", "error", err)
		}
	}()
}

// Stop 停止服务
func (s *Server) Stop() {
	s.mu.Lock()
	if !s.running {
		s.mu.Unlock()
		return
	}
	s.running = false
	s.mu.Unlock()

	close(s.stopCh)
	if s.listener != nil {
		s.listener.Close()
	}
	s.wg.Wait()
	s.logger.Info("RPC server stopped")
}

func (s *Server) acceptLoop() {
	defer s.wg.Done()
	for {
		conn, err := s.listener.Accept()
		if err != nil {
			select {
			case <-s.stopCh:
				return
			default:
				s.logger.Error("RPC accept error", "error", err)
				continue
			}
		}
		s.wg.Add(1)
		go s.handleConn(conn)
	}
}

func (s *Server) handleConn(conn net.Conn) {
	defer s.wg.Done()
	defer conn.Close()
	s.rpcServer.ServeCodec(jsonrpc.NewServerCodec(conn))
}
```

- [ ] **Step 2: 编译验证**（此步会失败，等待后续 Task 实现服务类型）

---

## Task 3: 实现 Dict 服务

**Files:**
- Create: `wind_input/internal/rpc/dict_service.go`

- [ ] **Step 1: 实现 Dict 服务**

```go
package rpc

import (
	"fmt"
	"log/slog"
	"strings"

	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/store"
	"github.com/huanfeng/wind_input/pkg/rpcapi"
)

// DictService 词库管理 RPC 服务
type DictService struct {
	store  *store.Store
	dm     *dict.DictManager
	logger *slog.Logger
}

// resolveSchemaID 解析方案 ID（空则使用当前活跃方案）
func (d *DictService) resolveSchemaID(id string) string {
	if id != "" {
		return id
	}
	return d.dm.GetActiveSchemaID()
}

// Search 搜索用户词库（分页）
func (d *DictService) Search(args *rpcapi.DictSearchArgs, reply *rpcapi.DictSearchReply) error {
	if d.store == nil {
		return fmt.Errorf("store not available")
	}

	schemaID := d.resolveSchemaID(args.SchemaID)
	prefix := strings.ToLower(args.Prefix)
	limit := args.Limit
	if limit <= 0 {
		limit = 50
	}

	// 查询所有匹配项（用于计算 total）
	allWords, err := d.store.SearchUserWordsPrefix(schemaID, prefix, 0)
	if err != nil {
		return fmt.Errorf("search: %w", err)
	}

	reply.Total = len(allWords)

	// 应用分页
	start := args.Offset
	if start > len(allWords) {
		start = len(allWords)
	}
	end := start + limit
	if end > len(allWords) {
		end = len(allWords)
	}

	pageWords := allWords[start:end]
	reply.Words = make([]rpcapi.WordEntry, len(pageWords))
	for i, w := range pageWords {
		// 从 key 恢复 code 信息需要额外处理
		reply.Words[i] = rpcapi.WordEntry{
			Text:      w.Text,
			Weight:    w.Weight,
			Count:     w.Count,
			CreatedAt: w.CreatedAt,
		}
	}

	return nil
}

// SearchByCode 精确编码查询
func (d *DictService) SearchByCode(args *rpcapi.DictSearchArgs, reply *rpcapi.DictSearchReply) error {
	if d.store == nil {
		return fmt.Errorf("store not available")
	}

	schemaID := d.resolveSchemaID(args.SchemaID)
	code := strings.ToLower(args.Prefix)

	words, err := d.store.GetUserWords(schemaID, code)
	if err != nil {
		return fmt.Errorf("search by code: %w", err)
	}

	reply.Total = len(words)
	reply.Words = make([]rpcapi.WordEntry, len(words))
	for i, w := range words {
		reply.Words[i] = rpcapi.WordEntry{
			Code:      code,
			Text:      w.Text,
			Weight:    w.Weight,
			Count:     w.Count,
			CreatedAt: w.CreatedAt,
		}
	}

	return nil
}

// Add 添加用户词条
func (d *DictService) Add(args *rpcapi.DictAddArgs, reply *rpcapi.Empty) error {
	if d.store == nil {
		return fmt.Errorf("store not available")
	}
	if args.Code == "" || args.Text == "" {
		return fmt.Errorf("code and text are required")
	}

	schemaID := d.resolveSchemaID(args.SchemaID)
	weight := args.Weight
	if weight <= 0 {
		weight = 1200 // 默认用户词权重
	}

	d.logger.Info("RPC Dict.Add", "schemaID", schemaID, "codeLen", len(args.Code), "textLen", len([]rune(args.Text)))
	return d.store.AddUserWord(schemaID, args.Code, args.Text, weight)
}

// Remove 删除用户词条
func (d *DictService) Remove(args *rpcapi.DictRemoveArgs, reply *rpcapi.Empty) error {
	if d.store == nil {
		return fmt.Errorf("store not available")
	}
	if args.Code == "" || args.Text == "" {
		return fmt.Errorf("code and text are required")
	}

	schemaID := d.resolveSchemaID(args.SchemaID)
	d.logger.Info("RPC Dict.Remove", "schemaID", schemaID, "codeLen", len(args.Code), "textLen", len([]rune(args.Text)))
	return d.store.RemoveUserWord(schemaID, args.Code, args.Text)
}

// Update 更新词条权重
func (d *DictService) Update(args *rpcapi.DictUpdateArgs, reply *rpcapi.Empty) error {
	if d.store == nil {
		return fmt.Errorf("store not available")
	}
	if args.Code == "" || args.Text == "" {
		return fmt.Errorf("code and text are required")
	}

	schemaID := d.resolveSchemaID(args.SchemaID)
	return d.store.UpdateUserWordWeight(schemaID, args.Code, args.Text, args.NewWeight)
}

// GetStats 获取词库统计
func (d *DictService) GetStats(args *rpcapi.Empty, reply *rpcapi.DictStatsReply) error {
	reply.Stats = d.dm.GetStats()
	return nil
}
```

- [ ] **Step 2: 编译验证**（等待后续 Task）

---

## Task 4: 实现 Shadow 服务

**Files:**
- Create: `wind_input/internal/rpc/shadow_service.go`

- [ ] **Step 1: 实现 Shadow 服务**

```go
package rpc

import (
	"fmt"
	"log/slog"

	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/store"
	"github.com/huanfeng/wind_input/pkg/rpcapi"
)

// ShadowService Shadow 规则管理 RPC 服务
type ShadowService struct {
	store  *store.Store
	dm     *dict.DictManager
	logger *slog.Logger
}

func (s *ShadowService) resolveSchemaID(id string) string {
	if id != "" {
		return id
	}
	return s.dm.GetActiveSchemaID()
}

// Pin 固定词到指定位置
func (s *ShadowService) Pin(args *rpcapi.ShadowPinArgs, reply *rpcapi.Empty) error {
	if s.store == nil {
		return fmt.Errorf("store not available")
	}
	schemaID := s.resolveSchemaID(args.SchemaID)
	s.logger.Info("RPC Shadow.Pin", "schemaID", schemaID, "code", args.Code, "position", args.Position)
	return s.store.PinShadow(schemaID, args.Code, args.Word, args.Position)
}

// Delete 隐藏词条
func (s *ShadowService) Delete(args *rpcapi.ShadowDeleteArgs, reply *rpcapi.Empty) error {
	if s.store == nil {
		return fmt.Errorf("store not available")
	}
	schemaID := s.resolveSchemaID(args.SchemaID)
	s.logger.Info("RPC Shadow.Delete", "schemaID", schemaID, "code", args.Code)
	return s.store.DeleteShadow(schemaID, args.Code, args.Word)
}

// RemoveRule 移除所有规则
func (s *ShadowService) RemoveRule(args *rpcapi.ShadowDeleteArgs, reply *rpcapi.Empty) error {
	if s.store == nil {
		return fmt.Errorf("store not available")
	}
	schemaID := s.resolveSchemaID(args.SchemaID)
	return s.store.RemoveShadowRule(schemaID, args.Code, args.Word)
}

// GetRules 获取指定编码的规则
func (s *ShadowService) GetRules(args *rpcapi.ShadowGetRulesArgs, reply *rpcapi.ShadowRulesReply) error {
	if s.store == nil {
		return fmt.Errorf("store not available")
	}
	schemaID := s.resolveSchemaID(args.SchemaID)
	rec, err := s.store.GetShadowRules(schemaID, args.Code)
	if err != nil {
		return err
	}

	for _, p := range rec.Pinned {
		reply.Pinned = append(reply.Pinned, rpcapi.PinnedEntry{Word: p.Word, Position: p.Position})
	}
	reply.Deleted = rec.Deleted

	return nil
}
```

---

## Task 5: 实现 System 服务

**Files:**
- Create: `wind_input/internal/rpc/system_service.go`

- [ ] **Step 1: 实现 System 服务**

```go
package rpc

import (
	"log/slog"

	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/pkg/rpcapi"
)

// SystemService 系统管理 RPC 服务
type SystemService struct {
	dm     *dict.DictManager
	server *Server
	logger *slog.Logger
}

// Ping 心跳检测
func (s *SystemService) Ping(args *rpcapi.Empty, reply *rpcapi.Empty) error {
	return nil
}

// GetStatus 获取系统状态
func (s *SystemService) GetStatus(args *rpcapi.Empty, reply *rpcapi.SystemStatusReply) error {
	reply.Running = true
	reply.StoreEnabled = s.dm.UseStore()

	stats := s.dm.GetStats()
	reply.UserWords = stats["user_words"]
	reply.TempWords = stats["temp_words"]
	reply.Phrases = stats["phrases"]
	reply.ShadowRules = stats["shadow_rules"]
	reply.SchemaID = s.dm.GetActiveSchemaID()

	s.server.mu.Lock()
	provider := s.server.statusProvider
	s.server.mu.Unlock()

	if provider != nil {
		reply.EngineType = provider.GetEngineType()
		reply.ChineseMode = provider.IsChineseMode()
	}

	return nil
}

// ReloadPhrases 重载短语
func (s *SystemService) ReloadPhrases(args *rpcapi.Empty, reply *rpcapi.Empty) error {
	s.logger.Info("RPC System.ReloadPhrases")
	return s.dm.ReloadPhrases()
}

// ReloadAll 重载所有
func (s *SystemService) ReloadAll(args *rpcapi.Empty, reply *rpcapi.Empty) error {
	s.logger.Info("RPC System.ReloadAll")
	if err := s.dm.ReloadPhrases(); err != nil {
		return err
	}
	return nil
}
```

---

## Task 6: 编译验证 + 接入 main.go

**Files:**
- Modify: `wind_input/cmd/service/main.go`

- [ ] **Step 1: 编译 RPC 包**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go build ./internal/rpc/ ./pkg/rpcapi/
```

- [ ] **Step 2: 在 main.go 中启动 RPC 服务**

在 `controlServer.StartAsync()` 之后添加：

```go
// Start RPC server (JSON-RPC over named pipe)
if dictManager.UseStore() {
    rpcServer := rpc.NewServer(logger, dictManager, dictManager.GetStore())
    defer rpcServer.Stop()
    rpcServer.StartAsync()
}
```

import 添加: `"github.com/huanfeng/wind_input/internal/rpc"`

- [ ] **Step 3: 全量编译**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go build ./...
```

---

## Task 7: 集成测试

**Files:**
- Create: `wind_input/internal/rpc/server_test.go`

- [ ] **Step 1: 编写集成测试**

```go
package rpc

import (
	"net"
	"net/rpc/jsonrpc"
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/store"
	"github.com/huanfeng/wind_input/pkg/rpcapi"
)

// startTestServer 创建测试用 RPC 服务（使用 net.Pipe 代替命名管道）
func startTestServer(t *testing.T) (*rpc.Server, net.Conn) {
	t.Helper()

	dir := t.TempDir()
	s, err := store.Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { s.Close() })

	dm := dict.NewDictManager(dir, dir, nil)
	dm.OpenStore(filepath.Join(dir, "test2.db"))  // 需要另一个 db
	dm.Initialize()

	// 注册 RPC 服务
	rpcServer := rpc.NewServer()
	rpcServer.RegisterName("Dict", &DictService{store: s, dm: dm, logger: slog.Default()})
	rpcServer.RegisterName("Shadow", &ShadowService{store: s, dm: dm, logger: slog.Default()})
	rpcServer.RegisterName("System", &SystemService{dm: dm, server: &Server{}, logger: slog.Default()})

	// 用 net.Pipe 模拟连接
	serverConn, clientConn := net.Pipe()
	go rpcServer.ServeCodec(jsonrpc.NewServerCodec(serverConn))

	return nil, clientConn
}

func TestDictAddAndSearch(t *testing.T) {
	_, clientConn := startTestServer(t)
	client := jsonrpc.NewClient(clientConn)
	defer client.Close()

	// 添加词条
	err := client.Call("Dict.Add", &rpcapi.DictAddArgs{
		Code: "ggtt", Text: "王国", Weight: 1200,
	}, &rpcapi.Empty{})
	if err != nil {
		t.Fatalf("Dict.Add: %v", err)
	}

	// 精确查询
	var reply rpcapi.DictSearchReply
	err = client.Call("Dict.SearchByCode", &rpcapi.DictSearchArgs{
		Prefix: "ggtt",
	}, &reply)
	if err != nil {
		t.Fatalf("Dict.SearchByCode: %v", err)
	}
	if reply.Total != 1 {
		t.Errorf("expected 1 word, got %d", reply.Total)
	}
	if reply.Words[0].Text != "王国" {
		t.Errorf("expected 王国, got %s", reply.Words[0].Text)
	}
}

func TestSystemPing(t *testing.T) {
	_, clientConn := startTestServer(t)
	client := jsonrpc.NewClient(clientConn)
	defer client.Close()

	err := client.Call("System.Ping", &rpcapi.Empty{}, &rpcapi.Empty{})
	if err != nil {
		t.Fatalf("System.Ping: %v", err)
	}
}

func TestShadowPinAndGet(t *testing.T) {
	_, clientConn := startTestServer(t)
	client := jsonrpc.NewClient(clientConn)
	defer client.Close()

	// Pin
	err := client.Call("Shadow.Pin", &rpcapi.ShadowPinArgs{
		Code: "gg", Word: "王", Position: 0,
	}, &rpcapi.Empty{})
	if err != nil {
		t.Fatalf("Shadow.Pin: %v", err)
	}

	// GetRules
	var reply rpcapi.ShadowRulesReply
	err = client.Call("Shadow.GetRules", &rpcapi.ShadowGetRulesArgs{
		Code: "gg",
	}, &reply)
	if err != nil {
		t.Fatalf("Shadow.GetRules: %v", err)
	}
	if len(reply.Pinned) != 1 || reply.Pinned[0].Word != "王" {
		t.Errorf("unexpected rules: %+v", reply)
	}
}
```

- [ ] **Step 2: 运行测试**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./internal/rpc/ -v -count=1
```

- [ ] **Step 3: 全量测试**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./... -count=1
```
