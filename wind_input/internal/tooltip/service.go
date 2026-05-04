package tooltip

import (
	"context"
	"sync"

	"github.com/huanfeng/wind_input/internal/candidate"
)

// Service 管理多个 Provider 并提供并行异步查询能力
type Service struct {
	providers []Provider
}

// NewService 创建一个新的 TooltipService
func NewService(providers ...Provider) *Service {
	return &Service{providers: providers}
}

// HasEnabledProviders 返回是否有任何启用的 provider
func (s *Service) HasEnabledProviders() bool {
	for _, p := range s.providers {
		if p.Enabled() {
			return true
		}
	}
	return false
}

// Query 并行查询所有启用的 provider，收集结果
// ctx 取消时立即返回已收集的结果
func (s *Service) Query(ctx context.Context, c candidate.Candidate) []Section {
	var enabled []Provider
	for _, p := range s.providers {
		if p.Enabled() {
			enabled = append(enabled, p)
		}
	}
	if len(enabled) == 0 {
		return nil
	}

	type result struct {
		idx     int
		section Section
	}

	ch := make(chan result, len(enabled))
	var wg sync.WaitGroup

	for i, p := range enabled {
		wg.Add(1)
		go func(idx int, provider Provider) {
			defer wg.Done()
			sec, err := provider.Query(ctx, c)
			if err != nil || ctx.Err() != nil {
				return
			}
			if len(sec.Lines) > 0 {
				ch <- result{idx: idx, section: sec}
			}
		}(i, p)
	}

	go func() {
		wg.Wait()
		close(ch)
	}()

	// 收集结果，保持 provider 注册顺序
	raw := make([]Section, len(enabled))
	seen := make([]bool, len(enabled))
	for r := range ch {
		if ctx.Err() != nil {
			break
		}
		raw[r.idx] = r.section
		seen[r.idx] = true
	}

	var sections []Section
	for i, sec := range raw {
		if seen[i] {
			sections = append(sections, sec)
		}
	}
	return sections
}
