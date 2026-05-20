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

	// ToolbarPositions 保存每个显示器上用户拖动后的工具栏位置。
	// key = "workRight,workBottom"（显示器工作区右下角坐标），value = [x, y]。
	// 与 remember_last_state 无关，始终持久化。
	ToolbarPositions map[string][2]int `yaml:"toolbar_positions,omitempty" json:"toolbar_positions,omitempty"`

	// CandidatePinPositions 保存启用了 pin_candidate_position 的应用候选窗拖动后的位置。
	// 外层 key = 小写进程名；内层 key = "workRight,workBottom"（显示器工作区右下角），value = [x, y]。
	// 与 remember_last_state 无关，始终持久化。关闭 pin 时该应用的条目会被清空。
	CandidatePinPositions map[string]map[string][2]int `yaml:"candidate_pin_positions,omitempty" json:"candidate_pin_positions,omitempty"`
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
