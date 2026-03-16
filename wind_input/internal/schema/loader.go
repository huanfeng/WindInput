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
// 优先级：dataDir > exeDir（同 ID 时 dataDir 覆盖 exeDir）
func DiscoverSchemas(exeDir, dataDir string) (map[string]*Schema, error) {
	schemas := make(map[string]*Schema)

	// 先加载内置方案
	exeSchemaDir := filepath.Join(exeDir, "schemas")
	if err := loadSchemasFromDir(exeSchemaDir, schemas); err != nil {
		return nil, fmt.Errorf("加载内置方案失败: %w", err)
	}

	// 再加载用户方案（同 ID 覆盖内置）
	userSchemaDir := filepath.Join(dataDir, "schemas")
	if err := loadSchemasFromDir(userSchemaDir, schemas); err != nil {
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

// validateSchema 校验方案必填字段
func validateSchema(s *Schema, path string) error {
	if s.Schema.ID == "" {
		return fmt.Errorf("方案文件 %s: schema.id 不能为空", path)
	}
	if s.Engine.Type == "" {
		return fmt.Errorf("方案 %s: engine.type 不能为空", s.Schema.ID)
	}
	if s.Engine.Type != EngineTypeCodeTable && s.Engine.Type != EngineTypePinyin {
		return fmt.Errorf("方案 %s: engine.type 不支持的值 %q（仅支持 codetable/pinyin）", s.Schema.ID, s.Engine.Type)
	}
	if len(s.Dicts) == 0 {
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
	if s.UserData.ShadowFile == "" {
		return fmt.Errorf("方案 %s: user_data.shadow_file 不能为空", s.Schema.ID)
	}
	if s.UserData.UserDictFile == "" {
		return fmt.Errorf("方案 %s: user_data.user_dict_file 不能为空", s.Schema.ID)
	}
	if s.Learning.Mode == "" {
		// 根据引擎类型设置默认学习模式
		switch s.Engine.Type {
		case EngineTypePinyin:
			s.Learning.Mode = LearningAuto
		default:
			s.Learning.Mode = LearningManual
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
	}
	return nil
}
