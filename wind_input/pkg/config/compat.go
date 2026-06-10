// Package config — 应用兼容性规则
package config

import (
	"os"
	"path/filepath"
	"strings"

	"gopkg.in/yaml.v3"
)

const (
	CompatFileName       = "compat.toml"
	LegacyCompatFileName = "compat.yaml" // 旧版文件名，双读回退用
)

// AppCompatRule 定义单个应用的兼容性规则。
type AppCompatRule struct {
	Process              string `yaml:"process"`                          // 进程名（不区分大小写），如 "Weixin.exe"
	Comment              string `yaml:"comment,omitempty"`                // 说明（仅文档用途）
	CaretUseTop          bool   `yaml:"caret_use_top,omitempty"`          // 使用 rect.top 而非 rect.bottom 定位候选框
	SkipCaretPending     bool   `yaml:"skip_caret_pending,omitempty"`     // 跳过首次 composition 的 CARET_PENDING 等待（光标稳定的应用）
	PinCandidatePosition bool   `yaml:"pin_candidate_position,omitempty"` // 固定候选窗位置：拖动后位置持久化记忆，跨会话恢复（开关，坐标存于 state.yaml）
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
// 加载顺序：{exeDir}/data/compat.toml → {userConfigDir}/compat.toml
// （各层均兼容旧版 .yaml 回退）。用户文件中的规则会覆盖系统预置中同进程名的规则。
func LoadAppCompat() *AppCompat {
	result := &AppCompat{}

	// Layer 1: 系统预置（程序目录/data/compat.toml）
	exeDir, err := GetExeDir()
	if err == nil {
		sysPath := filepath.Join(GetDataDir(exeDir), CompatFileName)
		if sysCompat, _, err := loadCompatFile(sysPath); err == nil {
			result.Apps = sysCompat.Apps
		}
	}

	// Layer 2: 用户覆盖（%APPDATA%\WindInput\compat.toml）
	configDir, err := GetConfigDir()
	if err == nil {
		userPath := filepath.Join(configDir, CompatFileName)
		if userCompat, migratedFrom, err := loadCompatFile(userPath); err == nil {
			result.Apps = mergeCompatRules(result.Apps, userCompat.Apps)
			// 旧格式一次性迁移：写出 TOML 成功后把旧文件改名备份
			if migratedFrom != "" {
				if err := saveUserCompat(configDir, userCompat); err == nil {
					renameLegacyFile(migratedFrom)
				}
			}
		}
	}

	result.buildLookup()
	return result
}

// loadCompatFile 从指定路径加载兼容性规则文件（.toml 缺失时回退同名旧版 .yaml）。
// migratedFrom 非空表示数据来自旧版文件。
func loadCompatFile(path string) (*AppCompat, string, error) {
	data, readPath, migratedFrom, err := readFileWithLegacyFallback(path)
	if err != nil {
		return nil, "", err
	}
	yamlData, err := normalizeToYAML(readPath, data)
	if err != nil {
		return nil, "", err
	}
	var compat AppCompat
	if err := yaml.Unmarshal(yamlData, &compat); err != nil {
		return nil, "", err
	}
	return &compat, migratedFrom, nil
}

// saveUserCompat 把用户层兼容性规则写出到 {configDir}/compat.toml。
func saveUserCompat(configDir string, compat *AppCompat) error {
	userPath := filepath.Join(configDir, CompatFileName)
	data, err := marshalForPath(userPath, compat)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(configDir, 0755); err != nil {
		return err
	}
	return os.WriteFile(userPath, data, 0644)
}

// ToggleUserSkipCaretPending 切换用户层 compat.toml 中指定进程的 skip_caret_pending
// 标志，并返回新值。文件不存在时自动创建。
func ToggleUserSkipCaretPending(processName string) (bool, error) {
	return toggleUserCompatFlag(processName,
		func(r *AppCompatRule) bool {
			r.SkipCaretPending = !r.SkipCaretPending
			return r.SkipCaretPending
		},
		AppCompatRule{Process: processName, SkipCaretPending: true})
}

// ToggleUserPinCandidatePosition 切换用户层 compat.toml 中指定进程的 pin_candidate_position
// 标志，并返回新值。文件不存在时自动创建。
func ToggleUserPinCandidatePosition(processName string) (bool, error) {
	return toggleUserCompatFlag(processName,
		func(r *AppCompatRule) bool {
			r.PinCandidatePosition = !r.PinCandidatePosition
			return r.PinCandidatePosition
		},
		AppCompatRule{Process: processName, PinCandidatePosition: true})
}

// toggleUserCompatFlag 读取用户层 compat 文件（兼容旧版 yaml 回退），对指定
// 进程的规则应用 toggle，写出 TOML；数据来自旧版文件时完成一次性迁移。
func toggleUserCompatFlag(processName string, toggle func(*AppCompatRule) bool, newRule AppCompatRule) (bool, error) {
	configDir, err := GetConfigDir()
	if err != nil {
		return false, err
	}
	userPath := filepath.Join(configDir, CompatFileName)

	var userCompat AppCompat
	migratedFrom := ""
	if loaded, from, err := loadCompatFile(userPath); err == nil {
		userCompat = *loaded
		migratedFrom = from
	}

	key := strings.ToLower(processName)
	newValue := true
	found := false
	for i, r := range userCompat.Apps {
		if strings.ToLower(r.Process) == key {
			newValue = toggle(&userCompat.Apps[i])
			found = true
			break
		}
	}
	if !found {
		userCompat.Apps = append(userCompat.Apps, newRule)
	}

	if err := saveUserCompat(configDir, &userCompat); err != nil {
		return false, err
	}
	if migratedFrom != "" {
		renameLegacyFile(migratedFrom)
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
