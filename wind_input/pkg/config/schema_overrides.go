package config

import (
	"fmt"
	"os"
	"path/filepath"
	"sync"

	"gopkg.in/yaml.v3"
)

const (
	SchemaOverridesFile = "schema_overrides.yaml"
)

// overridesMu 文件锁，防止并发读写
var overridesMu sync.Mutex

// getSchemaOverridesPath 返回 schema_overrides.yaml 的完整路径
func getSchemaOverridesPath() (string, error) {
	configDir, err := GetConfigDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(configDir, SchemaOverridesFile), nil
}

// LoadSchemaOverrides 加载所有方案的覆盖配置
// 返回 map[schemaID] -> 覆盖配置 (map[string]any)
// 文件不存在时返回空 map（不报错）
func LoadSchemaOverrides() (map[string]map[string]any, error) {
	overridesMu.Lock()
	defer overridesMu.Unlock()

	return loadSchemaOverridesLocked()
}

// loadSchemaOverridesLocked 在已持有锁的情况下加载覆盖配置
func loadSchemaOverridesLocked() (map[string]map[string]any, error) {
	path, err := getSchemaOverridesPath()
	if err != nil {
		return make(map[string]map[string]any), err
	}

	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return make(map[string]map[string]any), nil
		}
		return make(map[string]map[string]any), fmt.Errorf("failed to read schema overrides file: %w", err)
	}

	if len(data) == 0 {
		return make(map[string]map[string]any), nil
	}

	var raw map[string]map[string]any
	if err := yaml.Unmarshal(data, &raw); err != nil {
		return make(map[string]map[string]any), fmt.Errorf("failed to parse schema overrides file: %w", err)
	}

	if raw == nil {
		return make(map[string]map[string]any), nil
	}

	return raw, nil
}

// SaveSchemaOverrides 保存完整的覆盖配置文件
func SaveSchemaOverrides(overrides map[string]map[string]any) error {
	overridesMu.Lock()
	defer overridesMu.Unlock()

	return saveSchemaOverridesLocked(overrides)
}

// saveSchemaOverridesLocked 在已持有锁的情况下保存覆盖配置（原子写入）
func saveSchemaOverridesLocked(overrides map[string]map[string]any) error {
	if err := EnsureConfigDir(); err != nil {
		return fmt.Errorf("failed to create config dir: %w", err)
	}

	path, err := getSchemaOverridesPath()
	if err != nil {
		return err
	}

	data, err := yaml.Marshal(overrides)
	if err != nil {
		return fmt.Errorf("failed to marshal schema overrides: %w", err)
	}

	// 原子写入：先写临时文件再 rename
	dir := filepath.Dir(path)
	tmp, err := os.CreateTemp(dir, "schema_overrides_*.yaml.tmp")
	if err != nil {
		return fmt.Errorf("failed to create temp file: %w", err)
	}
	tmpPath := tmp.Name()

	_, writeErr := tmp.Write(data)
	closeErr := tmp.Close()

	if writeErr != nil {
		os.Remove(tmpPath)
		return fmt.Errorf("failed to write temp file: %w", writeErr)
	}
	if closeErr != nil {
		os.Remove(tmpPath)
		return fmt.Errorf("failed to close temp file: %w", closeErr)
	}

	if err := os.Rename(tmpPath, path); err != nil {
		os.Remove(tmpPath)
		return fmt.Errorf("failed to rename temp file: %w", err)
	}

	return nil
}

// GetSchemaOverride 获取单个方案的覆盖配置
// 返回 nil, nil 表示该方案无覆盖
func GetSchemaOverride(schemaID string) (map[string]any, error) {
	overridesMu.Lock()
	defer overridesMu.Unlock()

	overrides, err := loadSchemaOverridesLocked()
	if err != nil {
		return nil, err
	}

	override, exists := overrides[schemaID]
	if !exists {
		return nil, nil
	}

	return override, nil
}

// SetSchemaOverride 设置单个方案的覆盖配置
// override 为整个方案的覆盖 map，直接替换该 schemaID 的条目
func SetSchemaOverride(schemaID string, override map[string]any) error {
	overridesMu.Lock()
	defer overridesMu.Unlock()

	overrides, err := loadSchemaOverridesLocked()
	if err != nil {
		return err
	}

	overrides[schemaID] = override

	return saveSchemaOverridesLocked(overrides)
}

// DeleteSchemaOverride 删除单个方案的覆盖配置（恢复默认）
// 如果删除后文件为空，则删除文件本身
func DeleteSchemaOverride(schemaID string) error {
	overridesMu.Lock()
	defer overridesMu.Unlock()

	overrides, err := loadSchemaOverridesLocked()
	if err != nil {
		return err
	}

	delete(overrides, schemaID)

	// 删除后为空则删除文件本身
	if len(overrides) == 0 {
		path, err := getSchemaOverridesPath()
		if err != nil {
			return err
		}
		if err := os.Remove(path); err != nil && !os.IsNotExist(err) {
			return fmt.Errorf("failed to remove schema overrides file: %w", err)
		}
		return nil
	}

	return saveSchemaOverridesLocked(overrides)
}
