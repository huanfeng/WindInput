package schema

import (
	"fmt"
	"log"
	"sync"
)

// SchemaManager 输入方案管理器
type SchemaManager struct {
	mu       sync.RWMutex
	schemas  map[string]*Schema
	activeID string
	exeDir   string
	dataDir  string
}

// NewSchemaManager 创建方案管理器
func NewSchemaManager(exeDir, dataDir string) *SchemaManager {
	return &SchemaManager{
		schemas: make(map[string]*Schema),
		exeDir:  exeDir,
		dataDir: dataDir,
	}
}

// LoadSchemas 扫描并加载所有方案文件
func (sm *SchemaManager) LoadSchemas() error {
	sm.mu.Lock()
	defer sm.mu.Unlock()

	schemas, err := DiscoverSchemas(sm.exeDir, sm.dataDir)
	if err != nil {
		return err
	}

	sm.schemas = schemas

	for id, s := range schemas {
		log.Printf("[schema] 已加载方案: %s (%s)", s.Schema.Name, id)
	}

	return nil
}

// GetSchema 按 ID 获取方案
func (sm *SchemaManager) GetSchema(id string) *Schema {
	sm.mu.RLock()
	defer sm.mu.RUnlock()
	return sm.schemas[id]
}

// GetActiveSchema 获取当前活跃方案
func (sm *SchemaManager) GetActiveSchema() *Schema {
	sm.mu.RLock()
	defer sm.mu.RUnlock()
	if sm.activeID == "" {
		return nil
	}
	return sm.schemas[sm.activeID]
}

// GetActiveID 获取当前活跃方案 ID
func (sm *SchemaManager) GetActiveID() string {
	sm.mu.RLock()
	defer sm.mu.RUnlock()
	return sm.activeID
}

// SetActive 设置活跃方案
func (sm *SchemaManager) SetActive(id string) error {
	sm.mu.Lock()
	defer sm.mu.Unlock()

	if _, ok := sm.schemas[id]; !ok {
		return fmt.Errorf("方案 %q 不存在", id)
	}
	sm.activeID = id
	return nil
}

// ListSchemas 列出所有可用方案信息
func (sm *SchemaManager) ListSchemas() []*SchemaInfo {
	sm.mu.RLock()
	defer sm.mu.RUnlock()

	result := make([]*SchemaInfo, 0, len(sm.schemas))
	for _, s := range sm.schemas {
		info := s.Schema
		result = append(result, &info)
	}
	return result
}

// SchemaCount 返回已加载的方案数量
func (sm *SchemaManager) SchemaCount() int {
	sm.mu.RLock()
	defer sm.mu.RUnlock()
	return len(sm.schemas)
}

// GetExeDir 获取可执行文件目录
func (sm *SchemaManager) GetExeDir() string {
	return sm.exeDir
}

// GetDataDir 获取用户数据目录
func (sm *SchemaManager) GetDataDir() string {
	return sm.dataDir
}
