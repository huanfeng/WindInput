// 码表测试程序
package main

import (
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"sort"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/engine/codetable"
)

func main() {
	if len(os.Args) < 2 {
		fmt.Println("用法: test_codetable <码表文件路径> [测试编码...]")
		fmt.Println("示例: test_codetable ref/极爽词库6.txt a aa aaaa")
		os.Exit(1)
	}

	dictPath := os.Args[1]

	// 获取绝对路径
	if !filepath.IsAbs(dictPath) {
		exePath, _ := os.Executable()
		exeDir := filepath.Dir(exePath)
		dictPath = filepath.Join(exeDir, dictPath)
	}

	fmt.Println("========================================")
	fmt.Println("WindInput 码表测试程序")
	fmt.Println("========================================")
	fmt.Printf("加载码表: %s\n", dictPath)
	fmt.Println()

	// 加载码表
	ct, err := dict.LoadCodeTable(dictPath)
	if err != nil {
		fmt.Printf("加载码表失败: %v\n", err)
		os.Exit(1)
	}

	// 显示码表信息
	fmt.Println("【码表信息】")
	fmt.Printf("  名称: %s\n", ct.Header.Name)
	fmt.Printf("  版本: %s\n", ct.Header.Version)
	fmt.Printf("  作者: %s\n", ct.Header.Author)
	fmt.Printf("  编码方案: %s\n", ct.Header.CodeScheme)
	fmt.Printf("  最大码长: %d\n", ct.Header.CodeLength)
	fmt.Printf("  特殊前缀: %s\n", ct.Header.SpecialPrefix)
	fmt.Printf("  词条数量: %d\n", ct.EntryCount())
	fmt.Printf("  是否五笔: %v\n", ct.IsWubi())
	fmt.Printf("  是否拼音: %v\n", ct.IsPinyin())
	fmt.Println()

	// 创建五笔引擎
	engine := codetable.NewEngine(&codetable.Config{
		MaxCodeLength:   ct.GetMaxCodeLength(),
		AutoCommitAt4:   true,
		ClearOnEmptyAt4: true,
		TopCodeCommit:   true, // 启用顶码
		PunctCommit:     true,
	}, slog.Default())
	engine.LoadCodeTable(dictPath)

	// 测试查询
	testCodes := os.Args[2:]
	if len(testCodes) == 0 {
		// 默认测试编码
		if ct.IsWubi() {
			testCodes = []string{"a", "aa", "aaa", "aaaa", "gggg", "wq", "wqiy"}
		} else {
			testCodes = []string{"a", "ai", "wo", "ni", "nihao"}
		}
	}

	fmt.Println("【编码测试】")
	for _, code := range testCodes {
		fmt.Printf("\n编码 [%s]:\n", code)

		// 直接码表查询（精确匹配）
		exactCandidates := ct.Lookup(code)
		fmt.Printf("  精确匹配: %d 个结果\n", len(exactCandidates))
		printCandidates(exactCandidates, 5)

		// 引擎转换（含前缀匹配和排序）
		result := engine.ConvertEx(code, 10)
		fmt.Printf("  引擎结果: %d 个候选\n", len(result.Candidates))
		fmt.Printf("  自动上屏: %v", result.ShouldCommit)
		if result.ShouldCommit {
			fmt.Printf(" -> %s", result.CommitText)
		}
		fmt.Println()
		fmt.Printf("  空码: %v, 清空: %v, 转英文: %v\n", result.IsEmpty, result.ShouldClear, result.ToEnglish)
		printCandidates(result.Candidates, 9)
	}

	// 测试顶码功能
	fmt.Println("\n【顶码测试】")
	testTopCodes := []string{"aaaaa", "ggggw", "wqiyg"}
	for _, code := range testTopCodes {
		commitText, newInput, shouldCommit := engine.HandleTopCode(code)
		fmt.Printf("  输入 [%s]: 顶字=%v", code, shouldCommit)
		if shouldCommit {
			fmt.Printf(" -> 上屏[%s], 新输入[%s]", commitText, newInput)
		}
		fmt.Println()
	}

	fmt.Println("\n========================================")
	fmt.Println("测试完成")
}

func printCandidates(candidates []candidate.Candidate, max int) {
	if len(candidates) == 0 {
		fmt.Println("    (无)")
		return
	}

	// 按权重排序
	sorted := make([]candidate.Candidate, len(candidates))
	copy(sorted, candidates)
	sort.Slice(sorted, func(i, j int) bool {
		return sorted[i].Weight > sorted[j].Weight
	})

	count := len(sorted)
	if count > max {
		count = max
	}
	for i := 0; i < count; i++ {
		c := sorted[i]
		hint := ""
		if c.Hint != "" {
			hint = fmt.Sprintf(" [%s]", c.Hint)
		}
		fmt.Printf("    %d. %s (词频:%d)%s\n", i+1, c.Text, c.Weight, hint)
	}
	if len(sorted) > max {
		fmt.Printf("    ... 还有 %d 个\n", len(sorted)-max)
	}
}
