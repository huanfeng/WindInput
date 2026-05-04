// handle_tooltip.go — 候选悬停 tooltip 异步查询
package coordinator

import (
	"context"
	"time"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/tooltip"
	"github.com/huanfeng/wind_input/pkg/config"
)

// buildTooltipService 根据配置和方案拆字路径创建 TooltipService
func buildTooltipService(cfg *config.Config, chaiziDBPath string) *tooltip.Service {
	if cfg == nil {
		return tooltip.NewService()
	}
	tcfg := &cfg.UI.Tooltip
	return tooltip.NewService(
		tooltip.NewPinyinProvider(&tcfg.Pinyin),
		tooltip.NewChaiziProvider(&tcfg.Chaizi, chaiziDBPath, ""),
		tooltip.NewDebugProvider(&tcfg.Debug),
	)
}

// rebuildTooltipServiceLocked 在持有 c.mu 时重建 tooltip service
func (c *Coordinator) rebuildTooltipServiceLocked() {
	c.cancelTooltipQuery() // 取消旧查询，防止旧 goroutine 继续使用过期 service
	chaiziPath := c.getChaiziDBPath()
	svc := buildTooltipService(c.config, chaiziPath)
	c.tooltipMu.Lock()
	c.tooltipService = svc
	c.tooltipMu.Unlock()
}

// getChaiziDBPath 返回当前活跃方案的拆字数据库路径（需持有 c.mu）
func (c *Coordinator) getChaiziDBPath() string {
	// 预留：从 engineMgr 获取当前方案的 Chaizi.DBPath
	// 目前方案级 chaizi 路径尚未接入，先返回空字符串
	return ""
}

// cancelTooltipQuery 取消当前待执行的 tooltip 查询
func (c *Coordinator) cancelTooltipQuery() {
	c.tooltipMu.Lock()
	if c.tooltipCancel != nil {
		c.tooltipCancel()
		c.tooltipCancel = nil
	}
	c.tooltipMu.Unlock()
}

// runTooltipQuery 在 goroutine 中执行延迟 + 异步 tooltip 查询
func (c *Coordinator) runTooltipQuery(
	ctx context.Context,
	hoverIdx int,
	cand candidate.Candidate,
	svc *tooltip.Service,
	centerX, y, delayMs int,
) {
	// 等待延迟
	if delayMs > 0 {
		select {
		case <-ctx.Done():
			return
		case <-time.After(time.Duration(delayMs) * time.Millisecond):
		}
	}

	// 检查是否仍在悬停同一候选
	c.tooltipMu.Lock()
	if c.tooltipHoverIdx != hoverIdx {
		c.tooltipMu.Unlock()
		return
	}
	c.tooltipMu.Unlock()

	// 查询所有 provider
	var sections []tooltip.Section
	c.mu.Lock()
	codeEnabled := c.config != nil && c.config.UI.Tooltip.Code.IsEnabled()
	c.mu.Unlock()
	if codeEnabled && cand.Code != "" {
		sections = append(sections, tooltip.Section{
			Label: "编码",
			Lines: []string{cand.Code},
		})
	}
	if svc != nil && svc.HasEnabledProviders() {
		sections = append(sections, svc.Query(ctx, cand)...)
	}

	if ctx.Err() != nil {
		return
	}

	content := tooltip.FormatContent("", sections)
	if content == "" {
		return
	}

	// 最终检查后显示
	c.tooltipMu.Lock()
	if c.tooltipHoverIdx != hoverIdx {
		c.tooltipMu.Unlock()
		return
	}
	c.tooltipMu.Unlock()

	if c.uiManager != nil {
		c.uiManager.ShowTooltipText(content, centerX, y)
	}
}
