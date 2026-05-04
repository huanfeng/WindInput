package tooltip

import (
	"context"
	"strings"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/pkg/config"
)

// PinyinProvider 为候选文字提供带声调的拼音提示。
// 数据来自 pinyinData（由 scripts/gen_pinyin_data 从 kXHC1983+kTGHZ2013+kMandarin_8105 生成），
// 仅含现代普通话读音，不含 kHanyuPinyin 古音。
type PinyinProvider struct {
	cfg *config.TooltipPinyinConfig
}

// NewPinyinProvider 创建拼音提示 provider
func NewPinyinProvider(cfg *config.TooltipPinyinConfig) *PinyinProvider {
	return &PinyinProvider{cfg: cfg}
}

func (p *PinyinProvider) Name() string { return "pinyin" }

func (p *PinyinProvider) Enabled() bool {
	return p.cfg != nil && p.cfg.Enabled
}

// readingsFor 返回单个汉字的现代读音列表。
// Heteronyms=false 时只返回首音；MaxReadings>0 时截断。
func (p *PinyinProvider) readingsFor(r rune) []string {
	readings, ok := pinyinData[r]
	if !ok || len(readings) == 0 {
		return nil
	}
	if p.cfg == nil || !p.cfg.Heteronyms {
		return readings[:1]
	}
	if p.cfg.MaxReadings > 0 && len(readings) > p.cfg.MaxReadings {
		return readings[:p.cfg.MaxReadings]
	}
	return readings
}

// Query 逐字查询拼音，统一以 "字：读音" 格式逐行展开（AlwaysExpand=true）。
func (p *PinyinProvider) Query(_ context.Context, c candidate.Candidate) (Section, error) {
	if c.Text == "" {
		return Section{}, nil
	}

	var lines []string
	var hasHan bool
	var nonHanBuf strings.Builder

	flushNonHan := func() {
		if nonHanBuf.Len() > 0 {
			lines = append(lines, nonHanBuf.String())
			nonHanBuf.Reset()
		}
	}

	for _, r := range []rune(c.Text) {
		readings := p.readingsFor(r)
		if len(readings) == 0 {
			nonHanBuf.WriteRune(r)
			continue
		}
		flushNonHan()
		hasHan = true
		lines = append(lines, string(r)+"："+strings.Join(readings, "/"))
	}
	flushNonHan()

	if !hasHan {
		return Section{}, nil
	}
	return Section{
		Label:        "拼音",
		Lines:        lines,
		Copyable:     true,
		AlwaysExpand: true,
	}, nil
}
