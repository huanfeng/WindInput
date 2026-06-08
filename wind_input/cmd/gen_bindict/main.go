package main

import (
	"bufio"
	"flag"
	"fmt"
	"log"
	"math"
	"os"
	"path/filepath"
	"strconv"
	"strings"

	"github.com/huanfeng/wind_input/internal/dict/binformat"
)

func main() {
	unigramFile := flag.String("unigram", "schemas/pinyin/unigram.txt", "Unigram 频次文件")
	outDir := flag.String("out", "schemas/pinyin", "输出目录")
	flag.Parse()

	// 生成 unigram.wdb（拼音词库已统一走 DAT，由运行时按需构建 .wdat）
	if err := genUnigramWdb(*unigramFile, *outDir); err != nil {
		log.Fatalf("生成 unigram.wdb 失败: %v", err)
	}

	log.Println("完成")
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
