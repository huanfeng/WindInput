package main

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

const (
	defaultTSFLogMode  = "none"
	defaultTSFLogLevel = "info"
)

var (
	validTSFLogModes = map[string]struct{}{
		"none":        {},
		"file":        {},
		"debugstring": {},
		"all":         {},
	}
	validTSFLogLevels = map[string]struct{}{
		"off":   {},
		"error": {},
		"warn":  {},
		"info":  {},
		"debug": {},
		"trace": {},
	}
)

type TSFLogConfig struct {
	Mode  string `json:"mode"`
	Level string `json:"level"`
}

func normalizeTSFLogMode(mode string) string {
	mode = strings.ToLower(strings.TrimSpace(mode))
	if _, ok := validTSFLogModes[mode]; ok {
		return mode
	}
	return defaultTSFLogMode
}

func normalizeTSFLogLevel(level string) string {
	level = strings.ToLower(strings.TrimSpace(level))
	if _, ok := validTSFLogLevels[level]; ok {
		return level
	}
	return defaultTSFLogLevel
}

func getTSFLogConfigPath() (string, error) {
	base := os.Getenv("LOCALAPPDATA")
	if base == "" {
		return "", fmt.Errorf("LOCALAPPDATA not set")
	}
	return filepath.Join(base, "WindInput", "logs", "tsf_log_config"), nil
}

func loadTSFLogConfig() (mode, level string, err error) {
	mode = defaultTSFLogMode
	level = defaultTSFLogLevel

	path, err := getTSFLogConfigPath()
	if err != nil {
		return mode, level, err
	}

	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return mode, level, nil
		}
		return mode, level, err
	}

	for _, rawLine := range strings.Split(string(data), "\n") {
		line := strings.TrimSpace(rawLine)
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}

		key, value, ok := strings.Cut(line, "=")
		if !ok {
			continue
		}

		switch strings.TrimSpace(key) {
		case "mode":
			mode = normalizeTSFLogMode(value)
		case "level":
			level = normalizeTSFLogLevel(value)
		}
	}

	return mode, level, nil
}

func saveTSFLogConfig(tsfCfg TSFLogConfig) error {
	path, err := getTSFLogConfigPath()
	if err != nil {
		return err
	}

	mode := normalizeTSFLogMode(tsfCfg.Mode)
	level := normalizeTSFLogLevel(tsfCfg.Level)

	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}

	content := fmt.Sprintf("mode=%s\nlevel=%s\n", mode, level)
	return os.WriteFile(path, []byte(content), 0o644)
}

func (a *App) GetTSFLogConfig() (*TSFLogConfig, error) {
	mode, level, err := loadTSFLogConfig()
	if err != nil {
		return nil, err
	}

	return &TSFLogConfig{
		Mode:  mode,
		Level: level,
	}, nil
}

func (a *App) SaveTSFLogConfig(cfg TSFLogConfig) error {
	return saveTSFLogConfig(cfg)
}
