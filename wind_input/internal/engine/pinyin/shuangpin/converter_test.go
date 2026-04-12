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
