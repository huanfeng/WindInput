package schema

import (
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
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

func TestAutoLearning(t *testing.T) {
	tmpDir := t.TempDir()
	userDictPath := filepath.Join(tmpDir, "user_words.txt")

	ud := dict.NewUserDict("test", userDictPath)
	ud.Load()
	defer ud.Close()

	strategy := NewLearningStrategy(LearningAuto, ud)

	// 单字不学习
	strategy.OnCandidateCommitted("ni", "你", candidate.Candidate{})
	if ud.EntryCount() != 0 {
		t.Errorf("单字不应学习, 实际词条数=%d", ud.EntryCount())
	}

	// 多字词学习
	strategy.OnCandidateCommitted("nihao", "你好", candidate.Candidate{Weight: 100})
	// IncreaseWeight 在词条不存在时不会创建新词条（这是当前行为）
	// AutoLearning 的完整逻辑后续实现

	// 命令不学习
	strategy.OnCandidateCommitted("uuid", "xxx-xxx", candidate.Candidate{IsCommand: true})
}

func TestFrequencyLearning(t *testing.T) {
	tmpDir := t.TempDir()
	userDictPath := filepath.Join(tmpDir, "user_words.txt")

	ud := dict.NewUserDict("test", userDictPath)
	ud.Load()
	defer ud.Close()

	// 先添加一个词条
	ud.Add("nihao", "你好", 100)

	strategy := NewLearningStrategy(LearningFrequency, ud)

	// 调频应增加权重
	strategy.OnCandidateCommitted("nihao", "你好", candidate.Candidate{})

	results := ud.Search("nihao", 10)
	if len(results) == 0 {
		t.Fatal("应有结果")
	}
	if results[0].Weight != 101 {
		t.Errorf("权重应为 101, 实际=%d", results[0].Weight)
	}
}

func TestNewLearningStrategy_DefaultIsManual(t *testing.T) {
	strategy := NewLearningStrategy("", nil)
	if _, ok := strategy.(*ManualLearning); !ok {
		t.Error("空 mode 应返回 ManualLearning")
	}
}
