package engine

import (
	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/engine/codetable"
	"github.com/huanfeng/wind_input/internal/engine/mixed"
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
	"github.com/huanfeng/wind_input/internal/engine/pinyin/shuangpin"
	"github.com/huanfeng/wind_input/internal/schema"
	"github.com/huanfeng/wind_input/pkg/config"
)

// UpdateFilterMode 更新所有引擎的过滤模式
func (m *Manager) UpdateFilterMode(mode string) {
	m.mu.Lock()
	defer m.mu.Unlock()

	for _, eng := range m.engines {
		switch e := eng.(type) {
		case *pinyin.Engine:
			if cfg := e.GetConfig(); cfg != nil {
				cfg.FilterMode = mode
			}
		case *codetable.Engine:
			if cfg := e.GetConfig(); cfg != nil {
				cfg.FilterMode = mode
			}
		case *mixed.Engine:
			if we := e.GetCodetableEngine(); we != nil {
				if cfg := we.GetConfig(); cfg != nil {
					cfg.FilterMode = mode
				}
			}
			if pe := e.GetPinyinEngine(); pe != nil {
				if cfg := pe.GetConfig(); cfg != nil {
					cfg.FilterMode = mode
				}
			}
		}
	}

	m.logger.Info("更新过滤模式", "mode", mode)
}

// UpdateCodetableOptions 更新码表引擎的选项（热更新）
func (m *Manager) UpdateCodetableOptions(spec *schema.CodeTableSpec) {
	if spec == nil {
		return
	}
	m.mu.Lock()
	defer m.mu.Unlock()

	for _, eng := range m.engines {
		// 直接的码表引擎
		if codetableEngine, ok := eng.(*codetable.Engine); ok {
			updateCodetableConfig(codetableEngine, spec)
		}
		// 混输引擎的码表子引擎
		if mixedEngine, ok := eng.(*mixed.Engine); ok {
			if we := mixedEngine.GetCodetableEngine(); we != nil {
				updateCodetableConfig(we, spec)
			}
		}
	}

	if m.dictManager != nil && spec.CandidateSortMode != "" {
		m.dictManager.SetSortMode(candidate.CandidateSortMode(spec.CandidateSortMode))
	}

	m.logger.Info("更新码表选项",
		"autoCommitAt4", spec.AutoCommitUnique,
		"clearOnEmptyAt4", spec.ClearOnEmptyMax,
		"topCodeCommit", spec.TopCodeCommit,
		"punctCommit", spec.PunctCommit,
		"showCodeHint", spec.ShowCodeHint,
		"singleCodeInput", spec.SingleCodeInput,
		"singleCodeComplete", spec.SingleCodeComplete,
		"candidateSortMode", spec.CandidateSortMode,
		"prefixMode", spec.PrefixMode,
		"weightMode", spec.WeightMode,
		"loadMode", spec.LoadMode,
		"charsetPreference", spec.CharsetPreference,
		"shortCodeFirst", spec.ShortCodeFirst != nil && *spec.ShortCodeFirst,
		"dedupCandidates", spec.DedupCandidates == nil || *spec.DedupCandidates)
}

// updateCodetableConfig 更新码表引擎配置（内部辅助函数）
func updateCodetableConfig(codetableEngine *codetable.Engine, spec *schema.CodeTableSpec) {
	cfg := codetableEngine.GetConfig()
	if cfg == nil {
		return
	}
	cfg.AutoCommitAt4 = spec.AutoCommitUnique
	cfg.ClearOnEmptyAt4 = spec.ClearOnEmptyMax
	cfg.TopCodeCommit = spec.TopCodeCommit
	cfg.PunctCommit = spec.PunctCommit
	cfg.ShowCodeHint = spec.ShowCodeHint
	cfg.SingleCodeInput = spec.SingleCodeInput
	cfg.SingleCodeComplete = spec.SingleCodeComplete
	if spec.CandidateSortMode != "" {
		cfg.CandidateSortMode = spec.CandidateSortMode
	}
	// 新增字段：高级选项的运行时热更新
	if spec.PrefixMode != "" {
		cfg.PrefixMode = spec.PrefixMode
	}
	if spec.WeightMode != "" {
		cfg.WeightMode = spec.WeightMode
	}
	if spec.LoadMode != "" {
		cfg.LoadMode = spec.LoadMode
	}
	if spec.CharsetPreference != "" {
		cfg.CharsetPreference = spec.CharsetPreference
	}
	if spec.ShortCodeFirst != nil {
		cfg.ShortCodeFirst = *spec.ShortCodeFirst
	}
	// DedupCandidates 默认 true：未设置或显式 true 都启用
	cfg.DedupCandidates = spec.DedupCandidates == nil || *spec.DedupCandidates
}

