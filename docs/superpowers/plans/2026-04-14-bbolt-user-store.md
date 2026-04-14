# bbolt 用户数据统一存储层实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将用户词库（UserDict）、临时词库（TempDict）、Shadow 规则、词频数据从分散的明文文件迁移到统一的 bbolt 数据库，并实现词频与词库权重分离。

**Architecture:** 新建 `internal/store` 包封装 bbolt 操作和异步写入缓冲；新建 `internal/dict/store_layer.go` 和 `freq_scorer.go` 将 Store 适配为现有 DictLayer 接口和词频评分器；DictManager 保持原有对外接口不变，内部切换到 Store 后端。旧文件首次启动时自动迁移。

**Tech Stack:** `go.etcd.io/bbolt` (纯 Go B+ 树嵌入式数据库), JSON 序列化

---

## 文件结构

### 新建文件

| 文件 | 职责 |
|------|------|
| `internal/store/store.go` | Store 主结构：Open/Close、Bucket 导航、元信息读写 |
| `internal/store/store_test.go` | Store 基础测试 |
| `internal/store/write_buffer.go` | WriteAheadBuffer：异步批量写入引擎 |
| `internal/store/write_buffer_test.go` | 写入缓冲测试 |
| `internal/store/user_words.go` | 用户造词 CRUD（读写 `Schemas/{id}/UserWords` bucket） |
| `internal/store/user_words_test.go` | 用户造词测试 |
| `internal/store/temp_words.go` | 临时词库 CRUD + 淘汰 + 晋升 |
| `internal/store/temp_words_test.go` | 临时词库测试 |
| `internal/store/freq.go` | 词频记录读写 + boost 计算函数 |
| `internal/store/freq_test.go` | 词频测试 |
| `internal/store/shadow.go` | Shadow 规则读写 |
| `internal/store/shadow_test.go` | Shadow 测试 |
| `internal/store/migration.go` | 旧格式（TSV/YAML）→ bbolt 迁移 |
| `internal/store/migration_test.go` | 迁移测试 |
| `internal/dict/store_layer.go` | 基于 Store 的 DictLayer/MutableLayer 适配器 |
| `internal/dict/store_layer_test.go` | Store layer 适配器测试 |
| `internal/dict/freq_scorer.go` | FreqScorer：词频加成评分器接口和实现 |
| `internal/dict/freq_scorer_test.go` | 词频评分器测试 |

### 修改文件

| 文件 | 修改内容 |
|------|----------|
| `go.mod` / `go.sum` | 添加 `go.etcd.io/bbolt` 依赖 |
| `internal/dict/composite.go` | 在排序前调用 FreqScorer 修改候选词权重 |
| `internal/dict/layer.go` | 新增 FreqScorer 接口定义 |
| `internal/dict/manager.go` | 新增 `OpenStore()` / `CloseStore()`；`SwitchSchemaFull` 切换为从 Store 加载 |
| `internal/schema/learning.go` | LearningStrategy 调用 Store 的词频记录接口 |

---

## Task 1: 添加 bbolt 依赖

**Files:**
- Modify: `wind_input/go.mod`

- [ ] **Step 1: 添加 bbolt 依赖**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go get go.etcd.io/bbolt@latest
```

- [ ] **Step 2: 验证依赖已添加**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && grep bbolt go.mod
```

Expected: `go.etcd.io/bbolt v1.x.x`

---

## Task 2: 实现 Store 核心（Open/Close/Bucket 导航）

**Files:**
- Create: `wind_input/internal/store/store.go`
- Create: `wind_input/internal/store/store_test.go`

- [ ] **Step 1: 编写 Store 测试**

```go
// store_test.go
package store

import (
	"os"
	"path/filepath"
	"testing"
)

func TestOpenClose(t *testing.T) {
	dir := t.TempDir()
	dbPath := filepath.Join(dir, "test.db")

	s, err := Open(dbPath)
	if err != nil {
		t.Fatalf("Open failed: %v", err)
	}

	// 验证文件已创建
	if _, err := os.Stat(dbPath); err != nil {
		t.Fatalf("db file not created: %v", err)
	}

	// 验证 Meta bucket 已初始化
	ver, err := s.GetMeta("version")
	if err != nil {
		t.Fatalf("GetMeta failed: %v", err)
	}
	if ver != "1" {
		t.Errorf("expected version=1, got %q", ver)
	}

	// 验证 device_id 已生成
	devID, err := s.GetMeta("device_id")
	if err != nil {
		t.Fatalf("GetMeta device_id failed: %v", err)
	}
	if devID == "" {
		t.Error("device_id should not be empty")
	}

	if err := s.Close(); err != nil {
		t.Fatalf("Close failed: %v", err)
	}

	// 重新打开应保留数据
	s2, err := Open(dbPath)
	if err != nil {
		t.Fatalf("Reopen failed: %v", err)
	}
	defer s2.Close()

	devID2, _ := s2.GetMeta("device_id")
	if devID2 != devID {
		t.Errorf("device_id changed after reopen: %q vs %q", devID, devID2)
	}
}
```

- [ ] **Step 2: 运行测试确认失败**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./internal/store/ -run TestOpenClose -v
```

Expected: FAIL — `store` package 不存在

- [ ] **Step 3: 实现 Store**

```go
// store.go
package store

import (
	"fmt"

	"github.com/google/uuid"
	bolt "go.etcd.io/bbolt"
)

// Bucket 名称常量
var (
	bucketMeta    = []byte("Meta")
	bucketSchemas = []byte("Schemas")
)

// Store 用户数据统一存储
// 基于 bbolt 单库分桶架构，管理用户词库、临时词库、词频和 Shadow 规则
type Store struct {
	db   *bolt.DB
	path string
}

// Open 打开或创建数据库
func Open(path string) (*Store, error) {
	db, err := bolt.Open(path, 0600, &bolt.Options{
		NoFreelistSync: true, // 提升写入性能
	})
	if err != nil {
		return nil, fmt.Errorf("open bbolt db: %w", err)
	}

	s := &Store{db: db, path: path}
	if err := s.init(); err != nil {
		db.Close()
		return nil, err
	}
	return s, nil
}

// init 初始化顶层 Bucket 和元数据
func (s *Store) init() error {
	return s.db.Update(func(tx *bolt.Tx) error {
		meta, err := tx.CreateBucketIfNotExists(bucketMeta)
		if err != nil {
			return fmt.Errorf("create Meta bucket: %w", err)
		}

		// 初始化版本号
		if meta.Get([]byte("version")) == nil {
			if err := meta.Put([]byte("version"), []byte("1")); err != nil {
				return err
			}
		}

		// 初始化设备 ID
		if meta.Get([]byte("device_id")) == nil {
			if err := meta.Put([]byte("device_id"), []byte(uuid.New().String())); err != nil {
				return err
			}
		}

		// 创建 Schemas 顶层桶
		if _, err := tx.CreateBucketIfNotExists(bucketSchemas); err != nil {
			return fmt.Errorf("create Schemas bucket: %w", err)
		}

		return nil
	})
}

// Close 关闭数据库
func (s *Store) Close() error {
	if s.db != nil {
		return s.db.Close()
	}
	return nil
}

// DB 返回底层 bbolt.DB（供高级用途）
func (s *Store) DB() *bolt.DB {
	return s.db
}

// Path 返回数据库文件路径
func (s *Store) Path() string {
	return s.path
}

// GetMeta 读取元信息
func (s *Store) GetMeta(key string) (string, error) {
	var val string
	err := s.db.View(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketMeta)
		if b == nil {
			return nil
		}
		v := b.Get([]byte(key))
		if v != nil {
			val = string(v)
		}
		return nil
	})
	return val, err
}

// SetMeta 写入元信息
func (s *Store) SetMeta(key, value string) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketMeta)
		if b == nil {
			return fmt.Errorf("Meta bucket not found")
		}
		return b.Put([]byte(key), []byte(value))
	})
}

// schemaBucket 获取或创建指定方案的 Bucket
// 路径: Schemas -> {schemaID}
func schemaBucket(tx *bolt.Tx, schemaID string, create bool) (*bolt.Bucket, error) {
	schemas := tx.Bucket(bucketSchemas)
	if schemas == nil {
		if !create {
			return nil, nil
		}
		return nil, fmt.Errorf("Schemas bucket not found")
	}

	if create {
		return schemas.CreateBucketIfNotExists([]byte(schemaID))
	}
	return schemas.Bucket([]byte(schemaID)), nil
}

// schemaSubBucket 获取或创建方案子 Bucket
// 路径: Schemas -> {schemaID} -> {sub}
func schemaSubBucket(tx *bolt.Tx, schemaID string, sub []byte, create bool) (*bolt.Bucket, error) {
	schema, err := schemaBucket(tx, schemaID, create)
	if err != nil {
		return nil, err
	}
	if schema == nil {
		return nil, nil
	}

	if create {
		return schema.CreateBucketIfNotExists(sub)
	}
	return schema.Bucket(sub), nil
}
```

- [ ] **Step 4: 运行测试确认通过**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./internal/store/ -run TestOpenClose -v
```

Expected: PASS

---

## Task 3: 实现 WriteAheadBuffer 异步写入引擎

**Files:**
- Create: `wind_input/internal/store/write_buffer.go`
- Create: `wind_input/internal/store/write_buffer_test.go`

- [ ] **Step 1: 编写 WriteAheadBuffer 测试**

