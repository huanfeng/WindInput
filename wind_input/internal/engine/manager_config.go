package engine

import (
	"log"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
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
		}
	}

	log.Printf("[EngineManager] 更新过滤模式: %s", mode)
}

// UpdateWubiOptions 更新五笔引擎的选项（热更新）
func (m *Manager) UpdateWubiOptions(autoCommitAt4, clearOnEmptyAt4, topCodeCommit, punctCommit, showCodeHint, singleCodeInput bool, candidateSortMode string) {
	m.mu.Lock()
	defer m.mu.Unlock()

	for _, eng := range m.engines {
		if wubiEngine, ok := eng.(*wubi.Engine); ok {
			if cfg := wubiEngine.GetConfig(); cfg != nil {
				cfg.AutoCommitAt4 = autoCommitAt4
				cfg.ClearOnEmptyAt4 = clearOnEmptyAt4
				cfg.TopCodeCommit = topCodeCommit
				cfg.PunctCommit = punctCommit
				cfg.ShowCodeHint = showCodeHint
				cfg.SingleCodeInput = singleCodeInput
				cfg.CandidateSortMode = candidateSortMode
			}
		}
	}

	if m.dictManager != nil {
		m.dictManager.SetSortMode(candidate.CandidateSortMode(candidateSortMode))
	}

	log.Printf("[EngineManager] 更新五笔选项: autoCommitAt4=%v, clearOnEmptyAt4=%v, topCodeCommit=%v, punctCommit=%v, showCodeHint=%v, singleCodeInput=%v, candidateSortMode=%s",
		autoCommitAt4, clearOnEmptyAt4, topCodeCommit, punctCommit, showCodeHint, singleCodeInput, candidateSortMode)
}

// UpdatePinyinOptions 更新拼音引擎的选项（热更新）
func (m *Manager) UpdatePinyinOptions(pinyinCfg *config.PinyinConfig) {
	m.mu.Lock()
	defer m.mu.Unlock()

	if pinyinCfg == nil {
		return
	}

	showWubiHint := pinyinCfg.ShowWubiHint

	for _, eng := range m.engines {
		pinyinEngine, ok := eng.(*pinyin.Engine)
		if !ok {
			continue
		}

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

		// 如果开启反查但五笔码表未加载，从 Schema 获取路径并加载
		if showWubiHint && m.schemaManager != nil {
			m.loadWubiReverseForPinyin(pinyinEngine)
		}
	}

	log.Printf("[EngineManager] 更新拼音选项: showWubiHint=%v, fuzzyEnabled=%v", showWubiHint, pinyinCfg.Fuzzy.Enabled)
}

// loadWubiReverseForPinyin 从方案配置中查找五笔反查路径并加载
func (m *Manager) loadWubiReverseForPinyin(pinyinEngine *pinyin.Engine) {
	if m.schemaManager == nil {
		return
	}

	// 查找拼音方案中的反查词库
	for _, info := range m.schemaManager.ListSchemas() {
		s := m.schemaManager.GetSchema(info.ID)
		if s == nil || s.Engine.Type != schema.EngineTypePinyin {
			continue
		}
		for _, d := range s.GetDictsByRole(schema.DictRoleReverseLookup) {
			rdPath := d.Path
			if m.exeDir != "" && !isAbsPath(rdPath) {
				rdPath = m.exeDir + "/" + rdPath
			}
			if err := schema.LoadWubiTableForPinyinEngine(pinyinEngine, rdPath); err != nil {
				log.Printf("[EngineManager] 加载五笔反查码表失败: %v", err)
			} else {
				log.Printf("[EngineManager] 五笔反查码表加载成功")
			}
			return
		}
	}
}
