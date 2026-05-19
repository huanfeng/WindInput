package shuangpin

import (
	"testing"
)

func TestXiaoheBasic(t *testing.T) {
	scheme := Get("xiaohe")
	if scheme == nil {
		t.Fatal("小鹤方案未注册")
	}
	conv := NewConverter(scheme)

	tests := []struct {
		input       string
		wantPinyin  string
		wantPartial bool
		desc        string
	}{
		{"ni", "ni", false, "ni→ni"},
		{"nihc", "nihao", false, "nihc→nihao (h+c=hao)"},
		{"womf", "women", false, "womf→women (m=m, f=en)"},
		{"n", "n", true, "单键 partial（含声母前缀）"},
		{"", "", false, "空输入"},
	}

	for _, tt := range tests {
		t.Run(tt.desc, func(t *testing.T) {
			result := conv.Convert(tt.input)
			if result.FullPinyin != tt.wantPinyin {
				t.Errorf("Convert(%q).FullPinyin = %q, want %q", tt.input, result.FullPinyin, tt.wantPinyin)
			}
			if result.HasPartial != tt.wantPartial {
				t.Errorf("Convert(%q).HasPartial = %v, want %v", tt.input, result.HasPartial, tt.wantPartial)
			}
		})
	}
}

func TestXiaoheSyllables(t *testing.T) {
	scheme := Get("xiaohe")
	conv := NewConverter(scheme)

	// "nihc" = ni + hao (小鹤：h=h, c=ao)
	result := conv.Convert("nihc")
	if len(result.Syllables) != 2 {
		t.Fatalf("期望 2 个音节，实际 %d", len(result.Syllables))
	}
	if result.Syllables[0].Pinyin != "ni" {
		t.Errorf("第 1 音节 = %q, 期望 'ni'", result.Syllables[0].Pinyin)
	}
	if result.Syllables[1].Pinyin != "hao" {
		t.Errorf("第 2 音节 = %q, 期望 'hao'", result.Syllables[1].Pinyin)
	}

	// 检查双拼位置映射
	if result.Syllables[0].SPStart != 0 || result.Syllables[0].SPEnd != 2 {
		t.Errorf("第 1 音节 SP 位置 = [%d,%d), 期望 [0,2)", result.Syllables[0].SPStart, result.Syllables[0].SPEnd)
	}
	if result.Syllables[1].SPStart != 2 || result.Syllables[1].SPEnd != 4 {
		t.Errorf("第 2 音节 SP 位置 = [%d,%d), 期望 [2,4)", result.Syllables[1].SPStart, result.Syllables[1].SPEnd)
	}
}

func TestXiaoheZhChSh(t *testing.T) {
	scheme := Get("xiaohe")
	conv := NewConverter(scheme)

	tests := []struct {
		input string
		want  string
		desc  string
	}{
		{"vs", "zhong", "v=zh, s=ong → zhong"},
		{"ig", "cheng", "i=ch, g=eng → cheng"},
		{"uf", "shen", "u=sh, f=en → shen"},
		{"vv", "zhui", "v=zh, v=ui → zhui"},
		{"dv", "dui", "d=d, v=ui → dui"},
		{"gv", "gui", "g=g, v=ui → gui"},
		{"go", "guo", "g=g, o=uo → guo"},
		{"ho", "huo", "h=h, o=uo → huo"},
		{"xp", "xie", "x=x, p=ie → xie"},
		{"bp", "bie", "b=b, p=ie → bie"},
		{"zz", "zou", "z=z, z=ou → zou"},
		{"dz", "dou", "d=d, z=ou → dou"},
		{"nv", "nv", "n=n, v=v(ü) → nv（女）"},
		{"lv", "lv", "l=l, v=v(ü) → lv（绿）"},
	}

	for _, tt := range tests {
		t.Run(tt.desc, func(t *testing.T) {
			result := conv.Convert(tt.input)
			if result.FullPinyin != tt.want {
				t.Errorf("Convert(%q) = %q, want %q", tt.input, result.FullPinyin, tt.want)
			}
		})
	}
}