```go
// write_buffer_test.go
package store

import (
	"path/filepath"
	"testing"
	"time"
)

func TestWriteBuffer_BasicFlush(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	wb := NewWriteBuffer(s.db, WriteBufferConfig{
		FlushSize:     3,
		FlushInterval: 10 * time.Second, // 不依赖定时器
	})
	defer wb.Close()

	// 写入 3 条达到阈值，触发自动 flush
	wb.Enqueue(WriteOp{Bucket: [][]byte{bucketMeta}, Key: "k1", Value: []byte("v1")})
	wb.Enqueue(WriteOp{Bucket: [][]byte{bucketMeta}, Key: "k2", Value: []byte("v2")})
	wb.Enqueue(WriteOp{Bucket: [][]byte{bucketMeta}, Key: "k3", Value: []byte("v3")})

	// 等待 flush 完成
	time.Sleep(100 * time.Millisecond)

	// 验证数据已写入
	v1, _ := s.GetMeta("k1")
	v2, _ := s.GetMeta("k2")
	v3, _ := s.GetMeta("k3")
	if v1 != "v1" || v2 != "v2" || v3 != "v3" {
		t.Errorf("expected v1/v2/v3, got %q/%q/%q", v1, v2, v3)
	}
}

func TestWriteBuffer_TimerFlush(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	wb := NewWriteBuffer(s.db, WriteBufferConfig{
		FlushSize:     100,              // 大阈值，不会触发
		FlushInterval: 50 * time.Millisecond, // 快速定时器
	})
	defer wb.Close()

	wb.Enqueue(WriteOp{Bucket: [][]byte{bucketMeta}, Key: "timer_key", Value: []byte("timer_val")})

	// 等待定时器触发
	time.Sleep(200 * time.Millisecond)

	v, _ := s.GetMeta("timer_key")
	if v != "timer_val" {
		t.Errorf("expected timer_val, got %q", v)
	}
}

func TestWriteBuffer_CloseFlushesRemaining(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	wb := NewWriteBuffer(s.db, WriteBufferConfig{
		FlushSize:     100,
		FlushInterval: 10 * time.Second,
	})

	wb.Enqueue(WriteOp{Bucket: [][]byte{bucketMeta}, Key: "close_key", Value: []byte("close_val")})
	wb.Close() // Close 应触发最终 flush

	v, _ := s.GetMeta("close_key")
	if v != "close_val" {
		t.Errorf("expected close_val, got %q", v)
	}
}

func TestWriteBuffer_Delete(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	// 先直接写入一个值
	s.SetMeta("del_key", "to_delete")

	wb := NewWriteBuffer(s.db, WriteBufferConfig{
		FlushSize:     1,
		FlushInterval: 10 * time.Second,
	})
	defer wb.Close()

	// Value=nil 表示删除
	wb.Enqueue(WriteOp{Bucket: [][]byte{bucketMeta}, Key: "del_key", Value: nil})
	time.Sleep(100 * time.Millisecond)

	v, _ := s.GetMeta("del_key")
	if v != "" {
		t.Errorf("expected empty after delete, got %q", v)
	}
}
```

- [ ] **Step 2: 运行测试确认失败**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./internal/store/ -run TestWriteBuffer -v
```

Expected: FAIL — `NewWriteBuffer` 未定义

- [ ] **Step 3: 实现 WriteAheadBuffer**

```go
// write_buffer.go
package store

import (
	"log/slog"
	"sync"
	"time"

	bolt "go.etcd.io/bbolt"
)

// WriteOp 单条写入操作
type WriteOp struct {
	Bucket [][]byte // Bucket 路径，如 [Meta] 或 [Schemas, wubi86, Freq]
	Key    string
	Value  []byte // nil 表示删除
}

// WriteBufferConfig 写入缓冲配置
type WriteBufferConfig struct {
	FlushSize     int           // 批次大小阈值（达到后立即 flush）
	FlushInterval time.Duration // 定时 flush 间隔
}

// DefaultWriteBufferConfig 默认配置
func DefaultWriteBufferConfig() WriteBufferConfig {
	return WriteBufferConfig{
		FlushSize:     50,
		FlushInterval: 30 * time.Second,
	}
}

// WriteBuffer 异步批量写入缓冲
// 累积写操作，达到阈值或定时触发时批量写入 bbolt
type WriteBuffer struct {
	db      *bolt.DB
	config  WriteBufferConfig
	mu      sync.Mutex
	pending []WriteOp
	writeCh chan struct{}
	done    chan struct{}
	wg      sync.WaitGroup
}

// NewWriteBuffer 创建写入缓冲并启动后台 flush 协程
func NewWriteBuffer(db *bolt.DB, config WriteBufferConfig) *WriteBuffer {
	if config.FlushSize <= 0 {
		config.FlushSize = 50
	}
	if config.FlushInterval <= 0 {
		config.FlushInterval = 30 * time.Second
	}

	wb := &WriteBuffer{
		db:      db,
		config:  config,
		writeCh: make(chan struct{}, 1),
		done:    make(chan struct{}),
	}

	wb.wg.Add(1)
	go wb.flushLoop()
	return wb
}

// Enqueue 入队一条写操作
func (wb *WriteBuffer) Enqueue(op WriteOp) {
	wb.mu.Lock()
	wb.pending = append(wb.pending, op)
	shouldFlush := len(wb.pending) >= wb.config.FlushSize
	wb.mu.Unlock()

	if shouldFlush {
		select {
		case wb.writeCh <- struct{}{}:
		default:
		}
	}
}

// Pending 返回当前待处理操作数
func (wb *WriteBuffer) Pending() int {
	wb.mu.Lock()
	defer wb.mu.Unlock()
	return len(wb.pending)
}

// Close 关闭缓冲（会 flush 剩余数据）
func (wb *WriteBuffer) Close() {
	close(wb.done)
	wb.wg.Wait()
}

func (wb *WriteBuffer) flushLoop() {
	defer wb.wg.Done()

	ticker := time.NewTicker(wb.config.FlushInterval)
	defer ticker.Stop()

	for {
		select {
		case <-wb.writeCh:
			wb.flush()
		case <-ticker.C:
			wb.flush()
		case <-wb.done:
			wb.flush() // 退出前最后刷一次
			return
		}
	}
}

func (wb *WriteBuffer) flush() {
	wb.mu.Lock()
	if len(wb.pending) == 0 {
		wb.mu.Unlock()
		return
	}
	ops := wb.pending
	wb.pending = make([]WriteOp, 0, cap(ops))
	wb.mu.Unlock()

	err := wb.db.Update(func(tx *bolt.Tx) error {
		for _, op := range ops {
			bucket, err := navigateBuckets(tx, op.Bucket, true)
			if err != nil {
				return err
			}
			if op.Value == nil {
				if err := bucket.Delete([]byte(op.Key)); err != nil {
					return err
				}
			} else {
				if err := bucket.Put([]byte(op.Key), op.Value); err != nil {
					return err
				}
			}
		}
		return nil
	})

	if err != nil {
		slog.Error("WriteBuffer flush failed", "ops", len(ops), "error", err)
		// 失败的操作重新入队
		wb.mu.Lock()
		wb.pending = append(ops, wb.pending...)
		wb.mu.Unlock()
	}
}

// navigateBuckets 沿路径导航到目标 Bucket
func navigateBuckets(tx *bolt.Tx, path [][]byte, create bool) (*bolt.Bucket, error) {
	if len(path) == 0 {
		return nil, fmt.Errorf("empty bucket path")
	}

	var bucket *bolt.Bucket
	var err error

	if create {
		bucket, err = tx.CreateBucketIfNotExists(path[0])
	} else {
		bucket = tx.Bucket(path[0])
	}
	if err != nil {
		return nil, err
	}
	if bucket == nil {
		return nil, nil
	}

	for _, name := range path[1:] {
		if create {
			bucket, err = bucket.CreateBucketIfNotExists(name)
			if err != nil {
				return nil, err
			}
		} else {
			bucket = bucket.Bucket(name)
			if bucket == nil {
				return nil, nil
			}
		}
	}

	return bucket, nil
}
```

注意：`write_buffer.go` 需要在文件顶部添加 `"fmt"` import（`navigateBuckets` 中使用了 `fmt.Errorf`）。

- [ ] **Step 4: 运行测试确认通过**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./internal/store/ -run TestWriteBuffer -v -count=1
```

Expected: PASS（4 个测试全部通过）

---

## Task 4: 实现词频记录（Freq）

**Files:**
- Create: `wind_input/internal/store/freq.go`
- Create: `wind_input/internal/store/freq_test.go`

- [ ] **Step 1: 编写词频测试**

```go
// freq_test.go
package store

import (
	"path/filepath"
	"testing"
	"time"
)

func TestFreq_IncrementAndGet(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	// 初始状态：不存在
	rec, err := s.GetFreq("wubi86", "ggtt", "王")
	if err != nil {
		t.Fatal(err)
	}
	if rec.Count != 0 {
		t.Errorf("expected count=0 for new key, got %d", rec.Count)
	}

	// 增加词频
	if err := s.IncrementFreq("wubi86", "ggtt", "王"); err != nil {
		t.Fatal(err)
	}
	if err := s.IncrementFreq("wubi86", "ggtt", "王"); err != nil {
		t.Fatal(err)
	}

	rec, err = s.GetFreq("wubi86", "ggtt", "王")
	if err != nil {
		t.Fatal(err)
	}
	if rec.Count != 2 {
		t.Errorf("expected count=2, got %d", rec.Count)
	}
	if rec.LastUsed == 0 {
		t.Error("LastUsed should be set")
	}
}

func TestFreq_CalcBoost(t *testing.T) {
	now := time.Now().Unix()

	tests := []struct {
		name     string
		rec      FreqRecord
		wantMin  int
		wantMax  int
	}{
		{"zero", FreqRecord{}, 0, 0},
		{"count_10_recent", FreqRecord{Count: 10, LastUsed: now - 60, Streak: 0}, 200, 600},
		{"count_50_old", FreqRecord{Count: 50, LastUsed: now - 700000, Streak: 0}, 100, 700},
		{"count_5_streak3", FreqRecord{Count: 5, LastUsed: now - 30, Streak: 3}, 400, 800},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			boost := CalcFreqBoost(tt.rec, now)
			if boost < tt.wantMin || boost > tt.wantMax {
				t.Errorf("CalcFreqBoost(%+v) = %d, want [%d, %d]", tt.rec, boost, tt.wantMin, tt.wantMax)
			}
		})
	}
}

func TestFreq_BoostMax(t *testing.T) {
	now := time.Now().Unix()
	rec := FreqRecord{Count: 100000, LastUsed: now, Streak: 10}
	boost := CalcFreqBoost(rec, now)
	if boost > FreqBoostMax {
		t.Errorf("boost %d exceeds max %d", boost, FreqBoostMax)
	}
}

func TestFreq_ResetStreak(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	// 连续选择同一个词
	s.IncrementFreq("wubi86", "ggtt", "王")
	s.IncrementFreq("wubi86", "ggtt", "王")

	rec, _ := s.GetFreq("wubi86", "ggtt", "王")
	if rec.Streak != 2 {
		t.Errorf("expected streak=2, got %d", rec.Streak)
	}

	// 选择另一个词后，重置 streak
	s.ResetStreak("wubi86", "ggtt", "王")
	rec, _ = s.GetFreq("wubi86", "ggtt", "王")
	if rec.Streak != 0 {
		t.Errorf("expected streak=0 after reset, got %d", rec.Streak)
	}
}
```

