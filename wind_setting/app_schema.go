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
	EngineType  string `json:"engine_type"`     // codetable | pinyin | mixed（从 engine.type 读取）
	Error       string `json:"error,omitempty"` // 验证错误信息，非空表示方案异常
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
	Mixed      map[string]interface{} `yaml:"mixed,omitempty" json:"mixed,omitempty"`
	FilterMode string                 `yaml:"filter_mode" json:"filter_mode"`
}

// SchemaConfigDict 词库配置项
type SchemaConfigDict struct {
	ID         string      `yaml:"id" json:"id"`
	Path       string      `yaml:"path" json:"path"`
	Type       string      `yaml:"type" json:"type"`
	Default    bool        `yaml:"default" json:"default"`
	Role       string      `yaml:"role,omitempty" json:"role,omitempty"`
	WeightSpec interface{} `yaml:"weight_spec,omitempty" json:"weight_spec,omitempty"`
}

// SchemaConfigUserData 用户数据配置
type SchemaConfigUserData struct {
	ShadowFile   string `yaml:"shadow_file" json:"shadow_file"`
	UserDictFile string `yaml:"user_dict_file" json:"user_dict_file"`
	TempDictFile string `yaml:"temp_dict_file,omitempty" json:"temp_dict_file,omitempty"`
	UserFreqFile string `yaml:"user_freq_file,omitempty" json:"user_freq_file,omitempty"`
}

// SchemaConfigLearning 学习策略配置
type SchemaConfigLearning struct {
	Mode             string `yaml:"mode" json:"mode"`
	UnigramPath      string `yaml:"unigram_path,omitempty" json:"unigram_path,omitempty"`
	ProtectTopN      int    `yaml:"protect_top_n,omitempty" json:"protect_top_n,omitempty"`
	TempMaxEntries   int    `yaml:"temp_max_entries,omitempty" json:"temp_max_entries,omitempty"`
	TempPromoteCount int    `yaml:"temp_promote_count,omitempty" json:"temp_promote_count,omitempty"`
}

// SchemaConfig 完整方案配置（YAML 结构，前端可直接编辑）
type SchemaConfig struct {
	Schema   SchemaConfigMeta     `yaml:"schema" json:"schema"`
	Engine   SchemaConfigEngine   `yaml:"engine" json:"engine"`
	Dicts    []SchemaConfigDict   `yaml:"dictionaries" json:"dictionaries"`
	UserData SchemaConfigUserData `yaml:"user_data" json:"user_data"`
	Learning SchemaConfigLearning `yaml:"learning" json:"learning"`
	// 以下字段由 wind_input 核心使用，设置界面不编辑但保存时必须保留
	Encoder interface{} `yaml:"encoder,omitempty" json:"encoder,omitempty"`
}

// GetAvailableSchemas 获取所有可用的输入方案列表
// 每个方案会进行轻量级验证（引擎类型、词典文件是否存在等），
// 异常方案的 Error 字段会包含错误描述。
func (a *App) GetAvailableSchemas() ([]SchemaInfo, error) {
	exeDir := getExeDir()
	configDir, err := config.GetConfigDir()
	if err != nil {
		configDir = ""
	}

	schemas := make(map[string]SchemaInfo)

	// 先加载内置方案（exeDir/data/schemas）
	builtinDir := filepath.Join(exeDir, "data", "schemas")
	loadSchemaInfoFromDir(builtinDir, schemas)

	// 再加载用户方案（覆盖同 ID）
	userDir := ""
	if configDir != "" {
		userDir = filepath.Join(configDir, "schemas")
		loadSchemaInfoFromDir(userDir, schemas)
	}

	// 对每个方案进行资源验证（词典文件是否存在）
	for id, s := range schemas {
		if errMsg := validateSchemaResources(s.ID, exeDir, configDir); errMsg != "" {
			s.Error = errMsg
			schemas[id] = s
		}
	}

	result := make([]SchemaInfo, 0, len(schemas))
	for _, s := range schemas {
		result = append(result, s)
	}
	return result, nil
}

