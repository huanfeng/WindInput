package main

import (
	"archive/zip"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"

	"github.com/bodgit/sevenzip"
	"github.com/huanfeng/wind_input/pkg/config"
	wailsRuntime "github.com/wailsapp/wails/v2/pkg/runtime"
	"gopkg.in/yaml.v3"
)

// archiveEntry 统一的压缩包文件条目接口
type archiveEntry struct {
	Name  string
	IsDir bool
	Open  func() (io.ReadCloser, error)
}

// readZipEntries 从 ZIP 读取所有条目
func readZipEntries(path string) ([]archiveEntry, io.Closer, error) {
	r, err := zip.OpenReader(path)
	if err != nil {
		return nil, nil, err
	}
	entries := make([]archiveEntry, 0, len(r.File))
	for _, f := range r.File {
		f := f
		entries = append(entries, archiveEntry{
			Name:  f.Name,
			IsDir: f.FileInfo().IsDir(),
			Open:  f.Open,
		})
	}
	return entries, r, nil
}

// read7zEntries 从 7z 读取所有条目
func read7zEntries(path string) ([]archiveEntry, io.Closer, error) {
	r, err := sevenzip.OpenReader(path)
	if err != nil {
		return nil, nil, err
	}
	entries := make([]archiveEntry, 0, len(r.File))
	for _, f := range r.File {
		f := f
		entries = append(entries, archiveEntry{
			Name:  f.Name,
			IsDir: f.FileInfo().IsDir(),
			Open:  f.Open,
		})
	}
	return entries, r, nil
}

// openArchive 根据扩展名自动选择解压方式
func openArchive(path string) ([]archiveEntry, io.Closer, error) {
	ext := strings.ToLower(filepath.Ext(path))
	switch ext {
	case ".7z":
		return read7zEntries(path)
	default:
		return readZipEntries(path)
	}
}

