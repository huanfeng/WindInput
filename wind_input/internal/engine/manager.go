package engine

import (
	"fmt"
	"log/slog"
	"sync"

	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/engine/codetable"
	"github.com/huanfeng/wind_input/internal/engine/mixed"
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
	"github.com/huanfeng/wind_input/internal/schema"
)

// Manager 引擎管理器
type Manager struct {
	mu            sync.RWMutex
	engines       map[string]Engine         // schemaID -> Engine
	systemLayers  map[string]dict.DictLayer // schemaID -> 该方案注册的系统词库层
	currentID     string                    // 当前活跃方案 ID
	currentEngine Engine

	// 临时方案切换
	tempSchemaID  string // 非空 = 临时方案模式
	savedSchemaID string // 临时切换前的方案 ID

	// 方案管理器
	schemaManager *schema.SchemaManager

	// 可执行文件目录
	exeDir string

	// 词库管理器
	dictManager *dict.DictManager

	// 反向索引缓存（字 → 编码列表）
	cachedReverseIndex    map[string][]string
	cachedReverseSchemaID string

	// 日志
	logger *slog.Logger
}

// NewManager 创建引擎管理器
func NewManager(logger *slog.Logger) *Manager {
	return &Manager{
		engines:      make(map[string]Engine),
		systemLayers: make(map[string]dict.DictLayer),
		logger:       logger,
	}
}

// SetSchemaManager 设置方案管理器
func (m *Manager) SetSchemaManager(sm *schema.SchemaManager) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.schemaManager = sm
}

// SetExeDir 设置可执行文件目录
func (m *Manager) SetExeDir(dir string) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.exeDir = dir
}

// SetDictManager 设置词库管理器
func (m *Manager) SetDictManager(dm *dict.DictManager) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.dictManager = dm
}

// GetDictManager 获取词库管理器
func (m *Manager) GetDictManager() *dict.DictManager {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.dictManager
}

// SwitchSchema 切换到指定方案（如引擎未加载则创建）
func (m *Manager) SwitchSchema(schemaID string) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	if m.currentID == schemaID {
		return nil
	}

	// 卸载旧引擎的系统词库层
	if m.dictManager != nil {
		m.dictManager.UnregisterSystemLayer("codetable-system")
		m.dictManager.UnregisterSystemLayer("pinyin-system")
	}

	// 检查是否已加载
	if eng, ok := m.engines[schemaID]; ok {
		m.currentID = schemaID
		m.currentEngine = eng
		// 清空反向索引缓存，切换方案后重建
		m.cachedReverseIndex = nil
		m.cachedReverseSchemaID = ""
		// 重新注册缓存引擎的系统词库层
		m.reRegisterSystemLayer(schemaID)
		m.logger.Info("切换到已加载方案", "schemaID", schemaID)
		return nil
	}

	// 需要创建引擎（factory 内部会注册系统词库层）
	if err := m.loadSchemaEngineLocked(schemaID); err != nil {
		return err
	}

	m.currentID = schemaID
	m.currentEngine = m.engines[schemaID]
	// 清空反向索引缓存，切换方案后重建
	m.cachedReverseIndex = nil
	m.cachedReverseSchemaID = ""
	m.logger.Info("加载并切换方案", "schemaID", schemaID)
	return nil
}

// ToggleSchema 按 available 列表循环切换方案
// available 为配置中启用的方案 ID 列表（顺序决定切换顺序）；
// 若为空则回退到 SchemaManager 中所有已加载方案。
func (m *Manager) ToggleSchema(available []string) (string, error) {
	m.mu.RLock()
	sm := m.schemaManager
	currentID := m.currentID
	m.mu.RUnlock()

	if sm == nil {
		return currentID, fmt.Errorf("SchemaManager 未设置")
	}

	// 使用 available 列表；若为空则回退到所有已加载方案
	var idList []string
	if len(available) > 0 {
		idList = available
	} else {
		schemas := sm.ListSchemas()
		for _, s := range schemas {
			idList = append(idList, s.ID)
		}
	}

	if len(idList) <= 1 {
		return currentID, nil
	}

	// 找当前方案在列表中的位置，切换到下一个
	nextID := ""
	for i, id := range idList {
		if id == currentID {
			nextID = idList[(i+1)%len(idList)]
			break
		}
	}
	if nextID == "" {
		nextID = idList[0]
	}

	if err := m.SwitchSchema(nextID); err != nil {
		return currentID, err
	}

	// 同步 DictManager
	m.mu.RLock()
	dm := m.dictManager
	m.mu.RUnlock()
	if dm != nil {
		s := sm.GetSchema(nextID)
		if s != nil {
			dm.SwitchSchemaFull(nextID, s.UserData.ShadowFile, s.UserData.UserDictFile,
				s.UserData.TempDictFile, s.Learning.TempMaxEntries, s.Learning.TempPromoteCount)
		}
	}

	// 更新 SchemaManager 的活跃方案
	sm.SetActive(nextID)

	return nextID, nil
}