// GetSchemaConfig 获取指定方案的完整配置
// 使用合并读取：先加载内置方案作为基础，再叠加用户方案覆盖
func (a *App) GetSchemaConfig(schemaID string) (*SchemaConfig, error) {
	var cfg SchemaConfig

	// Layer 1: 加载内置方案作为基础
	builtinPath, builtinErr := findBuiltinSchemaFile(schemaID)
	if builtinErr == nil {
		data, err := os.ReadFile(builtinPath)
		if err != nil {
			return nil, fmt.Errorf("读取内置方案文件失败: %w", err)
		}
		if err := yaml.Unmarshal(data, &cfg); err != nil {
			return nil, fmt.Errorf("解析内置方案文件失败: %w", err)
		}
	}

	// Layer 2: 叠加用户方案覆盖（缺失字段保留内置值）
	userPath, userErr := findUserSchemaFile(schemaID)
	if userErr == nil {
		data, err := os.ReadFile(userPath)
		if err != nil {
			return nil, fmt.Errorf("读取用户方案文件失败: %w", err)
		}
		if err := yaml.Unmarshal(data, &cfg); err != nil {
			return nil, fmt.Errorf("解析用户方案文件失败: %w", err)
		}
	}

	// 内置和用户方案都不存在
	if builtinErr != nil && userErr != nil {
		return nil, fmt.Errorf("方案文件不存在: %s", schemaID)
	}

	return &cfg, nil
}

// SaveSchemaConfig 保存方案配置（写入用户目录的方案文件）
// 使用 diff 保存：只将与内置方案不同的字段写入用户文件，
// 使未修改的字段能自动跟随内置方案的更新。
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

	path := filepath.Join(userSchemaDir, schemaID+".schema.yaml")

	// 尝试加载内置方案作为 diff 基准
	var data []byte
	builtinPath, builtinErr := findBuiltinSchemaFile(schemaID)
	if builtinErr == nil {
		builtinData, err := os.ReadFile(builtinPath)
		if err == nil {
			var baseCfg SchemaConfig
			if err := yaml.Unmarshal(builtinData, &baseCfg); err == nil {
				diff, err := config.ComputeYAMLDiff(&baseCfg, cfg)
				if err == nil {
					// 确保 schema.id 始终存在（合并时需要用 ID 匹配内置方案）
					ensureSchemaID(diff, schemaID)
					data, err = yaml.Marshal(diff)
					if err != nil {
						data = nil // 回退到全量保存
					}
				}
			}
		}
	}

	// diff 失败或无内置方案（用户自定义方案）：全量保存
	if data == nil {
		var err error
		data, err = yaml.Marshal(cfg)
		if err != nil {
			return fmt.Errorf("序列化方案配置失败: %w", err)
		}
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

// ensureSchemaID 确保 diff map 中包含 schema.id 字段
func ensureSchemaID(diff map[string]interface{}, schemaID string) {
	schemaMap, ok := diff["schema"].(map[string]interface{})
	if !ok {
		schemaMap = make(map[string]interface{})
		diff["schema"] = schemaMap
	}
	schemaMap["id"] = schemaID
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

	validEngineTypes := map[string]bool{
		"codetable": true, "pinyin": true, "mixed": true,
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

		info := SchemaInfo{
			ID:          cfg.Schema.ID,
			Name:        cfg.Schema.Name,
			IconLabel:   cfg.Schema.IconLabel,
			Version:     cfg.Schema.Version,
			Description: cfg.Schema.Description,
			EngineType:  cfg.Engine.Type,
		}

		// 结构验证：引擎类型
		if cfg.Engine.Type == "" {
			info.Error = "engine.type 未配置"
		} else if !validEngineTypes[cfg.Engine.Type] {
			info.Error = fmt.Sprintf("engine.type 不支持: %s", cfg.Engine.Type)
		}

		// 结构验证：混输引用式方案可以没有词库
		isMixedRef := cfg.Engine.Type == "mixed" && cfg.Engine.Mixed != nil &&
			(cfg.Engine.Mixed["primary_schema"] != nil || cfg.Engine.Mixed["secondary_schema"] != nil)
		if len(cfg.Dicts) == 0 && !isMixedRef && info.Error == "" {
			info.Error = "未配置词库"
		}

		schemas[cfg.Schema.ID] = info
	}
}

// validateSchemaResources 验证方案引用的词典文件是否存在
// 返回空字符串表示验证通过，否则返回错误描述
func validateSchemaResources(schemaID, exeDir, configDir string) string {
	path, err := findSchemaFile(schemaID)
	if err != nil {
		return "方案文件不存在"
	}

	data, err := os.ReadFile(path)
	if err != nil {
		return "无法读取方案文件"
	}

	var cfg SchemaConfig
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return fmt.Sprintf("方案文件解析失败: %s", err.Error())
	}

	// 混输引用式方案通过引用其他方案获取词库，不需要检查词典文件
	isMixedRef := cfg.Engine.Type == "mixed" && cfg.Engine.Mixed != nil &&
		(cfg.Engine.Mixed["primary_schema"] != nil || cfg.Engine.Mixed["secondary_schema"] != nil)
	if isMixedRef {
		return ""
	}

	// 检查每个词典文件是否存在
	exeDataDir := filepath.Join(exeDir, "data")
	var missing []string
	for _, d := range cfg.Dicts {
		if d.Path == "" {
			continue
		}
		if !resolveDictFileExists(d.Path, exeDataDir, configDir) {
			missing = append(missing, d.Path)
		}
	}

	if len(missing) > 0 {
		if len(missing) == 1 {
			return fmt.Sprintf("词典文件不存在: %s", missing[0])
		}
		return fmt.Sprintf("缺少 %d 个词典文件", len(missing))
	}
	return ""
}

