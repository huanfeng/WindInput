package s2t

import (
	"encoding/binary"
	"sort"
	"testing"

	"github.com/huanfeng/wind_input/pkg/config"
)

// makeTestDict 用给定的 key->val 映射构造内存版 .octrie 字节流。
// 与 cmd/gen_opencc_dict 编译输出格式一致，用于测试解析与查询逻辑。
func makeTestDict(t *testing.T, m map[string]string) []byte {
	t.Helper()
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Strings(keys)

	type meta struct {
		keyOff, valOff uint32
		keyLen, valLen uint16
	}
	metas := make([]meta, len(keys))
	st := make([]byte, 0, 64)
	maxKey := 0
	for i, k := range keys {
		v := m[k]
		kb, vb := []byte(k), []byte(v)
		if len(kb) > maxKey {
			maxKey = len(kb)
		}
		metas[i].keyOff = uint32(len(st))
		metas[i].keyLen = uint16(len(kb))
		st = append(st, kb...)
		metas[i].valOff = uint32(len(st))
		metas[i].valLen = uint16(len(vb))
		st = append(st, vb...)
	}

	out := make([]byte, 0, HeaderSize+len(metas)*EntrySize+len(st))
	out = append(out, []byte(FormatMagic)...)
	out = binary.LittleEndian.AppendUint32(out, FormatVersion)
	out = binary.LittleEndian.AppendUint32(out, uint32(len(keys)))
	out = binary.LittleEndian.AppendUint16(out, uint16(maxKey))
	out = binary.LittleEndian.AppendUint16(out, 0)
	for _, m := range metas {
		out = binary.LittleEndian.AppendUint32(out, m.keyOff)
		out = binary.LittleEndian.AppendUint16(out, m.keyLen)
		out = binary.LittleEndian.AppendUint32(out, m.valOff)
		out = binary.LittleEndian.AppendUint16(out, m.valLen)
	}
	out = append(out, st...)
	return out
}

func TestDictLookupAndLongestPrefix(t *testing.T) {
	data := makeTestDict(t, map[string]string{
		"软":  "軟",
		"件":  "件",
		"软件": "軟體",
	})
	d, err := ParseDict("test", data)
	if err != nil {
		t.Fatalf("ParseDict: %v", err)
	}
	if got, ok := d.Lookup([]byte("软件")); !ok || string(got) != "軟體" {
		t.Errorf("Lookup 软件 = %q,%v, want 軟體", string(got), ok)
	}
	if n, val, ok := d.LongestPrefix([]byte("软件设计")); !ok || n != len("软件") || string(val) != "軟體" {
		t.Errorf("LongestPrefix(软件设计) = (%d,%q,%v); want (6, 軟體, true)", n, string(val), ok)
	}
	if n, val, ok := d.LongestPrefix([]byte("软X")); !ok || n != len("软") || string(val) != "軟" {
		t.Errorf("LongestPrefix(软X) = (%d,%q,%v); want (3, 軟, true)", n, string(val), ok)
	}
	if _, _, ok := d.LongestPrefix([]byte("X")); ok {
		t.Error("LongestPrefix(X) should miss")
	}
}

func TestConverterApplyStep(t *testing.T) {
	dictA := mustParse(t, makeTestDict(t, map[string]string{
		"软件": "軟體",
		"系统": "系統",
	}))
	dictB := mustParse(t, makeTestDict(t, map[string]string{
		"软": "軟",
		"件": "件",
		"系": "系",
		"统": "統",
	}))
	// 单 group：A 与 B 共同竞争最长匹配（OpenCC 语义）
	c := NewConverter([][]*Dict{{dictA, dictB}}, 8)

	cases := []struct{ in, want string }{
		{"软件设计", "軟體设计"},
		{"操作系统", "操作系統"},
		{"hello", "hello"},
		{"软", "軟"}, // dictA 不命中, dictB 命中
		{"", ""},
	}
	for _, tc := range cases {
		got := c.Convert(tc.in)
		if got != tc.want {
			t.Errorf("Convert(%q) = %q, want %q", tc.in, got, tc.want)
		}
	}
	// 命中缓存
	if got := c.Convert("软件设计"); got != "軟體设计" {
		t.Errorf("cached Convert mismatch: %q", got)
	}
	c.ResetCache()
	if got := c.Convert("软件设计"); got != "軟體设计" {
		t.Errorf("after reset, Convert mismatch: %q", got)
	}
}

func mustParse(t *testing.T, data []byte) *Dict {
	t.Helper()
	d, err := ParseDict("test", data)
	if err != nil {
		t.Fatal(err)
	}
	return d
}

