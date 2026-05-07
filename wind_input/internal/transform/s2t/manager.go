package s2t

import (
	"fmt"
	"sync"

	"github.com/huanfeng/wind_input/pkg/config"
)

// Manager 简入繁出转换的运行时单例。
//
// 职责：
//   - 维护启用状态与当前变体
//   - 按需加载/释放词典（DictPool）
//   - 暴露 Convert / Apply 给 coordinator 候选链路
//
// 并发：
//   - 修改状态（Reconfigure / SetEnabled / SetVariant）走写锁
//   - 查询路径（Convert）走读锁，转换器自带 LRU 也是线程安全的
type Manager struct {
	pool *DictPool

	mu        sync.RWMutex
	enabled   bool
	variant   config.S2TVariant
	converter *Converter
}

// NewManager 用给定的 OpenCC 词典目录构造管理器。
// 此时不加载任何词典；首次 Enable 才触发加载。
func NewManager(dictDir string) *Manager {
	return &Manager{
		pool:    NewDictPool(dictDir),
		variant: config.S2TStandard,
	}
}

// IsEnabled 返回当前是否启用。
func (m *Manager) IsEnabled() bool {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.enabled
}

// Variant 返回当前变体。
func (m *Manager) Variant() config.S2TVariant {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.variant
}

// Reconfigure 根据配置重新设置启用状态与变体。
// 仅在状态发生变化时触发词典加载/释放。
// 返回 (启用状态变化, 变体变化, 错误)。
func (m *Manager) Reconfigure(cfg config.S2TConfig) (enabledChanged, variantChanged bool, err error) {
	v := cfg.Variant
	if !v.Valid() {
		v = config.S2TStandard
	}
	m.mu.Lock()
	defer m.mu.Unlock()

	enabledChanged = cfg.Enabled != m.enabled
	variantChanged = v != m.variant

	if !cfg.Enabled {
		if m.enabled {
			m.enabled = false
			m.converter = nil
			m.pool.ReleaseAll()
		}
		m.variant = v
		return enabledChanged, variantChanged, nil
	}

	// cfg.Enabled == true
	if !enabledChanged && !variantChanged && m.converter != nil {
		return false, false, nil
	}
	if err := m.rebuildLocked(v); err != nil {
		return enabledChanged, variantChanged, err
	}
	m.enabled = true
	m.variant = v
	return enabledChanged, variantChanged, nil
}

// SetEnabled 切换启用状态（不改变变体）。
func (m *Manager) SetEnabled(enabled bool) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	if enabled == m.enabled {
		return nil
	}
	if !enabled {
		m.enabled = false
		m.converter = nil
		m.pool.ReleaseAll()
		return nil
	}
	if err := m.rebuildLocked(m.variant); err != nil {
		return err
	}
	m.enabled = true
	return nil
}

// SetVariant 切换变体；启用状态下立即生效，未启用时仅记录目标变体。
func (m *Manager) SetVariant(v config.S2TVariant) error {
	if !v.Valid() {
		return fmt.Errorf("invalid variant: %q", v)
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	if v == m.variant {
		return nil
	}
	m.variant = v
	if !m.enabled {
		return nil
	}
	return m.rebuildLocked(v)
}

// Convert 对单个字符串做转换。未启用时直接返回原值。
func (m *Manager) Convert(s string) string {
	m.mu.RLock()
	conv := m.converter
	enabled := m.enabled
	m.mu.RUnlock()
	if !enabled || conv == nil || s == "" {
		return s
	}
	return conv.Convert(s)
}

// ApplyToTexts 对一组字符串就地转换。
func (m *Manager) ApplyToTexts(texts []string) {
	if len(texts) == 0 {
		return
	}
	m.mu.RLock()
	conv := m.converter
	enabled := m.enabled
	m.mu.RUnlock()
	if !enabled || conv == nil {
		return
	}
	for i, t := range texts {
		texts[i] = conv.Convert(t)
	}
}

// LoadedDicts 返回当前已加载的词典名（诊断用）。
func (m *Manager) LoadedDicts() []string {
	return m.pool.LoadedNames()
}

// rebuildLocked 在已持写锁的前提下，按变体重建 converter。
func (m *Manager) rebuildLocked(v config.S2TVariant) error {
	groups := Chain(v)
	steps := make([][]*Dict, 0, len(groups))
	for _, names := range groups {
		dicts, err := m.pool.AcquireAll(names)
		if err != nil {
			return err
		}
		steps = append(steps, dicts)
	}
	const cacheCap = 1024
	m.converter = NewConverter(steps, cacheCap)
	return nil
}
