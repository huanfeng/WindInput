package schema

import (
	"testing"
)

func TestManualLearning(t *testing.T) {
	ls := &LearningSpec{} // 默认不启用
	strategy := NewLearningStrategy(ls, nil)

	// ManualLearning 不应 panic
	strategy.OnWordCommitted("test", "测试")
	strategy.Reset()

	if _, ok := strategy.(*ManualLearning); !ok {
		t.Error("默认配置应返回 *ManualLearning")
	}
}

func TestAutoLearning_NilSafe(t *testing.T) {
	ls := &LearningSpec{
		AutoLearn: &AutoLearnSpec{Enabled: true},
	}
	strategy := NewLearningStrategy(ls, nil)

	// nil userLayer 不应 panic
	strategy.OnWordCommitted("nihao", "你好")
	strategy.Reset()

	if _, ok := strategy.(*AutoLearning); !ok {
		t.Error("auto_learn.enabled=true 应返回 *AutoLearning")
	}
}

func TestAutoLearning_SkipShortWord(t *testing.T) {
	ls := &LearningSpec{
		AutoLearn: &AutoLearnSpec{
			Enabled:       true,
			MinWordLength: 2,
		},
	}
	strategy := NewLearningStrategy(ls, nil)

	// 单字不应造词（不 panic 即通过）
	strategy.OnWordCommitted("a", "啊")
}

func TestAutoLearning_CustomConfig(t *testing.T) {
	ls := &LearningSpec{
		AutoLearn: &AutoLearnSpec{
			Enabled:        true,
			CountThreshold: 5,
			MinWordLength:  3,
			WeightDelta:    30,
			AddWeight:      1000,
		},
	}
	config := ls.GetAutoLearnConfig()

	if config.CountThreshold != 5 {
		t.Errorf("CountThreshold 应为 5, 实际=%d", config.CountThreshold)
	}
	if config.MinWordLength != 3 {
		t.Errorf("MinWordLength 应为 3, 实际=%d", config.MinWordLength)
	}
	if config.WeightDelta != 30 {
		t.Errorf("WeightDelta 应为 30, 实际=%d", config.WeightDelta)
	}
	if config.AddWeight != 1000 {
		t.Errorf("AddWeight 应为 1000, 实际=%d", config.AddWeight)
	}
}

func TestLearningSpec_EnabledFlags(t *testing.T) {
	// 都不启用
	ls := &LearningSpec{}
	if ls.IsAutoLearnEnabled() {
		t.Error("空配置不应启用自动造词")
	}
	if ls.IsFreqEnabled() {
		t.Error("空配置不应启用调频")
	}

	// 仅调频
	ls = &LearningSpec{Freq: &FreqSpec{Enabled: true}}
	if ls.IsAutoLearnEnabled() {
		t.Error("仅调频不应启用自动造词")
	}
	if !ls.IsFreqEnabled() {
		t.Error("应启用调频")
	}

	// 两者都启用
	ls = &LearningSpec{
		AutoLearn: &AutoLearnSpec{Enabled: true},
		Freq:      &FreqSpec{Enabled: true, HalfLife: 48},
	}
	if !ls.IsAutoLearnEnabled() {
		t.Error("应启用自动造词")
	}
	if !ls.IsFreqEnabled() {
		t.Error("应启用调频")
	}
}

func TestLearningSpec_GetAutoLearnConfig_Defaults(t *testing.T) {
	ls := &LearningSpec{
		AutoLearn: &AutoLearnSpec{Enabled: true},
	}
	config := ls.GetAutoLearnConfig()

	if config.CountThreshold != 2 {
		t.Errorf("默认 CountThreshold 应为 2, 实际=%d", config.CountThreshold)
	}
	if config.MinWordLength != 2 {
		t.Errorf("默认 MinWordLength 应为 2, 实际=%d", config.MinWordLength)
	}
	if config.WeightDelta != 20 {
		t.Errorf("默认 WeightDelta 应为 20, 实际=%d", config.WeightDelta)
	}
	if config.AddWeight != 800 {
		t.Errorf("默认 AddWeight 应为 800, 实际=%d", config.AddWeight)
	}
}

func TestLearningSpec_GetFreqProfile_Defaults(t *testing.T) {
	ls := &LearningSpec{}
	p := ls.GetFreqProfile()

	if p.DecayHalfLife != 72 {
		t.Errorf("默认 DecayHalfLife 应为 72, 实际=%f", p.DecayHalfLife)
	}
	if p.BoostMax != 2000 {
		t.Errorf("默认 BoostMax 应为 2000, 实际=%d", p.BoostMax)
	}
}

func TestLearningSpec_GetFreqProfile_Custom(t *testing.T) {
	ls := &LearningSpec{
		Freq: &FreqSpec{
			Enabled:  true,
			HalfLife: 48,
			BoostMax: 1500,
		},
	}
	p := ls.GetFreqProfile()

	if p.DecayHalfLife != 48 {
		t.Errorf("DecayHalfLife 应为 48, 实际=%f", p.DecayHalfLife)
	}
	if p.BoostMax != 1500 {
		t.Errorf("BoostMax 应为 1500, 实际=%d", p.BoostMax)
	}
	// 未设置的字段保持默认
	if p.MaxRecency != 100 {
		t.Errorf("MaxRecency 应为默认 100, 实际=%f", p.MaxRecency)
	}
}
