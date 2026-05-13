package engine

import (
	"github.com/huanfeng/wind_input/internal/engine/mixed"
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
	"github.com/huanfeng/wind_input/internal/schema"
)

// --- 临时拼音支持 ---

// EnsurePinyinLoaded 确保拼音引擎已加载（不切换当前引擎）
// 混输模式下无需额外加载：直接复用混输引擎内置的拼音子引擎。
//
// 锁策略：构建（重 IO）发生在 m.mu 之外，按键路径不被阻塞。
func (m *Manager) EnsurePinyinLoaded() error {
	m.mu.RLock()
	// 混输模式：内置拼音子引擎已就绪，无需创建独立引擎
	if _, ok := m.currentEngine.(*mixed.Engine); ok {
		m.mu.RUnlock()
		return nil
	}
	pinyinID := m.findPinyinSchemaID()
	if _, ok := m.engines[pinyinID]; ok {
		m.mu.RUnlock()
		return nil
	}
	m.mu.RUnlock()

	m.logger.Info("临时拼音：加载拼音引擎")
	// 跳过反查码表加载：临时拼音模式由 Manager.GetReverseIndex() 动态提供当前主方案的反向索引
	// 使用独立 CompositeDict：避免拼音词库层泄漏到混输引擎的主 CompositeDict
	return m.ensureEngineBuilt(pinyinID, schema.EngineCreateOptions{
		SkipReverseLookup:  true,
		UseIndependentDict: true,
	})
}

// ActivateTempPinyin 激活临时拼音模式：交换系统词库层
// 临时移除码表层 + 注册拼音词库层，避免码表候选污染拼音查询结果。
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

	// 1. 临时移除码表层，避免码表候选污染拼音查询结果
	//    直接操作 CompositeDict（不通过 DictManager.UnregisterSystemLayer），
	//    保留 DictManager.systemLayers 中的引用供后续恢复。
	if compositeDict.GetLayerByName("codetable-system") != nil {
		compositeDict.RemoveLayer("codetable-system")
		m.logger.Info("临时拼音：暂时移除码表层")
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
		m.logger.Info("临时拼音：注册拼音词库层")
	}
}

// DeactivateTempPinyin 退出临时拼音模式：恢复系统词库层
// 卸载拼音词库层 + 恢复码表层。
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
		m.logger.Info("临时拼音：卸载拼音词库层")
	}

	// 2. 恢复码表层
	m.mu.RLock()
	currentID := m.currentID
	codetableLayer, ok := m.systemLayers[currentID]
	m.mu.RUnlock()

	if ok && codetableLayer != nil && compositeDict.GetLayerByName(codetableLayer.Name()) == nil {
		compositeDict.AddLayer(codetableLayer)
		m.logger.Info("临时拼音：恢复码表层")
	}
}

