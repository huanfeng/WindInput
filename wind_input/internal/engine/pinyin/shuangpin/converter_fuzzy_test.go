package shuangpin

import "testing"

// 历史 BUG: 双拼模糊音 (z/zh, c/ch, s/sh) 只作用在全拼层；
// 双拼键对若声母原始映射拼不出合法音节 (如小鹤 s+l → siang/suang 皆非法),
// Convert 会 fallback 把两键原样写回 fullPinyin, 全拼层也救不回来。
// 现修复: Converter 自身按 fuzzy 开关把对偶声母变体并入候选,
// 使 sl pb (小鹤双拼) 在 fuzzy s↔sh 开启时也能解析为 shuang pin → "双拼"。
//
// 不变量:
//   1. fuzzy 关 → 原始合法对照表不变 (回归保护)
//   2. fuzzy 开且原始合法 → 原始声母音节排第 0 位, 后续候选不会破坏现有
//      "全拼层 fuzzy 兜底" 的行为 (zi → 仍输出 zi, 由全拼层模糊到 zhi)
//   3. fuzzy 开且原始非法 → 用对偶声母补救 (sl → shuang)

func TestXiaoheFuzzy_SLNeedsFuzzyToShuang(t *testing.T) {
	scheme := Get("xiaohe")
	conv := NewConverter(scheme)

	// fuzzy 关：s+l 无合法 → fallback "sl"
	got := conv.Convert("sl").FullPinyin
	if got != "sl" {
		t.Errorf("baseline (no fuzzy): Convert(\"sl\") = %q, want %q (fallback 行为)", got, "sl")
	}

	// fuzzy s↔sh 开启：s+l 应被 sh+l 模糊补救 → "shuang"
	conv.SetFuzzyInitials(false, false, true)
	got = conv.Convert("sl").FullPinyin
	if got != "shuang" {
		t.Errorf("with ShS fuzzy: Convert(\"sl\") = %q, want %q", got, "shuang")
	}

	// 整段 sl pb → shuang pin
	got = conv.Convert("slpb").FullPinyin
	if got != "shuangpin" {
		t.Errorf("with ShS fuzzy: Convert(\"slpb\") = %q, want %q", got, "shuangpin")
	}
}

func TestXiaoheFuzzy_OriginalLegalNotShadowed(t *testing.T) {
	scheme := Get("xiaohe")
	conv := NewConverter(scheme)
	conv.SetFuzzyInitials(true, true, true)

	// zi 原始合法 → 不应被 fuzzy 改写为 zhi（全拼层负责 z↔zh 模糊匹配）
	got := conv.Convert("zi").FullPinyin
	if got != "zi" {
		t.Errorf("fuzzy on but original legal: Convert(\"zi\") = %q, want %q (原始优先)", got, "zi")
	}

	// zisi 同理
	got = conv.Convert("zisi").FullPinyin
	if got != "zisi" {
		t.Errorf("fuzzy on but original legal: Convert(\"zisi\") = %q, want %q", got, "zisi")
	}
}

func TestXiaoheFuzzy_Bidirectional(t *testing.T) {
	// 小鹤 v=zh 声母键；fuzzy zs 开启时 v+韵母 应同时产 z+韵母 候选。
	// vd: v(zh) + d(ai) → "zhai" 原始合法；fuzzy 后兜底 "zai" 也算候选。
	// 主流程取 [0] 仍是原始合法 "zhai"，但 results 应同时含 "zai"。
	scheme := Get("xiaohe")
	conv := NewConverter(scheme)
	conv.SetFuzzyInitials(true, false, false)

	results := conv.convertPair('v', 'd')
	hasZhai, hasZai := false, false
	for _, s := range results {
		if s == "zhai" {
			hasZhai = true
		}
		if s == "zai" {
			hasZai = true
		}
	}
	if !hasZhai {
		t.Errorf("convertPair('v','d') 缺 zhai: %v", results)
	}
	if !hasZai {
		t.Errorf("convertPair('v','d') fuzzy z↔zh 开启后应含 zai 候选: %v", results)
	}
	if results[0] != "zhai" {
		t.Errorf("原始声母合法时应排首位, got results[0]=%q (full=%v)", results[0], results)
	}
}

func TestXiaoheFuzzy_DisabledSwitch(t *testing.T) {
	// fuzzy 关时不引入任何对偶变体（保证默认行为）。
	scheme := Get("xiaohe")
	conv := NewConverter(scheme)

	results := conv.convertPair('s', 'l')
	if len(results) != 0 {
		t.Errorf("fuzzy 关时 s+l 应为空, got %v", results)
	}
}
