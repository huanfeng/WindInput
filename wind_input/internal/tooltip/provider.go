// Package tooltip 提供候选悬停增强提示的 Provider 框架
package tooltip

import (
	"context"

	"github.com/huanfeng/wind_input/internal/candidate"
)

// Section 表示 tooltip 中的一个信息块
type Section struct {
	Label        string   // 显示标签，如 "拼音"、"拆字"、"调试"
	Lines        []string // 内容行
	Copyable     bool     // 是否支持通过右键菜单复制
	AlwaysExpand bool     // 强制多行展开格式（即使只有 1 行内容）
}

// Provider 是 tooltip 信息来源的接口
type Provider interface {
	// Name 返回 provider 的唯一名称（用于日志和调试）
	Name() string
	// Enabled 返回该 provider 当前是否启用
	Enabled() bool
	// Query 查询候选的 tooltip 信息，ctx 用于取消
	Query(ctx context.Context, c candidate.Candidate) (Section, error)
}