// ActivateTempSchema 临时激活方案（如码表方案下临时用拼音）
func (m *Manager) ActivateTempSchema(schemaID string) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	if m.tempSchemaID != "" {
		return fmt.Errorf("已在临时方案模式中: %s", m.tempSchemaID)
	}

	// 保存当前方案
	m.savedSchemaID = m.currentID
	m.tempSchemaID = schemaID

	// 加载目标方案（如未加载）
	if _, ok := m.engines[schemaID]; !ok {
		if err := m.loadSchemaEngineLocked(schemaID); err != nil {
			m.tempSchemaID = ""
			m.savedSchemaID = ""
			return err
		}
	}

	m.currentID = schemaID
	m.currentEngine = m.engines[schemaID]
	m.logger.Info("临时激活方案", "schemaID", schemaID, "saved", m.savedSchemaID)
	return nil
}

// DeactivateTempSchema 退出临时方案，恢复到之前的方案
func (m *Manager) DeactivateTempSchema() {
	m.mu.Lock()
	defer m.mu.Unlock()

	if m.tempSchemaID == "" {
		return
	}

	if eng, ok := m.engines[m.savedSchemaID]; ok {
		m.currentID = m.savedSchemaID
		m.currentEngine = eng
	}

	m.logger.Info("退出临时方案", "tempSchemaID", m.tempSchemaID, "restored", m.savedSchemaID)
	m.tempSchemaID = ""
	m.savedSchemaID = ""
}

// IsTempSchemaActive 是否处于临时方案模式
func (m *Manager) IsTempSchemaActive() bool {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.tempSchemaID != ""
}

// loadSchemaEngineLocked 加载方案引擎（调用方必须持有锁）
func (m *Manager) loadSchemaEngineLocked(schemaID string, opts ...schema.EngineCreateOptions) error {
	if m.schemaManager == nil {
		return fmt.Errorf("SchemaManager 未设置")
	}

	s := m.schemaManager.GetSchema(schemaID)
	if s == nil {
		return fmt.Errorf("方案 %q 不存在", schemaID)
	}

	resolver := func(id string) *schema.Schema {
		return m.schemaManager.GetSchema(id)
	}
	dataDir := ""
	if m.schemaManager != nil {
		dataDir = m.schemaManager.GetDataDir()
	}
	bundle, err := schema.CreateEngineFromSchema(s, m.exeDir, dataDir, m.dictManager, m.logger, resolver, opts...)
	if err != nil {
		return fmt.Errorf("创建方案 %q 引擎失败: %w", schemaID, err)
	}

	// 将引擎包装为 Engine 接口
	switch eng := bundle.Engine.(type) {
	case *pinyin.Engine:
		m.engines[schemaID] = eng
	case *codetable.Engine:
		m.engines[schemaID] = eng
	case *mixed.Engine:
		m.engines[schemaID] = eng
	default:
		return fmt.Errorf("未知引擎类型: %T", bundle.Engine)
	}

	// 缓存 factory 注册的系统词库层（用于切换回缓存引擎时重新注册）
	// 根据引擎类型查找对应的层名，避免在 CompositeDict 中同时存在多种层时缓存错误的层
	if m.dictManager != nil {
		var layerName string
		switch bundle.Engine.(type) {
		case *pinyin.Engine:
			layerName = "pinyin-system"
		case *codetable.Engine:
			layerName = "codetable-system"
		case *mixed.Engine:
			// 混输引擎注册的是 codetable-system 层（拼音层在独立 CompositeDict 中）
			layerName = "codetable-system"
		}
		if layerName != "" {
			if layer := m.dictManager.GetCompositeDict().GetLayerByName(layerName); layer != nil {
				m.systemLayers[schemaID] = layer
			}
		}
	}

	return nil
}