- [ ] **Step 2: 运行测试确认失败**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./internal/store/ -run TestFreq -v
```

Expected: FAIL

- [ ] **Step 3: 实现词频模块**

```go
// freq.go
package store

import (
	"encoding/json"
	"fmt"
	"math"
	"time"

	bolt "go.etcd.io/bbolt"
)

// FreqBoostMax 词频加成硬上限
const FreqBoostMax = 2000

var bucketFreq = []byte("Freq")

// FreqRecord 词频记录
type FreqRecord struct {
	Count    uint32 `json:"c"`           // 累计选中次数
	LastUsed int64  `json:"t"`           // 最近使用时间戳（秒）
	Streak   uint8  `json:"s,omitempty"` // 连续选择次数
}

// freqKey 生成词频的 key: "code:text"
func freqKey(code, text string) string {
	return code + ":" + text
}

// GetFreq 读取词频记录
func (s *Store) GetFreq(schemaID, code, text string) (FreqRecord, error) {
	var rec FreqRecord
	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, bucketFreq, false)
		if err != nil {
			return err
		}
		if b == nil {
			return nil
		}
		v := b.Get([]byte(freqKey(code, text)))
		if v == nil {
			return nil
		}
		return json.Unmarshal(v, &rec)
	})
	return rec, err
}

// IncrementFreq 增加词频计数（同步写入，适合低频调用；高频场景应使用 WriteBuffer）
func (s *Store) IncrementFreq(schemaID, code, text string) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, bucketFreq, true)
		if err != nil {
			return err
		}

		key := []byte(freqKey(code, text))
		var rec FreqRecord
		if v := b.Get(key); v != nil {
			json.Unmarshal(v, &rec)
		}

		rec.Count++
		rec.LastUsed = time.Now().Unix()
		if rec.Streak < 255 {
			rec.Streak++
		}

		data, err := json.Marshal(&rec)
		if err != nil {
			return err
		}
		return b.Put(key, data)
	})
}

// ResetStreak 重置连续选择计数
func (s *Store) ResetStreak(schemaID, code, text string) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, bucketFreq, false)
		if err != nil {
			return err
		}
		if b == nil {
			return nil
		}

		key := []byte(freqKey(code, text))
		var rec FreqRecord
		v := b.Get(key)
		if v == nil {
			return nil
		}
		if err := json.Unmarshal(v, &rec); err != nil {
			return err
		}

		rec.Streak = 0
		data, err := json.Marshal(&rec)
		if err != nil {
			return err
		}
		return b.Put(key, data)
	})
}

// BatchIncrementFreq 通过 WriteBuffer 异步增加词频（高频场景）
func BatchIncrementFreq(wb *WriteBuffer, schemaID, code, text string, rec FreqRecord) {
	rec.Count++
	rec.LastUsed = time.Now().Unix()
	if rec.Streak < 255 {
		rec.Streak++
	}

	data, _ := json.Marshal(&rec)
	wb.Enqueue(WriteOp{
		Bucket: [][]byte{bucketSchemas, []byte(schemaID), bucketFreq},
		Key:    freqKey(code, text),
		Value:  data,
	})
}

// CalcFreqBoost 计算词频加成分数
func CalcFreqBoost(rec FreqRecord, now int64) int {
	if rec.Count == 0 {
		return 0
	}

	// 基础频次加成：对数增长
	base := int(math.Log2(float64(rec.Count+1)) * 100)

	// 时间衰减
	elapsed := now - rec.LastUsed
	recency := 0
	if elapsed < 3600 { // 1 小时内
		recency = 200
	} else if elapsed < 86400 { // 1 天内
		recency = 100
	} else if elapsed < 604800 { // 1 周内
		recency = 50
	}

	// 连击加成
	streak := int(rec.Streak) * 50
	if streak > 250 {
		streak = 250
	}

	total := base + recency + streak
	if total > FreqBoostMax {
		return FreqBoostMax
	}
	return total
}

// GetAllFreq 获取指定方案的所有词频记录（用于调试和导出）
func (s *Store) GetAllFreq(schemaID string) (map[string]FreqRecord, error) {
	result := make(map[string]FreqRecord)
	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, bucketFreq, false)
		if err != nil {
			return err
		}
		if b == nil {
			return nil
		}
		return b.ForEach(func(k, v []byte) error {
			var rec FreqRecord
			if err := json.Unmarshal(v, &rec); err != nil {
				return fmt.Errorf("unmarshal freq %s: %w", k, err)
			}
			result[string(k)] = rec
			return nil
		})
	})
	return result, err
}
```

- [ ] **Step 4: 运行测试确认通过**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./internal/store/ -run TestFreq -v -count=1
```

Expected: PASS

---

## Task 5: 实现用户造词 CRUD

**Files:**
- Create: `wind_input/internal/store/user_words.go`
- Create: `wind_input/internal/store/user_words_test.go`

- [ ] **Step 1: 编写用户造词测试**

```go
// user_words_test.go
package store

import (
	"path/filepath"
	"testing"
)

func TestUserWords_AddAndGet(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	// 添加词条
	err = s.AddUserWord("wubi86", "ggtt", "王国", 1200)
	if err != nil {
		t.Fatal(err)
	}

	// 精确查询
	words, err := s.GetUserWords("wubi86", "ggtt")
	if err != nil {
		t.Fatal(err)
	}
	if len(words) != 1 {
		t.Fatalf("expected 1 word, got %d", len(words))
	}
	if words[0].Text != "王国" || words[0].Weight != 1200 {
		t.Errorf("unexpected word: %+v", words[0])
	}
}

func TestUserWords_AddDuplicate(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	s.AddUserWord("wubi86", "ggtt", "王国", 100)
	s.AddUserWord("wubi86", "ggtt", "王国", 200) // 更新权重

	words, _ := s.GetUserWords("wubi86", "ggtt")
	if len(words) != 1 {
		t.Fatalf("expected 1 word after dedup, got %d", len(words))
	}
	if words[0].Weight != 200 {
		t.Errorf("expected weight=200 after update, got %d", words[0].Weight)
	}
}

func TestUserWords_Remove(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	s.AddUserWord("wubi86", "ggtt", "王国", 100)
	s.AddUserWord("wubi86", "ggtt", "国王", 200)

	err = s.RemoveUserWord("wubi86", "ggtt", "王国")
	if err != nil {
		t.Fatal(err)
	}

	words, _ := s.GetUserWords("wubi86", "ggtt")
	if len(words) != 1 || words[0].Text != "国王" {
		t.Errorf("unexpected words after remove: %+v", words)
	}
}

func TestUserWords_PrefixSearch(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	s.AddUserWord("wubi86", "gg", "王", 100)
	s.AddUserWord("wubi86", "ggtt", "王国", 200)
	s.AddUserWord("wubi86", "ggxy", "王者", 300)
	s.AddUserWord("wubi86", "ab", "其他", 100)

	words, err := s.SearchUserWordsPrefix("wubi86", "gg", 10)
	if err != nil {
		t.Fatal(err)
	}
	if len(words) != 3 {
		t.Errorf("expected 3 words with prefix 'gg', got %d", len(words))
	}
}

func TestUserWords_MultipleSchemas(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	s.AddUserWord("wubi86", "gg", "王", 100)
	s.AddUserWord("pinyin", "wang", "王", 200)

	w1, _ := s.GetUserWords("wubi86", "gg")
	w2, _ := s.GetUserWords("pinyin", "wang")

	if len(w1) != 1 || w1[0].Weight != 100 {
		t.Errorf("wubi86 unexpected: %+v", w1)
	}
	if len(w2) != 1 || w2[0].Weight != 200 {
		t.Errorf("pinyin unexpected: %+v", w2)
	}
}

func TestUserWords_EntryCount(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	s.AddUserWord("wubi86", "gg", "王", 100)
	s.AddUserWord("wubi86", "ggtt", "王国", 200)

	count, err := s.UserWordCount("wubi86")
	if err != nil {
		t.Fatal(err)
	}
	if count != 2 {
		t.Errorf("expected count=2, got %d", count)
	}
}
```

- [ ] **Step 2: 运行测试确认失败**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./internal/store/ -run TestUserWords -v
```

Expected: FAIL

- [ ] **Step 3: 实现用户造词模块**

```go
// user_words.go
package store

import (
	"encoding/json"
	"fmt"
	"strings"
	"time"

	bolt "go.etcd.io/bbolt"
)

var bucketUserWords = []byte("UserWords")

// UserWordRecord 用户造词记录（存储在 bbolt 中）
type UserWordRecord struct {
	Text      string `json:"t"`
	Weight    int    `json:"w"`
	Count     int    `json:"c,omitempty"` // 选中次数（误选保护）
	CreatedAt int64  `json:"ts"`          // Unix 时间戳
}

// userWordsKey 用户词的 key 格式: "code\x00text"
// 使用 \x00 分隔可以利用 bbolt 的有序遍历按 code 前缀查询
func userWordsKey(code, text string) []byte {
	return []byte(code + "\x00" + text)
}

