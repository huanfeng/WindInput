package updater

import (
	"encoding/json"
	"errors"
	"log"
	"os"
	"path/filepath"
)

// Config 存储更新相关设置，独立于 wind_input RPC 配置。
type Config struct {
	NetworkConsent bool `json:"network_consent"` // 用户已同意联网
	AutoCheck      bool `json:"auto_check"`      // 启动时自动检查
	AutoInstall    bool `json:"auto_install"`    // 下载完成后自动安装
}

func configPath() (string, error) {
	dir, err := os.UserConfigDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "WindInput", "update_config.json"), nil
}

// LoadConfig 读取本地更新配置；文件不存在时返回默认值（AutoCheck=true）。
func LoadConfig() Config {
	defaults := Config{AutoCheck: true}
	path, err := configPath()
	if err != nil {
		return defaults
	}
	data, err := os.ReadFile(path)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return defaults
		}
		log.Printf("updater: failed to read config: %v", err)
		return defaults
	}
	var cfg Config
	if err := json.Unmarshal(data, &cfg); err != nil {
		log.Printf("updater: config file is malformed, using defaults: %v", err)
		return defaults
	}
	return cfg
}

// SaveConfig 将更新配置写入磁盘。
func SaveConfig(cfg Config) error {
	path, err := configPath()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
		return err
	}
	data, err := json.MarshalIndent(cfg, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(path, data, 0644)
}
