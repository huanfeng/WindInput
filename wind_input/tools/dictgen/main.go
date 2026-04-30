// dictgen 五笔主词库生成工具
//
// 从极点五笔 rime-wubi86-jidian 原始词库出发，结合 unigram 真实词频，
// 重新排序并输出 WindInput 所用的 rime YAML 格式词库。
//
// 用法:
//
//	go run ./tools/dictgen -config tools/dictgen/dictgen.yaml
package main

import (
	"flag"
	"fmt"
	"os"
)

func main() {
	configPath := flag.String("config", "tools/dictgen/dictgen.yaml", "配置文件路径")
	outputPath := flag.String("output", "", "覆盖配置文件中的输出路径（可选）")
	flag.Parse()

	cfg, err := loadConfig(*configPath)
	if err != nil {
		fmt.Fprintf(os.Stderr, "[错误] 加载配置失败: %v\n", err)
		os.Exit(1)
	}
	if *outputPath != "" {
		cfg.OutputPath = *outputPath
	}

	fmt.Printf("已加载配置: %s\n", *configPath)
	fmt.Printf("  jidian   : %s\n", cfg.JidianPath)
	fmt.Printf("  unigram  : %s\n", cfg.UnigramPath)
	fmt.Printf("  输出路径 : %s\n", cfg.OutputPath)
	fmt.Printf("  权重归一化: 中位→%d  上限=%d  下限=%d  单字提权×%.1f\n",
		cfg.TargetMedian, cfg.WeightMax, cfg.WeightMin, cfg.CharBoostFactor)
	fmt.Printf("  生僻保底 : p30→%d  p20→%d  p10→%d\n",
		cfg.Fallback.Priority30, cfg.Fallback.Priority20, cfg.Fallback.Priority10)
	if cfg.Shortcodes.Enabled {
		fmt.Printf("  简码分层 : 一级=%d  二级基=%d  三级基=%d  普通上限=%d\n",
			cfg.Shortcodes.Level1Weight, cfg.Shortcodes.Level2BaseWeight,
			cfg.Shortcodes.Level3BaseWeight, cfg.RegularWeightMax)
	}
	fmt.Printf("\n")

	if err := enrich(cfg); err != nil {
		fmt.Fprintf(os.Stderr, "[错误] %v\n", err)
		os.Exit(1)
	}
}
