package tooltip

import (
	"context"
	"strings"
	"testing"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/pkg/config"
)

func TestPinyinProvider_Query(t *testing.T) {
	cfg := &config.TooltipPinyinConfig{Enabled: true, Heteronyms: true, MaxReadings: 2}
	p := NewPinyinProvider(cfg)

	tests := []struct {
		name      string
		text      string
		wantLines int    // 期望行数
		wantPart  string // Lines[0] 中应包含的子串
	}{
		{"单字统一逐字格式", "汉", 1, "汉：hàn"},
		{"词组逐字展开", "汉字", 2, "汉：hàn"},
		{"纯非汉字返回空", "abc", 0, ""},
		{"混合保留非汉字行", "a汉", 2, "a"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			c := candidate.Candidate{Text: tt.text}
			sec, err := p.Query(context.Background(), c)
			if err != nil {
				t.Fatalf("Query error: %v", err)
			}
			if len(sec.Lines) != tt.wantLines {
				t.Fatalf("expected %d lines, got %d: %v", tt.wantLines, len(sec.Lines), sec.Lines)
			}
			if tt.wantPart != "" && !strings.Contains(sec.Lines[0], tt.wantPart) {
				t.Errorf("Lines[0]=%q, want contains %q", sec.Lines[0], tt.wantPart)
			}
		})
	}
}

func TestPinyinProvider_Heteronyms(t *testing.T) {
	// 关闭多音：长 只返回单音，无 /
	cfgOff := &config.TooltipPinyinConfig{Enabled: true, Heteronyms: false}
	pOff := NewPinyinProvider(cfgOff)
	sec, _ := pOff.Query(context.Background(), candidate.Candidate{Text: "长"})
	if len(sec.Lines) != 1 {
		t.Fatalf("expected 1 line, got %d", len(sec.Lines))
	}
	if strings.Contains(sec.Lines[0], "/") {
		t.Errorf("expected single reading without /, got %q", sec.Lines[0])
	}

	// 开启多音 + MaxReadings=2：长 有 cháng/zhǎng 两个现代常用音
	cfgOn := &config.TooltipPinyinConfig{Enabled: true, Heteronyms: true, MaxReadings: 2}
	pOn := NewPinyinProvider(cfgOn)
	secOn, _ := pOn.Query(context.Background(), candidate.Candidate{Text: "长"})
	if len(secOn.Lines) != 1 {
		t.Fatalf("expected 1 line, got %d", len(secOn.Lines))
	}
	if !strings.Contains(secOn.Lines[0], "/") {
		t.Errorf("expected multiple readings with /, got %q", secOn.Lines[0])
	}
}

func TestPinyinProvider_MaxReadings(t *testing.T) {
	// MaxReadings=1：杜 只显示 dù，不显示古音
	cfg := &config.TooltipPinyinConfig{Enabled: true, Heteronyms: true, MaxReadings: 1}
	p := NewPinyinProvider(cfg)
	sec, _ := p.Query(context.Background(), candidate.Candidate{Text: "杜"})
	if len(sec.Lines) != 1 {
		t.Fatalf("expected 1 line, got %d", len(sec.Lines))
	}
	if strings.Contains(sec.Lines[0], "/") {
		t.Errorf("MaxReadings=1 should produce no /, got %q", sec.Lines[0])
	}
}

func TestPinyinProvider_Disabled(t *testing.T) {
	cfg := &config.TooltipPinyinConfig{Enabled: false}
	p := NewPinyinProvider(cfg)
	if p.Enabled() {
		t.Error("expected Enabled() = false")
	}
}
