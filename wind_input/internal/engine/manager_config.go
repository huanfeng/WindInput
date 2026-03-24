package engine

import (
	"log"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/engine/mixed"
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
	"github.com/huanfeng/wind_input/internal/engine/pinyin/shuangpin"
	"github.com/huanfeng/wind_input/internal/engine/wubi"
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
		case *wubi.Engine:
			if cfg := e.GetConfig(); cfg != nil {
				cfg.FilterMode = mode
			}
		case *mixed.Engine:
			if we := e.GetWubiEngine(); we != nil {
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

	log.Printf("[EngineManager] 更新过滤模式: %s", mode)
}

// UpdateWubiOptions 更新五笔引擎的选项（热更新）
func (m *Manager) UpdateWubiOptions(autoCommitAt4, clearOnEmptyAt4, topCodeCommit, punctCommit, showCodeHint, singleCodeInput bool, candidateSortMode string) {
	m.mu.Lock()
	defer m.mu.Unlock()

	for _, eng := range m.engines {
		// 直接的五笔引擎
		if wubiEngine, ok := eng.(*wubi.Engine); ok {
			updateWubiConfig(wubiEngine, autoCommitAt4, clearOnEmptyAt4, topCodeCommit, punctCommit, showCodeHint, singleCodeInput, candidateSortMode)
		}
		// 混输引擎的五笔子引擎
		if mixedEngine, ok := eng.(*mixed.Engine); ok {
			if we := mixedEngine.GetWubiEngine(); we != nil {
				updateWubiConfig(we, autoCommitAt4, clearOnEmptyAt4, topCodeCommit, punctCommit, showCodeHint, singleCodeInput, candidateSortMode)
			}
		}
	}

	if m.dictManager != nil && candidateSortMode != "" {
		m.dictManager.SetSortMode(candidate.CandidateSortMode(candidateSortMode))
	}

	log.Printf("[EngineManager] 更新五笔选项: autoCommitAt4=%v, clearOnEmptyAt4=%v, topCodeCommit=%v, punctCommit=%v, showCodeHint=%v, singleCodeInput=%v, candidateSortMode=%s",
		autoCommitAt4, clearOnEmptyAt4, topCodeCommit, punctCommit, showCodeHint, singleCodeInput, candidateSortMode)
}

// updateWubiConfig 更新五笔引擎配置（内部辅助函数）
func updateWubiConfig(wubiEngine *wubi.Engine, autoCommitAt4, clearOnEmptyAt4, topCodeCommit, punctCommit, showCodeHint, singleCodeInput bool, candidateSortMode string) {
	if cfg := wubiEngine.GetConfig(); cfg != nil {
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
	showWubiHint := pinyinCfg.ShowWubiHint
	if cfg := pinyinEngine.GetConfig(); cfg != nil {
		oldShowWubiHint := cfg.ShowWubiHint
		cfg.ShowWubiHint = showWubiHint

		if pinyinCfg.Fuzzy.Enabled {
			cfg.Fuzzy = &pinyin.FuzzyConfig{
				ZhZ:   pinyinCfg.Fuzzy.ZhZ,
				ChC:   pinyinCfg.Fuzzy.ChC,
				ShS:   pinyinCfg.Fuzzy.ShS,
				NL:    pinyinCfg.Fuzzy.NL,
				FH:    pinyinCfg.Fuzzy.FH,
				RL:    pinyinCfg.Fuzzy.RL,
				AnAng: pinyinCfg.Fuzzy.AnAng,
				EnEng: pinyinCfg.Fuzzy.EnEng,
				InIng: pinyinCfg.Fuzzy.InIng,
			}
		} else {
			cfg.Fuzzy = nil
		}

		if oldShowWubiHint && !showWubiHint {
			pinyinEngine.ReleaseWubiHint()
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
			if pinyinCfg.ShowWubiHint && m.schemaManager != nil {
				m.loadWubiReverseForPinyin(pinyinEngine)
			}
		}
		// 混输引擎的拼音子引擎
		if mixedEngine, ok := eng.(*mixed.Engine); ok {
			if pe := mixedEngine.GetPinyinEngine(); pe != nil {
				updatePinyinConfig(pe, pinyinCfg)
				if pinyinCfg.ShowWubiHint && m.schemaManager != nil {
					m.loadWubiReverseForPinyin(pe)
				}
			}
		}
	}

	log.Printf("[EngineManager] 更新拼音选项: showWubiHint=%v, fuzzyEnabled=%v", pinyinCfg.ShowWubiHint, pinyinCfg.Fuzzy.Enabled)
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
		log.Printf("[EngineManager] 切换到全拼模式")
	} else {
		log.Printf("[EngineManager] 更新双拼方案: %s", layoutID)
	}
}

// loadWubiReverseForPinyin 从方案配置中查找五笔反查路径并加载
func (m *Manager) loadWubiReverseForPinyin(pinyinEngine *pinyin.Engine) {
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
			if err := schema.LoadWubiTableForPinyinEngine(pinyinEngine, rdPath, d.Type); err != nil {
				log.Printf("[EngineManager] 加载五笔反查码表失败: %v", err)
			} else {
				log.Printf("[EngineManager] 五笔反查码表加载成功")
			}
			return
		}
	}
}