func TestXiaoheZeroInitial(t *testing.T) {
	scheme := Get("xiaohe")
	conv := NewConverter(scheme)

	tests := []struct {
		input string
		want  string
		desc  string
	}{
		{"aa", "a", "aa→a（单韵母重复键）"},
		{"oo", "o", "oo→o"},
		{"ee", "e", "ee→e"},
		// 直接表音匹配：双拼零声母规则——直接用拼音字母输入
		// ai/an/ei/en/ou 的第二键在 FinalMap 中不映射为这些零声母音节，需直接表音匹配
		{"ai", "ai", "ai→ai（直接表音）"},
		{"an", "an", "an→an（直接表音）"},
		{"ei", "ei", "ei→ei（直接表音）"},
		{"en", "en", "en→en（直接表音）"},
		{"ou", "ou", "ou→ou（直接表音）"},
		// ao 不在此列：'a'+'o' 中 FinalMap['o']=["uo","o"]，validPinyins["o"]=true 先命中，
		// 结果为 "o"（正确），"ao" 的双拼编码是 'a'+'c'
	}

	for _, tt := range tests {
		t.Run(tt.desc, func(t *testing.T) {
			result := conv.Convert(tt.input)
			if result.FullPinyin != tt.want {
				t.Errorf("Convert(%q) = %q, want %q", tt.input, result.FullPinyin, tt.want)
			}
		})
	}
}

func TestConsumedLengthMapping(t *testing.T) {
	scheme := Get("xiaohe")
	conv := NewConverter(scheme)

	// "nihc" → "nihao" (4个双拼键 → 5个全拼字符)
	result := conv.Convert("nihc")

	tests := []struct {
		fpConsumed int
		wantSP     int
		desc       string
	}{
		{0, 0, "全拼消耗0"},
		{2, 2, "全拼消耗2(ni)→双拼消耗2"},
		{5, 4, "全拼消耗5(nihao)→双拼消耗4"},
	}

	for _, tt := range tests {
		t.Run(tt.desc, func(t *testing.T) {
			gotSP := result.MapConsumedLength(tt.fpConsumed)
			if gotSP != tt.wantSP {
				t.Errorf("MapConsumedLength(%d) = %d, want %d", tt.fpConsumed, gotSP, tt.wantSP)
			}
		})
	}
}

func TestConsumedLengthAbbrev(t *testing.T) {
	scheme := Get("xiaohe")
	conv := NewConverter(scheme)

	// "bzd" → 简拼（无有效键对），全拼="bzd"(3字节)，双拼也是3字节
	result := conv.Convert("bzd")
	gotSP := result.MapConsumedLength(3)
	if gotSP != 3 {
		t.Errorf("MapConsumedLength(3) for 'bzd' = %d, want 3", gotSP)
	}

	// "nihcbzd" → 2个有效键对 + 简拼尾部
	result2 := conv.Convert("nihcbzd")
	// 全拼 "nihao"(5) + "bzd"(3) = 8，消耗全部应返回7
	gotSP2 := result2.MapConsumedLength(8)
	if gotSP2 != 7 {
		t.Errorf("MapConsumedLength(8) for 'nihcbzd' = %d, want 7", gotSP2)
	}
	// 只消耗 "nihao"(5) 应返回4
	gotSP3 := result2.MapConsumedLength(5)
	if gotSP3 != 4 {
		t.Errorf("MapConsumedLength(5) for 'nihcbzd' = %d, want 4", gotSP3)
	}
}

func TestPartialInput(t *testing.T) {
	scheme := Get("xiaohe")
	conv := NewConverter(scheme)

	// 3 个键 = 1 完整音节 + 1 partial
	result := conv.Convert("nih")
	if len(result.Syllables) != 1 {
		t.Errorf("期望 1 个完成音节，实际 %d", len(result.Syllables))
	}
	if !result.HasPartial {
		t.Error("期望 HasPartial=true")
	}
	if result.PartialInitial != "h" {
		t.Errorf("PartialInitial = %q, 期望 'h'", result.PartialInitial)
	}
}

