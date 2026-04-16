package schema

import (
	"testing"

	"github.com/huanfeng/wind_input/internal/candidate"
)

func TestManualLearning(t *testing.T) {
	strategy := NewLearningStrategy(LearningManual, nil)

	// ManualLearning 不应 panic
	strategy.OnCandidateCommitted("test", "测试", candidate.Candidate{})
	strategy.Reset()

	if _, ok := strategy.(*ManualLearning); !ok {
		t.Error("LearningManual 应返回 *ManualLearning")
	}
}

func TestAutoLearning_NilSafe(t *testing.T) {
	strategy := NewLearningStrategy(LearningAuto, nil)

	// nil userLayer 不应 panic
	strategy.OnCandidateCommitted("nihao", "你好", candidate.Candidate{Weight: 100})
	strategy.Reset()
}

func TestFrequencyLearning_NilSafe(t *testing.T) {
	strategy := NewLearningStrategy(LearningFrequency, nil)

	// nil userLayer 不应 panic
	strategy.OnCandidateCommitted("nihao", "你好", candidate.Candidate{})
	strategy.Reset()
}

func TestNewLearningStrategy_DefaultIsManual(t *testing.T) {
	strategy := NewLearningStrategy("", nil)
	if _, ok := strategy.(*ManualLearning); !ok {
		t.Error("空 mode 应返回 ManualLearning")
	}
}
