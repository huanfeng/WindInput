// handle_s2t.go — 简入繁出（S2T）热键、状态切换、候选转换接入。
package coordinator

import (
	"log/slog"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/transform/s2t"
	"github.com/huanfeng/wind_input/pkg/config"
)

// newS2TManager 用 data/opencc 目录构造 S2T 管理器，路径解析失败时仍返回非 nil 实例
// （后续 Apply 时若词典不存在会返回原文，不影响输入流程）。
func newS2TManager(logger *slog.Logger) *s2t.Manager {
	dir, err := config.GetOpenCCDir()
	if err != nil && logger != nil {
		logger.Warn("s2t: failed to resolve opencc dir, using empty path", "error", err)
	}
	return s2t.NewManager(dir)
}

// reconfigureS2T 根据 cfg.S2T 重新设置启用状态/变体，并在加载失败时回退为关闭。
// 注意：调用方应确保已持锁；此函数不动其他状态。
func (c *Coordinator) reconfigureS2T(s2tCfg config.S2TConfig) {
	if c.s2tManager == nil {
		return
	}
	enabledChanged, variantChanged, err := c.s2tManager.Reconfigure(s2tCfg)
	if err != nil {
		c.logger.Error("s2t: reconfigure failed",
			"variant", s2tCfg.Variant, "enabled", s2tCfg.Enabled,
			"error", err)
		// 加载失败：强制回退为关闭，避免反复尝试
		_ = c.s2tManager.SetEnabled(false)
		if c.config != nil {
			c.config.S2T.Enabled = false
		}
		return
	}
	if enabledChanged || variantChanged {
		c.logger.Info("s2t: reconfigured",
			"enabled", s2tCfg.Enabled,
			"variant", string(s2tCfg.Variant),
			"loaded_dicts", len(c.s2tManager.LoadedDicts()))
	}
}

// applyS2TToCandidates 在候选生成后调用：若 S2T 启用则就地把候选 Text 转为目标繁体。
// 仅转换 Text；Code/Hint/Pinyin 等保持原样。
func (c *Coordinator) applyS2TToCandidates() {
	if c.s2tManager == nil || !c.s2tManager.IsEnabled() {
		return
	}
	for i := range c.candidates {
		t := c.candidates[i].Text
		if t == "" {
			continue
		}
		c.candidates[i].Text = c.s2tManager.Convert(t)
	}
}

// handleToggleS2T 处理 ToggleS2T 热键：翻转启用状态、广播、提示，并异步持久化。
// 调用方持 c.mu。
func (c *Coordinator) handleToggleS2T() *bridge.KeyEventResult {
	if c.config == nil || c.s2tManager == nil {
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}

	target := !c.config.S2T.Enabled
	c.config.S2T.Enabled = target
	c.reconfigureS2T(c.config.S2T)

	if c.hasPendingInput() {
		c.updateCandidates()
		c.showUI()
	}
	c.showS2TIndicator(target)

	c.persistS2TConfigAsync()
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}

// handleSetS2TVariant 切换变体（来自右键菜单/控制 IPC）。调用方持 c.mu。
func (c *Coordinator) handleSetS2TVariant(v config.S2TVariant) {
	if c.config == nil || c.s2tManager == nil || !v.Valid() {
		return
	}
	if c.config.S2T.Variant == v {
		return
	}
	c.config.S2T.Variant = v
	c.reconfigureS2T(c.config.S2T)

	if c.hasPendingInput() && c.s2tManager.IsEnabled() {
		c.updateCandidates()
		c.showUI()
	}
	c.showS2TIndicator(c.config.S2T.Enabled)

	c.persistS2TConfigAsync()
}

// persistS2TConfigAsync 在后台 goroutine 中持久化 S2T 配置并广播。
// 与 saveThemeConfig 等同模式：通过 cfgMu 安全拷贝，避免在 c.mu 内做 IO。
func (c *Coordinator) persistS2TConfigAsync() {
	notifier := c.eventNotifier
	go func() {
		c.cfgMu.Lock()
		cfgCopy := *c.config
		c.cfgMu.Unlock()

		if err := config.Save(&cfgCopy); err != nil {
			c.logger.Warn("s2t: save config failed", "error", err)
		}
		if notifier != nil {
			notifier.NotifyConfigUpdate()
		}
	}()
}

// showS2TIndicator 在状态指示窗显示当前简繁状态。
func (c *Coordinator) showS2TIndicator(enabled bool) {
	if c.uiManager == nil || !c.uiManager.IsReady() {
		return
	}
	label := s2tStatusLabel(enabled, c.config.S2T.Variant)
	x, y := c.getIndicatorPosition()
	c.uiManager.ShowModeIndicator(label, x, y)
}

// s2tStatusLabel 返回当前简繁状态在工具栏/提示窗显示的文案。
func s2tStatusLabel(enabled bool, v config.S2TVariant) string {
	if !enabled {
		return "简"
	}
	switch v {
	case config.S2TTaiwan:
		return "繁(台)"
	case config.S2TTaiwanPhrase:
		return "繁(台词)"
	case config.S2THongKong:
		return "繁(港)"
	default:
		return "繁"
	}
}