// parseUserWordsKey 解析 key
func parseUserWordsKey(key []byte) (code, text string) {
	parts := strings.SplitN(string(key), "\x00", 2)
	if len(parts) == 2 {
		return parts[0], parts[1]
	}
	return string(key), ""
}

// AddUserWord 添加或更新用户词条
func (s *Store) AddUserWord(schemaID, code, text string, weight int) error {
	code = strings.ToLower(code)
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, bucketUserWords, true)
		if err != nil {
			return err
		}

		key := userWordsKey(code, text)

		// 检查是否已存在
		var rec UserWordRecord
		if v := b.Get(key); v != nil {
			json.Unmarshal(v, &rec)
			rec.Weight = weight // 更新权重
		} else {
			rec = UserWordRecord{
				Text:      text,
				Weight:    weight,
				CreatedAt: time.Now().Unix(),
			}
		}

		data, err := json.Marshal(&rec)
		if err != nil {
			return err
		}
		return b.Put(key, data)
	})
}

// RemoveUserWord 删除用户词条
func (s *Store) RemoveUserWord(schemaID, code, text string) error {
	code = strings.ToLower(code)
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, bucketUserWords, false)
		if err != nil {
			return err
		}
		if b == nil {
			return nil
		}
		return b.Delete(userWordsKey(code, text))
	})
}

// UpdateUserWordWeight 更新用户词条权重
func (s *Store) UpdateUserWordWeight(schemaID, code, text string, newWeight int) error {
	code = strings.ToLower(code)
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, bucketUserWords, false)
		if err != nil {
			return err
		}
		if b == nil {
			return fmt.Errorf("schema %s has no user words", schemaID)
		}

		key := userWordsKey(code, text)
		v := b.Get(key)
		if v == nil {
			return fmt.Errorf("word not found: %s/%s", code, text)
		}

		var rec UserWordRecord
		if err := json.Unmarshal(v, &rec); err != nil {
			return err
		}
		rec.Weight = newWeight

		data, err := json.Marshal(&rec)
		if err != nil {
			return err
		}
		return b.Put(key, data)
	})
}

// GetUserWords 精确查询指定编码下的所有词条
func (s *Store) GetUserWords(schemaID, code string) ([]UserWordRecord, error) {
	code = strings.ToLower(code)
	var results []UserWordRecord

	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, bucketUserWords, false)
		if err != nil {
			return err
		}
		if b == nil {
			return nil
		}

		prefix := []byte(code + "\x00")
		c := b.Cursor()
		for k, v := c.Seek(prefix); k != nil && hasPrefix(k, prefix); k, v = c.Next() {
			var rec UserWordRecord
			if err := json.Unmarshal(v, &rec); err != nil {
				continue
			}
			results = append(results, rec)
		}
		return nil
	})
	return results, err
}

// SearchUserWordsPrefix 前缀搜索用户词条
func (s *Store) SearchUserWordsPrefix(schemaID, prefix string, limit int) ([]UserWordRecord, error) {
	prefix = strings.ToLower(prefix)
	var results []UserWordRecord

	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, bucketUserWords, false)
		if err != nil {
			return err
		}
		if b == nil {
			return nil
		}

		prefixBytes := []byte(prefix)
		c := b.Cursor()
		for k, v := c.Seek(prefixBytes); k != nil && hasPrefix(k, prefixBytes); k, v = c.Next() {
			var rec UserWordRecord
			if err := json.Unmarshal(v, &rec); err != nil {
				continue
			}
			// 从 key 中提取 code（key 格式: code\x00text）
			kCode, _ := parseUserWordsKey(k)
			rec.Text = rec.Text // 已在 JSON 中
			_ = kCode
			results = append(results, rec)
			if limit > 0 && len(results) >= limit {
				break
			}
		}
		return nil
	})
	return results, err
}

// UserWordCount 返回指定方案的用户词条总数
func (s *Store) UserWordCount(schemaID string) (int, error) {
	var count int
	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, bucketUserWords, false)
		if err != nil {
			return err
		}
		if b == nil {
			return nil
		}
		count = b.Stats().KeyN
		return nil
	})
	return count, err
}

// hasPrefix 检查 key 是否以 prefix 开头
func hasPrefix(key, prefix []byte) bool {
	if len(key) < len(prefix) {
		return false
	}
	for i, b := range prefix {
		if key[i] != b {
			return false
		}
	}
	return true
}
```

- [ ] **Step 4: 运行测试确认通过**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./internal/store/ -run TestUserWords -v -count=1
```

Expected: PASS

---

## Task 6: 实现临时词库 CRUD

**Files:**
- Create: `wind_input/internal/store/temp_words.go`
- Create: `wind_input/internal/store/temp_words_test.go`

- [ ] **Step 1: 编写临时词库测试**

```go
// temp_words_test.go
package store

import (
	"path/filepath"
	"testing"
)

func TestTempWords_LearnAndPromote(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	// 学习新词
	err = s.LearnTempWord("wubi86", "ggtt", "王国", 10)
	if err != nil {
		t.Fatal(err)
	}

	words, _ := s.GetTempWords("wubi86", "ggtt")
	if len(words) != 1 || words[0].Weight != 10 {
		t.Errorf("unexpected after learn: %+v", words)
	}

	// 再次学习，增加权重
	s.LearnTempWord("wubi86", "ggtt", "王国", 10)
	words, _ = s.GetTempWords("wubi86", "ggtt")
	if words[0].Weight != 20 || words[0].Count != 2 {
		t.Errorf("expected weight=20 count=2, got %+v", words[0])
	}

	// 晋升到用户词库
	err = s.PromoteTempWord("wubi86", "ggtt", "王国")
	if err != nil {
		t.Fatal(err)
	}

	// 临时词库应为空
	words, _ = s.GetTempWords("wubi86", "ggtt")
	if len(words) != 0 {
		t.Errorf("expected 0 temp words after promote, got %d", len(words))
	}

	// 用户词库应有此词
	uwords, _ := s.GetUserWords("wubi86", "ggtt")
	if len(uwords) != 1 || uwords[0].Text != "王国" {
		t.Errorf("expected promoted word in user dict, got %+v", uwords)
	}
}

func TestTempWords_Evict(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	// 添加 5 个词条
	s.LearnTempWord("wubi86", "a", "词1", 10)
	s.LearnTempWord("wubi86", "b", "词2", 50)
	s.LearnTempWord("wubi86", "c", "词3", 30)
	s.LearnTempWord("wubi86", "d", "词4", 20)
	s.LearnTempWord("wubi86", "e", "词5", 40)

	// 淘汰到只剩 3 个（保留权重最高的）
	evicted, err := s.EvictTempWords("wubi86", 3)
	if err != nil {
		t.Fatal(err)
	}
	if evicted != 2 {
		t.Errorf("expected 2 evicted, got %d", evicted)
	}

	count, _ := s.TempWordCount("wubi86")
	if count != 3 {
		t.Errorf("expected 3 remaining, got %d", count)
	}
}

func TestTempWords_ClearAll(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	s.LearnTempWord("wubi86", "a", "词1", 10)
	s.LearnTempWord("wubi86", "b", "词2", 20)

	cleared, err := s.ClearTempWords("wubi86")
	if err != nil {
		t.Fatal(err)
	}
	if cleared != 2 {
		t.Errorf("expected 2 cleared, got %d", cleared)
	}

	count, _ := s.TempWordCount("wubi86")
	if count != 0 {
		t.Errorf("expected 0 after clear, got %d", count)
	}
}
```

- [ ] **Step 2: 运行测试确认失败**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./internal/store/ -run TestTempWords -v
```

Expected: FAIL

- [ ] **Step 3: 实现临时词库模块**

```go
// temp_words.go
package store

import (
	"encoding/json"
	"sort"
	"strings"
	"time"

	bolt "go.etcd.io/bbolt"
)

var bucketTempWords = []byte("TempWords")

// LearnTempWord 学习临时词条（已存在则增加权重和计数）
func (s *Store) LearnTempWord(schemaID, code, text string, weightDelta int) error {
	code = strings.ToLower(code)
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, bucketTempWords, true)
		if err != nil {
			return err
		}

		key := userWordsKey(code, text) // 复用 key 格式

		var rec UserWordRecord
		if v := b.Get(key); v != nil {
			json.Unmarshal(v, &rec)
			rec.Weight += weightDelta
			if rec.Weight > 10000 {
				rec.Weight = 10000
			}
			rec.Count++
		} else {
			rec = UserWordRecord{
				Text:      text,
				Weight:    weightDelta,
				Count:     1,
				CreatedAt: time.Now().Unix(),
			}
		}

		data, err := json.Marshal(&rec)
		if err != nil {
			return err
		}
		return b.Put(key, data)
	})
}

// GetTempWords 查询指定编码下的临时词条
func (s *Store) GetTempWords(schemaID, code string) ([]UserWordRecord, error) {
	code = strings.ToLower(code)
	var results []UserWordRecord

	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, bucketTempWords, false)
		if err != nil {
			return err
		}
		if b == nil {
			return nil
		}

		prefix := []byte(code + "\x00")
		c := b.Cursor()
		for k, v := c.Seek(prefix); k != nil && hasPrefix(k, prefix); k, v = c.Next() {
			var rec UserWordRecord
			if err := json.Unmarshal(v, &rec); err != nil {
				continue
			}
			results = append(results, rec)
		}
		return nil
	})
	return results, err
}

// SearchTempWordsPrefix 前缀搜索临时词条
func (s *Store) SearchTempWordsPrefix(schemaID, prefix string, limit int) ([]UserWordRecord, error) {
	prefix = strings.ToLower(prefix)
	var results []UserWordRecord

	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, bucketTempWords, false)
		if err != nil {
			return err
		}
		if b == nil {
			return nil
		}

		prefixBytes := []byte(prefix)
		c := b.Cursor()
		for k, v := c.Seek(prefixBytes); k != nil && hasPrefix(k, prefixBytes); k, v = c.Next() {
			var rec UserWordRecord
			if err := json.Unmarshal(v, &rec); err != nil {
				continue
			}
			results = append(results, rec)
			if limit > 0 && len(results) >= limit {
				break
			}
		}
		return nil
	})
	return results, err
}

