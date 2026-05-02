package main

import (
	"fmt"
	"os"
	"path/filepath"

	"gopkg.in/yaml.v3"
)

type FallbackWeights struct {
	Priority30 int `yaml:"priority_30"`
	Priority20 int `yaml:"priority_20"`
	Priority10 int `yaml:"priority_10"`
}

// ShortcodeConfig 简码权重分层配置
// 一/二/三级简码权重固定在普通词条之上，确保简码优先级不被词频排序破坏
type ShortcodeConfig struct {
	Enabled          bool `yaml:"enabled"`
	Level1Weight     int  `yaml:"level1_weight"`      // 一级简码固定权重（每码唯一）
	Level2BaseWeight int  `yaml:"level2_base_weight"` // 二级简码基础权重（组内按 jidian 顺序递减）
	Level3BaseWeight int  `yaml:"level3_base_weight"` // 三级简码基础权重（组内按 jidian 顺序递减）
}

// DemotionConfig 简码降权策略配置
// 当一个字同时占据简码和4码全码首选时，根据第二候选的权重决定是否降权
type DemotionConfig struct {
	Enabled             bool    `yaml:"enabled"`                // 是否启用降权策略
	FilterThreshold     int     `yaml:"filter_threshold"`       // 第二候选权重低于此值直接忽略（不参与降权计算）
	SingleCharPromoteWt int     `yaml:"single_char_promote_wt"` // 单字第二候选权重达到此值则触发降权
	WordPromoteWt       int     `yaml:"word_promote_wt"`        // 词组第二候选权重达到此值则触发降权
	MaxGapRatioSingle   float64 `yaml:"max_gap_ratio_single"`   // 单字：gap/首字权重 超过此比例则保留（首字优势太大）
	MaxGapRatioWord     float64 `yaml:"max_gap_ratio_word"`     // 词组：gap/首字权重 超过此比例则保留
}

type DropRule struct {
	CodePrefix  string   `yaml:"code_prefix"`
	Code        string   `yaml:"code"`
	Reason      string   `yaml:"reason"`
	ExceptCodes []string `yaml:"except_codes"`
}

type Config struct {
	// 输入
	JidianPath  string `yaml:"jidian_path"`
	UnigramPath string `yaml:"unigram_path"`

	// 自定义词表（可选，不存在则跳过）
	CustomWordsPath string `yaml:"custom_words_path"`

	// 词序提升表（可选，不存在则跳过）：调整已有 (code, text) 条目的权重
	BoostsPath string `yaml:"boosts_path"`

	// 输出
	OutputPath  string `yaml:"output_path"`
	OutputName  string `yaml:"output_name"`
	DroppedPath string `yaml:"dropped_path"` // 过滤条目输出路径，留空则不写

	// 权重归一化
	TargetMedian    int             `yaml:"target_median"`
	WeightMax       int             `yaml:"weight_max"`
	WeightMin       int             `yaml:"weight_min"`
	CharBoostFactor float64         `yaml:"char_boost_factor"`
	Fallback        FallbackWeights `yaml:"fallback"`

	// 内置过滤
	DropZCode     bool `yaml:"drop_z_code"`
	DropDollar    bool `yaml:"drop_dollar"`
	DropEmoji     bool `yaml:"drop_emoji"`
	DropPureLatin bool `yaml:"drop_pure_latin"`
	DropPUA       bool `yaml:"drop_pua"`
	RequireCJK    bool `yaml:"require_cjk"`
	MaxCodeLen    int  `yaml:"max_code_len"`
	MaxTextLen    int  `yaml:"max_text_len"`

	// 手动过滤规则
	DropRules []DropRule `yaml:"drop_rules"`

	// 生成文件中的 import_tables（引用扩展词库）
	ImportTables []string `yaml:"import_tables"`

	// 简码优先级分层
	Shortcodes         ShortcodeConfig `yaml:"shortcodes"`
	RegularWeightMax   int             `yaml:"regular_weight_max"`   // 普通词条权重上限，应低于最低简码权重
	ConflictReportPath string          `yaml:"conflict_report_path"` // 简码避让冲突报告路径，空则不输出
	DemotionReportPath string          `yaml:"demotion_report_path"` // 简码降权待处理报告路径，空则不输出

	// 简码降权策略
	Demotion DemotionConfig `yaml:"demotion"`
}

func defaultConfig() Config {
	return Config{
		OutputName:      "wubi86_jidian",
		TargetMedian:    1000,
		WeightMax:       9999,
		WeightMin:       1,
		CharBoostFactor: 1.3,
		Fallback:        FallbackWeights{Priority30: 180, Priority20: 150, Priority10: 120},
		DropZCode:       true,
		DropDollar:      true,
		DropEmoji:       true,
		DropPureLatin:   true,
		DropPUA:         false,
		RequireCJK:      false,
		MaxCodeLen:      4,
		MaxTextLen:      16,
		Shortcodes: ShortcodeConfig{
			Enabled:          true,
			Level1Weight:     9999,
			Level2BaseWeight: 9950,
			Level3BaseWeight: 9000,
		},
		RegularWeightMax: 8999,
		Demotion: DemotionConfig{
			Enabled:             true,
			FilterThreshold:     200,
			SingleCharPromoteWt: 1000,
			WordPromoteWt:       800,
			MaxGapRatioSingle:   0.60,
			MaxGapRatioWord:     0.65,
		},
	}
}

func loadConfig(path string) (*Config, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("读取配置文件失败: %w", err)
	}
	cfg := defaultConfig()
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return nil, fmt.Errorf("解析配置文件失败: %w", err)
	}

	// 相对路径相对于配置文件所在目录解析
	cfgDir := filepath.Dir(filepath.Clean(path))
	resolve := func(p string) string {
		if p == "" || filepath.IsAbs(p) {
			return p
		}
		return filepath.Clean(filepath.Join(cfgDir, p))
	}
	cfg.JidianPath = resolve(cfg.JidianPath)
	cfg.UnigramPath = resolve(cfg.UnigramPath)
	cfg.OutputPath = resolve(cfg.OutputPath)
	cfg.CustomWordsPath = resolve(cfg.CustomWordsPath)
	cfg.BoostsPath = resolve(cfg.BoostsPath)
	cfg.DroppedPath = resolve(cfg.DroppedPath)
	cfg.ConflictReportPath = resolve(cfg.ConflictReportPath)
	cfg.DemotionReportPath = resolve(cfg.DemotionReportPath)

	return &cfg, nil
}
