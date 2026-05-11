package engine

import (
	"path/filepath"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/dict/dictcache"
)

// EnsureEnglishLoaded 确保英文词库已加载到内存
func (m *Manager) EnsureEnglishLoaded() error {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.ensureEnglishLoadedLocked()
}

// ensureEnglishLoadedLocked 确保英文词库已加载（调用方必须持有 m.mu 写锁）。
func (m *Manager) ensureEnglishLoadedLocked() error {
	if m.englishDict != nil {
		return nil // 已加载
	}

	// 英文词库路径：dataRoot/schemas/english/
	// 注意：m.dataRoot 实际是 dataRoot（已包含 data 目录）
	dictDir := filepath.Join(m.dataRoot, "schemas", "english")

	wdbCachePath := dictcache.CachePath("english")
	d := dict.NewEnglishDict(m.logger)
	if err := d.LoadRimeDir(dictDir, wdbCachePath); err != nil {
		m.logger.Warn("加载英文词库失败", "dir", dictDir, "error", err)
		return err
	}

	m.englishDict = d
	m.englishLayer = dict.NewEnglishDictLayer("english-system", d)
	m.systemLayers["english"] = m.englishLayer
	m.logger.Info("英文词库已加载", "entries", d.EntryCount())
	return nil
}

// ActivateTempEnglish 激活临时英文模式的英文词库层
// 将英文词库层注册到 CompositeDict，供临时英文模式查询候选。
func (m *Manager) ActivateTempEnglish() {
	if m.englishLayer == nil || m.dictManager == nil {
		return
	}
	compositeDict := m.dictManager.GetCompositeDict()
	if compositeDict == nil {
		return
	}

	if compositeDict.GetLayerByName("english-system") != nil {
		return // 已注册
	}

	m.dictManager.RegisterSystemLayer(m.englishLayer.Name(), m.englishLayer)
	m.logger.Info("临时英文：注册英文词库层")

	// 同时激活英文 Store 层（用户词 + Shadow）
	if m.dictManager != nil {
		m.dictManager.ActivateEnglishStoreLayers()
	}
}

// DeactivateTempEnglish 停用临时英文模式的英文词库层
func (m *Manager) DeactivateTempEnglish() {
	if m.dictManager == nil {
		return
	}
	compositeDict := m.dictManager.GetCompositeDict()
	if compositeDict == nil {
		return
	}

	if compositeDict.GetLayerByName("english-system") != nil {
		m.dictManager.UnregisterSystemLayer("english-system")
		m.logger.Info("临时英文：卸载英文词库层")
	}

	// 同时停用英文 Store 层
	if m.dictManager != nil {
		m.dictManager.DeactivateEnglishStoreLayers()
	}
}

// SearchEnglish 查询英文候选（前缀匹配）
// 不依赖 CompositeDict 层注册状态，直接查询英文词库。
// 用于临时英文模式和混输模式。
func (m *Manager) SearchEnglish(prefix string, limit int) []candidate.Candidate {
	m.mu.RLock()
	d := m.englishDict
	m.mu.RUnlock()

	if d == nil {
		return nil
	}
	return d.LookupPrefix(prefix, limit)
}

// IsEnglishLoaded 检查英文词库是否已加载
func (m *Manager) IsEnglishLoaded() bool {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.englishDict != nil
}