// PromoteTempWord 将临时词条晋升到用户词库
func (s *Store) PromoteTempWord(schemaID, code, text string) error {
	code = strings.ToLower(code)
	return s.db.Update(func(tx *bolt.Tx) error {
		tempB, err := schemaSubBucket(tx, schemaID, bucketTempWords, false)
		if err != nil {
			return err
		}
		if tempB == nil {
			return nil
		}

		key := userWordsKey(code, text)
		v := tempB.Get(key)
		if v == nil {
			return nil
		}

		var rec UserWordRecord
		if err := json.Unmarshal(v, &rec); err != nil {
			return err
		}

		// 写入用户词库
		userB, err := schemaSubBucket(tx, schemaID, bucketUserWords, true)
		if err != nil {
			return err
		}

		data, err := json.Marshal(&rec)
		if err != nil {
			return err
		}
		if err := userB.Put(key, data); err != nil {
			return err
		}

		// 从临时词库删除
		return tempB.Delete(key)
	})
}

// EvictTempWords 淘汰低权重临时词条，保留 maxKeep 条
func (s *Store) EvictTempWords(schemaID string, maxKeep int) (int, error) {
	var evicted int
	err := s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, bucketTempWords, false)
		if err != nil {
			return err
		}
		if b == nil {
			return nil
		}

		// 收集所有词条
		type entry struct {
			key    []byte
			weight int
		}
		var all []entry
		b.ForEach(func(k, v []byte) error {
			var rec UserWordRecord
			if err := json.Unmarshal(v, &rec); err == nil {
				keyCopy := make([]byte, len(k))
				copy(keyCopy, k)
				all = append(all, entry{key: keyCopy, weight: rec.Weight})
			}
			return nil
		})

		if len(all) <= maxKeep {
			return nil
		}

		// 按权重升序排列，淘汰权重最低的
		sort.Slice(all, func(i, j int) bool {
			return all[i].weight < all[j].weight
		})

		removeCount := len(all) - maxKeep
		for i := 0; i < removeCount; i++ {
			if err := b.Delete(all[i].key); err != nil {
				return err
			}
			evicted++
		}
		return nil
	})
	return evicted, err
}

// ClearTempWords 清空指定方案的临时词库
func (s *Store) ClearTempWords(schemaID string) (int, error) {
	var count int
	err := s.db.Update(func(tx *bolt.Tx) error {
		schema, err := schemaBucket(tx, schemaID, false)
		if err != nil {
			return err
		}
		if schema == nil {
			return nil
		}

		b := schema.Bucket(bucketTempWords)
		if b == nil {
			return nil
		}

		count = b.Stats().KeyN
		// 删除并重建 bucket 是最快的清空方式
		if err := schema.DeleteBucket(bucketTempWords); err != nil {
			return err
		}
		_, err = schema.CreateBucket(bucketTempWords)
		return err
	})
	return count, err
}

// TempWordCount 返回临时词条总数
func (s *Store) TempWordCount(schemaID string) (int, error) {
	var count int
	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, bucketTempWords, false)
		if err != nil {
			return err
		}
		if b == nil {
			return nil
		}
		count = b.Stats().KeyN
		return nil
	})
	return count, err
}
```

- [ ] **Step 4: 运行测试确认通过**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./internal/store/ -run TestTempWords -v -count=1
```

Expected: PASS

---

## Task 7: 实现 Shadow 规则读写

**Files:**
- Create: `wind_input/internal/store/shadow.go`
- Create: `wind_input/internal/store/shadow_test.go`

- [ ] **Step 1: 编写 Shadow 测试**

```go
// shadow_test.go
package store

import (
	"path/filepath"
	"testing"
)

func TestShadow_PinAndGet(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	// Pin 一个词
	err = s.PinShadow("wubi86", "gg", "王", 0)
	if err != nil {
		t.Fatal(err)
	}

	rules, err := s.GetShadowRules("wubi86", "gg")
	if err != nil {
		t.Fatal(err)
	}
	if len(rules.Pinned) != 1 || rules.Pinned[0].Word != "王" || rules.Pinned[0].Position != 0 {
		t.Errorf("unexpected rules: %+v", rules)
	}
}

func TestShadow_DeleteAndGet(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	err = s.DeleteShadow("wubi86", "gg", "王国")
	if err != nil {
		t.Fatal(err)
	}

	rules, err := s.GetShadowRules("wubi86", "gg")
	if err != nil {
		t.Fatal(err)
	}
	if len(rules.Deleted) != 1 || rules.Deleted[0] != "王国" {
		t.Errorf("unexpected rules: %+v", rules)
	}
}

func TestShadow_PinOverridesDelete(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	// 先删除
	s.DeleteShadow("wubi86", "gg", "王")
	// 再 Pin（应自动从 deleted 中移除）
	s.PinShadow("wubi86", "gg", "王", 0)

	rules, _ := s.GetShadowRules("wubi86", "gg")
	if len(rules.Deleted) != 0 {
		t.Errorf("expected no deleted after pin, got %v", rules.Deleted)
	}
	if len(rules.Pinned) != 1 {
		t.Errorf("expected 1 pinned, got %d", len(rules.Pinned))
	}
}

func TestShadow_RemoveRule(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	s.PinShadow("wubi86", "gg", "王", 0)
	s.DeleteShadow("wubi86", "gg", "王国")

	err = s.RemoveShadowRule("wubi86", "gg", "王")
	if err != nil {
		t.Fatal(err)
	}

	rules, _ := s.GetShadowRules("wubi86", "gg")
	if len(rules.Pinned) != 0 {
		t.Errorf("expected 0 pinned after remove, got %d", len(rules.Pinned))
	}
	if len(rules.Deleted) != 1 {
		t.Errorf("deleted should remain: %v", rules.Deleted)
	}
}

func TestShadow_GetRuleCount(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	s.PinShadow("wubi86", "gg", "王", 0)
	s.PinShadow("wubi86", "tt", "天", 1)
	s.DeleteShadow("wubi86", "gg", "国王")

	count, err := s.ShadowRuleCount("wubi86")
	if err != nil {
		t.Fatal(err)
	}
	if count != 2 { // 2 个编码有规则
		t.Errorf("expected 2 codes with rules, got %d", count)
	}
}
```

- [ ] **Step 2: 运行测试确认失败**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./internal/store/ -run TestShadow -v
```

Expected: FAIL

- [ ] **Step 3: 实现 Shadow 模块**

```go
// shadow.go
package store

import (
	"encoding/json"
	"strings"

	bolt "go.etcd.io/bbolt"
)

var bucketShadow = []byte("Shadow")

// ShadowRecord Shadow 规则存储记录
type ShadowRecord struct {
	Pinned  []ShadowPin `json:"p,omitempty"`
	Deleted []string    `json:"d,omitempty"`
}

// ShadowPin 固定位置规则
type ShadowPin struct {
	Word     string `json:"w"`
	Position int    `json:"pos"`
}

// GetShadowRules 获取指定编码的 Shadow 规则
func (s *Store) GetShadowRules(schemaID, code string) (ShadowRecord, error) {
	code = strings.ToLower(code)
	var rec ShadowRecord

	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, bucketShadow, false)
		if err != nil {
			return err
		}
		if b == nil {
			return nil
		}
		v := b.Get([]byte(code))
		if v == nil {
			return nil
		}
		return json.Unmarshal(v, &rec)
	})
	return rec, err
}

// PinShadow 固定词到指定位置
// 如果词在 deleted 中则自动移除；如果已有 pin 则更新位置并移到头部（LIFO）
func (s *Store) PinShadow(schemaID, code, word string, position int) error {
	code = strings.ToLower(code)
	if position < 0 {
		position = 0
	}

	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, bucketShadow, true)
		if err != nil {
			return err
		}

		var rec ShadowRecord
		if v := b.Get([]byte(code)); v != nil {
			json.Unmarshal(v, &rec)
		}

		// 从 deleted 中移除
		rec.Deleted = removeStr(rec.Deleted, word)

		// 从 pinned 中移除旧记录
		for i, p := range rec.Pinned {
			if p.Word == word {
				rec.Pinned = append(rec.Pinned[:i], rec.Pinned[i+1:]...)
				break
			}
		}

		// 插入头部（LIFO）
		rec.Pinned = append([]ShadowPin{{Word: word, Position: position}}, rec.Pinned...)

		return putShadow(b, code, &rec)
	})
}

// DeleteShadow 隐藏词条
func (s *Store) DeleteShadow(schemaID, code, word string) error {
	code = strings.ToLower(code)
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, bucketShadow, true)
		if err != nil {
			return err
		}

		var rec ShadowRecord
		if v := b.Get([]byte(code)); v != nil {
			json.Unmarshal(v, &rec)
		}

		// 从 pinned 中移除
		for i, p := range rec.Pinned {
			if p.Word == word {
				rec.Pinned = append(rec.Pinned[:i], rec.Pinned[i+1:]...)
				break
			}
		}

		// 添加到 deleted（去重）
		for _, d := range rec.Deleted {
			if d == word {
				return nil
			}
		}
		rec.Deleted = append(rec.Deleted, word)

		return putShadow(b, code, &rec)
	})
}

