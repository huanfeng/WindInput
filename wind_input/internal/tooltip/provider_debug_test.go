package tooltip

import (
	"context"
	"strings"
	"testing"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/pkg/config"
)

func TestDebugProvider_Basic(t *testing.T) {
	cfg := &config.TooltipDebugConfig{Enabled: true}
	p := NewDebugProvider(cfg)

	c := candidate.Candidate{
		Text:   "汉字",
		Code:   "hanz",
		Weight: 1200,
		Source: candidate.SourcePinyin,
		Meta: candidate.CandidateMeta{
			LexiconName: "pinyin_base",
			RawWeight:   1000,
			FreqBoost:   200,
		},
	}

	sec, err := p.Query(context.Background(), c)
	if err != nil {
		t.Fatalf("Query error: %v", err)
	}
	if len(sec.Lines) == 0 {
		t.Fatal("expected non-empty debug lines")
	}
	if !sec.Copyable {
		t.Error("expected Copyable=true")
	}

	joined := strings.Join(sec.Lines, "\n")
	if !strings.Contains(joined, "hanz") {
		t.Errorf("expected code in output, got: %s", joined)
	}
	if !strings.Contains(joined, "1200") {
		t.Errorf("expected weight in output, got: %s", joined)
	}
	if !strings.Contains(joined, "pinyin") {
		t.Errorf("expected source in output, got: %s", joined)
	}
}

func TestDebugProvider_Disabled(t *testing.T) {
	cfg := &config.TooltipDebugConfig{Enabled: false}
	p := NewDebugProvider(cfg)
	if p.Enabled() {
		t.Error("expected Enabled()=false")
	}
}

func TestDebugProvider_UserAndTempFlags(t *testing.T) {
	cfg := &config.TooltipDebugConfig{Enabled: true}
	p := NewDebugProvider(cfg)

	c := candidate.Candidate{
		Text:   "测",
		Weight: 500,
		Meta: candidate.CandidateMeta{
			IsUserDict: true,
			IsTempDict: false,
		},
		HasShadow: true,
	}

	sec, err := p.Query(context.Background(), c)
	if err != nil {
		t.Fatalf("Query error: %v", err)
	}
	joined := strings.Join(sec.Lines, "\n")
	if !strings.Contains(joined, "用户词") {
		t.Errorf("expected 用户词 in output, got: %s", joined)
	}
	if !strings.Contains(joined, "已调整") {
		t.Errorf("expected 已调整 in output, got: %s", joined)
	}
}
