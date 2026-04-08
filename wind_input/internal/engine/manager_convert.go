package engine

import (
	"fmt"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/engine/codetable"
	"github.com/huanfeng/wind_input/internal/engine/mixed"
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
	"github.com/huanfeng/wind_input/internal/schema"
)

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
	if codetableEngine, ok := engine.(*codetable.Engine); ok {
		return codetableEngine.ConvertRaw(input, maxCandidates)
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

	if codetableEngine, ok := engine.(*codetable.Engine); ok {
		codetableResult := codetableEngine.ConvertEx(input, maxCandidates)
		return &ConvertResult{
			Candidates:   codetableResult.Candidates,
			ShouldCommit: codetableResult.ShouldCommit,
			CommitText:   codetableResult.CommitText,
			IsEmpty:      codetableResult.IsEmpty,
			ShouldClear:  codetableResult.ShouldClear,
			ToEnglish:    codetableResult.ToEnglish,
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
		m.logger.Warn("转换错误", "error", err)
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
	if codetableEngine, ok := engine.(*codetable.Engine); ok {
		return codetableEngine.GetConfig().MaxCodeLength
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
	if codetableEngine, ok := engine.(*codetable.Engine); ok {
		return codetableEngine.HandleTopCode(input)
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
		codetableEng := mixedEngine.GetCodetableEngine()
		if codetableEng != nil {
			info := codetableEng.GetCodeTableInfo()
			if info != nil {
				return fmt.Sprintf("%s: %s+拼音混输 (%d词条)", schemaID, info.Name, codetableEng.GetEntryCount())
			}
		}
		return schemaID + ": 混输"
	}

	if codetableEngine, ok := engine.(*codetable.Engine); ok {
		info := codetableEngine.GetCodeTableInfo()
		if info != nil {
			return fmt.Sprintf("%s: %s (%d词条)", schemaID, info.Name, codetableEngine.GetEntryCount())
		}
	}

	return schemaID
}

// OnCandidateSelected 选词回调（拼音 + 码表 + 混输统一路由）
// source 为可选参数，混输模式下传入候选来源（"codetable"/"pinyin"）以路由到正确的子引擎
func (m *Manager) OnCandidateSelected(code, text string, source ...candidate.CandidateSource) {
	engine := m.GetCurrentEngine()
	if engine == nil {
		return
	}
	// 混输引擎：按来源路由到对应子引擎
	if mixedEngine, ok := engine.(*mixed.Engine); ok {
		src := candidate.SourceNone
		if len(source) > 0 {
			src = source[0]
		}
		mixedEngine.OnCandidateSelected(code, text, src)
		return
	}
	if pinyinEngine, ok := engine.(*pinyin.Engine); ok {
		pinyinEngine.OnCandidateSelected(code, text)
		return
	}
	if codetableEngine, ok := engine.(*codetable.Engine); ok {
		codetableEngine.OnCandidateSelected(code, text)
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

// GetEncoderRules 获取当前方案的编码规则（用于加词时自动计算编码）
func (m *Manager) GetEncoderRules() []schema.EncoderRule {
	m.mu.RLock()
	defer m.mu.RUnlock()

	if m.schemaManager == nil {
		return nil
	}

	s := m.schemaManager.GetSchema(m.currentID)
	if s == nil {
		return nil
	}

	encoder := m.resolveEncoder(s)
	if encoder == nil {
		return nil
	}
	return encoder.Rules
}

// GetEncoderMaxWordLength 获取当前方案的最大造词长度（0 表示无限制）
func (m *Manager) GetEncoderMaxWordLength() int {
	m.mu.RLock()
	defer m.mu.RUnlock()

	if m.schemaManager == nil {
		return 0
	}

	s := m.schemaManager.GetSchema(m.currentID)
	if s == nil {
		return 0
	}

	encoder := m.resolveEncoder(s)
	if encoder == nil {
		return 0
	}
	return encoder.MaxWordLength
}

// resolveEncoder 解析编码规则：混输方案自身没有定义时从主方案继承
func (m *Manager) resolveEncoder(s *schema.Schema) *schema.EncoderSpec {
	if s.Encoder != nil {
		return s.Encoder
	}
	// 混输方案：从 primary_schema 继承
	if s.Engine.Mixed != nil && s.Engine.Mixed.PrimarySchema != "" && m.schemaManager != nil {
		if primary := m.schemaManager.GetSchema(s.Engine.Mixed.PrimarySchema); primary != nil {
			return primary.Encoder
		}
	}
	return nil
}

// GetReverseIndex 获取当前码表的反向索引（字 → 编码列表）
// 首次调用时构建并缓存，切换方案后自动重建
func (m *Manager) GetReverseIndex() map[string][]string {
	m.mu.Lock()
	defer m.mu.Unlock()

	// 缓存命中
	if m.cachedReverseIndex != nil && m.cachedReverseSchemaID == m.currentID {
		return m.cachedReverseIndex
	}

	if m.dictManager == nil {
		return nil
	}

	composite := m.dictManager.GetCompositeDict()
	if composite == nil {
		return nil
	}

	// 从系统层中找到 CodeTableLayer
	layers := composite.GetLayersByType(dict.LayerTypeSystem)
	for _, layer := range layers {
		if ctl, ok := layer.(*dict.CodeTableLayer); ok {
			ct := ctl.GetCodeTable()
			if ct != nil {
				m.cachedReverseIndex = ct.BuildReverseIndex()
				m.cachedReverseSchemaID = m.currentID
				return m.cachedReverseIndex
			}
		}
	}

	// 备选：直接通过层名查找 codetable-system
	if ctLayer := composite.GetLayerByName("codetable-system"); ctLayer != nil {
		if ctl, ok := ctLayer.(*dict.CodeTableLayer); ok {
			ct := ctl.GetCodeTable()
			if ct != nil {
				m.cachedReverseIndex = ct.BuildReverseIndex()
				m.cachedReverseSchemaID = m.currentID
				return m.cachedReverseIndex
			}
		}
	}

	// 回退：从 systemLayers 缓存中查找（临时拼音模式下码表层已从 CompositeDict 移除，
	// 但 systemLayers 中仍保留引用）
	if layer, ok := m.systemLayers[m.currentID]; ok && layer != nil {
		if ctl, ok := layer.(*dict.CodeTableLayer); ok {
			ct := ctl.GetCodeTable()
			if ct != nil {
				m.cachedReverseIndex = ct.BuildReverseIndex()
				m.cachedReverseSchemaID = m.currentID
				return m.cachedReverseIndex
			}
		}
	}

	// 最终回退：直接从当前引擎获取码表构建反向索引
	// 当 CompositeDict 和 systemLayers 缓存都找不到 CodeTableLayer 时，
	// 直接从当前引擎（codetable 或 mixed）获取 CodeTable
	if engine := m.currentEngine; engine != nil {
		var ct *dict.CodeTable
		if codetableEngine, ok := engine.(*codetable.Engine); ok {
			ct = codetableEngine.GetCodeTable()
		} else if mixedEngine, ok := engine.(*mixed.Engine); ok {
			if codetableEngine := mixedEngine.GetCodetableEngine(); codetableEngine != nil {
				ct = codetableEngine.GetCodeTable()
			}
		}
		if ct != nil {
			m.cachedReverseIndex = ct.BuildReverseIndex()
			m.cachedReverseSchemaID = m.currentID
			return m.cachedReverseIndex
		}
	}

	return nil
}
