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
func (m *Manager) UpdateCodetableOptions(autoCommitAt4, clearOnEmptyAt4, topCodeCommit, punctCommit, showCodeHint, singleCodeInput bool, candidateSortMode string) {
	m.mu.Lock()
	defer m.mu.Unlock()

	for _, eng := range m.engines {
		// 直接的码表引擎
		if codetableEngine, ok := eng.(*codetable.Engine); ok {
			updateCodetableConfig(codetableEngine, autoCommitAt4, clearOnEmptyAt4, topCodeCommit, punctCommit, showCodeHint, singleCodeInput, candidateSortMode)
		}
		// 混输引擎的码表子引擎
		if mixedEngine, ok := eng.(*mixed.Engine); ok {
			if we := mixedEngine.GetCodetableEngine(); we != nil {
				updateCodetableConfig(we, autoCommitAt4, clearOnEmptyAt4, topCodeCommit, punctCommit, showCodeHint, singleCodeInput, candidateSortMode)
			}
		}
	}

	if m.dictManager != nil && candidateSortMode != "" {
		m.dictManager.SetSortMode(candidate.CandidateSortMode(candidateSortMode))
	}

	m.logger.Info("更新码表选项",
		"autoCommitAt4", autoCommitAt4,
		"clearOnEmptyAt4", clearOnEmptyAt4,
		"topCodeCommit", topCodeCommit,
		"punctCommit", punctCommit,
		"showCodeHint", showCodeHint,
		"singleCodeInput", singleCodeInput,
		"candidateSortMode", candidateSortMode)
}

// updateCodetableConfig 更新码表引擎配置（内部辅助函数）
func updateCodetableConfig(codetableEngine *codetable.Engine, autoCommitAt4, clearOnEmptyAt4, topCodeCommit, punctCommit, showCodeHint, singleCodeInput bool, candidateSortMode string) {
	if cfg := codetableEngine.GetConfig(); cfg != nil {
		cfg.AutoCommitAt4 = autoCommitAt4
		cfg.ClearOnEmptyAt4 = clearOnEmptyAt4
		cfg.TopCodeCommit = topCodeCommit
		cfg.PunctCommit = punctCommit
		cfg.ShowCodeHint = showCodeHint
		cfg.SingleCodeInput = singleCodeInput
		if candidateSortMode != "" {
			cfg.CandidateSortMode = candidateSortMode
		}
	}
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
		if pinyinEngine, ok := eng.(*pinyin.Engine); ok {
			updatePinyinConfig(pinyinEngine, pinyinCfg)
			if pinyinCfg.ShowCodeHint && m.schemaManager != nil {
				m.loadCodetableReverseForPinyin(pinyinEngine)
			}
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

	// 构建 FreqHandler
	var freqHandler *dict.FreqHandler
	if ls.IsFreqEnabled() {
		freqProfile := ls.GetFreqProfile()
		dm.SetFreqProfile(freqProfile)
		dataSchemaID := m.currentID
		if sm := m.schemaManager; sm != nil {
			if s := sm.GetSchema(m.currentID); s != nil {
				dataSchemaID = s.DataSchemaID()
			}
		}
		freqHandler = dict.NewFreqHandler(dm.GetStore(), dataSchemaID)
	} else {
		// 调频关闭时，清除 CompositeDict 上的 FreqScorer，停止应用旧的 boost
		dm.ClearFreqScorer()
	}

	// 构建 LearningStrategy
	learningStrategy := schema.NewLearningStrategy(ls, dm.GetStoreUserLayer())
	if al, ok := learningStrategy.(*schema.AutoLearning); ok {
		if dm.GetStoreTempLayer() != nil {
			al.SetTempLayer(dm.GetStoreTempLayer())
		}
		al.SetSystemChecker(dm)
	}

	// 注入到当前引擎
	switch e := engine.(type) {
	case *codetable.Engine:
		e.SetFreqHandler(freqHandler)
		e.SetLearningStrategy(learningStrategy)
	case *pinyin.Engine:
		e.SetFreqHandler(freqHandler)
		e.SetLearningStrategy(learningStrategy)
	case *mixed.Engine:
		// 混输引擎：分别注入到码表和拼音子引擎
		if ce := e.GetCodetableEngine(); ce != nil {
			ce.SetFreqHandler(freqHandler)
			ce.SetLearningStrategy(learningStrategy)
		}
		if pe := e.GetPinyinEngine(); pe != nil {
			pe.SetFreqHandler(freqHandler)
			pe.SetLearningStrategy(learningStrategy)
		}
	}

	m.logger.Info("学习配置已热更新",
		"freqEnabled", ls.IsFreqEnabled(),
		"autoLearnEnabled", ls.IsAutoLearnEnabled())
}

// loadCodetableReverseForPinyin 从方案配置中查找码表反查路径并加载
func (m *Manager) loadCodetableReverseForPinyin(pinyinEngine *pinyin.Engine) {
	if m.schemaManager == nil {
		return
	}

	// 查找拼音或混输方案中的反查词库
	for _, info := range m.schemaManager.ListSchemas() {
		s := m.schemaManager.GetSchema(info.ID)
		if s == nil || (s.Engine.Type != schema.EngineTypePinyin && s.Engine.Type != schema.EngineTypeMixed) {
			continue
		}
		for _, d := range s.GetDictsByRole(schema.DictRoleReverseLookup) {
			rdPath := d.Path
			if m.exeDir != "" && !isAbsPath(rdPath) {
				rdPath = m.exeDir + "/" + rdPath
			}
			if err := schema.LoadCodetableForPinyinEngine(pinyinEngine, rdPath, d.Type, info.ID, m.logger); err != nil {
				m.logger.Warn("加载码表反查失败", "error", err)
			} else {
				m.logger.Info("码表反查加载成功")
			}
			return
		}
	}
}