// RemoveShadowRule 移除词的所有规则
func (s *Store) RemoveShadowRule(schemaID, code, word string) error {
	code = strings.ToLower(code)
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, bucketShadow, false)
		if err != nil {
			return err
		}
		if b == nil {
			return nil
		}

		v := b.Get([]byte(code))
		if v == nil {
			return nil
		}

		var rec ShadowRecord
		if err := json.Unmarshal(v, &rec); err != nil {
			return err
		}

		// 移除 pinned
		for i, p := range rec.Pinned {
			if p.Word == word {
				rec.Pinned = append(rec.Pinned[:i], rec.Pinned[i+1:]...)
				break
			}
		}
		// 移除 deleted
		rec.Deleted = removeStr(rec.Deleted, word)

		// 如果规则为空，删除 key
		if len(rec.Pinned) == 0 && len(rec.Deleted) == 0 {
			return b.Delete([]byte(code))
		}

		return putShadow(b, code, &rec)
	})
}

// ShadowRuleCount 返回有规则的编码数
func (s *Store) ShadowRuleCount(schemaID string) (int, error) {
	var count int
	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, bucketShadow, false)
		if err != nil {
			return err
		}
		if b == nil {
			return nil
		}
		count = b.Stats().KeyN
		return nil
	})
	return count, err
}

// GetAllShadowRules 获取指定方案的所有 Shadow 规则
func (s *Store) GetAllShadowRules(schemaID string) (map[string]ShadowRecord, error) {
	result := make(map[string]ShadowRecord)
	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, bucketShadow, false)
		if err != nil {
			return err
		}
		if b == nil {
			return nil
		}
		return b.ForEach(func(k, v []byte) error {
			var rec ShadowRecord
			if err := json.Unmarshal(v, &rec); err == nil {
				result[string(k)] = rec
			}
			return nil
		})
	})
	return result, err
}

func putShadow(b *bolt.Bucket, code string, rec *ShadowRecord) error {
	data, err := json.Marshal(rec)
	if err != nil {
		return err
	}
	return b.Put([]byte(code), data)
}

func removeStr(slice []string, s string) []string {
	for i, v := range slice {
		if v == s {
			return append(slice[:i], slice[i+1:]...)
		}
	}
	return slice
}
```

- [ ] **Step 4: 运行测试确认通过**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./internal/store/ -run TestShadow -v -count=1
```

Expected: PASS

---

## Task 8: 实现数据迁移（旧文件 → bbolt）

**Files:**
- Create: `wind_input/internal/store/migration.go`
- Create: `wind_input/internal/store/migration_test.go`

- [ ] **Step 1: 编写迁移测试**

```go
// migration_test.go
package store

import (
	"os"
	"path/filepath"
	"testing"
)

func TestMigration_UserDict(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	// 创建旧格式 TSV 文件
	tsvContent := `# Wind Input 用户词库
# 格式: 编码<tab>词语<tab>权重<tab>时间戳<tab>选中次数

ggtt	王国	1200	1712000000	5
gg	王	800	1712000001	3
ggtt	国王	600	1712000002	1
`
	tsvPath := filepath.Join(dir, "test.userwords.txt")
	os.WriteFile(tsvPath, []byte(tsvContent), 0644)

	migrated, err := s.MigrateUserDict("wubi86", tsvPath)
	if err != nil {
		t.Fatal(err)
	}
	if migrated != 3 {
		t.Errorf("expected 3 migrated, got %d", migrated)
	}

	// 验证数据
	words, _ := s.GetUserWords("wubi86", "ggtt")
	if len(words) != 2 {
		t.Fatalf("expected 2 words for ggtt, got %d", len(words))
	}

	// 再次迁移应跳过（幂等）
	migrated2, _ := s.MigrateUserDict("wubi86", tsvPath)
	if migrated2 != 0 {
		t.Errorf("expected 0 on repeat migration, got %d", migrated2)
	}
}

func TestMigration_Shadow(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	yamlContent := `rules:
  gg:
    pinned:
      - word: "王"
        position: 0
    deleted:
      - "王国"
  tt:
    pinned:
      - word: "天"
        position: 1
`
	yamlPath := filepath.Join(dir, "test.shadow.yaml")
	os.WriteFile(yamlPath, []byte(yamlContent), 0644)

	migrated, err := s.MigrateShadow("wubi86", yamlPath)
	if err != nil {
		t.Fatal(err)
	}
	if migrated != 2 {
		t.Errorf("expected 2 codes migrated, got %d", migrated)
	}

	rules, _ := s.GetShadowRules("wubi86", "gg")
	if len(rules.Pinned) != 1 || rules.Pinned[0].Word != "王" {
		t.Errorf("unexpected pinned: %+v", rules.Pinned)
	}
	if len(rules.Deleted) != 1 || rules.Deleted[0] != "王国" {
		t.Errorf("unexpected deleted: %+v", rules.Deleted)
	}
}

func TestMigration_NonExistentFile(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	// 不存在的文件不应报错
	migrated, err := s.MigrateUserDict("wubi86", filepath.Join(dir, "nonexistent.txt"))
	if err != nil {
		t.Fatal(err)
	}
	if migrated != 0 {
		t.Errorf("expected 0 for nonexistent file, got %d", migrated)
	}
}
```

- [ ] **Step 2: 运行测试确认失败**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./internal/store/ -run TestMigration -v
```

Expected: FAIL

- [ ] **Step 3: 实现迁移模块**

```go
// migration.go
package store

import (
	"bufio"
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"

	bolt "go.etcd.io/bbolt"
	"gopkg.in/yaml.v3"
)

// MigrateUserDict 从旧 TSV 格式迁移用户词库
// 返回迁移的词条数，已迁移过则返回 0
func (s *Store) MigrateUserDict(schemaID, tsvPath string) (int, error) {
	// 检查迁移标记
	marker := "migrated_userdict_" + schemaID
	val, _ := s.GetMeta(marker)
	if val == "true" {
		return 0, nil
	}

	// 读取旧文件
	file, err := os.Open(tsvPath)
	if err != nil {
		if os.IsNotExist(err) {
			return 0, nil
		}
		return 0, fmt.Errorf("open tsv: %w", err)
	}
	defer file.Close()

	type wordEntry struct {
		code      string
		text      string
		weight    int
		count     int
		createdAt int64
	}
	var entries []wordEntry

	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}

		parts := strings.Split(line, "\t")
		if len(parts) < 2 {
			continue
		}

		code := strings.ToLower(strings.TrimSpace(parts[0]))
		text := strings.TrimSpace(parts[1])

		weight := 100
		if len(parts) >= 3 {
			if w, err := strconv.Atoi(strings.TrimSpace(parts[2])); err == nil {
				weight = w
			}
		}

		var createdAt int64
		if len(parts) >= 4 {
			if ts, err := strconv.ParseInt(strings.TrimSpace(parts[3]), 10, 64); err == nil {
				createdAt = ts
			}
		}
		if createdAt == 0 {
			createdAt = time.Now().Unix()
		}

		count := 0
		if len(parts) >= 5 {
			if c, err := strconv.Atoi(strings.TrimSpace(parts[4])); err == nil {
				count = c
			}
		}

		entries = append(entries, wordEntry{
			code: code, text: text, weight: weight,
			count: count, createdAt: createdAt,
		})
	}
	if err := scanner.Err(); err != nil {
		return 0, fmt.Errorf("scan tsv: %w", err)
	}

	if len(entries) == 0 {
		s.SetMeta(marker, "true")
		return 0, nil
	}

	// 批量写入 bbolt
	migrated := 0
	err = s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, bucketUserWords, true)
		if err != nil {
			return err
		}

		seen := make(map[string]bool)
		for _, e := range entries {
			key := userWordsKey(e.code, e.text)
			keyStr := string(key)

			// 去重：同 code+text 保留更高权重
			if seen[keyStr] {
				continue
			}
			seen[keyStr] = true

			rec := UserWordRecord{
				Text:      e.text,
				Weight:    e.weight,
				Count:     e.count,
				CreatedAt: e.createdAt,
			}
			data, err := json.Marshal(&rec)
			if err != nil {
				return err
			}
			if err := b.Put(key, data); err != nil {
				return err
			}
			migrated++
		}
		return nil
	})
	if err != nil {
		return 0, err
	}

	// 设置迁移标记
	s.SetMeta(marker, "true")
	return migrated, nil
}

// shadowYAMLConfig shadow.yaml 顶层结构（用于迁移解析）
type shadowYAMLConfig struct {
	Rules map[string]*shadowYAMLCode `yaml:"rules"`
}

type shadowYAMLCode struct {
	Pinned  []shadowYAMLPin `yaml:"pinned,omitempty"`
	Deleted []string        `yaml:"deleted,omitempty"`
}

type shadowYAMLPin struct {
	Word     string `yaml:"word"`
	Position int    `yaml:"position"`
}

// MigrateShadow 从旧 YAML 格式迁移 Shadow 规则
func (s *Store) MigrateShadow(schemaID, yamlPath string) (int, error) {
	marker := "migrated_shadow_" + schemaID
	val, _ := s.GetMeta(marker)
	if val == "true" {
		return 0, nil
	}

	data, err := os.ReadFile(yamlPath)
	if err != nil {
		if os.IsNotExist(err) {
			return 0, nil
		}
		return 0, fmt.Errorf("read shadow yaml: %w", err)
	}

	var config shadowYAMLConfig
	if err := yaml.Unmarshal(data, &config); err != nil {
		return 0, fmt.Errorf("parse shadow yaml: %w", err)
	}

	if len(config.Rules) == 0 {
		s.SetMeta(marker, "true")
		return 0, nil
	}

	migrated := 0
	err = s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, bucketShadow, true)
		if err != nil {
			return err
		}

		for code, cc := range config.Rules {
			code = strings.ToLower(code)
			rec := ShadowRecord{}
			for _, p := range cc.Pinned {
				rec.Pinned = append(rec.Pinned, ShadowPin{Word: p.Word, Position: p.Position})
			}
			rec.Deleted = append(rec.Deleted, cc.Deleted...)

			if len(rec.Pinned) == 0 && len(rec.Deleted) == 0 {
				continue
			}

			recData, err := json.Marshal(&rec)
			if err != nil {
				return err
			}
			if err := b.Put([]byte(code), recData); err != nil {
				return err
			}
			migrated++
		}
		return nil
	})
	if err != nil {
		return 0, err
	}

	s.SetMeta(marker, "true")
	return migrated, nil
}
```

注意：`migration.go` 使用了 `encoding/json`（已在同包其他文件中导入，但每个 Go 文件需独立声明 import）。

- [ ] **Step 4: 运行测试确认通过**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./internal/store/ -run TestMigration -v -count=1
```

