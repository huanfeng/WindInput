package schema

import (
	"fmt"
	"log/slog"
	"sync"

	"github.com/huanfeng/wind_input/pkg/config"
	"gopkg.in/yaml.v3"
)

// SchemaManager 输入方案管理器
type SchemaManager struct {
	mu       sync.RWMutex
	schemas  map[string]*Schema
	activeID string
	exeDir   string
	dataDir  string
	logger   *slog.Logger
}

// NewSchemaManager 创建方案管理器
func NewSchemaManager(exeDir, dataDir string, logger *slog.Logger) *SchemaManager {
	if logger == nil {
		logger = slog.Default()
	}
	return &SchemaManager{
		schemas: make(map[string]*Schema),
		exeDir:  exeDir,
		dataDir: dataDir,
		logger:  logger,
	}
}

// LoadSchemas 扫描并加载所有方案文件
// 加载顺序：Layer1 内置方案 → Layer2 用户方案 → Layer3 schema_overrides.yaml
func (sm *SchemaManager) LoadSchemas() error {
	sm.mu.Lock()
	defer sm.mu.Unlock()

	// Layer 1 + Layer 2: 内置方案 + 用户方案
	schemas, err := DiscoverSchemas(sm.exeDir, sm.dataDir)
	if err != nil {
		return err
	}

	// Layer 3: 叠加 schema_overrides.yaml 覆盖配置
	overrides, overrideErr := config.LoadSchemaOverrides()
	if overrideErr != nil {
		sm.logger.Warn("加载方案覆盖配置失败，跳过 Layer3", "error", overrideErr)
	} else {
		for schemaID, override := range overrides {
			s, ok := schemas[schemaID]
			if !ok {
				continue
			}
			overrideData, marshalErr := yaml.Marshal(override)
			if marshalErr != nil {
				sm.logger.Warn("序列化方案覆盖配置失败", "schema", schemaID, "error", marshalErr)
				continue
			}
			if err := yaml.Unmarshal(overrideData, s); err != nil {
				sm.logger.Warn("应用方案覆盖配置失败", "schema", schemaID, "error", err)
			}
		}
	}

	sm.schemas = schemas

	for id, s := range schemas {
		sm.logger.Info("已加载方案", "name", s.Schema.Name, "id", id)
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
