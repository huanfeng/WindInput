package tooltip

import (
	"bufio"
	"context"
	"fmt"
	"os"
	"strings"
	"sync"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/pkg/config"
)

// ChaiziProvider 为候选文字提供拆字提示（形码方案通用）
// 数据库文件格式：每行 <char>\t<components>\t<code>，UTF-8 编码
type ChaiziProvider struct {
	cfg        *config.TooltipChaiziConfig
	dbPath     string
	fontFamily string

	once sync.Once
	data map[rune]chaiziEntry
}

type chaiziEntry struct {
	components string
	code       string
}

// NewChaiziProvider 创建拆字 provider
func NewChaiziProvider(cfg *config.TooltipChaiziConfig, dbPath, fontFamily string) *ChaiziProvider {
	return &ChaiziProvider{
		cfg:        cfg,
		dbPath:     dbPath,
		fontFamily: fontFamily,
	}
}

func (p *ChaiziProvider) Name() string { return "chaizi" }

func (p *ChaiziProvider) Enabled() bool {
	return p.cfg != nil && p.cfg.Enabled && p.dbPath != ""
}

// Query 逐字查询拆字信息，格式：字：字根 [编码]
func (p *ChaiziProvider) Query(_ context.Context, c candidate.Candidate) (Section, error) {
	if !p.Enabled() {
		return Section{}, nil
	}

	p.once.Do(func() {
		p.data = loadChaiziDB(p.dbPath)
	})

	if len(p.data) == 0 {
		return Section{}, nil
	}

	var lines []string
	for _, r := range []rune(c.Text) {
		entry, ok := p.data[r]
		if !ok {
			continue
		}
		var line string
		if entry.code != "" {
			line = fmt.Sprintf("%s：%s [%s]", string(r), entry.components, entry.code)
		} else {
			line = fmt.Sprintf("%s：%s", string(r), entry.components)
		}
		lines = append(lines, line)
	}

	if len(lines) == 0 {
		return Section{}, nil
	}

	return Section{
		Label:        "拆字",
		Lines:        lines,
		Copyable:     true,
		AlwaysExpand: true,
	}, nil
}

// loadChaiziDB 从文件加载拆字数据库
// 格式：每行 <char>\t<components>\t<code>（第三字段可选）
func loadChaiziDB(path string) map[rune]chaiziEntry {
	f, err := os.Open(path)
	if err != nil {
		return nil
	}
	defer f.Close()

	data := make(map[rune]chaiziEntry)
	scanner := bufio.NewScanner(f)
	for scanner.Scan() {
		line := scanner.Text()
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		parts := strings.SplitN(line, "\t", 3)
		if len(parts) < 2 {
			continue
		}
		runes := []rune(strings.TrimSpace(parts[0]))
		if len(runes) != 1 {
			continue
		}
		components := strings.TrimSpace(parts[1])
		if components == "" {
			continue
		}
		entry := chaiziEntry{components: components}
		if len(parts) == 3 {
			entry.code = strings.TrimSpace(parts[2])
		}
		data[runes[0]] = entry
	}
	return data
}
