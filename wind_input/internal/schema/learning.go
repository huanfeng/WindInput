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
// 选词即学，记录到用户词库
// TODO: 后续完善权重策略、遗忘机制、噪音过滤等
type AutoLearning struct {
	userDict *dict.UserDict
}

func NewAutoLearning(userDict *dict.UserDict) *AutoLearning {
	return &AutoLearning{userDict: userDict}
}

func (a *AutoLearning) OnCandidateCommitted(code, text string, cand candidate.Candidate) {
	if a.userDict == nil || cand.IsCommand {
		return
	}
	// 仅学习多字词
	if len([]rune(text)) < 2 {
		return
	}
	// 已存在则增加权重，不存在则新建
	a.userDict.IncreaseWeight(code, text, 10)
}

func (a *AutoLearning) Reset() {}

// FrequencyLearning 仅调频策略
// 不造新词，仅增加已有词条的选择频次
type FrequencyLearning struct {
	userDict *dict.UserDict
}

func NewFrequencyLearning(userDict *dict.UserDict) *FrequencyLearning {
	return &FrequencyLearning{userDict: userDict}
}

func (f *FrequencyLearning) OnCandidateCommitted(code, text string, cand candidate.Candidate) {
	if f.userDict == nil || cand.IsCommand {
		return
	}
	f.userDict.IncreaseWeight(code, text, 1)
}

func (f *FrequencyLearning) Reset() {}

// NewLearningStrategy 根据方案配置创建学习策略
func NewLearningStrategy(mode LearningMode, userDict *dict.UserDict) LearningStrategy {
	switch mode {
	case LearningAuto:
		return NewAutoLearning(userDict)
	case LearningFrequency:
		return NewFrequencyLearning(userDict)
	default:
		return &ManualLearning{}
	}
}
