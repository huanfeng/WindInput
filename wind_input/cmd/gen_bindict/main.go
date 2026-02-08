package main

import (
	"bufio"
	"flag"
	"fmt"
	"log"
	"math"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"

	"github.com/huanfeng/wind_input/internal/dict/binformat"
)

func main() {
	dictDir := flag.String("dict", "dict/pinyin", "Rime 词库目录（包含 8105.dict.yaml 和 base.dict.yaml）")
	unigramFile := flag.String("unigram", "dict/pinyin/unigram.txt", "Unigram 频次文件")
	outDir := flag.String("out", "dict/pinyin", "输出目录")
	flag.Parse()

	// 生成 pinyin.wdb
	if err := genPinyinWdb(*dictDir, *outDir); err != nil {
		log.Fatalf("生成 pinyin.wdb 失败: %v", err)
	}

	// 生成 unigram.wdb
	if err := genUnigramWdb(*unigramFile, *outDir); err != nil {
		log.Fatalf("生成 unigram.wdb 失败: %v", err)
	}

	log.Println("完成")
}

type dictEntry struct {
	text   string
	weight int
}

func genPinyinWdb(dictDir, outDir string) error {
	// 按 code 聚合所有条目
	codeEntries := make(map[string][]dictEntry)
	// 按 abbrev 聚合简拼条目
	abbrevEntries := make(map[string][]dictEntry)

	files := []string{"8105.dict.yaml", "base.dict.yaml"}
	totalCount := 0

	for _, name := range files {
		path := filepath.Join(dictDir, name)
		if _, err := os.Stat(path); os.IsNotExist(err) {
			log.Printf("跳过不存在的文件: %s", path)
			continue
		}

		count, err := loadRimeFile(path, codeEntries, abbrevEntries)
		if err != nil {
			return fmt.Errorf("加载 %s 失败: %w", name, err)
		}
		log.Printf("加载 %s: %d 条", name, count)
		totalCount += count
	}

	if totalCount == 0 {
		return fmt.Errorf("未加载到任何词条")
	}

	// 构建 DictWriter
	writer := binformat.NewDictWriter()

	// 对每个 code 的条目按 weight 降序排列
	for code, entries := range codeEntries {
		sort.Slice(entries, func(i, j int) bool {
			return entries[i].weight > entries[j].weight
		})
		binEntries := make([]binformat.DictEntry, len(entries))
		for i, e := range entries {
			binEntries[i] = binformat.DictEntry{
				Text:   e.text,
				Weight: int32(e.weight),
			}
		}
		writer.AddCode(code, binEntries)
	}

	// 添加简拼条目
	for abbrev, entries := range abbrevEntries {
		sort.Slice(entries, func(i, j int) bool {
			return entries[i].weight > entries[j].weight
		})
		binEntries := make([]binformat.DictEntry, len(entries))
		for i, e := range entries {
			binEntries[i] = binformat.DictEntry{
				Text:   e.text,
				Weight: int32(e.weight),
			}
		}
		writer.AddAbbrev(abbrev, binEntries)
	}

	// 写入文件
	outPath := filepath.Join(outDir, "pinyin.wdb")
	f, err := os.Create(outPath)
	if err != nil {
		return fmt.Errorf("创建文件失败: %w", err)
	}
	defer f.Close()

	bw := bufio.NewWriter(f)
	if err := writer.Write(bw); err != nil {
		return fmt.Errorf("写入失败: %w", err)
	}
	if err := bw.Flush(); err != nil {
		return fmt.Errorf("flush 失败: %w", err)
	}

	log.Printf("pinyin.wdb 生成完毕: %d codes, %d abbrevs → %s", len(codeEntries), len(abbrevEntries), outPath)
	return nil
}

func loadRimeFile(path string, codeEntries map[string][]dictEntry, abbrevEntries map[string][]dictEntry) (int, error) {
	file, err := os.Open(path)
	if err != nil {
		return 0, err
	}
	defer file.Close()

	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 0, 1024*1024), 1024*1024)

	inHeader := true
	count := 0

	for scanner.Scan() {
		line := scanner.Text()

		if inHeader {
			if strings.TrimSpace(line) == "..." {
				inHeader = false
			}
			continue
		}

		line = strings.TrimSpace(line)
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}

		parts := strings.Split(line, "\t")
		if len(parts) < 3 {
			continue
		}

		text := parts[0]
		pinyin := parts[1]
		weight, err := strconv.Atoi(strings.TrimSpace(parts[2]))
		if err != nil || weight <= 0 {
			continue
		}

		code := strings.ReplaceAll(pinyin, " ", "")

		codeEntries[code] = append(codeEntries[code], dictEntry{
			text:   text,
			weight: weight,
		})

		// 构建简拼索引（2 字及以上）
		syllables := strings.Fields(pinyin)
		if len(syllables) >= 2 {
			var abbrevBuilder strings.Builder
			for _, s := range syllables {
				if len(s) == 0 {
					break
				}
				abbrevBuilder.WriteByte(s[0])
			}
			abbrev := abbrevBuilder.String()
			if abbrev != "" {
				abbrevEntries[abbrev] = append(abbrevEntries[abbrev], dictEntry{
					text:   text,
					weight: weight,
				})
			}
		}

		count++
	}

	return count, scanner.Err()
}

func genUnigramWdb(unigramFile, outDir string) error {
	file, err := os.Open(unigramFile)
	if err != nil {
		return fmt.Errorf("打开 unigram 文件失败: %w", err)
	}
	defer file.Close()

	freqs := make(map[string]float64)
	var total float64

	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}

		parts := strings.Split(line, "\t")
		if len(parts) < 2 {
			continue
		}

		word := parts[0]
		freq, err := strconv.ParseFloat(parts[1], 64)
		if err != nil {
			continue
		}

		freqs[word] = freq
		total += freq
	}

	if err := scanner.Err(); err != nil {
		return fmt.Errorf("读取 unigram 文件失败: %w", err)
	}

	if total == 0 {
		return fmt.Errorf("unigram 文件为空")
	}

	writer := binformat.NewUnigramWriter()
	for word, freq := range freqs {
		logProb := math.Log(freq / total)
		writer.Add(word, logProb)
	}

	outPath := filepath.Join(outDir, "unigram.wdb")
	f, err := os.Create(outPath)
	if err != nil {
		return fmt.Errorf("创建文件失败: %w", err)
	}
	defer f.Close()

	bw := bufio.NewWriter(f)
	if err := writer.Write(bw); err != nil {
		return fmt.Errorf("写入失败: %w", err)
	}
	if err := bw.Flush(); err != nil {
		return fmt.Errorf("flush 失败: %w", err)
	}

	log.Printf("unigram.wdb 生成完毕: %d 词条 → %s", len(freqs), outPath)
	return nil
}
