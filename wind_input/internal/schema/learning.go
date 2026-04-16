package schema

import (
	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
)

// LearningStrategy 学习策略接口
// 由方案配置中的 learning.mode 决定使用哪种策略
type LearningStrategy interface {
	// OnCandidateCommitted 用户提交候选词时的回调
	OnCandidateCommitted(code, text string, cand candidate.Candidate)

	// Reset 重置学习状态
	Reset()
}

// ManualLearning 手动学习策略（codetable 类型默认）
// 不自动造词，用户通过右键菜单操作 Shadow
type ManualLearning struct{}

func (m *ManualLearning) OnCandidateCommitted(code, text string, cand candidate.Candidate) {
	// 手动模式不自动学词
}

func (m *ManualLearning) Reset() {}

// AutoLearning 自动学习策略（pinyin 类型默认）
// 选词即学，记录到临时词库（如有），否则记录到用户词库
type AutoLearning struct {
	userLayer *dict.StoreUserLayer
	tempLayer *dict.StoreTempLayer
}

func NewAutoLearning(userLayer *dict.StoreUserLayer) *AutoLearning {
	return &AutoLearning{userLayer: userLayer}
}

// SetTempLayer 设置临时词库层（自动学习优先写入临时词库）
func (a *AutoLearning) SetTempLayer(tl *dict.StoreTempLayer) {
	a.tempLayer = tl
}

func (a *AutoLearning) OnCandidateCommitted(code, text string, cand candidate.Candidate) {
	if cand.IsCommand {
		return
	}
	// 仅学习多字词
	if len([]rune(text)) < 2 {
		return
	}

	// 优先写入临时词库
	if a.tempLayer != nil {
		promoted := a.tempLayer.LearnWord(code, text, 10)
		if promoted {
			// 达到晋升条件，自动迁移到用户词库
			a.tempLayer.PromoteWord(code, text)
		}
		return
	}

	// 没有临时词库时，直接写入用户词库
	if a.userLayer != nil {
		a.userLayer.IncreaseWeight(code, text, 10)
	}
}

func (a *AutoLearning) Reset() {}

// FrequencyLearning 仅调频策略
// 不造新词，仅增加已有词条的选择频次
type FrequencyLearning struct {
	userLayer *dict.StoreUserLayer
}

func NewFrequencyLearning(userLayer *dict.StoreUserLayer) *FrequencyLearning {
	return &FrequencyLearning{userLayer: userLayer}
}

func (f *FrequencyLearning) OnCandidateCommitted(code, text string, cand candidate.Candidate) {
	if f.userLayer == nil || cand.IsCommand {
		return
	}
	f.userLayer.IncreaseWeight(code, text, 1)
}

func (f *FrequencyLearning) Reset() {}

// NewLearningStrategy 根据方案配置创建学习策略
func NewLearningStrategy(mode LearningMode, userLayer *dict.StoreUserLayer) LearningStrategy {
	switch mode {
	case LearningAuto:
		return NewAutoLearning(userLayer)
	case LearningFrequency:
		return NewFrequencyLearning(userLayer)
	default:
		return &ManualLearning{}
	}
}