func TestZiranmaVKey(t *testing.T) {
	scheme := Get("ziranma")
	conv := NewConverter(scheme)

	tests := []struct {
		input string
		want  string
		desc  string
	}{
		{"dv", "dui", "d=d, v=ui → dui"},
		{"gv", "gui", "g=g, v=ui → gui"},
		{"nv", "nv", "n=n, v=v(ü) → nv（女）"},
		{"lv", "lv", "l=l, v=v(ü) → lv（绿）"},
	}

	for _, tt := range tests {
		t.Run(tt.desc, func(t *testing.T) {
			result := conv.Convert(tt.input)
			if result.FullPinyin != tt.want {
				t.Errorf("Convert(%q) = %q, want %q", tt.input, result.FullPinyin, tt.want)
			}
		})
	}
}

func TestSogouVKey(t *testing.T) {
	scheme := Get("sogou")
	conv := NewConverter(scheme)

	tests := []struct {
		input string
		want  string
		desc  string
	}{
		{"dv", "dui", "d=d, v=ui → dui"},
		{"gv", "gui", "g=g, v=ui → gui"},
		// 搜狗双拼中 ü 通过 y 键输入（y=uai/v），v 键仅映射 ui
		{"ny", "nv", "n=n, y=v(ü) → nv（女）"},
		{"ly", "lv", "l=l, y=v(ü) → lv（绿）"},
	}

	for _, tt := range tests {
		t.Run(tt.desc, func(t *testing.T) {
			result := conv.Convert(tt.input)
			if result.FullPinyin != tt.want {
				t.Errorf("Convert(%q) = %q, want %q", tt.input, result.FullPinyin, tt.want)
			}
		})
	}
}

func TestZiguangScheme(t *testing.T) {
	scheme := Get("ziguang")
	if scheme == nil {
		t.Fatal("紫光方案未注册")
	}
	conv := NewConverter(scheme)

	tests := []struct {
		input string
		want  string
		desc  string
	}{
		// 声母：u=zh, i=sh, a=ch
		{"ut", "zheng", "u=zh, t=eng → zheng"},
		{"ux", "zhua", "u=zh, x=ua → zhua"},
		{"ir", "shan", "i=sh, r=an → shan"},
		{"ik", "shei", "i=sh, k=ei → shei"},
		{"aq", "chao", "a=ch, q=ao → chao"},
		// 韵母键
		{"nb", "niao", "n=n, b=iao → niao"},
		{"mw", "men", "m=m, w=en → men"},
		{"ds", "dang", "d=d, s=ang → dang"},
		{"gh", "gong", "g=g, h=ong → gong"},
		{"jj", "jiu", "j=j, j=iu → jiu"},
		{"lk", "lei", "l=l, k=ei → lei"},
		{"ll", "luan", "l=l, l=uan → luan"},
		{"xy", "xin", "x=x, y=in → xin"},
		{"gz", "gou", "g=g, z=ou → gou"},
		// nv/lv 通过 n 键（n键=ue/ui/ve）
		{"nn", "nve", "n=n, n=ve → nve（女）"},
		{"ln", "lve", "l=l, n=ve → lve（绿）"},
	}

	for _, tt := range tests {
		t.Run(tt.desc, func(t *testing.T) {
			result := conv.Convert(tt.input)
			if result.FullPinyin != tt.want {
				t.Errorf("Convert(%q) = %q, want %q", tt.input, result.FullPinyin, tt.want)
			}
		})
	}
}

