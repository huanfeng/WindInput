// gen_unigram 从词频数据生成 Unigram 语言模型文件
//
// 用法:
//
//	gen_unigram -rime <rime词库目录> -output <输出文件>
//
// 从白霜拼音 (rime-frost) 的 .dict.yaml 文件提取真实词频。
// 词库目录下应包含 8105.dict.yaml、base.dict.yaml 等文件。
//
// 输出格式: 文字\t频次
package main

import (
	"bufio"
	"flag"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
)

type wordFreq struct {
	word string
	freq float64
}

func main() {
	rimePath := flag.String("rime", "", "Rime 词库目录（包含 .dict.yaml 文件）")
	outputPath := flag.String("output", "", "输出 Unigram 文件路径")
	flag.Parse()

	if *outputPath == "" || *rimePath == "" {
		flag.Usage()
		os.Exit(1)
	}

	freqMap := loadFromRime(*rimePath)

	// 按频次降序排序；频次相同则按词条字典序升序，保证输出稳定
	// （map 迭代顺序随机；若仅按 freq 排序，相同频次的条目顺序每次运行都会变）
	words := make([]wordFreq, 0, len(freqMap))
	for word, freq := range freqMap {
		words = append(words, wordFreq{word, freq})
	}
	sort.Slice(words, func(i, j int) bool {
		if words[i].freq != words[j].freq {
			return words[i].freq > words[j].freq
		}
		return words[i].word < words[j].word
	})

	// 写入文件
	file, err := os.Create(*outputPath)
	if err != nil {
		log.Fatalf("创建输出文件失败: %v", err)
	}
	defer file.Close()

	writer := bufio.NewWriter(file)
	writer.WriteString("# WindInput Unigram 语言模型\n")
	writer.WriteString("# 格式: 词语\\t频次\n")
	writer.WriteString("# 词频来源: 白霜拼音 (rime-frost) + 腾讯词向量\n")
	writer.WriteString(strings.Repeat("#", 40) + "\n")

	for _, wf := range words {
		fmt.Fprintf(writer, "%s\t%.0f\n", wf.word, wf.freq)
	}
	writer.Flush()

	fmt.Printf("Unigram 模型生成成功: %d 词条 -> %s\n", len(words), *outputPath)
}

// loadFromRime 从 Rime .dict.yaml 文件加载词频
func loadFromRime(dirPath string) map[string]float64 {
	freqMap := make(map[string]float64)

	// 按优先级加载词库文件（后加载的同名词条不会覆盖高频值）
	files := []string{
		"8105.dict.yaml",        // 单字（含真实词频）
		"41448.dict.yaml",       // 扩展字表（生僻字）
		"base.dict.yaml",        // 基础词组
		"ext.dict.yaml",         // 扩展词组
		"others.dict.yaml",      // 容错词（多音字异读）
		"corrections.dict.yaml", // 错音词（weight=0，被过滤，不进入 unigram）
		"tencent.dict.yaml",     // 腾讯词向量补充
	}

	for _, name := range files {
		path := filepath.Join(dirPath, name)
		if _, err := os.Stat(path); os.IsNotExist(err) {
			fmt.Printf("跳过不存在的词库文件: %s\n", path)
			continue
		}
		count := parseRimeDict(path, freqMap)
		fmt.Printf("加载 %s: %d 条词频数据\n", name, count)
	}

	fmt.Printf("Rime 词频数据合计: %d 词条\n", len(freqMap))
	return freqMap
}

// parseRimeDict 解析单个 Rime .dict.yaml 文件
// 支持两种格式:
//   - 三列: 文字\t拼音\t词频 (8105, base)
//   - 两列: 文字\t词频         (tencent, columns: text + weight)
func parseRimeDict(path string, freqMap map[string]float64) int {
	file, err := os.Open(path)
	if err != nil {
		log.Printf("打开文件失败: %s: %v", path, err)
		return 0
	}
	defer file.Close()

	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 0, 1024*1024), 1024*1024)

	inHeader := true
	// 检测列格式：默认三列 (text, code, weight)
	twoColumnMode := false
	count := 0

	for scanner.Scan() {
		line := scanner.Text()

		// 解析 YAML 头部
		if inHeader {
			trimmed := strings.TrimSpace(line)
			// 检测 columns 中是否缺少 code 列（即两列模式: text + weight）
			if strings.HasPrefix(trimmed, "- weight") {
				twoColumnMode = true
			}
			if trimmed == "..." {
				inHeader = false
			}
			continue
		}

		// 跳过空行和注释
		line = strings.TrimSpace(line)
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}

		parts := strings.Split(line, "\t")

		var word string
		var freq float64

		if twoColumnMode {
			// 两列模式: 文字\t词频
			if len(parts) < 2 {
				continue
			}
			word = parts[0]
			freq, err = strconv.ParseFloat(strings.TrimSpace(parts[1]), 64)
		} else {
			// 三列模式: 文字\t拼音\t词频
			if len(parts) < 3 {
				continue
			}
			word = parts[0]
			freq, err = strconv.ParseFloat(strings.TrimSpace(parts[2]), 64)
		}

		if err != nil || freq <= 0 {
			continue
		}

		// 取最高频次
		if existing, ok := freqMap[word]; !ok || freq > existing {
			freqMap[word] = freq
		}
		count++
	}

	if err := scanner.Err(); err != nil {
		log.Printf("读取文件失败: %s: %v", path, err)
	}

	return count
}