// TestGroupSemantics 验证 OpenCC 风格的"group 内跨成员选最长匹配"语义。
// 这正是 s2twp 链路要求的：STPhrases ∪ STCharacters 在同一步同时竞争最长前缀。
// 若按串行步骤实现（先 STPhrases 后 STCharacters），TWPhrases 会在第二步看到尚未
// 转换的简体而无法命中习惯词替换。
func TestGroupSemantics(t *testing.T) {
	// 模拟 STPhrases：词级，仅有少量短语
	stPhrases := mustParse(t, makeTestDict(t, map[string]string{
		"操作系统": "操作系統",
	}))
	// 模拟 STCharacters：字级，覆盖单字
	stChars := mustParse(t, makeTestDict(t, map[string]string{
		"软": "軟",
		"件": "件",
		"操": "操",
		"作": "作",
		"系": "系",
		"统": "統",
	}))
	// 模拟 TWPhrases：繁体 → 台湾习惯词
	twPhrases := mustParse(t, makeTestDict(t, map[string]string{
		"軟件": "軟體",
	}))

	// s2twp 风格链：group1(STPhrases ∪ STCharacters) → group2(TWPhrases)
	c := NewConverter([][]*Dict{
		{stPhrases, stChars},
		{twPhrases},
	}, 0)

	cases := []struct{ in, want string }{
		// 软件：STPhrases 没有，STCharacters 有 → 軟件 → TWPhrases 命中 → 軟體
		{"软件", "軟體"},
		// 操作系统：STPhrases 直接命中（比逐字更长） → 操作系統 → TWPhrases 不命中 → 保持
		{"操作系统", "操作系統"},
		// 仅单字
		{"软", "軟"},
	}
	for _, tc := range cases {
		if got := c.Convert(tc.in); got != tc.want {
			t.Errorf("Convert(%q) = %q, want %q", tc.in, got, tc.want)
		}
	}
}

// TestChainShape 守护各变体链路结构，避免误改 group 边界。
func TestChainShape(t *testing.T) {
	cases := []struct {
		variant     config.S2TVariant
		stepCount   int
		firstStep   []string
		mustContain []string // 某步骤必须包含的词典名
	}{
		{config.S2TStandard, 1, []string{"STPhrases", "STCharacters"}, nil},
		{config.S2TTaiwan, 2, []string{"STPhrases", "STCharacters"}, []string{"TWVariants"}},
		{config.S2TTaiwanPhrase, 3, []string{"STPhrases", "STCharacters"}, []string{"TWPhrases", "TWVariants"}},
		{config.S2THongKong, 2, []string{"STPhrases", "STCharacters"}, []string{"HKVariants"}},
	}
	for _, tc := range cases {
		groups := Chain(tc.variant)
		if len(groups) != tc.stepCount {
			t.Errorf("variant %s: step count = %d, want %d", tc.variant, len(groups), tc.stepCount)
			continue
		}
		first := groups[0]
		if !sliceEqual(first, tc.firstStep) {
			t.Errorf("variant %s: step1 = %v, want %v", tc.variant, first, tc.firstStep)
		}
		// 后续 group 必须包含期望的词典
		flat := flatten(groups[1:])
		for _, name := range tc.mustContain {
			if !sliceContains(flat, name) {
				t.Errorf("variant %s: missing dict %s in steps[1:]", tc.variant, name)
			}
		}
	}
}

// TestManagerReconfigureNoOp 验证未变化时 Reconfigure 不会报错或重建。
func TestManagerReconfigureNoOp(t *testing.T) {
	m := NewManager(t.TempDir()) // 空目录：未启用时不应触发任何加载
	cfg := config.S2TConfig{Enabled: false, Variant: config.S2TStandard}

	if _, _, err := m.Reconfigure(cfg); err != nil {
		t.Fatalf("Reconfigure(disabled) returned error: %v", err)
	}
	if m.IsEnabled() {
		t.Error("manager should not be enabled after disabled Reconfigure")
	}
	// 未启用时 Convert 应原样返回
	if got := m.Convert("软件"); got != "软件" {
		t.Errorf("disabled Convert mutated text: %q", got)
	}
	// 重复 Reconfigure 不报错
	if _, _, err := m.Reconfigure(cfg); err != nil {
		t.Fatalf("idempotent Reconfigure failed: %v", err)
	}
}

func sliceEqual(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

func sliceContains(s []string, target string) bool {
	for _, v := range s {
		if v == target {
			return true
		}
	}
	return false
}

func flatten(groups [][]string) []string {
	var out []string
	for _, g := range groups {
		out = append(out, g...)
	}
	return out
}