Expected: PASS

---

## Task 9: 实现 FreqScorer 接口和 Store 适配层

**Files:**
- Create: `wind_input/internal/dict/freq_scorer.go`
- Create: `wind_input/internal/dict/freq_scorer_test.go`
- Create: `wind_input/internal/dict/store_layer.go`
- Create: `wind_input/internal/dict/store_layer_test.go`

- [ ] **Step 1: 编写 FreqScorer 接口**

在 `internal/dict/freq_scorer.go` 中定义接口，在 `internal/dict/composite.go` 中使用：

```go
// freq_scorer.go
package dict

// FreqScorer 词频评分器接口
// 在 CompositeDict 排序前调用，为候选词附加词频加成
type FreqScorer interface {
	// FreqBoost 返回指定候选词的词频加成分数
	// 返回 0 表示无加成
	FreqBoost(code, text string) int
}
```

- [ ] **Step 2: 编写 StoreUserLayer 和 StoreTempLayer 测试**

```go
// store_layer_test.go
package dict

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/internal/store"
)

func TestStoreUserLayer_SearchAndAdd(t *testing.T) {
	dir := t.TempDir()
	s, err := store.Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	layer := NewStoreUserLayer(s, "wubi86")

	// 初始为空
	results := layer.Search("ggtt", 10)
	if len(results) != 0 {
		t.Errorf("expected 0 results, got %d", len(results))
	}

	// 添加词条
	if err := layer.Add("ggtt", "王国", 1200); err != nil {
		t.Fatal(err)
	}
	if err := layer.Add("ggtt", "国王", 600); err != nil {
		t.Fatal(err)
	}

	results = layer.Search("ggtt", 10)
	if len(results) != 2 {
		t.Fatalf("expected 2 results, got %d", len(results))
	}
	// 应按权重降序
	if results[0].Text != "王国" || results[1].Text != "国王" {
		t.Errorf("unexpected order: %v", results)
	}

	// 前缀搜索
	if err := layer.Add("gg", "王", 800); err != nil {
		t.Fatal(err)
	}
	prefixResults := layer.SearchPrefix("gg", 10)
	if len(prefixResults) != 3 {
		t.Errorf("expected 3 prefix results, got %d", len(prefixResults))
	}
}

func TestStoreUserLayer_Remove(t *testing.T) {
	dir := t.TempDir()
	s, err := store.Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	layer := NewStoreUserLayer(s, "wubi86")
	layer.Add("ggtt", "王国", 1200)

	if err := layer.Remove("ggtt", "王国"); err != nil {
		t.Fatal(err)
	}

	results := layer.Search("ggtt", 10)
	if len(results) != 0 {
		t.Errorf("expected 0 after remove, got %d", len(results))
	}
}

func TestStoreUserLayer_TypeAndName(t *testing.T) {
	dir := t.TempDir()
	s, _ := store.Open(filepath.Join(dir, "test.db"))
	defer s.Close()

	userLayer := NewStoreUserLayer(s, "wubi86")
	if userLayer.Type() != LayerTypeUser {
		t.Errorf("expected LayerTypeUser, got %v", userLayer.Type())
	}

	tempLayer := NewStoreTempLayer(s, "wubi86")
	if tempLayer.Type() != LayerTypeTemp {
		t.Errorf("expected LayerTypeTemp, got %v", tempLayer.Type())
	}
}

func TestStoreFreqScorer(t *testing.T) {
	dir := t.TempDir()
	s, err := store.Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	// 写入词频数据
	s.IncrementFreq("wubi86", "gg", "王")
	s.IncrementFreq("wubi86", "gg", "王")
	s.IncrementFreq("wubi86", "gg", "王")

	scorer := NewStoreFreqScorer(s, "wubi86")
	boost := scorer.FreqBoost("gg", "王")
	if boost <= 0 {
		t.Errorf("expected positive boost, got %d", boost)
	}

	// 不存在的词应返回 0
	boost2 := scorer.FreqBoost("xyz", "不存在")
	if boost2 != 0 {
		t.Errorf("expected 0 for unknown word, got %d", boost2)
	}
}

// cleanupTestDB 仅用于防止测试残留（如果需要）
func cleanupTestDB(dir string) {
	os.RemoveAll(dir)
}
```

- [ ] **Step 3: 运行测试确认失败**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./internal/dict/ -run "TestStore" -v
```

Expected: FAIL

- [ ] **Step 4: 实现 StoreUserLayer、StoreTempLayer 和 StoreFreqScorer**

```go
// store_layer.go
package dict

import (
	"sort"
	"strings"
	"time"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/store"
)

// ── StoreUserLayer: 基于 Store 的用户词库层 ──

// StoreUserLayer 实现 MutableLayer 接口，后端为 bbolt Store
type StoreUserLayer struct {
	store    *store.Store
	schemaID string
	name     string
}

// NewStoreUserLayer 创建基于 Store 的用户词库层
func NewStoreUserLayer(s *store.Store, schemaID string) *StoreUserLayer {
	return &StoreUserLayer{
		store:    s,
		schemaID: schemaID,
		name:     "user_" + schemaID,
	}
}

func (l *StoreUserLayer) Name() string        { return l.name }
func (l *StoreUserLayer) Type() LayerType      { return LayerTypeUser }

func (l *StoreUserLayer) Search(code string, limit int) []candidate.Candidate {
	code = strings.ToLower(code)
	words, err := l.store.GetUserWords(l.schemaID, code)
	if err != nil || len(words) == 0 {
		return nil
	}

	results := make([]candidate.Candidate, 0, len(words))
	for _, w := range words {
		results = append(results, candidate.Candidate{
			Text:     w.Text,
			Code:     code,
			Weight:   w.Weight,
			IsCommon: true,
		})
	}

	sort.Slice(results, func(i, j int) bool {
		return candidate.Better(results[i], results[j])
	})

	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}
	return results
}

func (l *StoreUserLayer) SearchPrefix(prefix string, limit int) []candidate.Candidate {
	prefix = strings.ToLower(prefix)
	words, err := l.store.SearchUserWordsPrefix(l.schemaID, prefix, 0)
	if err != nil || len(words) == 0 {
		return nil
	}

	results := make([]candidate.Candidate, 0, len(words))
	for _, w := range words {
		results = append(results, candidate.Candidate{
			Text:     w.Text,
			Weight:   w.Weight,
			IsCommon: true,
		})
	}

	sort.Slice(results, func(i, j int) bool {
		return candidate.Better(results[i], results[j])
	})

	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}
	return results
}

func (l *StoreUserLayer) Add(code string, text string, weight int) error {
	return l.store.AddUserWord(l.schemaID, code, text, weight)
}

func (l *StoreUserLayer) Remove(code string, text string) error {
	return l.store.RemoveUserWord(l.schemaID, code, text)
}

func (l *StoreUserLayer) Update(code string, text string, newWeight int) error {
	return l.store.UpdateUserWordWeight(l.schemaID, code, text, newWeight)
}

func (l *StoreUserLayer) Save() error {
	return nil // bbolt 自动持久化，无需手动 save
}

// EntryCount 返回词条数量
func (l *StoreUserLayer) EntryCount() int {
	count, _ := l.store.UserWordCount(l.schemaID)
	return count
}

// IncreaseWeight 增加词条权重（兼容旧接口）
func (l *StoreUserLayer) IncreaseWeight(code string, text string, delta int) {
	code = strings.ToLower(code)
	words, err := l.store.GetUserWords(l.schemaID, code)
	if err != nil {
		return
	}
	for _, w := range words {
		if w.Text == text {
			newWeight := w.Weight + delta
			if newWeight > MaxDynamicWeight {
				newWeight = MaxDynamicWeight
			}
			l.store.UpdateUserWordWeight(l.schemaID, code, text, newWeight)
			return
		}
	}
}

// OnWordSelected 带误选保护的选词回调（兼容旧接口）
func (l *StoreUserLayer) OnWordSelected(code, text string, addWeight, boostDelta, countThreshold int) {
	code = strings.ToLower(code)
	words, _ := l.store.GetUserWords(l.schemaID, code)
	for _, w := range words {
		if w.Text == text {
			// 已存在：增加 count，达到阈值后提权
			// 这里需要通过 bbolt 事务原子更新，委托给 Store
			l.store.OnWordSelected(l.schemaID, code, text, boostDelta, countThreshold)
			return
		}
	}
	// 不存在：添加新词
	l.store.AddUserWord(l.schemaID, code, text, addWeight)
}

// ── StoreTempLayer: 基于 Store 的临时词库层 ──

// StoreTempLayer 实现 DictLayer 接口，后端为 bbolt Store 的 TempWords bucket
type StoreTempLayer struct {
	store        *store.Store
	schemaID     string
	name         string
	maxEntries   int
	promoteCount int
}

// NewStoreTempLayer 创建基于 Store 的临时词库层
func NewStoreTempLayer(s *store.Store, schemaID string) *StoreTempLayer {
	return &StoreTempLayer{
		store:    s,
		schemaID: schemaID,
		name:     "temp_" + schemaID,
	}
}

// SetLimits 设置临时词库限制
func (l *StoreTempLayer) SetLimits(maxEntries, promoteCount int) {
	l.maxEntries = maxEntries
	l.promoteCount = promoteCount
}

func (l *StoreTempLayer) Name() string        { return l.name }
func (l *StoreTempLayer) Type() LayerType      { return LayerTypeTemp }