// updatePinyinConfig 更新拼音引擎配置（内部辅助函数）
func updatePinyinConfig(pinyinEngine *pinyin.Engine, pinyinCfg *config.PinyinConfig) {
	showCodeHint := pinyinCfg.ShowCodeHint
	if cfg := pinyinEngine.GetConfig(); cfg != nil {
		oldShowCodeHint := cfg.ShowCodeHint
		cfg.ShowCodeHint = showCodeHint

		if pinyinCfg.Fuzzy.Enabled {
			pinyinEngine.SetFuzzyConfig(&pinyin.FuzzyConfig{
				ZhZ:     pinyinCfg.Fuzzy.ZhZ,
				ChC:     pinyinCfg.Fuzzy.ChC,
				ShS:     pinyinCfg.Fuzzy.ShS,
				NL:      pinyinCfg.Fuzzy.NL,
				FH:      pinyinCfg.Fuzzy.FH,
				RL:      pinyinCfg.Fuzzy.RL,
				AnAng:   pinyinCfg.Fuzzy.AnAng,
				EnEng:   pinyinCfg.Fuzzy.EnEng,
				InIng:   pinyinCfg.Fuzzy.InIng,
				IanIang: pinyinCfg.Fuzzy.IanIang,
				UanUang: pinyinCfg.Fuzzy.UanUang,
			})
		} else {
			pinyinEngine.SetFuzzyConfig(nil)
		}

		if oldShowCodeHint && !showCodeHint {
			pinyinEngine.ReleaseCodeHint()
		}
	}
}

// UpdatePinyinOptions 更新拼音引擎的选项（热更新）
func (m *Manager) UpdatePinyinOptions(pinyinCfg *config.PinyinConfig) {
	m.mu.Lock()
	defer m.mu.Unlock()

	if pinyinCfg == nil {
		return
	}

	for _, eng := range m.engines {
		// 直接的拼音引擎
		// 编码提示数据已迁移到 Manager.ApplyCodeHintsToCandidates（数据来自主码表方案的反向索引），
		// 此处仅更新引擎配置；ShowCodeHint 开关本身不再触发反查码表加载。
		if pinyinEngine, ok := eng.(*pinyin.Engine); ok {
			updatePinyinConfig(pinyinEngine, pinyinCfg)
		}
		// 混输引擎的拼音子引擎（仅更新配置，反查由 mixed.Engine.addCodeHintsFromCodetable 处理）
		if mixedEngine, ok := eng.(*mixed.Engine); ok {
			if pe := mixedEngine.GetPinyinEngine(); pe != nil {
				updatePinyinConfig(pe, pinyinCfg)
			}
		}
	}

	m.logger.Info("更新拼音选项", "showCodeHint", pinyinCfg.ShowCodeHint, "fuzzyEnabled", pinyinCfg.Fuzzy.Enabled)
}

// UpdateShuangpinLayout 热更新双拼方案布局
// layoutID: 方案 ID（如 "xiaohe", "ziranma", "mspy" 等），空字符串表示切回全拼
func (m *Manager) UpdateShuangpinLayout(layoutID string) {
	m.mu.Lock()
	defer m.mu.Unlock()

	for _, eng := range m.engines {
		var pe *pinyin.Engine
		switch e := eng.(type) {
		case *pinyin.Engine:
			pe = e
		case *mixed.Engine:
			pe = e.GetPinyinEngine()
		}
		if pe == nil {
			continue
		}
		if layoutID == "" {
			// 切回全拼
			pe.SetShuangpinConverter(nil)
		} else {
			scheme := shuangpin.Get(layoutID)
			if scheme != nil {
				pe.SetShuangpinConverter(shuangpin.NewConverter(scheme))
			}
		}
	}

	if layoutID == "" {
		m.logger.Info("切换到全拼模式")
	} else {
		m.logger.Info("更新双拼方案", "layoutID", layoutID)
	}
}

