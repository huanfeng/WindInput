package config

import (
	"fmt"
	"os"

	"gopkg.in/yaml.v3"
)

// RuntimeState 运行时状态（用于记忆前次状态）
type RuntimeState struct {
	ChineseMode  bool   `yaml:"chinese_mode" json:"chinese_mode"`
	FullWidth    bool   `yaml:"full_width" json:"full_width"`
	ChinesePunct bool   `yaml:"chinese_punct" json:"chinese_punct"`
	EngineType   string `yaml:"engine_type" json:"engine_type"`
}

// DefaultRuntimeState 返回默认运行时状态
func DefaultRuntimeState() *RuntimeState {
	return &RuntimeState{
		ChineseMode:  true,
		FullWidth:    false,
		ChinesePunct: true,
		EngineType:   "pinyin",
	}
}

// LoadRuntimeState 加载运行时状态
func LoadRuntimeState() (*RuntimeState, error) {
	statePath, err := GetStatePath()
	if err != nil {
		return DefaultRuntimeState(), err
	}

	data, err := os.ReadFile(statePath)
	if err != nil {
		if os.IsNotExist(err) {
			return DefaultRuntimeState(), nil
		}
		return DefaultRuntimeState(), fmt.Errorf("failed to read state file: %w", err)
	}

	state := DefaultRuntimeState()
	if err := yaml.Unmarshal(data, state); err != nil {
		return DefaultRuntimeState(), fmt.Errorf("failed to parse state file: %w", err)
	}

	return state, nil
}

// SaveRuntimeState 保存运行时状态
func SaveRuntimeState(state *RuntimeState) error {
	if err := EnsureConfigDir(); err != nil {
		return fmt.Errorf("failed to create config dir: %w", err)
	}

	statePath, err := GetStatePath()
	if err != nil {
		return err
	}

	data, err := yaml.Marshal(state)
	if err != nil {
		return fmt.Errorf("failed to marshal state: %w", err)
	}

	if err := os.WriteFile(statePath, data, 0644); err != nil {
		return fmt.Errorf("failed to write state file: %w", err)
	}

	return nil
}
