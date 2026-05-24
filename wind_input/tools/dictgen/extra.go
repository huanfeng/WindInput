package main

import (
	"bufio"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"
)

// extraCategory 扩展词库条目分类
type extraCategory int

const (
	catCJK     extraCategory = iota // 含 CJK 的中文词条（主类）
	catEmoji                        // 含 emoji 的条目
	catEnglish                      // 全 ASCII 英文/品牌
	catSymbol                       // 其它非 CJK 非 emoji 非 ASCII 字母（特殊字符）
)

func (c extraCategory) suffix() string {
	switch c {
	case catCJK:
		return "extra"
	case catEmoji:
		return "emoji"
	case catEnglish:
		return "english"
	case catSymbol:
		return "symbols"
	}
	return "unknown"
}

// classifyExtraEntry 按 text 字符构成判定分类。
// 优先级：emoji > CJK > english > symbol
// （emoji 优先是为了把"🐶 + 备注"这种条目正确归到 emoji 桶；
//
//	CJK 次之是因为绝大多数中文条目走这条路）
func classifyExtraEntry(text string) extraCategory {
	if hasEmoji(text) {
		return catEmoji
	}
	if hasCJK(text) {
		return catCJK
	}
	onlyASCII := true
	hasLetter := false
	for _, r := range text {
		if r > 0x7E || r < 0x20 {
			onlyASCII = false
			break
		}
		if (r >= 'A' && r <= 'Z') || (r >= 'a' && r <= 'z') {
			hasLetter = true
		}
	}
	if onlyASCII && hasLetter {
		return catEnglish
	}
	return catSymbol
}

// parseExtraDict 解析 extra 词库文件（rime-wubi-jidian extra 格式）
// 列顺序：text<TAB>code[<TAB>weight][<TAB>note]
// 跳过 ## 分组注释、空行、# 行
func parseExtraDict(path string) ([]Entry, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()

	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 0, 1024*1024), 1024*1024)

	inHeader := true
	var entries []Entry
	pos := 0

	for scanner.Scan() {
		line := scanner.Text()
		if inHeader {
			if strings.TrimSpace(line) == "..." {
				inHeader = false
			}
			continue
		}
		trimmed := strings.TrimSpace(line)
		if trimmed == "" || strings.HasPrefix(trimmed, "#") {
			continue
		}
		parts := strings.Split(line, "\t")
		if len(parts) < 2 {
			continue
		}
		text := strings.TrimSpace(parts[0])
		code := strings.ToLower(strings.TrimSpace(parts[1])) // 容错：源数据偶有大写 code（如 "api\tAPI"），统一为小写
		if text == "" || code == "" {
			continue
		}
		weight := 0
		if len(parts) >= 3 {
			if w, err := strconv.Atoi(strings.TrimSpace(parts[2])); err == nil {
				weight = w
			}
		}
		entries = append(entries, Entry{
			Text:       text,
			Code:       code,
			OrigWeight: weight,
			origPos:    pos,
		})
		pos++
	}
	return entries, scanner.Err()
}

// processExtra 处理扩展词库：分类 + 加权 + 写出 4 个 yaml 文件。
// 文件名按 cfg.OutputPath 的后缀模式派生（保持与主输出一致：dev 带 .out / build 不带）。
func processExtra(cfg *Config, unigram map[string]int64, logMedian float64) error {
	if !cfg.Extra.Enabled {
		return nil
	}
	if cfg.Extra.InputPath == "" {
		return fmt.Errorf("extra.input_path 未配置")
	}
	if _, err := os.Stat(cfg.Extra.InputPath); err != nil {
		fmt.Printf("\n[extra] 跳过：输入文件不存在 (%s)\n", cfg.Extra.InputPath)
		return nil
	}

	fmt.Printf("\n[extra] 处理扩展词库: %s\n", cfg.Extra.InputPath)
	entries, err := parseExtraDict(cfg.Extra.InputPath)
	if err != nil {
		return fmt.Errorf("解析 extra 失败: %w", err)
	}
	fmt.Printf("      读取 %d 条原始条目\n", len(entries))

	buckets := make(map[extraCategory][]Entry)
	for _, e := range entries {
		cat := classifyExtraEntry(e.Text)
		buckets[cat] = append(buckets[cat], e)
	}

	// CJK 部分按 unigram 加权
	cjkEntries := buckets[catCJK]
	cjkHit := 0
	for i := range cjkEntries {
		if logMedian > 0 {
			if freq, ok := unigram[cjkEntries[i].Text]; ok {
				cjkEntries[i].OrigWeight = computeWeight(freq, logMedian, cfg)
				cjkHit++
				continue
			}
		}
		// unigram 未命中：用原始优先级保底，无原始权重则给 priority_10
		if cjkEntries[i].OrigWeight > 0 {
			cjkEntries[i].OrigWeight = fallbackWeight(cjkEntries[i].OrigWeight, cfg)
		} else {
			cjkEntries[i].OrigWeight = cfg.Fallback.Priority10
		}
	}
	buckets[catCJK] = cjkEntries

	// 其它桶（emoji/english/symbol）：保留原权重；无权重则给低权重默认值
	defaultWeight := cfg.Extra.DefaultWeight
	if defaultWeight <= 0 {
		defaultWeight = 100
	}
	for _, cat := range []extraCategory{catEmoji, catEnglish, catSymbol} {
		list := buckets[cat]
		for i := range list {
			if list[i].OrigWeight <= 0 {
				list[i].OrigWeight = defaultWeight
			}
		}
		buckets[cat] = list
	}

	// 合并自定义 emoj 编码 emoji（手动维护的常用 emoji 快捷入口）
	if customEmoji, cerr := loadCustomEmoji(cfg.CustomEmojiPath); cerr != nil {
		fmt.Printf("      [custom_emoji] 加载失败，跳过: %v\n", cerr)
	} else if len(customEmoji) > 0 {
		buckets[catEmoji] = append(customEmoji, buckets[catEmoji]...)
		fmt.Printf("      [custom_emoji] 注入 %d 条 emoj 编码条目\n", len(customEmoji))
	}

	for _, cat := range []extraCategory{catCJK, catEmoji, catEnglish, catSymbol} {
		list := buckets[cat]
		name := fmt.Sprintf("%s_%s", cfg.OutputName, cat.suffix())
		path := extraOutputPath(cfg.OutputPath, cfg.OutputName, cat.suffix())
		if err := writeExtraYAML(path, list, name, cat); err != nil {
			return fmt.Errorf("写出 %s 失败: %w", path, err)
		}
		fmt.Printf("      [%s] %d 条 → %s\n", cat.suffix(), len(list), path)
	}
	if logMedian > 0 && len(buckets[catCJK]) > 0 {
		fmt.Printf("      CJK unigram 命中率: %d/%d (%.1f%%)\n",
			cjkHit, len(buckets[catCJK]),
			100*float64(cjkHit)/float64(len(buckets[catCJK])))
	}
	return nil
}

