package engine

import (
	"fmt"
	"log"
	"sync"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/engine/mixed"
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
	"github.com/huanfeng/wind_input/internal/engine/wubi"
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
}

// NewManager 创建引擎管理器
func NewManager() *Manager {
	return &Manager{
		engines:      make(map[string]Engine),
		systemLayers: make(map[string]dict.DictLayer),
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
		// 重新注册缓存引擎的系统词库层
		m.reRegisterSystemLayer(schemaID)
		log.Printf("[EngineManager] 切换到已加载方案: %s", schemaID)
		return nil
	}

	// 需要创建引擎（factory 内部会注册系统词库层）
	if err := m.loadSchemaEngineLocked(schemaID); err != nil {
		return err
	}

	m.currentID = schemaID
	m.currentEngine = m.engines[schemaID]
	log.Printf("[EngineManager] 加载并切换方案: %s", schemaID)
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
		case *wubi.Engine:
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
		log.Printf("[EngineManager] 重新注册系统词库层: %s (方案: %s)", layer.Name(), schemaID)
	}
}

// --- 查询方法（保持现有 API）---

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
			return EngineType(s.Engine.Type)
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
			dm.SwitchSchema(schemaID, s.UserData.ShadowFile, s.UserData.UserDictFile)
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

	if mixedEngine, ok := engine.(*mixed.Engine); ok {
		mixedResult := mixedEngine.ConvertEx(input, maxCandidates)
		result := &ConvertResult{
			Candidates:   mixedResult.Candidates,
			ShouldCommit: mixedResult.ShouldCommit,
			CommitText:   mixedResult.CommitText,
			IsEmpty:      mixedResult.IsEmpty,
			ShouldClear:  mixedResult.ShouldClear,
			ToEnglish:    mixedResult.ToEnglish,
			NewInput:     mixedResult.NewInput,
		}
		// 拼音降级模式时填充预编辑区信息
		if mixedResult.IsPinyinFallback {
			result.PreeditDisplay = mixedResult.PreeditDisplay
			result.CompletedSyllables = mixedResult.CompletedSyllables
			result.PartialSyllable = mixedResult.PartialSyllable
			result.HasPartial = mixedResult.HasPartial
		}
		return result
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
	if mixedEngine, ok := engine.(*mixed.Engine); ok {
		return mixedEngine.GetMaxCodeLength()
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
	if mixedEngine, ok := engine.(*mixed.Engine); ok {
		return mixedEngine.HandleTopCode(input)
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

	if mixedEngine, ok := engine.(*mixed.Engine); ok {
		wubiEng := mixedEngine.GetWubiEngine()
		if wubiEng != nil {
			info := wubiEng.GetCodeTableInfo()
			if info != nil {
				return fmt.Sprintf("%s: %s+拼音混输 (%d词条)", schemaID, info.Name, wubiEng.GetEntryCount())
			}
		}
		return schemaID + ": 混输"
	}

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

// ActivateTempPinyin 激活临时拼音模式：交换系统词库层
// 临时移除五笔码表层 + 注册拼音词库层，避免五笔候选污染拼音查询结果。
// 调用方（coordinator）在进入临时拼音模式时调用。
func (m *Manager) ActivateTempPinyin() {
	m.mu.RLock()
	pinyinID := m.findPinyinSchemaID()
	m.mu.RUnlock()

	if m.dictManager == nil {
		return
	}
	compositeDict := m.dictManager.GetCompositeDict()
	if compositeDict == nil {
		return
	}

	// 1. 临时移除五笔码表层，避免五笔候选污染拼音查询结果
	//    直接操作 CompositeDict（不通过 DictManager.UnregisterSystemLayer），
	//    保留 DictManager.systemLayers 中的引用供后续恢复。
	if compositeDict.GetLayerByName("codetable-system") != nil {
		compositeDict.RemoveLayer("codetable-system")
		log.Printf("[EngineManager] 临时拼音：暂时移除五笔码表层")
	}

	// 2. 如果拼音词库层已注册（首次由 createPinyinEngine 注册），直接返回
	if compositeDict.GetLayerByName("pinyin-system") != nil {
		return
	}

	// 3. 重新注册拼音词库层（第二次及后续进入临时拼音时）
	m.mu.RLock()
	layer, ok := m.systemLayers[pinyinID]
	m.mu.RUnlock()

	if ok && layer != nil {
		m.dictManager.RegisterSystemLayer(layer.Name(), layer)
		log.Printf("[EngineManager] 临时拼音：注册拼音词库层")
	}
}

// DeactivateTempPinyin 退出临时拼音模式：恢复系统词库层
// 卸载拼音词库层 + 恢复五笔码表层。
func (m *Manager) DeactivateTempPinyin() {
	if m.dictManager == nil {
		return
	}
	compositeDict := m.dictManager.GetCompositeDict()
	if compositeDict == nil {
		return
	}

	// 1. 卸载拼音词库层
	if compositeDict.GetLayerByName("pinyin-system") != nil {
		m.dictManager.UnregisterSystemLayer("pinyin-system")
		log.Printf("[EngineManager] 临时拼音：卸载拼音词库层")
	}

	// 2. 恢复五笔码表层
	m.mu.RLock()
	currentID := m.currentID
	wubiLayer, ok := m.systemLayers[currentID]
	m.mu.RUnlock()

	if ok && wubiLayer != nil && compositeDict.GetLayerByName(wubiLayer.Name()) == nil {
		compositeDict.AddLayer(wubiLayer)
		log.Printf("[EngineManager] 临时拼音：恢复五笔码表层")
	}
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

// OnCandidateSelected 选词回调（拼音 + 五笔 + 混输统一路由）
// source 为可选参数，混输模式下传入候选来源（"wubi"/"pinyin"）以路由到正确的子引擎
func (m *Manager) OnCandidateSelected(code, text string, source ...string) {
	engine := m.GetCurrentEngine()
	if engine == nil {
		return
	}
	// 混输引擎：按来源路由到对应子引擎
	if mixedEngine, ok := engine.(*mixed.Engine); ok {
		src := candidate.SourceNone
		if len(source) > 0 {
			src = candidate.CandidateSource(source[0])
		}
		mixedEngine.OnCandidateSelected(code, text, src)
		return
	}
	if pinyinEngine, ok := engine.(*pinyin.Engine); ok {
		pinyinEngine.OnCandidateSelected(code, text)
		return
	}
	if wubiEngine, ok := engine.(*wubi.Engine); ok {
		wubiEngine.OnCandidateSelected(code, text)
		return
	}
}

// SaveUserFreqs 保存用户词频
func (m *Manager) SaveUserFreqs() {
	m.mu.RLock()
	defer m.mu.RUnlock()

	for schemaID, eng := range m.engines {
		// 直接的拼音引擎
		if pinyinEngine, ok := eng.(*pinyin.Engine); ok {
			m.savePinyinUserFreqsLocked(schemaID, pinyinEngine)
			continue
		}
		// 混输引擎：保存内部拼音引擎的用户词频
		if mixedEngine, ok := eng.(*mixed.Engine); ok {
			if pinyinEngine := mixedEngine.GetPinyinEngine(); pinyinEngine != nil {
				m.savePinyinUserFreqsLocked(schemaID, pinyinEngine)
			}
			continue
		}
	}
}

// savePinyinUserFreqsLocked 保存拼音用户词频（调用方已持有读锁）
func (m *Manager) savePinyinUserFreqsLocked(schemaID string, pinyinEngine *pinyin.Engine) {
	cfg := pinyinEngine.GetConfig()
	if cfg == nil || !cfg.EnableUserFreq {
		return
	}
	userFreqFile := ""
	if m.schemaManager != nil {
		if s := m.schemaManager.GetSchema(schemaID); s != nil {
			userFreqFile = s.UserData.UserFreqFile
		}
	}
	if userFreqFile == "" {
		return
	}
	userFreqPath := userFreqFile
	if m.exeDir != "" {
		userFreqPath = m.exeDir + "/" + userFreqFile
	}
	schema.SavePinyinUserFreqs(pinyinEngine, userFreqPath)
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
	newID, err := m.ToggleSchema(nil)
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