// UpdateLearningConfig 热更新当前引擎的学习配置（调频 + 造词）
func (m *Manager) UpdateLearningConfig(ls *schema.LearningSpec) {
	m.mu.Lock()
	defer m.mu.Unlock()

	dm := m.dictManager
	if dm == nil {
		return
	}

	engine := m.currentEngine
	if engine == nil {
		return
	}

	// 构建 FreqHandler（使用方案自身 ID，混输方案词频独立于主方案）
	var freqHandler *dict.FreqHandler
	if ls.IsFreqEnabled() {
		freqProfile := ls.GetFreqProfile()
		dm.SetFreqProfile(freqProfile)
		freqHandler = dict.NewFreqHandler(dm.GetStore(), m.currentID)
	} else {
		// 调频关闭时，清除 CompositeDict 上的 FreqScorer，停止应用旧的 boost
		dm.ClearFreqScorer()
	}

	// 构建 LearningStrategy
	var codetableLearning codetable.LearningStrategy
	var pinyinLearning pinyin.LearningStrategy

	// 检查当前引擎是否包含码表（码表或混输引擎）
	hasCodetable := false
	switch engine.(type) {
	case *codetable.Engine:
		hasCodetable = true
	case *mixed.Engine:
		hasCodetable = true
	}

	// 码表引擎：auto_learn 或 auto_phrase 启用时使用码表自动造词
	if hasCodetable && (ls.IsAutoPhraseEnabled() || ls.IsAutoLearnEnabled()) {
		autoPhrase := schema.NewCodeTableLearningStrategy(ls, m.logger)
		if dm.GetStoreUserLayer() != nil {
			autoPhrase.SetUserLayer(dm.GetStoreUserLayer())
		}
		if dm.GetStoreTempLayer() != nil {
			autoPhrase.SetTempLayer(dm.GetStoreTempLayer())
		}
		autoPhrase.SetSystemChecker(dm)
		if s := m.schemaManager.GetSchema(m.currentID); s != nil {
			encoder := m.resolveEncoder(s)
			if encoder != nil && len(encoder.Rules) > 0 {
				if ct := m.getCodeTable(); ct != nil {
					calc := schema.NewEncoderWordCodeCalc(encoder.Rules, ct)
					autoPhrase.SetWordCodeCalculator(calc)
				}
			}
		}
		codetableLearning = autoPhrase
	} else if hasCodetable {
		codetableLearning = &schema.ManualLearning{}
	}

	// 拼音策略：始终使用 AutoLearning（默认绑定当前活跃 user/temp 层；
	// 混输模式下会在 case *mixed.Engine 分支中重建为独立 pinyin bucket）
	pinyinLearning = schema.NewLearningStrategy(ls, dm.GetStoreUserLayer())
	if al, ok := pinyinLearning.(*schema.AutoLearning); ok {
		if dm.GetStoreTempLayer() != nil {
			al.SetTempLayer(dm.GetStoreTempLayer())
		}
		al.SetSystemChecker(dm)
	}

	// 注入到当前引擎
	switch e := engine.(type) {
	case *codetable.Engine:
		e.SetFreqHandler(freqHandler)
		e.SetLearningStrategy(codetableLearning)
	case *pinyin.Engine:
		e.SetFreqHandler(freqHandler)
		e.SetLearningStrategy(pinyinLearning)
	case *mixed.Engine:
		// 混输引擎：码表子引擎用码表策略，拼音子引擎用独立 dataSchemaID 的拼音策略
		// 避免拼音学到的词污染主码表用户词库
		if ce := e.GetCodetableEngine(); ce != nil {
			ce.SetFreqHandler(freqHandler)
			ce.SetLearningStrategy(codetableLearning)
		}
		if pe := e.GetPinyinEngine(); pe != nil {
			// caller 已持有 m.mu 写锁；不可调用 m.GetPrimaryPinyinID()（内部 RLock 会死锁），
			// 直接读字段。primaryPinyinID 由 SetPrimarySchemas 写入，与本路径同锁保护。
			pinyinDataSchemaID := m.primaryPinyinID
			if pinyinDataSchemaID == "" {
				pinyinDataSchemaID = "pinyin"
			}
			var pinyinFreq *dict.FreqHandler
			if ls.IsFreqEnabled() {
				pinyinFreq = dict.NewFreqHandler(dm.GetStore(), pinyinDataSchemaID)
			}
			pinyinUserLayer := dm.GetOrCreateStoreUserLayer(pinyinDataSchemaID)
			mixedPinyinLearning := schema.NewLearningStrategy(ls, pinyinUserLayer)
			if al, ok := mixedPinyinLearning.(*schema.AutoLearning); ok {
				if tl := dm.GetOrCreateStoreTempLayer(pinyinDataSchemaID); tl != nil {
					al.SetTempLayer(tl)
				}
				al.SetSystemChecker(dm)
			}
			pe.SetFreqHandler(pinyinFreq)
			pe.SetLearningStrategy(mixedPinyinLearning)
		}
	}

	m.logger.Info("学习配置已热更新",
		"freqEnabled", ls.IsFreqEnabled(),
		"autoLearnEnabled", ls.IsAutoLearnEnabled(),
		"autoPhraseEnabled", ls.IsAutoPhraseEnabled(),
		"codetableAutoPhrase", codetableLearning != nil)
}

// getCodeTable 从当前引擎获取码表（须持有 mu 锁）
func (m *Manager) getCodeTable() *dict.CodeTable {
	if m.currentEngine == nil {
		return nil
	}
	switch e := m.currentEngine.(type) {
	case *codetable.Engine:
		return e.GetCodeTable()
	case *mixed.Engine:
		if ce := e.GetCodetableEngine(); ce != nil {
			return ce.GetCodeTable()
		}
	}
	return nil
}
