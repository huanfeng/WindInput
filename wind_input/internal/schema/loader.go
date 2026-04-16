package schema

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"gopkg.in/yaml.v3"
)

const schemaFileSuffix = ".schema.yaml"

// LoadSchemaFile 加载单个方案文件
func LoadSchemaFile(path string) (*Schema, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("读取方案文件失败 %s: %w", path, err)
	}

	var s Schema
	if err := yaml.Unmarshal(data, &s); err != nil {
		return nil, fmt.Errorf("解析方案文件失败 %s: %w", path, err)
	}

	if err := validateSchema(&s, path); err != nil {
		return nil, err
	}

	return &s, nil
}

// DiscoverSchemas 扫描目录加载所有方案
// 优先级：dataDir > exeDir（同 ID 时用户方案与内置方案合并，用户配置覆盖内置默认值）
func DiscoverSchemas(exeDir, dataDir string) (map[string]*Schema, error) {
	schemas := make(map[string]*Schema)

	// 先加载内置方案
	exeSchemaDir := filepath.Join(exeDir, "schemas")
	if err := loadSchemasFromDir(exeSchemaDir, schemas); err != nil {
		return nil, fmt.Errorf("加载内置方案失败: %w", err)
	}

	// 再加载用户方案（同 ID 时合并：内置方案为基础，用户配置覆盖其上，缺失字段保留内置值）
	userSchemaDir := filepath.Join(dataDir, "schemas")
	if err := loadAndMergeUserSchemas(userSchemaDir, schemas); err != nil {
		// 用户目录不存在不算错误
		if !os.IsNotExist(err) {
			return nil, fmt.Errorf("加载用户方案失败: %w", err)
		}
	}

	if len(schemas) == 0 {
		return nil, fmt.Errorf("未找到任何输入方案文件")
	}

	return schemas, nil
}

// loadSchemasFromDir 从指定目录加载所有方案文件
func loadSchemasFromDir(dir string, schemas map[string]*Schema) error {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return err
	}

	for _, entry := range entries {
		if entry.IsDir() {
			continue
		}
		name := entry.Name()
		if !strings.HasSuffix(name, schemaFileSuffix) {
			continue
		}

		path := filepath.Join(dir, name)
		s, err := LoadSchemaFile(path)
		if err != nil {
			// 单个文件加载失败不中断，记录日志后跳过
			fmt.Fprintf(os.Stderr, "[schema] 跳过无效方案文件 %s: %v\n", path, err)
			continue
		}

		schemas[s.Schema.ID] = s
	}

	return nil
}

// loadAndMergeUserSchemas 从用户目录加载方案并与内置方案合并
// 同 ID 时：以内置方案为基础，用户配置覆盖其上（缺失字段保留内置值）
// 不同 ID 时：作为全新用户方案加载
func loadAndMergeUserSchemas(dir string, schemas map[string]*Schema) error {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return err
	}

	for _, entry := range entries {
		if entry.IsDir() {
			continue
		}
		name := entry.Name()
		if !strings.HasSuffix(name, schemaFileSuffix) {
			continue
		}

		path := filepath.Join(dir, name)
		data, err := os.ReadFile(path)
		if err != nil {
			fmt.Fprintf(os.Stderr, "[schema] 跳过无法读取的方案文件 %s: %v\n", path, err)
			continue
		}

		// 先提取 schema ID，判断是否需要与内置方案合并
		var peek struct {
			Schema struct {
				ID string `yaml:"id"`
			} `yaml:"schema"`
		}
		if err := yaml.Unmarshal(data, &peek); err != nil {
			fmt.Fprintf(os.Stderr, "[schema] 跳过无效方案文件 %s: %v\n", path, err)
			continue
		}

		var s *Schema
		if builtin, ok := schemas[peek.Schema.ID]; ok && peek.Schema.ID != "" {
			// 存在同 ID 内置方案：深拷贝内置方案作为基础，用户配置覆盖其上
			s = deepCopySchema(builtin)
			if err := yaml.Unmarshal(data, s); err != nil {
				fmt.Fprintf(os.Stderr, "[schema] 合并方案文件失败 %s: %v\n", path, err)
				continue
			}
		} else {
			// 无内置方案：作为全新方案加载
			s = &Schema{}
			if err := yaml.Unmarshal(data, s); err != nil {
				fmt.Fprintf(os.Stderr, "[schema] 跳过无效方案文件 %s: %v\n", path, err)
				continue
			}
		}

		if err := validateSchema(s, path); err != nil {
			fmt.Fprintf(os.Stderr, "[schema] 跳过无效方案文件 %s: %v\n", path, err)
			continue
		}

		schemas[s.Schema.ID] = s
	}

	return nil
}

