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
			result.FullPinyinInput = mixedResult.FullPinyinInput
		}
		if mixedResult.Timing != nil {
			c, ex, pf, w, sd, sh, ft := mixedResult.Timing.TimingFields()
			result.Timing = &EngineTiming{Convert: c, Exact: ex, Prefix: pf, Weight: w, Sort: sd, Shadow: sh, Filter: ft}
		}
		return result
	}

	if codetableEngine, ok := engine.(*codetable.Engine); ok {
		codetableResult := codetableEngine.ConvertEx(input, maxCandidates)
		out := &ConvertResult{
			Candidates:   codetableResult.Candidates,
			ShouldCommit: codetableResult.ShouldCommit,
			CommitText:   codetableResult.CommitText,
			IsEmpty:      codetableResult.IsEmpty,
			ShouldClear:  codetableResult.ShouldClear,
			ToEnglish:    codetableResult.ToEnglish,
		}
		if codetableResult.Timing != nil {
			c, ex, pf, w, sd, sh, ft := codetableResult.Timing.TimingFields()
			out.Timing = &EngineTiming{Convert: c, Exact: ex, Prefix: pf, Weight: w, Sort: sd, Shadow: sh, Filter: ft}
		}
		return out
	}

	if pinyinEngine, ok := engine.(*pinyin.Engine); ok {
		pinyinResult := pinyinEngine.ConvertEx(input, maxCandidates)
		// 反查/编码提示：从主码表方案的反向索引派生（不再由拼音引擎自带 codeHintTable）
		if cfg := pinyinEngine.GetConfig(); cfg != nil && cfg.ShowCodeHint {
			m.ApplyCodeHintsToCandidates(pinyinResult.Candidates)
		}
		result := &ConvertResult{
			Candidates:      pinyinResult.Candidates,
			IsEmpty:         pinyinResult.IsEmpty,
			PreeditDisplay:  pinyinResult.PreeditDisplay,
			FullPinyinInput: pinyinResult.FullPinyinInput,
		}
		if pinyinResult.Composition != nil {
			result.CompletedSyllables = pinyinResult.Composition.CompletedSyllables
			result.PartialSyllable = pinyinResult.Composition.PartialSyllable
			result.HasPartial = pinyinResult.Composition.HasPartial()
		}
		if pinyinResult.Timing != nil {
			c, ex, pf, w, sd, sh, ft := pinyinResult.Timing.TimingFields()
			result.Timing = &EngineTiming{Convert: c, Exact: ex, Prefix: pf, Weight: w, Sort: sd, Shadow: sh, Filter: ft}
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

// OnPhraseTerminated 短语终止信号（标点、回车、焦点切换等）
// 通知造词策略当前的连续单字序列已结束，触发自动组词
func (m *Manager) OnPhraseTerminated() {
	engine := m.GetCurrentEngine()
	if engine == nil {
		return
	}
	switch e := engine.(type) {
	case *codetable.Engine:
		e.OnPhraseTerminated()
	case *mixed.Engine:
		if ce := e.GetCodetableEngine(); ce != nil {
			ce.OnPhraseTerminated()
		}
	}
	// 拼音引擎不需要终止信号（使用 AutoLearning 选词即学）
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
	if m.dictManager == nil || m.dictManager.GetStore() == nil {
		return
	}
	// FreqHandler 或 LearningStrategy 已注入时才有用户词频需要保存
	if pinyinEngine.GetUnigram() == nil {
		return
	}
	schema.SavePinyinUserFreqs(pinyinEngine, m.dictManager.GetStore(), schemaID)
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

// GetReverseIndex 获取主码表的反向索引（字 → 编码列表）
//
// 数据源优先级：
//  1. 主码表方案（primaryCodetableID）已加载的 codetable / mixed 引擎（与 currentID 解耦，
//     从而拼音方案下也能稳定取到反向索引）；
//  2. 当前引擎（兼容未配置主方案的旧路径）。
//
// 主码表未加载时返回 nil，并触发后台异步加载（不阻塞按键路径）。
// 缓存键为 primaryCodetableID（或当前 ID 兜底），主方案切换时由 SetPrimarySchemas 清空缓存。
func (m *Manager) GetReverseIndex() map[string][]string {
	m.mu.Lock()
	primaryID := m.primaryCodetableID
	if primaryID == "" {
		primaryID = m.currentID
	}
	if m.cachedReverseIndex != nil && m.cachedReverseSchemaID == primaryID {
		idx := m.cachedReverseIndex
		m.mu.Unlock()
		return idx
	}
	// 优先用主码表方案的引擎
	var ct *dict.CodeTable
	if eng, ok := m.engines[primaryID]; ok {
		ct = extractCodeTable(eng)
	}
	// 兜底：当前引擎
	if ct == nil && m.currentEngine != nil {
		ct = extractCodeTable(m.currentEngine)
	}
	if ct != nil {
		idx := ct.BuildReverseIndex()
		m.cachedReverseIndex = idx
		m.cachedReverseSchemaID = primaryID
		m.mu.Unlock()
		return idx
	}

	// 主码表未加载：返回 nil（编码提示降级为不显示）。
	// 不在此处触发异步加载，避免在用户当前的拼音 CompositeDict 上注册 codetable 层污染候选；
	// 主码表会在用户切换到该方案时自然加载，加载后下次查询即可命中缓存。
	m.mu.Unlock()
	return nil
}

// extractCodeTable 从引擎中提取 CodeTable（codetable / mixed 引擎）
func extractCodeTable(eng Engine) *dict.CodeTable {
	if codetableEngine, ok := eng.(*codetable.Engine); ok {
		return codetableEngine.GetCodeTable()
	}
	if mixedEngine, ok := eng.(*mixed.Engine); ok {
		if ce := mixedEngine.GetCodetableEngine(); ce != nil {
			return ce.GetCodeTable()
		}
	}
	return nil
}

// ApplyCodeHintsToCandidates 用主码表反向索引为候选填充 Comment（编码提示）。
// 已有 Comment 时不覆盖；候选 Source 不限制（独立拼音引擎下 Source 字段未设置）。
//
// IsPinyinSchema 判断当前活跃方案是否为拼音类型（全拼或双拼）。
func (m *Manager) IsPinyinSchema() bool {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.schemaManager == nil {
		return false
	}
	s := m.schemaManager.GetSchema(m.currentID)
	return s != nil && s.Engine.Type == schema.EngineTypePinyin
}

// IsSchemaTypePinyin 判断指定 schemaID 是否为拼音类型（全拼或双拼）。
func (m *Manager) IsSchemaTypePinyin(schemaID string) bool {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.schemaManager == nil {
		return false
	}
	s := m.schemaManager.GetSchema(schemaID)
	return s != nil && s.Engine.Type == schema.EngineTypePinyin
}

// GetEncoderRulesForSchema 获取指定 schemaID 的编码规则（用于导入时自动计算编码）。
func (m *Manager) GetEncoderRulesForSchema(schemaID string) []schema.EncoderRule {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.schemaManager == nil {
		return nil
	}
	s := m.schemaManager.GetSchema(schemaID)
	if s == nil {
		return nil
	}
	encoder := m.resolveEncoder(s)
	if encoder == nil {
		return nil
	}
	return encoder.Rules
}

// DataSchemaID 返回给定方案 ID 的数据存储 ID（即 bbolt bucket 键）。
// 拼音方案统一返回 "pinyin"，使全拼与双拼共享同一用户词库。
func (m *Manager) DataSchemaID(schemaID string) string {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.schemaManager == nil {
		return schemaID
	}
	s := m.schemaManager.GetSchema(schemaID)
	if s == nil {
		return schemaID
	}
	return s.DataSchemaID()
}

// GeneratePinyinCode 为词语生成全拼编码（如"你好" → "nihao"）。
// 在所有已加载的引擎中寻找第一个拼音引擎来生成编码。
// 若无拼音引擎或词语含未知字符，返回空串。
func (m *Manager) GeneratePinyinCode(word string) string {
	m.mu.RLock()
	defer m.mu.RUnlock()
	for _, eng := range m.engines {
		if pe, ok := eng.(*pinyin.Engine); ok {
			return pe.GenerateWordPinyin(word)
		}
		if me, ok := eng.(*mixed.Engine); ok {
			if pe := me.GetPinyinEngine(); pe != nil {
				return pe.GenerateWordPinyin(word)
			}
		}
	}
	return ""
}

// 混输引擎已有自己的 addCodeHintsFromCodetable（按 Source 区分拼音/码表候选），无需调用本函数；
// 本函数面向独立拼音引擎和临时拼音模式。
func (m *Manager) ApplyCodeHintsToCandidates(cands []candidate.Candidate) {
	if len(cands) == 0 {
		return
	}
	idx := m.GetReverseIndex()
	if len(idx) == 0 {
		return
	}
	for i := range cands {
		if cands[i].Comment != "" {
			continue
		}
		codes := idx[cands[i].Text]
		if len(codes) > 0 {
			cands[i].Comment = codes[0]
		}
	}
}
