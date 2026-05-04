package tooltip

import (
	"context"
	"fmt"
	"strings"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/pkg/config"
)

// DebugProvider 为候选提供调试信息（来源/权重/词库等）
type DebugProvider struct {
	cfg *config.TooltipDebugConfig
}

// NewDebugProvider 创建调试信息 provider
func NewDebugProvider(cfg *config.TooltipDebugConfig) *DebugProvider {
	return &DebugProvider{cfg: cfg}
}

func (p *DebugProvider) Name() string { return "debug" }

func (p *DebugProvider) Enabled() bool {
	return p.cfg != nil && p.cfg.Enabled
}

// Query 收集候选的调试信息
func (p *DebugProvider) Query(_ context.Context, c candidate.Candidate) (Section, error) {
	var lines []string

	// 文字和编码
	if c.Code != "" {
		lines = append(lines, fmt.Sprintf("编码: %s", c.Code))
	}

	// 权重信息
	weightStr := formatWeight(c)
	if weightStr != "" {
		lines = append(lines, "权重: "+weightStr)
	}

	// 来源（混输模式下区分引擎）
	if c.Source != "" {
		lines = append(lines, fmt.Sprintf("引擎: %s", c.Source))
	}

	// 词库信息
	if c.Meta.LexiconName != "" {
		lines = append(lines, fmt.Sprintf("词库: %s", c.Meta.LexiconName))
	}

	// 用户词/临时词标记
	var flags []string
	if c.Meta.IsUserDict {
		flags = append(flags, "用户词")
	}
	if c.Meta.IsTempDict {
		flags = append(flags, "临时词")
	}
	if c.HasShadow {
		flags = append(flags, "已调整")
	}
	if len(flags) > 0 {
		lines = append(lines, "标记: "+strings.Join(flags, " "))
	}

	if len(lines) == 0 {
		return Section{}, nil
	}

	return Section{
		Label:    "调试",
		Lines:    lines,
		Copyable: true,
	}, nil
}

// formatWeight 将权重信息格式化为可读字符串
func formatWeight(c candidate.Candidate) string {
	w := c.Weight
	meta := c.Meta

	if meta.RawWeight == 0 && meta.FreqBoost == 0 {
		return fmt.Sprintf("%d", w)
	}

	parts := []string{fmt.Sprintf("%d", w)}
	detail := fmt.Sprintf("基础 %d", meta.RawWeight)
	if meta.FreqBoost != 0 {
		if meta.FreqBoost > 0 {
			detail += fmt.Sprintf(" + 词频 %d", meta.FreqBoost)
		} else {
			detail += fmt.Sprintf(" - 词频 %d", -meta.FreqBoost)
		}
	}
	parts = append(parts, fmt.Sprintf("(%s)", detail))
	return strings.Join(parts, " ")
}
