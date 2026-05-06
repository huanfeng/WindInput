package tooltip

import (
	"context"
	"os"
	"reflect"
	"testing"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/pkg/config"
)

func TestChaiziProvider_EmptyPath(t *testing.T) {
	cfg := &config.TooltipChaiziConfig{Enabled: true}
	p := NewChaiziProvider(cfg, "", "")
	if p.Enabled() {
		t.Error("expected Enabled()=false when dbPath is empty")
	}
}

func TestChaiziProvider_Query(t *testing.T) {
	// 创建临时拆字数据库文件
	f, err := os.CreateTemp("", "chaizi_test_*.txt")
	if err != nil {
		t.Fatal(err)
	}
	defer os.Remove(f.Name())

	// 写入测试数据
	f.WriteString("汉\t氵廿口\n")
	f.WriteString("字\t宀子\n")
	f.WriteString("# 注释行\n")
	f.WriteString("\n") // 空行
	f.Close()

	cfg := &config.TooltipChaiziConfig{Enabled: true}
	p := NewChaiziProvider(cfg, f.Name(), "")

	sec, err := p.Query(context.Background(), candidate.Candidate{Text: "汉字"})
	if err != nil {
		t.Fatalf("Query error: %v", err)
	}
	if len(sec.Lines) != 2 {
		t.Fatalf("expected 2 lines, got %d: %v", len(sec.Lines), sec.Lines)
	}
	if sec.Lines[0] != "汉：氵廿口" {
		t.Errorf("unexpected chaizi result for 汉: %q", sec.Lines[0])
	}
	if sec.Lines[1] != "字：宀子" {
		t.Errorf("unexpected chaizi result for 字: %q", sec.Lines[1])
	}
	if !sec.Copyable {
		t.Error("expected Copyable=true")
	}
}

// TestChaiziProvider_DBCacheShared 验证多个 Provider 实例共用进程级 db 缓存：
// 方案切换时反复构造的 Provider，对同一 path 应只解析一次文件，
// 内部 data map 应指向同一份底层 hmap。
func TestChaiziProvider_DBCacheShared(t *testing.T) {
	f, err := os.CreateTemp("", "chaizi_cache_test_*.txt")
	if err != nil {
		t.Fatal(err)
	}
	defer os.Remove(f.Name())
	f.WriteString("汉\t氵廿口\n")
	f.Close()

	cfg := &config.TooltipChaiziConfig{Enabled: true}
	p1 := NewChaiziProvider(cfg, f.Name(), "")
	p2 := NewChaiziProvider(cfg, f.Name(), "")

	// 触发 sync.Once 加载
	if _, err := p1.Query(context.Background(), candidate.Candidate{Text: "汉"}); err != nil {
		t.Fatal(err)
	}
	if _, err := p2.Query(context.Background(), candidate.Candidate{Text: "汉"}); err != nil {
		t.Fatal(err)
	}

	if p1.data == nil || p2.data == nil {
		t.Fatal("expected data loaded for both providers")
	}
	if reflect.ValueOf(p1.data).Pointer() != reflect.ValueOf(p2.data).Pointer() {
		t.Error("expected p1.data and p2.data to share the same backing map (process-level cache)")
	}
}

func TestChaiziProvider_FileNotExist(t *testing.T) {
	cfg := &config.TooltipChaiziConfig{Enabled: true}
	p := NewChaiziProvider(cfg, "/nonexistent/path/chaizi.txt", "")
	// Enabled() 为 true（路径非空），但查询时文件不存在应返回空 section
	sec, err := p.Query(context.Background(), candidate.Candidate{Text: "汉"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(sec.Lines) != 0 {
		t.Errorf("expected empty section for missing file, got: %v", sec.Lines)
	}
}
