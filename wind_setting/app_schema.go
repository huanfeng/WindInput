package main

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/huanfeng/wind_input/pkg/config"
	"gopkg.in/yaml.v3"
)

// SchemaInfo 方案基本信息（前端展示用）
type SchemaInfo struct {
	ID          string `json:"id" yaml:"id"`
	Name        string `json:"name" yaml:"name"`
	IconLabel   string `json:"icon_label" yaml:"icon_label"`
	Version     string `json:"version" yaml:"version"`
	Description string `json:"description" yaml:"description"`
	EngineType  string `json:"engine_type"` // codetable | pinyin（从 engine.type 读取）
}

// SchemaConfigMeta 方案元信息
type SchemaConfigMeta struct {
	ID          string `yaml:"id" json:"id"`
	Name        string `yaml:"name" json:"name"`
	IconLabel   string `yaml:"icon_label" json:"icon_label"`
	Version     string `yaml:"version" json:"version"`
	Author      string `yaml:"author" json:"author"`
	Description string `yaml:"description" json:"description"`
}

// SchemaConfigEngine 引擎配置
type SchemaConfigEngine struct {
	Type       string                 `yaml:"type" json:"type"`
	CodeTable  map[string]interface{} `yaml:"codetable,omitempty" json:"codetable,omitempty"`
	Pinyin     map[string]interface{} `yaml:"pinyin,omitempty" json:"pinyin,omitempty"`
	FilterMode string                 `yaml:"filter_mode" json:"filter_mode"`
}

// SchemaConfigDict 词库配置项
type SchemaConfigDict struct {
	ID      string `yaml:"id" json:"id"`
	Path    string `yaml:"path" json:"path"`
	Type    string `yaml:"type" json:"type"`
	Default bool   `yaml:"default" json:"default"`
	Role    string `yaml:"role,omitempty" json:"role,omitempty"`
}

// SchemaConfigUserData 用户数据配置
type SchemaConfigUserData struct {
	ShadowFile   string `yaml:"shadow_file" json:"shadow_file"`
	UserDictFile string `yaml:"user_dict_file" json:"user_dict_file"`
	UserFreqFile string `yaml:"user_freq_file,omitempty" json:"user_freq_file,omitempty"`
}

// SchemaConfigLearning 学习策略配置
type SchemaConfigLearning struct {
	Mode        string `yaml:"mode" json:"mode"`
	UnigramPath string `yaml:"unigram_path,omitempty" json:"unigram_path,omitempty"`
}

// SchemaConfig 完整方案配置（YAML 结构，前端可直接编辑）
type SchemaConfig struct {
	Schema   SchemaConfigMeta     `yaml:"schema" json:"schema"`
	Engine   SchemaConfigEngine   `yaml:"engine" json:"engine"`
	Dicts    []SchemaConfigDict   `yaml:"dictionaries" json:"dictionaries"`
	UserData SchemaConfigUserData `yaml:"user_data" json:"user_data"`
	Learning SchemaConfigLearning `yaml:"learning" json:"learning"`
}

// GetAvailableSchemas 获取所有可用的输入方案列表
func (a *App) GetAvailableSchemas() ([]SchemaInfo, error) {
	exeDir := getExeDir()
	configDir, err := config.GetConfigDir()
	if err != nil {
		configDir = ""
	}

	schemas := make(map[string]SchemaInfo)

	// 先加载内置方案
	loadSchemaInfoFromDir(filepath.Join(exeDir, "schemas"), schemas)

	// 再加载用户方案（覆盖同 ID）
	if configDir != "" {
		loadSchemaInfoFromDir(filepath.Join(configDir, "schemas"), schemas)
	}

	result := make([]SchemaInfo, 0, len(schemas))
	for _, s := range schemas {
		result = append(result, s)
	}
	return result, nil
}

// GetSchemaConfig 获取指定方案的完整配置
func (a *App) GetSchemaConfig(schemaID string) (*SchemaConfig, error) {
	path, err := findSchemaFile(schemaID)
	if err != nil {
		return nil, err
	}

	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("读取方案文件失败: %w", err)
	}

	var cfg SchemaConfig
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return nil, fmt.Errorf("解析方案文件失败: %w", err)
	}

	return &cfg, nil
}

// SaveSchemaConfig 保存方案配置（写入用户目录的方案文件）
func (a *App) SaveSchemaConfig(schemaID string, cfg *SchemaConfig) error {
	configDir, err := config.GetConfigDir()
	if err != nil {
		return fmt.Errorf("获取配置目录失败: %w", err)
	}

	// 确保用户方案目录存在
	userSchemaDir := filepath.Join(configDir, "schemas")
	if err := os.MkdirAll(userSchemaDir, 0755); err != nil {
		return fmt.Errorf("创建方案目录失败: %w", err)
	}

	// 写入用户目录的方案文件
	path := filepath.Join(userSchemaDir, schemaID+".schema.yaml")
	data, err := yaml.Marshal(cfg)
	if err != nil {
		return fmt.Errorf("序列化方案配置失败: %w", err)
	}

	if err := os.WriteFile(path, data, 0644); err != nil {
		return fmt.Errorf("写入方案文件失败: %w", err)
	}

	// 通知 wind_input 服务重新加载配置
	if a.controlClient != nil {
		a.controlClient.NotifyReload("schema")
	}

	return nil
}

// SwitchActiveSchema 切换活跃方案
func (a *App) SwitchActiveSchema(schemaID string) error {
	// 更新 config.yaml 的 schema.active
	if err := config.UpdateSchemaActive(schemaID); err != nil {
		return fmt.Errorf("更新活跃方案失败: %w", err)
	}

	// 通知 wind_input 服务
	if a.controlClient != nil {
		a.controlClient.NotifyReload("config")
	}

	return nil
}

// --- 内部辅助函数 ---

func loadSchemaInfoFromDir(dir string, schemas map[string]SchemaInfo) {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return
	}

	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".schema.yaml") {
			continue
		}

		path := filepath.Join(dir, entry.Name())
		data, err := os.ReadFile(path)
		if err != nil {
			continue
		}

		var cfg SchemaConfig
		if err := yaml.Unmarshal(data, &cfg); err != nil {
			continue
		}

		if cfg.Schema.ID == "" {
			continue
		}

		schemas[cfg.Schema.ID] = SchemaInfo{
			ID:          cfg.Schema.ID,
			Name:        cfg.Schema.Name,
			IconLabel:   cfg.Schema.IconLabel,
			Version:     cfg.Schema.Version,
			Description: cfg.Schema.Description,
			EngineType:  cfg.Engine.Type,
		}
	}
}

func findSchemaFile(schemaID string) (string, error) {
	filename := schemaID + ".schema.yaml"

	// 优先查找用户目录
	configDir, err := config.GetConfigDir()
	if err == nil {
		userPath := filepath.Join(configDir, "schemas", filename)
		if _, err := os.Stat(userPath); err == nil {
			return userPath, nil
		}
	}

	// 回退到应用目录
	exeDir := getExeDir()
	exePath := filepath.Join(exeDir, "schemas", filename)
	if _, err := os.Stat(exePath); err == nil {
		return exePath, nil
	}

	return "", fmt.Errorf("方案文件不存在: %s", schemaID)
}

func getExeDir() string {
	exe, err := os.Executable()
	if err != nil {
		return "."
	}
	return filepath.Dir(exe)
}
