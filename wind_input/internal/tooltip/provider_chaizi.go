package tooltip

import (
	"bufio"
	"context"
	"os"
	"strings"
	"sync"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/pkg/config"
)

// ChaiziProvider 为候选文字提供拆字提示（形码方案通用）
// 数据库文件格式：每行 <char>\t<components>，UTF-8 编码
type ChaiziProvider struct {
	cfg        *config.TooltipChaiziConfig
	dbPath     string
	fontFamily string // 拆字显示字体（空=使用默认）

	once sync.Once
	data map[rune]string // char -> components string
}

// NewChaiziProvider 创建拆字 provider
// dbPath: 拆字数据库文件路径，空字符串表示禁用
// fontFamily: 显示拆字所用字体名，空字符串表示使用默认字体
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

// Query 查询候选文字的拆字信息
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

	runes := []rune(c.Text)
	var parts []string
	for _, r := range runes {
		if comp, ok := p.data[r]; ok {
			parts = append(parts, comp)
		}
	}

	if len(parts) == 0 {
		return Section{}, nil
	}

	return Section{
		Label:    "拆字",
		Lines:    []string{strings.Join(parts, " ")},
		Copyable: true,
	}, nil
}

// loadChaiziDB 从文件加载拆字数据库
// 格式：每行 <char>\t<components>（UTF-8）
// 文件不存在或解析失败时返回空 map
func loadChaiziDB(path string) map[rune]string {
	f, err := os.Open(path)
	if err != nil {
		return nil
	}
	defer f.Close()

	data := make(map[rune]string)
	scanner := bufio.NewScanner(f)
	for scanner.Scan() {
		line := scanner.Text()
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		parts := strings.SplitN(line, "\t", 2)
		if len(parts) != 2 {
			continue
		}
		runes := []rune(strings.TrimSpace(parts[0]))
		if len(runes) != 1 {
			continue
		}
		components := strings.TrimSpace(parts[1])
		if components != "" {
			data[runes[0]] = components
		}
	}
	return data
}
