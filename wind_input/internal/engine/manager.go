package engine

import (
	"fmt"
	"log"
	"sync"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
	"github.com/huanfeng/wind_input/internal/engine/wubi"
	"github.com/huanfeng/wind_input/internal/schema"
)

// Manager 引擎管理器
type Manager struct {
	mu            sync.RWMutex
	engines       map[string]Engine // schemaID -> Engine
	currentID     string            // 当前活跃方案 ID
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
}

// NewManager 创建引擎管理器
func NewManager() *Manager {
	return &Manager{
		engines: make(map[string]Engine),
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

	// 检查是否已加载
	if eng, ok := m.engines[schemaID]; ok {
		m.currentID = schemaID
		m.currentEngine = eng
		log.Printf("[EngineManager] 切换到已加载方案: %s", schemaID)
		return nil
	}

	// 需要创建引擎
	if err := m.loadSchemaEngineLocked(schemaID); err != nil {
		return err
	}

	m.currentID = schemaID
	m.currentEngine = m.engines[schemaID]
	log.Printf("[EngineManager] 加载并切换方案: %s", schemaID)
	return nil
}

// ToggleSchema 按 available 列表循环切换方案
func (m *Manager) ToggleSchema() (string, error) {
	m.mu.RLock()
	sm := m.schemaManager
	currentID := m.currentID
	m.mu.RUnlock()

	if sm == nil {
		return currentID, fmt.Errorf("SchemaManager 未设置")
	}

	activeSchema := sm.GetActiveSchema()
	if activeSchema == nil {
		return currentID, fmt.Errorf("无活跃方案")
	}

	// 获取可用方案列表，找到下一个
	schemas := sm.ListSchemas()
	if len(schemas) <= 1 {
		return currentID, nil
	}

	// 找当前方案在列表中的位置，切换到下一个
	nextID := ""
	for i, s := range schemas {
		if s.ID == currentID {
			nextID = schemas[(i+1)%len(schemas)].ID
			break
		}
	}
	if nextID == "" {
		nextID = schemas[0].ID
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
			dm.SwitchSchema(nextID, s.UserData.ShadowFile, s.UserData.UserDictFile)
		}
	}

	// 更新 SchemaManager 的活跃方案
	sm.SetActive(nextID)

	return nextID, nil
}

// ActivateTempSchema 临时激活方案（如五笔下临时用拼音）
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
	log.Printf("[EngineManager] 临时激活方案: %s (保存: %s)", schemaID, m.savedSchemaID)
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

	log.Printf("[EngineManager] 退出临时方案: %s, 恢复: %s", m.tempSchemaID, m.savedSchemaID)
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
func (m *Manager) loadSchemaEngineLocked(schemaID string) error {
	if m.schemaManager == nil {
		return fmt.Errorf("SchemaManager 未设置")
	}

	s := m.schemaManager.GetSchema(schemaID)
	if s == nil {
		return fmt.Errorf("方案 %q 不存在", schemaID)
	}

	bundle, err := schema.CreateEngineFromSchema(s, m.exeDir, m.dictManager)
	if err != nil {
		return fmt.Errorf("创建方案 %q 引擎失败: %w", schemaID, err)
	}

	// 将引擎包装为 Engine 接口
	switch eng := bundle.Engine.(type) {
	case *pinyin.Engine:
		m.engines[schemaID] = eng
	case *wubi.Engine:
		m.engines[schemaID] = eng
	default:
		return fmt.Errorf("未知引擎类型: %T", bundle.Engine)
	}

	return nil
}

// --- 查询方法（保持现有 API）---

// GetCurrentEngine 获取当前引擎
func (m *Manager) GetCurrentEngine() Engine {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.currentEngine
}