// ConvertWithPinyin 使用拼音引擎转换（用于临时拼音模式）
// 混输模式下直接复用内置拼音子引擎，避免创建独立引擎带来的词库污染和状态不一致。
func (m *Manager) ConvertWithPinyin(input string, maxCandidates int) *ConvertResult {
	m.mu.RLock()
	currentEngine := m.currentEngine
	m.mu.RUnlock()

	// 混输模式：直接使用内置拼音子引擎
	var pe *pinyin.Engine
	if me, ok := currentEngine.(*mixed.Engine); ok {
		pe = me.GetPinyinEngine()
	}

	// 码表模式：使用独立加载的拼音引擎
	if pe == nil {
		m.mu.RLock()
		pinyinID := m.findPinyinSchemaIDLocked()
		pinyinEngine, ok := m.engines[pinyinID]
		m.mu.RUnlock()

		if !ok {
			return &ConvertResult{IsEmpty: true}
		}
		pe, ok = pinyinEngine.(*pinyin.Engine)
		if !ok {
			return &ConvertResult{IsEmpty: true}
		}
	}

	pinyinResult := pe.ConvertEx(input, maxCandidates)

	// 使用主码表方案的反向索引添加编码提示（而非拼音引擎自带的反查码表），
	// 这样切换不同主码表（五笔/郑码等）时，临时拼音始终显示当前主编码。
	m.ApplyCodeHintsToCandidates(pinyinResult.Candidates)

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

// IsTempPinyinEnabled 检查当前码表方案是否开启了临时拼音
func (m *Manager) IsTempPinyinEnabled() bool {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.schemaManager == nil || m.currentID == "" {
		return false
	}
	currentSchema := m.schemaManager.GetSchema(m.currentID)
	if currentSchema == nil || currentSchema.Engine.CodeTable == nil {
		return false
	}
	tp := currentSchema.Engine.CodeTable.TempPinyin
	if tp == nil {
		return true // 默认开启（向后兼容）
	}
	return tp.Enabled
}

// IsZKeyRepeatEnabled 检查当前方案是否开启了 Z 键重复上屏功能
func (m *Manager) IsZKeyRepeatEnabled() bool {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.schemaManager == nil || m.currentID == "" {
		return false
	}
	s := m.schemaManager.GetSchema(m.currentID)
	if s == nil {
		return false
	}
	// 码表方案：从 CodeTableSpec 读取
	if s.Engine.CodeTable != nil && s.Engine.CodeTable.ZKeyRepeat != nil {
		return *s.Engine.CodeTable.ZKeyRepeat
	}
	// 混输方案：从 MixedSpec 读取
	if s.Engine.Mixed != nil && s.Engine.Mixed.ZKeyRepeat != nil {
		return *s.Engine.Mixed.ZKeyRepeat
	}
	return false
}

// HasCommandPrefix 检查是否存在指定前缀的快捷短语（用于 zz 快捷短语与临时拼音的优先判断）
func (m *Manager) HasCommandPrefix(prefix string) bool {
	m.mu.RLock()
	dm := m.dictManager
	m.mu.RUnlock()
	if dm == nil {
		return false
	}
	phraseLayer := dm.GetPhraseLayer()
	if phraseLayer == nil {
		return false
	}
	return len(phraseLayer.SearchCommand(prefix, 1)) > 0
}

// HasPrefix 检查码表/用户词/快捷短语中是否存在以 prefix 开头的任何条目。
// 用于 z 键混合模式下的渐进决策：当 inputBuffer 仍能扩展出码表或短语候选时
// 继续走正常输入流程；否则把首 z 视作临时拼音触发键回退。
func (m *Manager) HasPrefix(prefix string) bool {
	if prefix == "" {
		return false
	}
	m.mu.RLock()
	dm := m.dictManager
	m.mu.RUnlock()
	if dm == nil {
		return false
	}
	// 快捷短语前缀（含字符组导航前缀，与 HasCommandPrefix 同源）
	if phraseLayer := dm.GetPhraseLayer(); phraseLayer != nil {
		if len(phraseLayer.SearchCommand(prefix, 1)) > 0 {
			return true
		}
	}
	// 码表/用户词/静态短语前缀（聚合所有词库层）
	if composite := dm.GetCompositeDict(); composite != nil {
		if len(composite.SearchPrefix(prefix, 1)) > 0 {
			return true
		}
	}
	return false
}

// findPinyinSchemaID 查找拼音方案 ID（需要持有读锁或写锁）
//
// 优先级：
//  1. 全局 primaryPinyinID（来自 config.Schema.PrimaryPinyin）
//  2. 当前码表方案的 temp_pinyin.schema 字段（已废弃但保留兼容读取）
//  3. SchemaManager 中第一个 pinyin 类方案
//  4. 兜底常量 "pinyin"
func (m *Manager) findPinyinSchemaID() string {
	if m.primaryPinyinID != "" {
		return m.primaryPinyinID
	}
	if m.schemaManager != nil && m.currentID != "" {
		currentSchema := m.schemaManager.GetSchema(m.currentID)
		if currentSchema != nil && currentSchema.Engine.CodeTable != nil &&
			currentSchema.Engine.CodeTable.TempPinyin != nil &&
			currentSchema.Engine.CodeTable.TempPinyin.Schema != "" {
			return currentSchema.Engine.CodeTable.TempPinyin.Schema
		}
	}
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

// shuangpinLayoutDisplayNames 双拼方案布局的中文简称
var shuangpinLayoutDisplayNames = map[string]string{
	"xiaohe":  "小鹤",
	"ziranma": "自然码",
	"mspy":    "微软",
	"sogou":   "搜狗",
	"abc":     "智能ABC",
	"ziguang": "紫光",
}

// GetTempPinyinModeLabel 返回临时拼音模式的显示标签。
// 全拼方案返回"临时全拼"，双拼方案返回"临时双拼（小鹤）"等。
func (m *Manager) GetTempPinyinModeLabel() string {
	m.mu.RLock()
	pinyinID := m.findPinyinSchemaIDLocked()
	m.mu.RUnlock()

	if m.schemaManager == nil {
		return "临时拼音"
	}
	s := m.schemaManager.GetSchema(pinyinID)
	if s == nil || s.Engine.Pinyin == nil {
		return "临时拼音"
	}
	if s.Engine.Pinyin.Scheme == schema.PinyinSchemeShuangpin {
		if s.Engine.Pinyin.Shuangpin != nil {
			if name, ok := shuangpinLayoutDisplayNames[s.Engine.Pinyin.Shuangpin.Layout]; ok {
				return "临时双拼（" + name + "）"
			}
		}
		return "临时双拼"
	}
	return "临时全拼"
}
