package main

import (
	"encoding/json"
	"fmt"

	"github.com/huanfeng/wind_input/pkg/config"
)

// SaveConfigResult 保存配置的结果（RequiresRestart=true 表示 advanced 变更需重启生效）
type SaveConfigResult struct {
	RequiresRestart bool `json:"requires_restart"`
}

// GetConfig 获取配置
func (a *App) GetConfig() (*config.Config, error) {
	if a.rpcClient == nil {
		return nil, fmt.Errorf("RPC client not initialized")
	}
	reply, err := a.rpcClient.ConfigGetAll()
	if err != nil {
		return nil, err
	}
	var cfg config.Config
	if err := json.Unmarshal(reply.Config, &cfg); err != nil {
		return nil, fmt.Errorf("unmarshal config: %w", err)
	}
	return &cfg, nil
}

// SaveConfig 保存配置，返回是否需要重启
func (a *App) SaveConfig(cfg *config.Config) (*SaveConfigResult, error) {
	if a.rpcClient == nil {
		return nil, fmt.Errorf("RPC client not initialized")
	}
	data, err := json.Marshal(cfg)
	if err != nil {
		return nil, fmt.Errorf("marshal config: %w", err)
	}
	reply, err := a.rpcClient.ConfigSetAll(data)
	if err != nil {
		return nil, err
	}
	return &SaveConfigResult{RequiresRestart: reply.RequiresRestart}, nil
}

// GetDefaultConfig 获取系统默认配置
func (a *App) GetDefaultConfig() (*config.Config, error) {
	if a.rpcClient == nil {
		return nil, fmt.Errorf("RPC client not initialized")
	}
	reply, err := a.rpcClient.ConfigGetDefaults()
	if err != nil {
		return nil, err
	}
	var cfg config.Config
	if err := json.Unmarshal(reply.Config, &cfg); err != nil {
		return nil, fmt.Errorf("unmarshal defaults: %w", err)
	}
	return &cfg, nil
}

// ReloadConfig 重新从服务端拉取一次配置以验证连接（服务内存即为权威状态，无需触发磁盘重读）
func (a *App) ReloadConfig() error {
	if a.rpcClient == nil {
		return fmt.Errorf("RPC client not initialized")
	}
	_, err := a.rpcClient.ConfigGetAll()
	return err
}
