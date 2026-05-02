package main

import (
	"bufio"
	"fmt"
	"os"
	"sort"
	"strconv"
	"strings"
)

// boostMode 表示 boost 规则的调整方式
type boostMode int

const (
	boostTop      boostMode = iota // 顶置：设为该 code 当前最高权重 +1
	boostRelative                  // 相对位置：上移/下移 N 位
	boostAbsolute                  // 绝对权重：直接覆盖
)

// BoostRule 单条 boost 规则
type BoostRule struct {
	Code   string
	Text   string
	Mode   boostMode
	Delta  int // boostRelative：+N 上移 / -N 下移
	Weight int // boostAbsolute：目标权重
}

// loadBoostRules 解析 boost 规则文件。
//
// 格式（TAB 分隔）：code  text  [adjust]
//
//	adjust 留空        → 顶置
//	adjust = +N / -N  → 相对位置（上移 / 下移 N 位）
//	adjust = N        → 绝对权重
func loadBoostRules(path string) ([]BoostRule, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()

	var rules []BoostRule
	scanner := bufio.NewScanner(f)
	lineNo := 0
	for scanner.Scan() {
		lineNo++
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		parts := strings.Split(line, "\t")
		if len(parts) < 2 {
			return nil, fmt.Errorf("第 %d 行格式错误（需 code<TAB>text [<TAB>adjust]）: %s", lineNo, line)
		}
		code := strings.TrimSpace(parts[0])
		text := strings.TrimSpace(parts[1])
		if code == "" || text == "" {
			return nil, fmt.Errorf("第 %d 行 code/text 不可为空", lineNo)
		}
		rule := BoostRule{Code: code, Text: text, Mode: boostTop}
		if len(parts) >= 3 {
			adj := strings.TrimSpace(parts[2])
			if adj != "" {
				n, err := strconv.Atoi(adj)
				if err != nil {
					return nil, fmt.Errorf("第 %d 行 adjust 解析失败: %s", lineNo, adj)
				}
				if strings.HasPrefix(adj, "+") || strings.HasPrefix(adj, "-") {
					rule.Mode = boostRelative
					rule.Delta = n
				} else {
					rule.Mode = boostAbsolute
					rule.Weight = n
				}
			}
		}
		rules = append(rules, rule)
	}
	return rules, scanner.Err()
}

// applyBoostRules 按规则调整匹配条目的 OrigWeight。
// 返回 (成功条数, 未匹配条数)。
func applyBoostRules(entries []Entry, rules []BoostRule) (applied, missing int) {
	if len(rules) == 0 {
		return 0, 0
	}

	keyOf := func(c, t string) string { return c + "\x00" + t }
	idx := make(map[string]int, len(entries))
	byCode := make(map[string][]int)
	for i, e := range entries {
		idx[keyOf(e.Code, e.Text)] = i
		byCode[e.Code] = append(byCode[e.Code], i)
	}

	// sortByCode 按权重降序对该 code 下的条目索引排序，结果缓存（同 code 下若有
	// 多条规则按顺序生效；前一条修改后下一条要看到新顺序，因此修改后必须 invalidate）
	sortByCode := func(code string) []int {
		list := byCode[code]
		sort.SliceStable(list, func(a, b int) bool {
			wa, wb := entries[list[a]].OrigWeight, entries[list[b]].OrigWeight
			if wa != wb {
				return wa > wb
			}
			return entries[list[a]].Text < entries[list[b]].Text
		})
		byCode[code] = list
		return list
	}

	for _, r := range rules {
		i, ok := idx[keyOf(r.Code, r.Text)]
		if !ok {
			fmt.Printf("        [警告] boost 未匹配: code=%s text=%s\n", r.Code, r.Text)
			missing++
			continue
		}

		switch r.Mode {
		case boostAbsolute:
			entries[i].OrigWeight = r.Weight

		case boostTop:
			list := sortByCode(r.Code)
			entries[i].OrigWeight = entries[list[0]].OrigWeight + 1

		case boostRelative:
			list := sortByCode(r.Code)
			cur := -1
			for k, v := range list {
				if v == i {
					cur = k
					break
				}
			}
			if cur < 0 {
				missing++
				continue
			}
			target := cur - r.Delta // +N 表示上移 → 索引减小
			if target < 0 {
				target = 0
			}
			if target >= len(list) {
				target = len(list) - 1
			}
			if target == cur {
				applied++
				continue
			}
			if target < cur {
				// 上移：取目标位置候选权重 +1，挤到其前面
				entries[i].OrigWeight = entries[list[target]].OrigWeight + 1
			} else {
				// 下移：取目标位置候选权重 -1，落到其后面
				entries[i].OrigWeight = entries[list[target]].OrigWeight - 1
			}
		}

		// 同 code 下顺序变了，使下次 sortByCode 重新排序
		sort.SliceStable(byCode[r.Code], func(a, b int) bool {
			wa, wb := entries[byCode[r.Code][a]].OrigWeight, entries[byCode[r.Code][b]].OrigWeight
			if wa != wb {
				return wa > wb
			}
			return entries[byCode[r.Code][a]].Text < entries[byCode[r.Code][b]].Text
		})
		applied++
	}
	return applied, missing
}