// SchemaInfo 方案基本信息（前端展示用）
type SchemaInfo struct {
	ID          string `json:"id" yaml:"id"`
	Name        string `json:"name" yaml:"name"`
	IconLabel   string `json:"icon_label" yaml:"icon_label"`
	Version     string `json:"version" yaml:"version"`
	Description string `json:"description" yaml:"description"`
	EngineType  string `json:"engine_type"`     // codetable | pinyin | mixed（从 engine.type 读取）
	Source      string `json:"source"`          // builtin | user（方案来源）
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

// SchemaConfigAutoLearn 自动造词配置
type SchemaConfigAutoLearn struct {
	Enabled        bool `yaml:"enabled" json:"enabled"`
	CountThreshold int  `yaml:"count_threshold,omitempty" json:"count_threshold,omitempty"`
	MinWordLength  int  `yaml:"min_word_length,omitempty" json:"min_word_length,omitempty"`
	WeightDelta    int  `yaml:"weight_delta,omitempty" json:"weight_delta,omitempty"`
	AddWeight      int  `yaml:"add_weight,omitempty" json:"add_weight,omitempty"`
}

// SchemaConfigFreq 自动调频配置
type SchemaConfigFreq struct {
	Enabled     bool    `yaml:"enabled" json:"enabled"`
	ProtectTopN int     `yaml:"protect_top_n,omitempty" json:"protect_top_n,omitempty"`
	HalfLife    float64 `yaml:"half_life,omitempty" json:"half_life,omitempty"`
	BoostMax    int     `yaml:"boost_max,omitempty" json:"boost_max,omitempty"`
	MaxRecency  float64 `yaml:"max_recency,omitempty" json:"max_recency,omitempty"`
	BaseScale   float64 `yaml:"base_scale,omitempty" json:"base_scale,omitempty"`
	StreakScale float64 `yaml:"streak_scale,omitempty" json:"streak_scale,omitempty"`
	StreakCap   float64 `yaml:"streak_cap,omitempty" json:"streak_cap,omitempty"`
}

// SchemaConfigLearning 学习策略配置
type SchemaConfigLearning struct {
	AutoLearn        *SchemaConfigAutoLearn `yaml:"auto_learn,omitempty" json:"auto_learn,omitempty"`
	Freq             *SchemaConfigFreq      `yaml:"freq,omitempty" json:"freq,omitempty"`
	ProtectTopN      int                    `yaml:"protect_top_n,omitempty" json:"protect_top_n,omitempty"`
	UnigramPath      string                 `yaml:"unigram_path,omitempty" json:"unigram_path,omitempty"`
	TempMaxEntries   int                    `yaml:"temp_max_entries,omitempty" json:"temp_max_entries,omitempty"`
	TempPromoteCount int                    `yaml:"temp_promote_count,omitempty" json:"temp_promote_count,omitempty"`
}

// SchemaConfig 完整方案配置（YAML 结构，前端可直接编辑）
type SchemaConfig struct {
	Schema   SchemaConfigMeta     `yaml:"schema" json:"schema"`
	Engine   SchemaConfigEngine   `yaml:"engine" json:"engine"`
	Dicts    []SchemaConfigDict   `yaml:"dictionaries" json:"dictionaries"`
	Learning SchemaConfigLearning `yaml:"learning" json:"learning"`
	// 以下字段由 wind_input 核心使用，设置界面不编辑但保存时必须保留
	Encoder interface{} `yaml:"encoder,omitempty" json:"encoder,omitempty"`
}

// GetAvailableSchemas 获取所有可用的输入方案列表
// 每个方案会进行轻量级验证（引擎类型、词典文件是否存在等），
// 异常方案的 Error 字段会包含错误描述。
// 使用合并读取：用户方案与内置方案合并后再验证，兼容 diff 精简文件。
func (a *App) GetAvailableSchemas() ([]SchemaInfo, error) {
	exeDir := getExeDir()
	configDir, err := config.GetConfigDir()
	if err != nil {
		configDir = ""
	}

	// 收集所有 schema ID（去重）
	schemaIDs := collectSchemaIDs(exeDir, configDir)

	validEngineTypes := map[string]bool{
		"codetable": true, "pinyin": true, "mixed": true,
	}

	schemas := make(map[string]SchemaInfo)
	for _, id := range schemaIDs {
		// 通过合并读取获取完整配置
		cfg, err := a.GetSchemaConfig(id)
		if err != nil {
			schemas[id] = SchemaInfo{ID: id, Error: fmt.Sprintf("加载失败: %v", err)}
			continue
		}

		// 判断方案来源
		source := "user"
		if _, builtinCheckErr := findBuiltinSchemaFile(id); builtinCheckErr == nil {
			source = "builtin"
		}

		info := SchemaInfo{
			ID:          cfg.Schema.ID,
			Name:        cfg.Schema.Name,
			IconLabel:   cfg.Schema.IconLabel,
			Version:     cfg.Schema.Version,
			Description: cfg.Schema.Description,
			EngineType:  cfg.Engine.Type,
			Source:      source,
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

	// 对每个方案进行资源验证（词典文件是否存在）
	for id, s := range schemas {
		if s.Error != "" {
			continue
		}
		if errMsg := validateSchemaResourcesMerged(id, a, exeDir, configDir); errMsg != "" {
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

// loadSchemaBase 加载方案基础配置（Layer1 + Layer2，不含覆盖层）
func (a *App) loadSchemaBase(schemaID string) (*SchemaConfig, error) {
	var cfg SchemaConfig

	// Layer 1: 内置方案
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

	// Layer 2: 用户方案文件
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

	if builtinErr != nil && userErr != nil {
		return nil, fmt.Errorf("方案文件不存在: %s", schemaID)
	}

	return &cfg, nil
}

// GetSchemaConfig 获取指定方案的完整配置
// 使用三层合并读取：内置方案 → 用户方案 → schema_overrides.yaml
func (a *App) GetSchemaConfig(schemaID string) (*SchemaConfig, error) {
	cfg, err := a.loadSchemaBase(schemaID)
	if err != nil {
		return nil, err
	}

	// Layer 3: 叠加用户覆盖配置 (schema_overrides.yaml)
	override, overrideErr := config.GetSchemaOverride(schemaID)
	if overrideErr == nil && override != nil {
		overrideData, marshalErr := yaml.Marshal(override)
		if marshalErr == nil {
			yaml.Unmarshal(overrideData, cfg)
		}
	}

	return cfg, nil
}

// SaveSchemaConfig 保存方案配置（写入 schema_overrides.yaml 覆盖层）
// 计算 cfg 与基础配置（Layer1+Layer2）的 diff，仅将差异写入覆盖层。
func (a *App) SaveSchemaConfig(schemaID string, cfg *SchemaConfig) error {
	// 加载基础配置（Layer1 + Layer2）
	base, err := a.loadSchemaBase(schemaID)
	if err != nil {
		return fmt.Errorf("加载方案基础配置失败: %w", err)
	}

	// 计算差异
	diff, err := config.ComputeYAMLDiff(base, cfg)
	if err != nil {
		return fmt.Errorf("计算方案配置差异失败: %w", err)
	}

	// 移除不应覆盖的元数据字段
	delete(diff, "schema")
	delete(diff, "dictionaries")
	delete(diff, "encoder")

	// 如果没有差异，删除已有的覆盖配置
	if len(diff) == 0 {
		config.DeleteSchemaOverride(schemaID)
	} else {
		if err := config.SetSchemaOverride(schemaID, diff); err != nil {
			return fmt.Errorf("保存方案覆盖配置失败: %w", err)
		}
	}

	// 通知 wind_input 服务重新加载配置
	if a.rpcClient != nil {
		a.rpcClient.SystemNotifyReload("schema")
	}

	return nil
}

// ResetSchemaConfig 恢复方案默认配置（删除用户覆盖）
// 对于有内置基础的方案，同时删除用户 diff 文件（Layer 2），使其完全回到内置默认值。
// 对于纯用户方案（无内置基础），仅删除覆盖层（Layer 3）。
func (a *App) ResetSchemaConfig(schemaID string) error {
	// 删除 Layer 3 覆盖
	if err := config.DeleteSchemaOverride(schemaID); err != nil {
		return fmt.Errorf("重置方案配置失败: %w", err)
	}

	// 对有内置基础的方案，删除 Layer 2 用户 diff 文件
	if _, builtinErr := findBuiltinSchemaFile(schemaID); builtinErr == nil {
		if userPath, userErr := findUserSchemaFile(schemaID); userErr == nil {
			os.Remove(userPath)
		}
	}

	// 通知 wind_input 服务重新加载配置
	if a.rpcClient != nil {
		a.rpcClient.SystemNotifyReload("schema")
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
	if a.rpcClient != nil {
		a.rpcClient.SystemNotifyReload("config")
	}

	return nil
}

// --- 内部辅助函数 ---

// collectSchemaIDs 从内置和用户目录收集所有去重的 schema ID
func collectSchemaIDs(exeDir, configDir string) []string {
	seen := make(map[string]bool)
	var ids []string

	collectFromDir := func(dir string) {
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
			var peek struct {
				Schema struct {
					ID string `yaml:"id"`
				} `yaml:"schema"`
			}
			if err := yaml.Unmarshal(data, &peek); err != nil || peek.Schema.ID == "" {
				continue
			}
			if !seen[peek.Schema.ID] {
				seen[peek.Schema.ID] = true
				ids = append(ids, peek.Schema.ID)
			}
		}
	}

	collectFromDir(filepath.Join(exeDir, "data", "schemas"))
	if configDir != "" {
		collectFromDir(filepath.Join(configDir, "schemas"))
	}

	return ids
}

// validateSchemaResourcesMerged 使用合并读取验证方案引用的词典文件是否存在
func validateSchemaResourcesMerged(schemaID string, a *App, exeDir, configDir string) string {
	cfg, err := a.GetSchemaConfig(schemaID)
	if err != nil {
		return fmt.Sprintf("加载方案失败: %v", err)
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

	// 找出被已启用方案引用但自身不在 available 中的方案
	// 只考虑已启用方案的引用关系，避免未启用的混输方案导致其引用的方案被错误显示
	var result []string
	for id, ref := range refs {
		if !availableSet[id] {
			continue
		}
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

// ExportSchema 将指定方案打包为 ZIP 导出
// ZIP 内容与 WindInputCodeTable 格式一致：{schemaID}.schema.yaml + 词典文件
func (a *App) ExportSchema(schemaID string) (string, error) {
	cfg, err := a.GetSchemaConfig(schemaID)
	if err != nil {
		return "", fmt.Errorf("加载方案配置失败: %w", err)
	}

	version := cfg.Schema.Version
	if version == "" {
		version = "1.0"
	}
	defaultName := fmt.Sprintf("%s-%s.zip", schemaID, version)

	savePath, err := wailsRuntime.SaveFileDialog(a.ctx, wailsRuntime.SaveDialogOptions{
		Title:           "导出输入方案",
		DefaultFilename: defaultName,
		Filters: []wailsRuntime.FileFilter{
			{DisplayName: "ZIP 文件", Pattern: "*.zip"},
		},
	})
	if err != nil {
		return "", fmt.Errorf("保存对话框失败: %w", err)
	}
	if savePath == "" {
		return "", nil // 用户取消
	}

	// 创建 ZIP 文件
	zipFile, err := os.Create(savePath)
	if err != nil {
		return "", fmt.Errorf("创建 ZIP 文件失败: %w", err)
	}
	defer zipFile.Close()

	w := zip.NewWriter(zipFile)
	defer w.Close()

	// 写入方案配置文件（完整合并后的配置）
	schemaYAML, err := yaml.Marshal(cfg)
	if err != nil {
		return "", fmt.Errorf("序列化方案配置失败: %w", err)
	}
	schemaFileName := schemaID + ".schema.yaml"
	fw, err := w.Create(schemaFileName)
	if err != nil {
		return "", fmt.Errorf("添加方案文件到 ZIP 失败: %w", err)
	}
	if _, err := fw.Write(schemaYAML); err != nil {
		return "", fmt.Errorf("写入方案文件失败: %w", err)
	}

	// 收集并写入词典文件
	exeDir := getExeDir()
	exeDataDir := filepath.Join(exeDir, "data")
	configDir, _ := config.GetConfigDir()

	for _, dict := range cfg.Dicts {
		if dict.Path == "" {
			continue
		}
		dictAbsPath := resolveDictFilePath(dict.Path, exeDataDir, configDir)
		if dictAbsPath == "" {
			continue
		}
		data, err := os.ReadFile(dictAbsPath)
		if err != nil {
			continue
		}
		dfw, err := w.Create(dict.Path)
		if err != nil {
			continue
		}
		dfw.Write(data)
	}

	return savePath, nil
}

// ImportPreviewSchema 单个方案的预览信息
type ImportPreviewSchema struct {
	ID          string `json:"id"`
	Name        string `json:"name"`
	Version     string `json:"version"`
	Author      string `json:"author"`
	Description string `json:"description"`
	EngineType  string `json:"engine_type"`
	DictCount   int    `json:"dict_count"`
	Conflict    bool   `json:"conflict"`
	ConflictSrc string `json:"conflict_src"` // builtin | user
}

// ImportPreview 导入预览信息
type ImportPreview struct {
	ZipPath   string                `json:"zip_path"`   // ZIP 文件路径（用于后续确认导入）
	Schemas   []ImportPreviewSchema `json:"schemas"`    // ZIP 中包含的所有方案
	FileCount int                   `json:"file_count"` // ZIP 文件总数
}

// PreviewImportSchema 打开文件对话框，读取 ZIP 中所有方案的预览信息（不解压）
func (a *App) PreviewImportSchema() (*ImportPreview, error) {
	openPath, err := wailsRuntime.OpenFileDialog(a.ctx, wailsRuntime.OpenDialogOptions{
		Title: "导入输入方案",
		Filters: []wailsRuntime.FileFilter{
			{DisplayName: "方案包 (*.zip, *.7z)", Pattern: "*.zip;*.7z"},
			{DisplayName: "所有文件 (*.*)", Pattern: "*.*"},
		},
	})
	if err != nil {
		return nil, fmt.Errorf("打开对话框失败: %w", err)
	}
	if openPath == "" {
		return nil, nil // 用户取消
	}

	entries, closer, err := openArchive(openPath)
	if err != nil {
		return nil, fmt.Errorf("打开压缩包失败: %w", err)
	}
	defer closer.Close()

	// 检测公共前缀（多一层目录的情况）
	prefix := detectCommonPrefix(entries)

	// 收集所有 .schema.yaml 条目
	type schemaEntry struct {
		name string
		open func() (io.ReadCloser, error)
	}
	var schemaEntries []schemaEntry
	fileCount := 0
	for _, e := range entries {
		if e.IsDir {
			continue
		}
		fileCount++
		name := filepath.Base(stripPrefix(e.Name, prefix))
		if strings.HasSuffix(name, ".schema.yaml") {
			schemaEntries = append(schemaEntries, schemaEntry{name: e.Name, open: e.Open})
		}
	}
	if len(schemaEntries) == 0 {
		return nil, fmt.Errorf("压缩包中未找到 .schema.yaml 文件")
	}

	preview := &ImportPreview{
		ZipPath:   openPath,
		FileCount: fileCount,
	}

	for _, se := range schemaEntries {
		rc, err := se.open()
		if err != nil {
			continue
		}
		data, err := io.ReadAll(rc)
		rc.Close()
		if err != nil {
			continue
		}

		var cfg SchemaConfig
		if err := yaml.Unmarshal(data, &cfg); err != nil {
			continue
		}
		schemaID := strings.TrimSuffix(filepath.Base(se.name), ".schema.yaml")
		if cfg.Schema.ID == "" {
			cfg.Schema.ID = schemaID
		}

		ps := ImportPreviewSchema{
			ID:          cfg.Schema.ID,
			Name:        cfg.Schema.Name,
			Version:     cfg.Schema.Version,
			Author:      cfg.Schema.Author,
			Description: cfg.Schema.Description,
			EngineType:  cfg.Engine.Type,
			DictCount:   len(cfg.Dicts),
		}

		if _, err := findBuiltinSchemaFile(cfg.Schema.ID); err == nil {
			ps.Conflict = true
			ps.ConflictSrc = "builtin"
		} else if _, err := findUserSchemaFile(cfg.Schema.ID); err == nil {
			ps.Conflict = true
			ps.ConflictSrc = "user"
		}

		preview.Schemas = append(preview.Schemas, ps)
	}

	return preview, nil
}

// ConfirmImportSchema 确认导入方案（从指定压缩包解压到用户方案目录）
func (a *App) ConfirmImportSchema(zipPath string) (*SchemaInfo, error) {
	entries, closer, err := openArchive(zipPath)
	if err != nil {
		return nil, fmt.Errorf("打开压缩包失败: %w", err)
	}
	defer closer.Close()

	// 检测是否多了一层目录（所有文件都在同一个顶层目录下）
	prefix := detectCommonPrefix(entries)

	// 查找第一个 .schema.yaml 以获取方案信息
	var firstCfg *SchemaConfig
	var firstSchemaID string
	for _, e := range entries {
		if e.IsDir {
			continue
		}
		name := stripPrefix(e.Name, prefix)
		baseName := filepath.Base(name)
		if strings.HasSuffix(baseName, ".schema.yaml") {
			rc, err := e.Open()
			if err != nil {
				continue
			}
			data, err := io.ReadAll(rc)
			rc.Close()
			if err != nil {
				continue
			}
			var cfg SchemaConfig
			if err := yaml.Unmarshal(data, &cfg); err != nil {
				continue
			}
			schemaID := strings.TrimSuffix(baseName, ".schema.yaml")
			if cfg.Schema.ID == "" {
				cfg.Schema.ID = schemaID
			}
			if firstCfg == nil {
				firstCfg = &cfg
				firstSchemaID = cfg.Schema.ID
			}
		}
	}
	if firstCfg == nil {
		return nil, fmt.Errorf("压缩包中未找到 .schema.yaml 文件")
	}

	// 解压到用户方案目录
	configDir, err := config.GetConfigDir()
	if err != nil {
		return nil, fmt.Errorf("获取配置目录失败: %w", err)
	}
	schemasDir := filepath.Join(configDir, "schemas")
	if err := os.MkdirAll(schemasDir, 0755); err != nil {
		return nil, fmt.Errorf("创建方案目录失败: %w", err)
	}

	for _, e := range entries {
		if e.IsDir {
			continue
		}
		name := stripPrefix(e.Name, prefix)
		cleanName := filepath.Clean(name)
		if strings.Contains(cleanName, "..") || cleanName == "." {
			continue
		}
		destPath := filepath.Join(schemasDir, cleanName)
		if err := os.MkdirAll(filepath.Dir(destPath), 0755); err != nil {
			continue
		}
		src, err := e.Open()
		if err != nil {
			continue
		}
		dst, err := os.Create(destPath)
		if err != nil {
			src.Close()
			continue
		}
		io.Copy(dst, src)
		dst.Close()
		src.Close()
	}

	info := SchemaInfo{
		ID:         firstSchemaID,
		Name:       firstCfg.Schema.Name,
		IconLabel:  firstCfg.Schema.IconLabel,
		Version:    firstCfg.Schema.Version,
		EngineType: firstCfg.Engine.Type,
		Source:     "user",
	}
	return &info, nil
}

// detectCommonPrefix 检测压缩包中是否所有文件都在同一个顶层目录下
// 如果是，返回该目录前缀（如 "jjm-1.0/"），否则返回空字符串
func detectCommonPrefix(entries []archiveEntry) string {
	var firstDir string
	for _, e := range entries {
		name := filepath.ToSlash(e.Name)
		parts := strings.SplitN(name, "/", 2)
		if len(parts) < 2 {
			// 有文件直接在根目录，无公共前缀
			return ""
		}
		dir := parts[0]
		if firstDir == "" {
			firstDir = dir
		} else if dir != firstDir {
			return ""
		}
	}
	if firstDir != "" {
		return firstDir + "/"
	}
	return ""
}

// stripPrefix 去除路径的公共前缀
func stripPrefix(name, prefix string) string {
	if prefix == "" {
		return name
	}
	name = filepath.ToSlash(name)
	if strings.HasPrefix(name, prefix) {
		return name[len(prefix):]
	}
	return name
}

// DeleteSchema 删除用户方案文件（仅允许删除用户方案，不允许删除内置方案）
func (a *App) DeleteSchema(schemaID string) error {
	// 检查是否为内置方案
	if _, err := findBuiltinSchemaFile(schemaID); err == nil {
		return fmt.Errorf("不允许删除内置方案: %s", schemaID)
	}

	configDir, err := config.GetConfigDir()
	if err != nil {
		return fmt.Errorf("获取配置目录失败: %w", err)
	}
	schemasDir := filepath.Join(configDir, "schemas")

	// 删除方案配置文件
	schemaFile := filepath.Join(schemasDir, schemaID+".schema.yaml")
	if _, err := os.Stat(schemaFile); err == nil {
		if err := os.Remove(schemaFile); err != nil {
			return fmt.Errorf("删除方案文件失败: %w", err)
		}
	}

	// 删除方案关联的词典目录（如果存在）
	dictDir := filepath.Join(schemasDir, schemaID)
	if info, err := os.Stat(dictDir); err == nil && info.IsDir() {
		if err := os.RemoveAll(dictDir); err != nil {
			return fmt.Errorf("删除词典目录失败: %w", err)
		}
	}

	// 清理方案覆盖配置
	config.DeleteSchemaOverride(schemaID)

	return nil
}

// ExportSchemas 将多个方案打包为一个 ZIP 导出
func (a *App) ExportSchemas(schemaIDs []string) (string, error) {
	if len(schemaIDs) == 0 {
		return "", fmt.Errorf("未选择要导出的方案")
	}

	// 构造默认文件名：以主码表方案的 ID 和版本号命名
	primaryID := schemaIDs[0]
	version := ""
	for _, sid := range schemaIDs {
		cfg, err := a.GetSchemaConfig(sid)
		if err != nil {
			continue
		}
		if cfg.Engine.Type != "mixed" {
			primaryID = sid
			version = cfg.Schema.Version
			break
		}
	}
	if version == "" {
		if cfg, err := a.GetSchemaConfig(schemaIDs[0]); err == nil {
			version = cfg.Schema.Version
		}
	}
	if version == "" {
		version = "1.0"
	}
	defaultName := fmt.Sprintf("%s-%s.zip", primaryID, version)

	savePath, err := wailsRuntime.SaveFileDialog(a.ctx, wailsRuntime.SaveDialogOptions{
		Title:           "导出输入方案",
		DefaultFilename: defaultName,
		Filters: []wailsRuntime.FileFilter{
			{DisplayName: "ZIP 文件", Pattern: "*.zip"},
		},
	})
	if err != nil {
		return "", fmt.Errorf("保存对话框失败: %w", err)
	}
	if savePath == "" {
		return "", nil
	}

	zipFile, err := os.Create(savePath)
	if err != nil {
		return "", fmt.Errorf("创建 ZIP 文件失败: %w", err)
	}
	defer zipFile.Close()

	w := zip.NewWriter(zipFile)
	defer w.Close()

	exeDir := getExeDir()
	exeDataDir := filepath.Join(exeDir, "data")
	configDir, _ := config.GetConfigDir()
	writtenPaths := make(map[string]bool) // 已写入的文件路径去重

	for _, sid := range schemaIDs {
		cfg, err := a.GetSchemaConfig(sid)
		if err != nil {
			continue
		}

		// 写入方案配置
		schemaFileName := sid + ".schema.yaml"
		if !writtenPaths[schemaFileName] {
			schemaYAML, err := yaml.Marshal(cfg)
			if err != nil {
				continue
			}
			fw, err := w.Create(schemaFileName)
			if err != nil {
				continue
			}
			fw.Write(schemaYAML)
			writtenPaths[schemaFileName] = true
		}

		// 写入词典文件（去重）
		for _, dict := range cfg.Dicts {
			if dict.Path == "" || writtenPaths[dict.Path] {
				continue
			}
			absPath := resolveDictFilePath(dict.Path, exeDataDir, configDir)
			if absPath == "" {
				continue
			}
			data, err := os.ReadFile(absPath)
			if err != nil {
				continue
			}
			dfw, err := w.Create(dict.Path)
			if err != nil {
				continue
			}
			dfw.Write(data)
			writtenPaths[dict.Path] = true
		}
	}

	return savePath, nil
}

// resolveDictFilePath 查找词典文件的绝对路径（找到第一个存在的）
func resolveDictFilePath(dictPath, exeDataDir, configDir string) string {
	if filepath.IsAbs(dictPath) {
		if _, err := os.Stat(dictPath); err == nil {
			return dictPath
		}
		return ""
	}
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
			return candidate
		}
	}
	return ""
}
