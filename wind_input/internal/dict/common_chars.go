// Package dict 通用规范汉字表
package dict

import (
	"bufio"
	"os"
	"strings"
	"sync"
)

// 通用规范汉字表（8105字）
// 一级字表：3500字（常用字）
// 二级字表：3000字（次常用字）
// 三级字表：1605字（专业用字）

var (
	commonCharMap  = make(map[rune]bool)
	commonCharOnce sync.Once
	commonCharFile = "data/schemas/common_chars.txt" // 通用汉字表文件路径
)

// 内置的一级常用字（3500字的核心部分，约2500字）
// 这是最常用的汉字，即使文件加载失败也能使用
const coreCommonChars = `的一是在不了有和人这中大为上个国我以要他时来用们生到作地于出就分对成会可主发年动同工也能下过子说产种面而方后多定行学法所民得经十三之进着等部度家电力里如水化高自二理起小物现实加量都两体制机当使点从业本去把性好应开它合还因由其些然前外天政四日那社义事平形相全表间样与关各重新线内数正心反你明看原又么利比或但质气第向道命此变条只没结解问意建月公无系军很情最何发成第见已于而被做或将之使等与由于可以我们他们这个那个什么怎么没有可能因为所以如果虽然但是而且或者不是就是只是不过还是因此然后这样那样这些那些自己什么为什么怎么样`

// InitCommonChars 初始化通用汉字表
// 优先从文件加载，失败则使用内置字表
func InitCommonChars() {
	commonCharOnce.Do(func() {
		for _, char := range coreCommonChars {
			if isCJKChar(char) {
				commonCharMap[char] = true
			}
		}

		// 尝试从文件加载完整字表
		loadCommonCharsFromFile()
	})
}

// InitCommonCharsWithPath 使用指定路径初始化
func InitCommonCharsWithPath(path string) {
	commonCharOnce.Do(func() {
		for _, char := range coreCommonChars {
			if isCJKChar(char) {
				commonCharMap[char] = true
			}
		}

		// 从指定文件加载
		if path != "" {
			loadCommonCharsFromFilePath(path)
		} else {
			loadCommonCharsFromFile()
		}
	})
}

// loadCommonCharsFromFile 从默认路径加载通用汉字表
func loadCommonCharsFromFile() {
	loadCommonCharsFromFilePath(commonCharFile)
}

// loadCommonCharsFromFilePath 从指定路径加载通用汉字表
func loadCommonCharsFromFilePath(path string) {
	file, err := os.Open(path)
	if err != nil {
		return // 文件不存在则使用内置字表
	}
	defer file.Close()

	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		// 跳过注释和空行
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		// 每个字符都是一个通用字（含 CJK Extension-A U+3400–U+4DBF）
		for _, char := range line {
			if isCJKChar(char) {
				commonCharMap[char] = true
			}
		}
	}
}

// IsCommonChar 判断单个字符是否为通用规范汉字
func IsCommonChar(char rune) bool {
	InitCommonChars()
	return commonCharMap[char]
}

// isCJKChar 判断是否为 CJK 汉字或相关字符
// 简化逻辑：只要字符可能是汉字、部首、笔画等，就应该检查是否在通用字表中
// 不在通用字表中的这类字符都被视为"生僻"
func isCJKChar(char rune) bool {
	// CJK 部首、笔画、符号区 (0x2E80-0x33FF)
	// 包含：部首补充、康熙部首、注音符号、CJK笔画等
	if char >= 0x2E80 && char <= 0x33FF {
		return true
	}
	// CJK 扩展A + 基本汉字区 (0x3400-0x9FFF)
	if char >= 0x3400 && char <= 0x9FFF {
		return true
	}
	// CJK 兼容汉字 (0xF900-0xFAFF)
	if char >= 0xF900 && char <= 0xFAFF {
		return true
	}
	// CJK 扩展 B-H (0x20000-0x323AF)
	// 合并所有扩展区，简化判断
	if char >= 0x20000 && char <= 0x323AF {
		return true
	}
	return false
}

// IsStringCommon 判断字符串中的所有汉字是否都是通用规范汉字
// 采用"一票否决"规则：只要有一个汉字不是通用字，就返回 false
func IsStringCommon(text string) bool {
	// 空字符串不是有效的候选词
	if text == "" {
		return false
	}
	InitCommonChars()
	for _, char := range text {
		// 检查所有 CJK 汉字（包括扩展区）
		if isCJKChar(char) {
			if !commonCharMap[char] {
				return false
			}
		}
	}
	return true
}

// GetCommonCharCount 获取通用汉字表的字数
func GetCommonCharCount() int {
	InitCommonChars()
	return len(commonCharMap)
}

// AddCommonChars 批量添加通用汉字（用于运行时扩展）
// TODO: 预留给用户自定义常用字功能
func AddCommonChars(chars string) {
	InitCommonChars()
	for _, char := range chars {
		if char > 0x4E00 {
			commonCharMap[char] = true
		}
	}
}

// ResetCommonCharsForTesting 重置通用汉字表（仅用于测试）
// 这个函数会清空现有数据并重置 sync.Once，以便重新初始化
func ResetCommonCharsForTesting() {
	commonCharMap = make(map[rune]bool)
	commonCharOnce = sync.Once{}
}