// reRegisterSystemLayer 为缓存引擎重新注册系统词库层到 CompositeDict
func (m *Manager) reRegisterSystemLayer(schemaID string) {
	if m.dictManager == nil {
		return
	}
	// 从缓存的 systemLayers 中取出该方案的系统词库层并重新注册
	if layer, ok := m.systemLayers[schemaID]; ok && layer != nil {
		m.dictManager.RegisterSystemLayer(layer.Name(), layer)
		m.logger.Info("重新注册系统词库层", "layer", layer.Name(), "schemaID", schemaID)
	}
}

// --- 查询方法 ---

// GetCurrentEngine 获取当前引擎
func (m *Manager) GetCurrentEngine() Engine {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.currentEngine
}

// GetCurrentType 获取当前引擎类型（通过 SchemaManager 读取真实的 engine.type）
func (m *Manager) GetCurrentType() EngineType {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.schemaManager != nil {
		if s := m.schemaManager.GetSchema(m.currentID); s != nil {
			return s.Engine.Type
		}
	}
	return EngineType(m.currentID) // fallback
}

// GetCurrentSchemaID 获取当前方案 ID
func (m *Manager) GetCurrentSchemaID() string {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.currentID
}

// GetEngineDisplayName 获取引擎显示名称（从 Schema 读取）
func (m *Manager) GetEngineDisplayName() string {
	m.mu.RLock()
	sm := m.schemaManager
	id := m.currentID
	m.mu.RUnlock()

	if sm != nil {
		s := sm.GetSchema(id)
		if s != nil {
			return s.Schema.IconLabel
		}
	}
	return "?"
}

// GetSchemaNameByID 按 ID 获取方案显示名称
func (m *Manager) GetSchemaNameByID(id string) string {
	m.mu.RLock()
	sm := m.schemaManager
	m.mu.RUnlock()

	if sm != nil {
		s := sm.GetSchema(id)
		if s != nil {
			return s.Schema.Name
		}
	}
	return id
}

// SwitchToSchemaByID 切换到指定方案（含 DictManager 同步和 SchemaManager 更新）
func (m *Manager) SwitchToSchemaByID(schemaID string) error {
	m.mu.RLock()
	sm := m.schemaManager
	currentID := m.currentID
	m.mu.RUnlock()

	if sm == nil {
		return fmt.Errorf("SchemaManager 未设置")
	}
	if schemaID == currentID {
		return nil
	}

	if err := m.SwitchSchema(schemaID); err != nil {
		return err
	}

	// 同步 DictManager
	m.mu.RLock()
	dm := m.dictManager
	m.mu.RUnlock()
	if dm != nil {
		s := sm.GetSchema(schemaID)
		if s != nil {
			dm.SwitchSchemaFull(schemaID, s.UserData.ShadowFile, s.UserData.UserDictFile,
				s.UserData.TempDictFile, s.Learning.TempMaxEntries, s.Learning.TempPromoteCount)
		}
	}

	// 更新 SchemaManager 的活跃方案
	sm.SetActive(schemaID)

	return nil
}

// GetSchemaDisplayInfo 获取方案显示信息（名称 + 图标）
func (m *Manager) GetSchemaDisplayInfo() (name, iconLabel string) {
	m.mu.RLock()
	sm := m.schemaManager
	id := m.currentID
	m.mu.RUnlock()

	if sm != nil {
		s := sm.GetSchema(id)
		if s != nil {
			return s.Schema.Name, s.Schema.IconLabel
		}
	}
	return id, "?"
}

// IsCurrentEngineType 检查当前方案的引擎类型
func (m *Manager) IsCurrentEngineType(engineType schema.EngineType) bool {
	m.mu.RLock()
	sm := m.schemaManager
	id := m.currentID
	m.mu.RUnlock()

	if sm != nil {
		s := sm.GetSchema(id)
		if s != nil {
			return s.Engine.Type == engineType
		}
	}
	return false
}

// isAbsPath 判断是否为绝对路径
func isAbsPath(path string) bool {
	if len(path) == 0 {
		return false
	}
	if len(path) >= 2 && path[1] == ':' {
		return true
	}
	if len(path) >= 2 && path[0] == '\\' && path[1] == '\\' {
		return true
	}
	return path[0] == '/'
}