// extraOutputPath 把主输出路径里的 outputName 替换成 outputName_<suffix>，保留其余部分。
// 例如 OutputPath=".../wubi86_jidian.dict.out.yaml" + suffix="emoji" →
//
//	".../wubi86_jidian_emoji.dict.out.yaml"
//
// 例如 OutputPath=".../wubi86_jidian.dict.yaml" + suffix="emoji" →
//
//	".../wubi86_jidian_emoji.dict.yaml"
func extraOutputPath(mainPath, outputName, suffix string) string {
	dir := filepath.Dir(mainPath)
	base := filepath.Base(mainPath)
	newBase := strings.Replace(base, outputName, outputName+"_"+suffix, 1)
	return filepath.Join(dir, newBase)
}

// loadCustomEmoji 从文件加载自定义 emoji 列表，每行一个 emoji，
// 按行序从高到低分配权重（起始 200），code 固定为 "emoj"。
// 文件不存在时返回空列表（非错误）。
func loadCustomEmoji(path string) ([]Entry, error) {
	if path == "" {
		return nil, nil
	}
	f, err := os.Open(path)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, err
	}
	defer f.Close()

	var emojis []string
	scanner := bufio.NewScanner(f)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		emojis = append(emojis, line)
	}
	if err := scanner.Err(); err != nil {
		return nil, err
	}

	const baseWeight = 200
	entries := make([]Entry, 0, len(emojis))
	for i, emoji := range emojis {
		w := baseWeight - i
		if w < 1 {
			w = 1
		}
		entries = append(entries, Entry{
			Text:       emoji,
			Code:       "emoj",
			OrigWeight: w,
		})
	}
	return entries, nil
}

func writeExtraYAML(path string, entries []Entry, name string, cat extraCategory) error {
	if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
		return err
	}

	sort.SliceStable(entries, func(i, j int) bool {
		if entries[i].Code != entries[j].Code {
			return entries[i].Code < entries[j].Code
		}
		if entries[i].OrigWeight != entries[j].OrigWeight {
			return entries[i].OrigWeight > entries[j].OrigWeight
		}
		return entries[i].Text < entries[j].Text
	})

	tmpPath := path + ".tmp"
	f, err := os.Create(tmpPath)
	if err != nil {
		return err
	}
	bw := bufio.NewWriter(f)
	version := time.Now().Format("2006-01-02")

	fmt.Fprintf(bw, "# Rime dictionary - WindInput 五笔扩展词库 (%s)\n", cat.suffix())
	fmt.Fprintf(bw, "# 来源: rime-wubi86-jidian extra，由 dictgen 按字符类型拆分\n")
	fmt.Fprintf(bw, "# 生成: %s\n", version)
	fmt.Fprintf(bw, "---\n")
	fmt.Fprintf(bw, "name: %s\n", name)
	fmt.Fprintf(bw, "version: \"%s\"\n", version)
	fmt.Fprintf(bw, "sort: by_weight\n")
	fmt.Fprintf(bw, "use_preset_vocabulary: false\n")
	fmt.Fprintf(bw, "columns:\n")
	fmt.Fprintf(bw, "  - code\n")
	fmt.Fprintf(bw, "  - text\n")
	fmt.Fprintf(bw, "  - weight\n")
	fmt.Fprintf(bw, "...\n")
	for _, e := range entries {
		fmt.Fprintf(bw, "%s\t%s\t%d\n", e.Code, e.Text, e.OrigWeight)
	}

	if err := bw.Flush(); err != nil {
		f.Close()
		os.Remove(tmpPath)
		return err
	}
	if err := f.Close(); err != nil {
		os.Remove(tmpPath)
		return err
	}
	os.Remove(path)
	return os.Rename(tmpPath, path)
}