// GetCurrentType 获取当前方案 ID（兼容旧代码，返回 EngineType）
func (m *Manager) GetCurrentType() EngineType {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return EngineType(m.currentID)
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

// --- 转换方法 ---

// Convert 使用当前引擎转换输入
func (m *Manager) Convert(input string, maxCandidates int) ([]candidate.Candidate, error) {
	engine := m.GetCurrentEngine()
	if engine == nil {
		return nil, fmt.Errorf("未设置当前引擎")
	}
	return engine.Convert(input, maxCandidates)
}

// ConvertRaw 使用当前引擎转换输入（不应用过滤）
func (m *Manager) ConvertRaw(input string, maxCandidates int) ([]candidate.Candidate, error) {
	engine := m.GetCurrentEngine()
	if engine == nil {
		return nil, fmt.Errorf("未设置当前引擎")
	}

	if pinyinEngine, ok := engine.(*pinyin.Engine); ok {
		return pinyinEngine.ConvertRaw(input, maxCandidates)
	}
	if wubiEngine, ok := engine.(*wubi.Engine); ok {
		return wubiEngine.ConvertRaw(input, maxCandidates)
	}
	return engine.Convert(input, maxCandidates)
}

// ConvertEx 扩展转换
func (m *Manager) ConvertEx(input string, maxCandidates int) *ConvertResult {
	engine := m.GetCurrentEngine()
	if engine == nil {
		return &ConvertResult{}
	}

	if wubiEngine, ok := engine.(*wubi.Engine); ok {
		wubiResult := wubiEngine.ConvertEx(input, maxCandidates)
		return &ConvertResult{
			Candidates:   wubiResult.Candidates,
			ShouldCommit: wubiResult.ShouldCommit,
			CommitText:   wubiResult.CommitText,
			IsEmpty:      wubiResult.IsEmpty,
			ShouldClear:  wubiResult.ShouldClear,
			ToEnglish:    wubiResult.ToEnglish,
		}
	}

	if pinyinEngine, ok := engine.(*pinyin.Engine); ok {
		pinyinResult := pinyinEngine.ConvertEx(input, maxCandidates)
		result := &ConvertResult{
			Candidates:     pinyinResult.Candidates,
			IsEmpty:        pinyinResult.IsEmpty,
			PreeditDisplay: pinyinResult.PreeditDisplay,
		}
		if pinyinResult.Composition != nil {
			result.CompletedSyllables = pinyinResult.Composition.CompletedSyllables
			result.PartialSyllable = pinyinResult.Composition.PartialSyllable
			result.HasPartial = pinyinResult.Composition.HasPartial()
		}
		return result
	}

	candidates, err := engine.Convert(input, maxCandidates)
	if err != nil {
		log.Printf("[EngineManager] 转换错误: %v", err)
	}
	return &ConvertResult{
		Candidates: candidates,
		IsEmpty:    len(candidates) == 0,
	}
}

// Reset 重置当前引擎
func (m *Manager) Reset() {
	engine := m.GetCurrentEngine()
	if engine != nil {
		engine.Reset()
	}
}

// GetMaxCodeLength 获取当前引擎的最大码长
func (m *Manager) GetMaxCodeLength() int {
	engine := m.GetCurrentEngine()
	if engine == nil {
		return 0
	}
	if wubiEngine, ok := engine.(*wubi.Engine); ok {
		return wubiEngine.GetConfig().MaxCodeLength
	}
	return 100
}

// HandleTopCode 处理顶码
func (m *Manager) HandleTopCode(input string) (commitText string, newInput string, shouldCommit bool) {
	engine := m.GetCurrentEngine()
	if engine == nil {
		return "", input, false
	}
	if wubiEngine, ok := engine.(*wubi.Engine); ok {
		return wubiEngine.HandleTopCode(input)
	}
	return "", input, false
}

// InvalidateCommandCache 清除命令结果缓存
func (m *Manager) InvalidateCommandCache() {
	m.mu.RLock()
	dm := m.dictManager
	m.mu.RUnlock()
	if dm == nil {
		return
	}
	if phraseLayer := dm.GetPhraseLayer(); phraseLayer != nil {
		phraseLayer.InvalidateCache()
	}
}

// GetEngineInfo 获取当前引擎信息
func (m *Manager) GetEngineInfo() string {
	engine := m.GetCurrentEngine()
	if engine == nil {
		return "未加载引擎"
	}

	schemaID := m.GetCurrentSchemaID()

	if wubiEngine, ok := engine.(*wubi.Engine); ok {
		info := wubiEngine.GetCodeTableInfo()
		if info != nil {
			return fmt.Sprintf("%s: %s (%d词条)", schemaID, info.Name, wubiEngine.GetEntryCount())
		}
	}

	return schemaID
}

// --- 临时拼音支持（兼容旧代码）---

// EnsurePinyinLoaded 确保拼音引擎已加载（不切换当前引擎）
func (m *Manager) EnsurePinyinLoaded() error {
	m.mu.Lock()
	defer m.mu.Unlock()

	// 查找拼音方案的 ID
	pinyinID := m.findPinyinSchemaID()
	if _, ok := m.engines[pinyinID]; ok {
		return nil
	}

	log.Printf("[EngineManager] 临时拼音：加载拼音引擎...")
	return m.loadSchemaEngineLocked(pinyinID)
}

// ConvertWithPinyin 使用拼音引擎转换（用于临时拼音模式）
func (m *Manager) ConvertWithPinyin(input string, maxCandidates int) *ConvertResult {
	m.mu.RLock()
	pinyinID := m.findPinyinSchemaIDLocked()
	pinyinEngine, ok := m.engines[pinyinID]
	m.mu.RUnlock()

	if !ok {
		return &ConvertResult{IsEmpty: true}
	}

	pe, ok := pinyinEngine.(*pinyin.Engine)
	if !ok {
		return &ConvertResult{IsEmpty: true}
	}

	pinyinResult := pe.ConvertEx(input, maxCandidates)
	pe.AddWubiHintsForced(pinyinResult.Candidates)

	result := &ConvertResult{
		Candidates:     pinyinResult.Candidates,
		IsEmpty:        pinyinResult.IsEmpty,
		PreeditDisplay: pinyinResult.PreeditDisplay,
	}
	if pinyinResult.Composition != nil {
		result.CompletedSyllables = pinyinResult.Composition.CompletedSyllables
		result.PartialSyllable = pinyinResult.Composition.PartialSyllable
		result.HasPartial = pinyinResult.Composition.HasPartial()
	}
	return result
}

// OnCandidateSelected 选词回调
func (m *Manager) OnCandidateSelected(code, text string) {
	engine := m.GetCurrentEngine()
	if engine == nil {
		return
	}
	if pinyinEngine, ok := engine.(*pinyin.Engine); ok {
		pinyinEngine.OnCandidateSelected(code, text)
	}
}

// SaveUserFreqs 保存用户词频
func (m *Manager) SaveUserFreqs() {
	m.mu.RLock()
	defer m.mu.RUnlock()

	for _, eng := range m.engines {
		if pinyinEngine, ok := eng.(*pinyin.Engine); ok {
			userFreqPath := "dict/pinyin/user_freq.txt"
			if m.exeDir != "" {
				userFreqPath = m.exeDir + "/" + userFreqPath
			}
			schema.SavePinyinUserFreqs(pinyinEngine, userFreqPath)
		}
	}
}

// findPinyinSchemaID 查找拼音方案 ID（需要持有读锁或写锁）
func (m *Manager) findPinyinSchemaID() string {
	if m.schemaManager != nil {
		for _, s := range m.schemaManager.ListSchemas() {
			sch := m.schemaManager.GetSchema(s.ID)
			if sch != nil && sch.Engine.Type == schema.EngineTypePinyin {
				return s.ID
			}
		}
	}
	return "pinyin"
}

// findPinyinSchemaIDLocked 查找拼音方案 ID（调用方已持有读锁）
func (m *Manager) findPinyinSchemaIDLocked() string {
	return m.findPinyinSchemaID()
}

// --- 兼容旧代码的方法 ---

// RegisterEngine 注册引擎（兼容旧代码）
// Deprecated: 使用 SwitchSchema
func (m *Manager) RegisterEngine(engineType EngineType, engine Engine) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.engines[string(engineType)] = engine
}

// SetCurrentEngine 设置当前引擎（兼容旧代码）
// Deprecated: 使用 SwitchSchema
func (m *Manager) SetCurrentEngine(engineType EngineType) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	id := string(engineType)
	engine, ok := m.engines[id]
	if !ok {
		return fmt.Errorf("引擎未注册: %s", engineType)
	}

	m.currentID = id
	m.currentEngine = engine
	return nil
}

// ToggleEngine 在方案之间切换（兼容旧代码）
// Deprecated: 使用 ToggleSchema
func (m *Manager) ToggleEngine() (EngineType, error) {
	newID, err := m.ToggleSchema()
	return EngineType(newID), err
}

// SwitchEngine 切换引擎（兼容旧代码）
// Deprecated: 使用 SwitchSchema
func (m *Manager) SwitchEngine(targetType EngineType) error {
	return m.SwitchSchema(string(targetType))
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