// deepCopySchema 通过 YAML 序列化/反序列化实现方案的深拷贝
func deepCopySchema(src *Schema) *Schema {
	data, err := yaml.Marshal(src)
	if err != nil {
		return &Schema{}
	}
	dst := &Schema{}
	_ = yaml.Unmarshal(data, dst)
	return dst
}

// validateSchema 校验方案必填字段
func validateSchema(s *Schema, path string) error {
	if s.Schema.ID == "" {
		return fmt.Errorf("方案文件 %s: schema.id 不能为空", path)
	}
	if s.Engine.Type == "" {
		return fmt.Errorf("方案 %s: engine.type 不能为空", s.Schema.ID)
	}
	if s.Engine.Type != EngineTypeCodeTable && s.Engine.Type != EngineTypePinyin && s.Engine.Type != EngineTypeMixed {
		return fmt.Errorf("方案 %s: engine.type 不支持的值 %q（仅支持 codetable/pinyin/mixed）", s.Schema.ID, s.Engine.Type)
	}
	// 混输方案引用其他方案时允许不定义 dictionaries
	hasMixedRef := s.Engine.Type == EngineTypeMixed && s.Engine.Mixed != nil &&
		(s.Engine.Mixed.PrimarySchema != "" || s.Engine.Mixed.SecondarySchema != "")
	if len(s.Dicts) == 0 && !hasMixedRef {
		return fmt.Errorf("方案 %s: dictionaries 不能为空", s.Schema.ID)
	}
	for i, d := range s.Dicts {
		if d.Path == "" {
			return fmt.Errorf("方案 %s: dictionaries[%d].path 不能为空", s.Schema.ID, i)
		}
		if d.Type == "" {
			return fmt.Errorf("方案 %s: dictionaries[%d].type 不能为空", s.Schema.ID, i)
		}
	}
	// 学习配置默认值：按引擎类型设定
	if s.Learning.AutoLearn == nil {
		switch s.Engine.Type {
		case EngineTypePinyin:
			s.Learning.AutoLearn = &AutoLearnSpec{Enabled: true}
		default:
			s.Learning.AutoLearn = &AutoLearnSpec{Enabled: false}
		}
	}
	if s.Learning.Freq == nil {
		switch s.Engine.Type {
		case EngineTypePinyin:
			s.Learning.Freq = &FreqSpec{Enabled: true}
		default:
			s.Learning.Freq = &FreqSpec{Enabled: false}
		}
	}
	// 设置默认方案名称
	if s.Schema.Name == "" {
		s.Schema.Name = s.Schema.ID
	}
	if s.Schema.IconLabel == "" {
		// 取名称的第一个字符
		runes := []rune(s.Schema.Name)
		if len(runes) > 0 {
			s.Schema.IconLabel = string(runes[0])
		}
	} else {
		// 防御：icon_label 仅保留第一个字符，多字符会导致 C++ 侧渲染异常
		runes := []rune(s.Schema.IconLabel)
		if len(runes) > 1 {
			s.Schema.IconLabel = string(runes[0])
		}
	}
	return nil
}