// TestZeroInitialAo 验证各方案零声母 "ao" 的正确解析（Bug 复现）
// 用户反馈：自然码打不出 ao（应输入 ak），小鹤打 ao/aa 有问题
// 各方案零声母规则：
//   - 小鹤：a+c=ao，a+a=a，a+i=ai，a+n=an，a+h=ang
//   - 自然码/mspy/搜狗：a+k=ao，a+a=a，a+l=ai，a+j=an，a+h=ang
func TestZeroInitialAo(t *testing.T) {
	cases := []struct {
		schemeID string
		input    string
		want     string
		desc     string
	}{
		// 小鹤：ao 的韵母键是 c
		{"xiaohe", "ac", "ao", "小鹤 a+c→ao"},
		{"xiaohe", "aa", "a", "小鹤 a+a→a（零声母双击）"},
		{"xiaohe", "ai", "ai", "小鹤 a+i→ai"},
		{"xiaohe", "an", "an", "小鹤 a+n→an"},
		{"xiaohe", "ah", "ang", "小鹤 a+h→ang"},
		// 自然码：ao 的韵母键是 k
		{"ziranma", "ak", "ao", "自然码 a+k→ao"},
		{"ziranma", "aa", "a", "自然码 a+a→a"},
		{"ziranma", "al", "ai", "自然码 a+l→ai"},
		{"ziranma", "aj", "an", "自然码 a+j→an"},
		{"ziranma", "ah", "ang", "自然码 a+h→ang"},
		// mspy：ao 的韵母键是 k
		{"mspy", "ak", "ao", "mspy a+k→ao"},
		{"mspy", "aa", "a", "mspy a+a→a"},
		{"mspy", "al", "ai", "mspy a+l→ai"},
		{"mspy", "aj", "an", "mspy a+j→an"},
		{"mspy", "ah", "ang", "mspy a+h→ang"},
		// 搜狗：ao 的韵母键是 k
		{"sogou", "ak", "ao", "搜狗 a+k→ao"},
		{"sogou", "aa", "a", "搜狗 a+a→a"},
		{"sogou", "al", "ai", "搜狗 a+l→ai"},
		{"sogou", "aj", "an", "搜狗 a+j→an"},
		{"sogou", "ah", "ang", "搜狗 a+h→ang"},
	}

	for _, tc := range cases {
		tc := tc
		t.Run(tc.desc, func(t *testing.T) {
			scheme := Get(tc.schemeID)
			if scheme == nil {
				t.Fatalf("方案 %q 未注册", tc.schemeID)
			}
			conv := NewConverter(scheme)
			result := conv.Convert(tc.input)
			if result.FullPinyin != tc.want {
				t.Errorf("Convert(%q) = %q, want %q", tc.input, result.FullPinyin, tc.want)
			}
		})
	}
}

// TestZeroInitialLiteralAo 用户实际输入 'a'+'o' 字面键（与方案规定的韵母键无关）
// 应当 fallback 到字面匹配产出 "ao"，而不是 FinalMap['o']=["uo","o"] 路径产出 "uo"/"o"。
// 复现：自然码/小鹤用户习惯性敲 ao 字面，不知道 ao 在方案里对应的韵母键。
func TestZeroInitialLiteralAo(t *testing.T) {
	cases := []struct {
		schemeID string
		input    string
		want     string
	}{
		{"xiaohe", "ao", "ao"},
		{"ziranma", "ao", "ao"},
		{"mspy", "ao", "ao"},
		{"sogou", "ao", "ao"},
	}
	for _, tc := range cases {
		tc := tc
		t.Run(tc.schemeID+"_"+tc.input, func(t *testing.T) {
			conv := NewConverter(Get(tc.schemeID))
			r := conv.Convert(tc.input)
			if r.FullPinyin != tc.want {
				t.Errorf("Convert(%q) under %s = %q, want %q", tc.input, tc.schemeID, r.FullPinyin, tc.want)
			}
		})
	}
}

func TestAllSchemesRegistered(t *testing.T) {
	expectedIDs := []string{"xiaohe", "ziranma", "mspy", "sogou", "abc", "ziguang"}
	for _, id := range expectedIDs {
		if Get(id) == nil {
			t.Errorf("方案 %q 未注册", id)
		}
	}
}

func TestPreeditDisplay(t *testing.T) {
	scheme := Get("xiaohe")
	conv := NewConverter(scheme)

	result := conv.Convert("nihc")
	if result.PreeditDisplay != "ni'hao" {
		t.Errorf("PreeditDisplay = %q, want %q", result.PreeditDisplay, "ni'hao")
	}
}
