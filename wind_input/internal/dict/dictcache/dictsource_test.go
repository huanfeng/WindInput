package dictcache

import (
	"log/slog"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
)

const rimeCodetableYAML = "---\n" +
	"name: t\n" +
	"version: \"1\"\n" +
	"sort: by_weight\n" +
	"columns:\n" +
	"  - text\n" +
	"  - code\n" +
	"  - weight\n" +
	"...\n" +
	"你好\tnihao\t100\n" +
	"好\thao\t50\n" +
	"# 行内注释应被跳过\n" +
	"\n" +
	"世界\tshijie\t30\n"

const rimePinyinYAML = "---\n" +
	"name: py\n" +
	"sort: by_weight\n" +
	"...\n" +
	"你好\tni hao\t100\n" +
	"好\thao\t50\n" +
	"世界\tshi jie\t30\n"

// 码表词库：yaml 直读 与 转 split 后读，产出的 codeEntries 应深度相等（强等价）。
func TestRimeYAMLToSplitEquivalence_Codetable(t *testing.T) {
	dir := t.TempDir()
	yamlPath := filepath.Join(dir, "t.dict.yaml")
	if err := os.WriteFile(yamlPath, []byte(rimeCodetableYAML), 0644); err != nil {
		t.Fatal(err)
	}

	mapA := map[string][]dictEntry{}
	orderA := 0
	nA, hwA, err := loadRimeCodetableFile(yamlPath, mapA, &orderA, slog.Default())
	if err != nil {
		t.Fatalf("加载 yaml 失败: %v", err)
	}

	tomlPath, tsvPath, err := ConvertRimeYAMLToSplit(yamlPath, "")
	if err != nil {
		t.Fatalf("转换 split 失败: %v", err)
	}

	mapB := map[string][]dictEntry{}
	orderB := 0
	nB, hwB, err := loadRimeCodetableFile(tomlPath, mapB, &orderB, slog.Default())
	if err != nil {
		t.Fatalf("加载 split 失败: %v", err)
	}

	if nA != nB || hwA != hwB {
		t.Fatalf("计数/权重标记不一致: yaml(n=%d hw=%v) split(n=%d hw=%v)", nA, hwA, nB, hwB)
	}
	if nA != 3 {
		t.Fatalf("期望 3 条，实际 %d", nA)
	}
	if !reflect.DeepEqual(mapA, mapB) {
		t.Fatalf("codeEntries 不一致:\nyaml=%+v\nsplit=%+v", mapA, mapB)
	}

	// 数据体应与 yaml `...` 之后逐字节同构
	wantBody := rimeCodetableYAML[strings.Index(rimeCodetableYAML, "...\n")+len("...\n"):]
	gotBody, err := os.ReadFile(tsvPath)
	if err != nil {
		t.Fatal(err)
	}
	if string(gotBody) != wantBody {
		t.Fatalf("tsv 体与 yaml 体不一致:\nwant=%q\ngot=%q", wantBody, string(gotBody))
	}

	// 生成的 toml 头应可解析回等价 DictHeader
	hdr, err := ReadDictHeader(tomlPath)
	if err != nil {
		t.Fatal(err)
	}
	if hdr.Name != "t" || hdr.Version != "1" || hdr.Sort != "by_weight" {
		t.Fatalf("toml 头解析异常: %+v", hdr)
	}
	if !reflect.DeepEqual(hdr.Columns, []string{"text", "code", "weight"}) {
		t.Fatalf("toml columns 异常: %+v", hdr.Columns)
	}
}

// 拼音词库：yaml 直读 与 转 split 后读，产出的 codeEntries + abbrevEntries 应深度相等。
func TestRimeYAMLToSplitEquivalence_Pinyin(t *testing.T) {
	dir := t.TempDir()
	yamlPath := filepath.Join(dir, "py.dict.yaml")
	if err := os.WriteFile(yamlPath, []byte(rimePinyinYAML), 0644); err != nil {
		t.Fatal(err)
	}

	codeA, abbrevA := map[string][]dictEntry{}, map[string][]dictEntry{}
	orderA := 0
	nA, err := loadRimeFile(yamlPath, codeA, abbrevA, &orderA, slog.Default())
	if err != nil {
		t.Fatalf("加载 yaml 失败: %v", err)
	}

	tomlPath, _, err := ConvertRimeYAMLToSplit(yamlPath, "")
	if err != nil {
		t.Fatalf("转换 split 失败: %v", err)
	}

	codeB, abbrevB := map[string][]dictEntry{}, map[string][]dictEntry{}
	orderB := 0
	nB, err := loadRimeFile(tomlPath, codeB, abbrevB, &orderB, slog.Default())
	if err != nil {
		t.Fatalf("加载 split 失败: %v", err)
	}

	if nA != nB || nA != 3 {
		t.Fatalf("计数不一致: yaml=%d split=%d", nA, nB)
	}
	if !reflect.DeepEqual(codeA, codeB) {
		t.Fatalf("codeEntries 不一致:\nyaml=%+v\nsplit=%+v", codeA, codeB)
	}
	if !reflect.DeepEqual(abbrevA, abbrevB) {
		t.Fatalf("abbrevEntries 不一致:\nyaml=%+v\nsplit=%+v", abbrevA, abbrevB)
	}
}

// import_tables 在两种格式下解析一致；split 主词库的 import 兄弟扩展名跟随主格式。
func TestImportTablesParity(t *testing.T) {
	dir := t.TempDir()
	yamlMain := filepath.Join(dir, "main.dict.yaml")
	yamlContent := "---\nname: main\nimport_tables:\n  - cn_dicts/8105\n  - others\n...\n"
	if err := os.WriteFile(yamlMain, []byte(yamlContent), 0644); err != nil {
		t.Fatal(err)
	}
	if _, _, err := ConvertRimeYAMLToSplit(yamlMain, ""); err != nil {
		t.Fatal(err)
	}
	tomlMain := filepath.Join(dir, "main.dict.toml")

	wantImports := []string{"cn_dicts/8105", "others"}
	if got := discoverRimeCodetableImports(yamlMain); !reflect.DeepEqual(got, wantImports) {
		t.Fatalf("yaml import_tables 解析异常: %+v", got)
	}
	if got := discoverRimeCodetableImports(tomlMain); !reflect.DeepEqual(got, wantImports) {
		t.Fatalf("toml import_tables 解析异常: %+v", got)
	}

	// 兄弟词库扩展名跟随主词库格式
	if dictSuffixOf(yamlMain) != dictSuffixYAML {
		t.Fatalf("yaml 主词库应用 .dict.yaml 后缀")
	}
	if dictSuffixOf(tomlMain) != dictSuffixTOML {
		t.Fatalf("toml 主词库应用 .dict.toml 后缀")
	}
}

// split 缺失配对 .dict.tsv 时，OpenDictSource 应返回错误而非 panic。
func TestSplitMissingTSV(t *testing.T) {
	dir := t.TempDir()
	tomlPath := filepath.Join(dir, "x.dict.toml")
	if err := os.WriteFile(tomlPath, []byte("name = \"x\"\nsort = \"by_weight\"\n"), 0644); err != nil {
		t.Fatal(err)
	}
	if _, _, err := OpenDictSource(tomlPath); err == nil {
		t.Fatal("缺失 .dict.tsv 时应返回错误")
	}
}
