package engine

import (
	"log"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
	"github.com/huanfeng/wind_input/internal/engine/wubi"
	"github.com/huanfeng/wind_input/pkg/config"
)

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

// SetDictPaths 设置词库路径
func (m *Manager) SetDictPaths(pinyinPath, wubiPath string) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.pinyinDictPath = pinyinPath
	m.wubiDictPath = wubiPath
}

// SetPinyinConfig 设置拼音配置
func (m *Manager) SetPinyinConfig(config *pinyin.Config) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.pinyinConfig = config
}

// SetWubiConfig 设置五笔配置
func (m *Manager) SetWubiConfig(config *wubi.Config) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.wubiConfig = config
}

// UpdateFilterMode 更新当前引擎的过滤模式
func (m *Manager) UpdateFilterMode(mode string) {
	m.mu.Lock()
	defer m.mu.Unlock()

	// 更新保存的配置
	if m.pinyinConfig != nil {
		m.pinyinConfig.FilterMode = mode
	}
	if m.wubiConfig != nil {
		m.wubiConfig.FilterMode = mode
	}

	// 更新当前运行的引擎配置
	if m.currentEngine != nil {
		switch e := m.currentEngine.(type) {
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

	// 更新所有已注册引擎的配置
	for _, engine := range m.engines {
		switch e := engine.(type) {
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

	// 更新保存的配置
	if m.wubiConfig != nil {
		m.wubiConfig.AutoCommitAt4 = autoCommitAt4
		m.wubiConfig.ClearOnEmptyAt4 = clearOnEmptyAt4
		m.wubiConfig.TopCodeCommit = topCodeCommit
		m.wubiConfig.PunctCommit = punctCommit
		m.wubiConfig.ShowCodeHint = showCodeHint
		m.wubiConfig.SingleCodeInput = singleCodeInput
		m.wubiConfig.CandidateSortMode = candidateSortMode
	}

	// 更新所有已注册的五笔引擎的配置
	for _, engine := range m.engines {
		if wubiEngine, ok := engine.(*wubi.Engine); ok {
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

	// 更新词库管理器的排序模式
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

	// 更新保存的配置
	if m.pinyinConfig != nil {
		m.pinyinConfig.ShowWubiHint = showWubiHint
		// 更新模糊拼音配置
		if pinyinCfg.Fuzzy.Enabled {
			m.pinyinConfig.Fuzzy = &pinyin.FuzzyConfig{
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
			}
		} else {
			m.pinyinConfig.Fuzzy = nil
		}
	}

	// 更新所有已注册的拼音引擎的配置
	for _, engine := range m.engines {
		if pinyinEngine, ok := engine.(*pinyin.Engine); ok {
			if cfg := pinyinEngine.GetConfig(); cfg != nil {
				oldShowWubiHint := cfg.ShowWubiHint
				cfg.ShowWubiHint = showWubiHint

				// 更新模糊拼音配置
				if pinyinCfg.Fuzzy.Enabled {
					cfg.Fuzzy = &pinyin.FuzzyConfig{
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
					}
				} else {
					cfg.Fuzzy = nil
				}

				// 从 true→false：释放反向索引
				if oldShowWubiHint && !showWubiHint {
					pinyinEngine.ReleaseWubiHint()
				}
			}
			// 如果开启反查但五笔码表未加载，则加载（仅 wdb 二进制）
			if showWubiHint && m.wubiDictPath != "" {
				wubiFullPath := m.wubiDictPath
				if m.exeDir != "" && !isAbsPath(m.wubiDictPath) {
					wubiFullPath = m.exeDir + "/" + m.wubiDictPath
				}
				if err := loadWubiTableForPinyin(pinyinEngine, wubiFullPath); err != nil {
					log.Printf("[EngineManager] 加载五笔反查码表失败: %v", err)
				} else {
					log.Printf("[EngineManager] 五笔反查码表加载成功")
				}
			}
		}
	}

	log.Printf("[EngineManager] 更新拼音选项: showWubiHint=%v, fuzzyEnabled=%v", showWubiHint, pinyinCfg.Fuzzy.Enabled)
}