func (l *StoreTempLayer) Search(code string, limit int) []candidate.Candidate {
	code = strings.ToLower(code)
	words, err := l.store.GetTempWords(l.schemaID, code)
	if err != nil || len(words) == 0 {
		return nil
	}

	results := make([]candidate.Candidate, 0, len(words))
	for _, w := range words {
		results = append(results, candidate.Candidate{
			Text:     w.Text,
			Code:     code,
			Weight:   w.Weight,
			IsCommon: true,
		})
	}

	sort.Slice(results, func(i, j int) bool {
		return candidate.Better(results[i], results[j])
	})

	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}
	return results
}

func (l *StoreTempLayer) SearchPrefix(prefix string, limit int) []candidate.Candidate {
	prefix = strings.ToLower(prefix)
	words, err := l.store.SearchTempWordsPrefix(l.schemaID, prefix, 0)
	if err != nil || len(words) == 0 {
		return nil
	}

	results := make([]candidate.Candidate, 0, len(words))
	for _, w := range words {
		results = append(results, candidate.Candidate{
			Text:     w.Text,
			Weight:   w.Weight,
			IsCommon: true,
		})
	}

	sort.Slice(results, func(i, j int) bool {
		return candidate.Better(results[i], results[j])
	})

	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}
	return results
}

// LearnWord 学习新词，返回是否达到晋升条件
func (l *StoreTempLayer) LearnWord(code, text string, weightDelta int) bool {
	l.store.LearnTempWord(l.schemaID, code, text, weightDelta)

	// 检查是否需要淘汰
	if l.maxEntries > 0 {
		l.store.EvictTempWords(l.schemaID, l.maxEntries)
	}

	// 检查晋升条件
	if l.promoteCount > 0 {
		words, _ := l.store.GetTempWords(l.schemaID, strings.ToLower(code))
		for _, w := range words {
			if w.Text == text && w.Count >= l.promoteCount {
				return true
			}
		}
	}
	return false
}

// PromoteWord 晋升词条到用户词库
func (l *StoreTempLayer) PromoteWord(code, text string) bool {
	err := l.store.PromoteTempWord(l.schemaID, code, text)
	return err == nil
}

// GetWordCount 获取词条数量
func (l *StoreTempLayer) GetWordCount() int {
	count, _ := l.store.TempWordCount(l.schemaID)
	return count
}

// Clear 清空临时词库
func (l *StoreTempLayer) Clear() int {
	count, _ := l.store.ClearTempWords(l.schemaID)
	return count
}

// ── StoreFreqScorer: 基于 Store 的词频评分器 ──

// StoreFreqScorer 实现 FreqScorer 接口
type StoreFreqScorer struct {
	store    *store.Store
	schemaID string
}

// NewStoreFreqScorer 创建基于 Store 的词频评分器
func NewStoreFreqScorer(s *store.Store, schemaID string) *StoreFreqScorer {
	return &StoreFreqScorer{store: s, schemaID: schemaID}
}

func (f *StoreFreqScorer) FreqBoost(code, text string) int {
	rec, err := f.store.GetFreq(f.schemaID, code, text)
	if err != nil || rec.Count == 0 {
		return 0
	}
	return store.CalcFreqBoost(rec, time.Now().Unix())
}
```

- [ ] **Step 5: 在 Store 中添加 OnWordSelected 方法**

在 `internal/store/user_words.go` 末尾追加：

```go
// OnWordSelected 带误选保护的选词更新（原子操作）
func (s *Store) OnWordSelected(schemaID, code, text string, boostDelta, countThreshold int) error {
	code = strings.ToLower(code)
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, bucketUserWords, false)
		if err != nil || b == nil {
			return err
		}

		key := userWordsKey(code, text)
		v := b.Get(key)
		if v == nil {
			return nil
		}

		var rec UserWordRecord
		if err := json.Unmarshal(v, &rec); err != nil {
			return err
		}

		rec.Count++
		if rec.Count >= countThreshold {
			rec.Weight += boostDelta
			if rec.Weight > 10000 {
				rec.Weight = 10000
			}
		}

		data, err := json.Marshal(&rec)
		if err != nil {
			return err
		}
		return b.Put(key, data)
	})
}
```

- [ ] **Step 6: 运行测试确认通过**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./internal/dict/ -run "TestStore" -v -count=1
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./internal/store/ -v -count=1
```

Expected: ALL PASS

---

## Task 10: 在 CompositeDict 中集成 FreqScorer

**Files:**
- Modify: `wind_input/internal/dict/composite.go:10-21` (添加 freqScorer 字段)
- Modify: `wind_input/internal/dict/composite.go:99-139` (searchInternal 中应用 freq boost)

- [ ] **Step 1: 在 CompositeDict 中添加 FreqScorer 支持**

在 `CompositeDict` 结构体中添加字段：

```go
// 在 CompositeDict 结构体中添加：
freqScorer FreqScorer
```

添加 setter 方法：

```go
// SetFreqScorer 设置词频评分器
func (c *CompositeDict) SetFreqScorer(scorer FreqScorer) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.freqScorer = scorer
}
```

- [ ] **Step 2: 修改 searchInternal 方法**

在 `searchInternal` 中排序前，应用 FreqScorer：

```go
// 在 searchInternal 的 "2. 排序" 之前，插入：
// 1.5 应用词频加成
if c.freqScorer != nil {
	for i := range results {
		boost := c.freqScorer.FreqBoost(results[i].Code, results[i].Text)
		if boost > 0 {
			results[i].Weight += boost
		}
	}
}
```

- [ ] **Step 3: 运行现有测试确保不破坏已有功能**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./internal/dict/ -v -count=1
```

Expected: ALL PASS（FreqScorer 为 nil 时不影响已有逻辑）

---

## Task 11: 适配 DictManager 使用 Store

**Files:**
- Modify: `wind_input/internal/dict/manager.go`

- [ ] **Step 1: 在 DictManager 中添加 Store 相关字段和方法**

在 `DictManager` 结构体中添加：

```go
// Store 相关（新增）
store          *store.Store
storeUserLayers map[string]*StoreUserLayer  // schemaID -> StoreUserLayer
storeTempLayers map[string]*StoreTempLayer  // schemaID -> StoreTempLayer
freqScorers    map[string]*StoreFreqScorer  // schemaID -> StoreFreqScorer
useStore       bool                         // 是否使用 Store 后端
```

添加 `OpenStore` 方法：

```go
// OpenStore 打开 bbolt 数据库并启用 Store 后端
// 应在 Initialize() 之前调用
func (dm *DictManager) OpenStore(dbPath string) error {
	dm.mu.Lock()
	defer dm.mu.Unlock()

	s, err := store.Open(dbPath)
	if err != nil {
		return fmt.Errorf("open store: %w", err)
	}

	dm.store = s
	dm.storeUserLayers = make(map[string]*StoreUserLayer)
	dm.storeTempLayers = make(map[string]*StoreTempLayer)
	dm.freqScorers = make(map[string]*StoreFreqScorer)
	dm.useStore = true

	dm.logger.Info("Store 后端已启用", "path", dbPath)
	return nil
}
```

- [ ] **Step 2: 修改 SwitchSchemaFull 支持 Store 后端**

在 `SwitchSchemaFull` 方法中，当 `dm.useStore` 为 true 时，使用 StoreUserLayer 和 StoreTempLayer 代替文件版本：

```go
// SwitchSchemaFull 中用户词库加载部分替换为：
if dm.useStore {
	// 使用 Store 后端
	userLayer, ok := dm.storeUserLayers[schemaID]
	if !ok {
		userLayer = NewStoreUserLayer(dm.store, schemaID)
		dm.storeUserLayers[schemaID] = userLayer

		// 尝试迁移旧文件
		oldPath := dm.resolveDataPath(userDictFile, schemaID+".userwords.txt")
		if migrated, err := dm.store.MigrateUserDict(schemaID, oldPath); err != nil {
			dm.logger.Warn("迁移用户词库失败", "schemaID", schemaID, "error", err)
		} else if migrated > 0 {
			dm.logger.Info("用户词库已迁移", "schemaID", schemaID, "entries", migrated)
		}
	}
	dm.compositeDict.AddLayer(userLayer)

	// 设置词频评分器
	scorer, ok := dm.freqScorers[schemaID]
	if !ok {
		scorer = NewStoreFreqScorer(dm.store, schemaID)
		dm.freqScorers[schemaID] = scorer
	}
	dm.compositeDict.SetFreqScorer(scorer)
} else {
	// 旧的文件后端逻辑（保持不变）
	...
}
```

Shadow 和 TempDict 的切换逻辑同理：Store 模式下使用 Store 后端，否则使用文件后端。

- [ ] **Step 3: 修改 Close 方法**

```go
func (dm *DictManager) Close() error {
	dm.mu.Lock()
	defer dm.mu.Unlock()

	// 关闭旧后端
	for _, ud := range dm.userDicts {
		ud.Close()
	}
	for _, td := range dm.tempDicts {
		td.Close()
	}
	for _, sl := range dm.shadowLayers {
		if sl.IsDirty() {
			sl.Save()
		}
	}

	// 关闭 Store
	if dm.store != nil {
		if err := dm.store.Close(); err != nil {
			dm.logger.Error("关闭 Store 失败", "error", err)
			return err
		}
	}

	return nil
}
```

- [ ] **Step 4: 编译验证**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go build ./...
```

Expected: 编译成功

- [ ] **Step 5: 运行全部 dict 测试**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./internal/dict/ -v -count=1
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./internal/store/ -v -count=1
```

Expected: ALL PASS

---

## Task 12: 全量编译和集成验证

**Files:**
- 全项目

- [ ] **Step 1: 格式化代码**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go fmt ./internal/store/... ./internal/dict/...
```

- [ ] **Step 2: 全量编译**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go build ./...
```

Expected: 编译成功

- [ ] **Step 3: 运行全部测试**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./... -count=1
```

Expected: ALL PASS

- [ ] **Step 4: 检查是否有未使用的导入或变量**

```bash
cd D:/Develop/workspace/go_dev/WindInput/wind_input && go vet ./internal/store/... ./internal/dict/...
```

Expected: 无错误
