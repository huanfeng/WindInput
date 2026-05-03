// Package config — 应用兼容性规则
package config

import (
	"os"
	"path/filepath"
	"strings"

	"gopkg.in/yaml.v3"
)

const CompatFileName = "compat.yaml"

// AppCompatRule 定义单个应用的兼容性规则。
type AppCompatRule struct {
	Process          string `yaml:"process"`                      // 进程名（不区分大小写），如 "Weixin.exe"
	Comment          string `yaml:"comment,omitempty"`            // 说明（仅文档用途）
	CaretUseTop      bool   `yaml:"caret_use_top,omitempty"`      // 使用 rect.top 而非 rect.bottom 定位候选框
	SkipCaretPending bool   `yaml:"skip_caret_pending,omitempty"` // 跳过首次 composition 的 CARET_PENDING 等待（光标稳定的应用）
}

// AppCompat 包含所有应用兼容性规则。
type AppCompat struct {
	Apps []AppCompatRule `yaml:"apps"`

	// 运行时查找表（小写进程名 → 规则）
	lookup map[string]*AppCompatRule
}

// GetRule 按进程名查找兼容性规则，未匹配返回 nil。
func (c *AppCompat) GetRule(processName string) *AppCompatRule {
	if c == nil || c.lookup == nil {
		return nil
	}
	return c.lookup[strings.ToLower(processName)]
}

// buildLookup 构建运行时查找表。
func (c *AppCompat) buildLookup() {
	c.lookup = make(map[string]*AppCompatRule, len(c.Apps))
	for i := range c.Apps {
		key := strings.ToLower(c.Apps[i].Process)
		c.lookup[key] = &c.Apps[i]
	}
}

// LoadAppCompat 加载应用兼容性规则，支持系统预置 + 用户覆盖。
// 加载顺序：{exeDir}/data/compat.yaml → {userConfigDir}/compat.yaml
// 用户文件中的规则会覆盖系统预置中同进程名的规则。
func LoadAppCompat() *AppCompat {
	result := &AppCompat{}

	// Layer 1: 系统预置（程序目录/data/compat.yaml）
	exeDir, err := GetExeDir()
	if err == nil {
		sysPath := filepath.Join(GetDataDir(exeDir), CompatFileName)
		if sysCompat, err := loadCompatFile(sysPath); err == nil {
			result.Apps = sysCompat.Apps
		}
	}

	// Layer 2: 用户覆盖（%APPDATA%\WindInput\compat.yaml）
	configDir, err := GetConfigDir()
	if err == nil {
		userPath := filepath.Join(configDir, CompatFileName)
		if userCompat, err := loadCompatFile(userPath); err == nil {
			result.Apps = mergeCompatRules(result.Apps, userCompat.Apps)
		}
	}

	result.buildLookup()
	return result
}

// loadCompatFile 从指定路径加载兼容性规则文件。
func loadCompatFile(path string) (*AppCompat, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var compat AppCompat
	if err := yaml.Unmarshal(data, &compat); err != nil {
		return nil, err
	}
	return &compat, nil
}

// ToggleUserSkipCaretPending 切换用户层 compat.yaml 中指定进程的 skip_caret_pending
// 标志，并返回新值。文件不存在时自动创建。
func ToggleUserSkipCaretPending(processName string) (bool, error) {
	configDir, err := GetConfigDir()
	if err != nil {
		return false, err
	}
	userPath := filepath.Join(configDir, CompatFileName)

	var userCompat AppCompat
	if data, err := os.ReadFile(userPath); err == nil {
		_ = yaml.Unmarshal(data, &userCompat)
	}

	key := strings.ToLower(processName)
	newValue := true
	found := false
	for i, r := range userCompat.Apps {
		if strings.ToLower(r.Process) == key {
			userCompat.Apps[i].SkipCaretPending = !r.SkipCaretPending
			newValue = userCompat.Apps[i].SkipCaretPending
			found = true
			break
		}
	}
	if !found {
		userCompat.Apps = append(userCompat.Apps, AppCompatRule{
			Process:          processName,
			SkipCaretPending: true,
		})
	}

	data, err := yaml.Marshal(&userCompat)
	if err != nil {
		return false, err
	}
	if err := os.MkdirAll(configDir, 0755); err != nil {
		return false, err
	}
	if err := os.WriteFile(userPath, data, 0644); err != nil {
		return false, err
	}
	return newValue, nil
}

// mergeCompatRules 合并两组规则，user 中的同名进程规则覆盖 base 中的。
func mergeCompatRules(base, user []AppCompatRule) []AppCompatRule {
	if len(user) == 0 {
		return base
	}
	// 用 user 的进程名建索引
	userMap := make(map[string]int, len(user))
	for i, r := range user {
		userMap[strings.ToLower(r.Process)] = i
	}

	// 保留 base 中未被 user 覆盖的规则
	var merged []AppCompatRule
	for _, r := range base {
		if _, overridden := userMap[strings.ToLower(r.Process)]; !overridden {
			merged = append(merged, r)
		}
	}
	// 追加所有 user 规则
	merged = append(merged, user...)
	return merged
}