// resolveDictFileExists 检查词典文件是否存在（与 wind_input 的 resolvePath 逻辑一致）
func resolveDictFileExists(dictPath, exeDataDir, configDir string) bool {
	if filepath.IsAbs(dictPath) {
		_, err := os.Stat(dictPath)
		return err == nil
	}

	// 按优先级在多个目录中查找
	searchDirs := make([]string, 0, 4)
	if exeDataDir != "" {
		searchDirs = append(searchDirs, exeDataDir, filepath.Join(exeDataDir, "schemas"))
	}
	if configDir != "" {
		searchDirs = append(searchDirs, configDir, filepath.Join(configDir, "schemas"))
	}
	for _, dir := range searchDirs {
		candidate := filepath.Join(dir, dictPath)
		if _, err := os.Stat(candidate); err == nil {
			return true
		}
	}
	return false
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

	// 回退到程序数据目录
	exeDir := getExeDir()
	exePath := filepath.Join(exeDir, "data", "schemas", filename)
	if _, err := os.Stat(exePath); err == nil {
		return exePath, nil
	}

	return "", fmt.Errorf("方案文件不存在: %s", schemaID)
}

// findBuiltinSchemaFile 查找内置方案文件（程序数据目录）
func findBuiltinSchemaFile(schemaID string) (string, error) {
	filename := schemaID + ".schema.yaml"
	exeDir := getExeDir()
	exePath := filepath.Join(exeDir, "data", "schemas", filename)
	if _, err := os.Stat(exePath); err == nil {
		return exePath, nil
	}
	return "", fmt.Errorf("内置方案文件不存在: %s", schemaID)
}

// findUserSchemaFile 查找用户方案文件（用户配置目录）
func findUserSchemaFile(schemaID string) (string, error) {
	filename := schemaID + ".schema.yaml"
	configDir, err := config.GetConfigDir()
	if err != nil {
		return "", err
	}
	userPath := filepath.Join(configDir, "schemas", filename)
	if _, err := os.Stat(userPath); err == nil {
		return userPath, nil
	}
	return "", fmt.Errorf("用户方案文件不存在: %s", schemaID)
}

func getExeDir() string {
	exe, err := os.Executable()
	if err != nil {
		return "."
	}
	return filepath.Dir(exe)
}

