package main

import (
	"fmt"

	"github.com/huanfeng/wind_input/pkg/config"
)

// ========== 配置管理 ==========

// GetConfig 获取配置
func (a *App) GetConfig() (*config.Config, error) {
	if a.configEditor == nil {
		return nil, fmt.Errorf("config editor not initialized")
	}

	cfg := a.configEditor.GetConfig()
	if cfg == nil {
		return nil, fmt.Errorf("config not loaded")
	}

	return cfg, nil
}

// SaveConfig 保存配置
func (a *App) SaveConfig(cfg *config.Config) error {
	if a.configEditor == nil {
		return fmt.Errorf("config editor not initialized")
	}

	a.configEditor.SetConfig(cfg)
	if err := a.configEditor.Save(); err != nil {
		return err
	}

	// 更新文件监控状态
	a.fileWatcher.UpdateState(a.configEditor.GetFilePath())

	// 通知主程序重载
	go a.NotifyReload("config")

	return nil
}

// CheckConfigModified 检查配置是否被外部修改
func (a *App) CheckConfigModified() (bool, error) {
	if a.configEditor == nil {
		return false, nil
	}
	return a.configEditor.HasChanged()
}

// GetDefaultConfig 获取系统默认配置（代码默认值 + data/config.yaml 合并结果）
func (a *App) GetDefaultConfig() (*config.Config, error) {
	return config.SystemDefaultConfig(), nil
}

// ReloadConfig 重新加载配置（丢弃本地修改）
func (a *App) ReloadConfig() error {
	if a.configEditor == nil {
		return fmt.Errorf("config editor not initialized")
	}
	return a.configEditor.Reload()
}
