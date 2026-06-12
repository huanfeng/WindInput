// factory_extra_dict_test.go — 混输方案扩展词库加载回归测试。
//
// 背景（bug）："孙燕姿(bauq)" 在纯五笔下能打、混输下打不出。根因是 createMixedEngine
// 手工重建码表加载流程时漏抄了"加载已启用扩展词库"这段（孙燕姿仅存在于扩展词库
// wubi86_jidian_extra），导致混输的 CompositeDict 里根本没有该词。
//
// 修复方式：抽出共享的 mixedExtraDictSource（决定加载哪些扩展词库）+ loadEnabledExtraDicts
// （实际加载），码表方案与混输方案共用，避免两条路径漂移。本文件锁定这两段逻辑。
package schema

import (
	"log/slog"
	"os"
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
)

func boolPtr(b bool) *bool { return &b }

// TestMixedExtraDictSource 锁定扩展词库来源选择：混输自身有 Dicts 用自身，
// 否则回退主方案的 Dicts（这正是漂移 bug 的修复点——历史上混输根本不看主方案 Dicts）。
func TestMixedExtraDictSource(t *testing.T) {
	primary := &Schema{
		Schema: SchemaInfo{ID: "wubi86"},
		Dicts: []DictSpec{
			{ID: "wubi86_main", Default: true},
			{ID: "wubi86_extra"},
		},
	}

	t.Run("混输无自有Dicts_回退主方案", func(t *testing.T) {
		mixedS := &Schema{Schema: SchemaInfo{ID: "wubi86_pinyin"}}
		dicts, cacheID := mixedExtraDictSource(mixedS, primary)
		if cacheID != "wubi86" {
			t.Errorf("cacheSchemaID = %q, want wubi86（复用主方案缓存）", cacheID)
		}
		if len(dicts) != 2 || dicts[1].ID != "wubi86_extra" {
			t.Errorf("dicts = %+v, want 主方案的 Dicts（含 wubi86_extra）", dicts)
		}
	})

	t.Run("混输有自有Dicts_用自身", func(t *testing.T) {
		mixedS := &Schema{
			Schema: SchemaInfo{ID: "wubi86_pinyin"},
			Dicts:  []DictSpec{{ID: "own_extra"}},
		}
		dicts, cacheID := mixedExtraDictSource(mixedS, primary)
		if cacheID != "wubi86_pinyin" {
			t.Errorf("cacheSchemaID = %q, want wubi86_pinyin", cacheID)
		}
		if len(dicts) != 1 || dicts[0].ID != "own_extra" {
			t.Errorf("dicts = %+v, want 混输自身 Dicts", dicts)
		}
	})

	t.Run("无主方案_兜底自身ID", func(t *testing.T) {
		mixedS := &Schema{Schema: SchemaInfo{ID: "wubi86_pinyin"}}
		dicts, cacheID := mixedExtraDictSource(mixedS, nil)
		if cacheID != "wubi86_pinyin" || dicts != nil {
			t.Errorf("got (%v, %q), want (nil, wubi86_pinyin)", dicts, cacheID)
		}
	})
}

// writeMinimalRimeCodetable 写一个最小可转换的 rime 码表 yaml（单文件、无 import）。
func writeMinimalRimeCodetable(t *testing.T, dir, name string, lines ...string) string {
	t.Helper()
	var b []byte
	header := "# test dict\n---\nname: " + name + "\nversion: \"test\"\nsort: by_weight\ncolumns:\n  - code\n  - text\n  - weight\n...\n"
	b = append(b, header...)
	for _, ln := range lines {
		b = append(b, ln...)
		b = append(b, '\n')
	}
	path := filepath.Join(dir, name+".dict.yaml")
	if err := os.WriteFile(path, b, 0o644); err != nil {
		t.Fatalf("写测试码表失败: %v", err)
	}
	return path
}

func compositeHasText(cd *dict.CompositeDict, code, want string) bool {
	for _, c := range cd.Search(code, dict.SearchOptions{}) {
		if c.Text == want {
			return true
		}
	}
	return false
}

func candTexts(cands []candidate.Candidate) []string {
	out := make([]string, 0, len(cands))
	for _, c := range cands {
		out = append(out, c.Text)
	}
	return out
}

// TestLoadEnabledExtraDicts 验证共享加载逻辑：
//   - 已启用的非默认扩展词库 → 加载并可在 CompositeDict 中检索到（复现"孙燕姿(bauq)"）；
//   - default 主词库与被禁用的扩展词库 → 跳过，不加载。
func TestLoadEnabledExtraDicts(t *testing.T) {
	logger := slog.New(slog.DiscardHandler)
	srcDir := t.TempDir()

	// 已启用扩展词库：含 孙燕姿(bauq)
	extraPath := writeMinimalRimeCodetable(t, srcDir, "test_extra_sunyanzi", "bauq\t孙燕姿\t1415")
	// 被禁用扩展词库：含一个不应出现的词
	disabledPath := writeMinimalRimeCodetable(t, srcDir, "test_extra_disabled", "zzzz\t不应出现\t1")

	dicts := []DictSpec{
		{ID: "main", Path: "no-such-main.dict.yaml", Type: DictTypeRimeCodetable, Default: true},         // default 跳过
		{ID: "extra_sunyanzi", Path: extraPath, Type: DictTypeRimeCodetable},                             // 启用 → 加载
		{ID: "extra_disabled", Path: disabledPath, Type: DictTypeRimeCodetable, Enabled: boolPtr(false)}, // 禁用 → 跳过
	}

	dm := dict.NewDictManager(t.TempDir(), t.TempDir(), logger)

	// 绝对路径已能被 resolvePath 直接返回，exeDir/dataDir 传空即可。
	layers := loadEnabledExtraDicts(dm, "wubitest_pinyin", "wubitest", dicts, "", "", logger)

	if len(layers) != 1 {
		t.Fatalf("加载层数 = %d, want 1（仅启用的非默认扩展词库）", len(layers))
	}

	cd := dm.GetCompositeDict()
	if !compositeHasText(cd, "bauq", "孙燕姿") {
		t.Errorf("CompositeDict.Search(\"bauq\") 未包含 孙燕姿；候选=%v", candTexts(cd.Search("bauq", dict.SearchOptions{})))
	}
	if compositeHasText(cd, "zzzz", "不应出现") {
		t.Errorf("被禁用的扩展词库不应被加载，但 zzzz→不应出现 被检索到")
	}
}