// SchemaReference 方案引用关系
type SchemaReference struct {
	PrimarySchema    string   `json:"primary_schema,omitempty"`     // 引用的主形码方案
	SecondarySchema  string   `json:"secondary_schema,omitempty"`   // 引用的拼音方案
	TempPinyinSchema string   `json:"temp_pinyin_schema,omitempty"` // 临时拼音引用的方案
	ReferencedBy     []string `json:"referenced_by,omitempty"`      // 被哪些方案引用
}

// GetSchemaReferences 获取所有方案的引用关系
// 返回 map[schemaID]SchemaReference
func (a *App) GetSchemaReferences() (map[string]SchemaReference, error) {
	// 加载所有方案
	allSchemas, err := a.GetAvailableSchemas()
	if err != nil {
		return nil, err
	}

	refs := make(map[string]SchemaReference)
	// 初始化每个方案的引用信息
	for _, s := range allSchemas {
		refs[s.ID] = SchemaReference{}
	}

	// 扫描所有方案的配置文件，查找引用关系
	for _, s := range allSchemas {
		if s.EngineType != "mixed" {
			continue
		}
		cfg, err := a.GetSchemaConfig(s.ID)
		if err != nil {
			continue
		}
		if cfg.Engine.Mixed == nil {
			continue
		}

		primaryID, _ := cfg.Engine.Mixed["primary_schema"].(string)
		secondaryID, _ := cfg.Engine.Mixed["secondary_schema"].(string)

		if primaryID == "" && secondaryID == "" {
			continue
		}

		// 设置混输方案的引用信息
		ref := refs[s.ID]
		ref.PrimarySchema = primaryID
		ref.SecondarySchema = secondaryID
		refs[s.ID] = ref

		// 设置被引用方案的反向引用
		if primaryID != "" {
			pRef := refs[primaryID]
			pRef.ReferencedBy = append(pRef.ReferencedBy, s.ID)
			refs[primaryID] = pRef
		}
		if secondaryID != "" {
			sRef := refs[secondaryID]
			sRef.ReferencedBy = append(sRef.ReferencedBy, s.ID)
			refs[secondaryID] = sRef
		}
	}

	// 检查 codetable 方案的临时拼音引用
	for _, s := range allSchemas {
		if s.EngineType != "codetable" {
			continue
		}
		cfg, err := a.GetSchemaConfig(s.ID)
		if err != nil {
			continue
		}
		if cfg.Engine.CodeTable == nil {
			continue
		}
		if tp, ok := cfg.Engine.CodeTable["temp_pinyin"].(map[string]interface{}); ok {
			if tpSchema, ok := tp["schema"].(string); ok && tpSchema != "" {
				ref := refs[s.ID]
				ref.TempPinyinSchema = tpSchema
				refs[s.ID] = ref

				// 反向引用
				tpRef := refs[tpSchema]
				tpRef.ReferencedBy = append(tpRef.ReferencedBy, s.ID)
				refs[tpSchema] = tpRef
			}
		}
	}

	return refs, nil
}

// GetReferencedSchemaIDs 获取所有被混输方案引用的方案ID
// 返回那些不在 available 列表中但被引用的方案ID
func (a *App) GetReferencedSchemaIDs() ([]string, error) {
	refs, err := a.GetSchemaReferences()
	if err != nil {
		return nil, err
	}

	// 获取当前 available 列表
	cfg, err := config.Load()
	if err != nil {
		return nil, err
	}
	availableSet := make(map[string]bool)
	for _, id := range cfg.Schema.Available {
		availableSet[id] = true
	}

	// 找出被引用但不在 available 中的方案
	var result []string
	for _, ref := range refs {
		if ref.PrimarySchema != "" && !availableSet[ref.PrimarySchema] {
			result = append(result, ref.PrimarySchema)
			availableSet[ref.PrimarySchema] = true // 去重
		}
		if ref.SecondarySchema != "" && !availableSet[ref.SecondarySchema] {
			result = append(result, ref.SecondarySchema)
			availableSet[ref.SecondarySchema] = true
		}
		if ref.TempPinyinSchema != "" && !availableSet[ref.TempPinyinSchema] {
			result = append(result, ref.TempPinyinSchema)
			availableSet[ref.TempPinyinSchema] = true
		}
	}
	return result, nil
}
