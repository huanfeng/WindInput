package shuangpin

// 本文件定义 6 个内置双拼方案的声母/韵母映射表。
//
// 双拼规则：
// - 每个汉字拼音 = 2 键输入：第 1 键是声母，第 2 键是韵母
// - 声母（21个）：b p m f d t n l g k h j q x zh ch sh r z c s y w
//   其中 zh/ch/sh 为翘舌音，需映射到单键
// - 韵母（约35个常用）：a o e i u v(ü) ai ei ao ou an en ang eng ong
//   ia ie iu(iou) ian in(ien) iang ing iong
//   ua uo uai ui(uei) uan un(uen) uang
//   ve(üe) vn(ün)
// - 零声母音节：以元音开头的音节（a ai an ang ao e ei en eng er o ou）
//   在双拼中用特殊方式输入

func init() {
	Register(xiaoheScheme())
	Register(ziranmaScheme())
	Register(mspyScheme())
	Register(sogouScheme())
	Register(abcScheme())
	Register(ziguangScheme())
}

// defaultInitialMap 生成默认声母映射（字母键映射为自身）
func defaultInitialMap() map[byte]string {
	m := make(map[byte]string)
	for _, c := range "bpmfdtnlgkhjqxrzcsyw" {
		m[byte(c)] = string(c)
	}
	return m
}

// xiaoheScheme 小鹤双拼
func xiaoheScheme() *Scheme {
	initials := defaultInitialMap()
	initials['v'] = "zh"
	initials['i'] = "ch"
	initials['u'] = "sh"

	finals := map[byte][]string{
		'a': {"a"},
		'o': {"uo", "o"},
		'e': {"e"},
		'i': {"i"},
		'u': {"u"},
		'v': {"ui", "v"},
		'b': {"in"},
		'c': {"ao"},
		'd': {"ai"},
		'f': {"en"},
		'g': {"eng"},
		'h': {"ang"},
		'j': {"an"},
		'k': {"uai", "ing"},
		'l': {"iang", "uang"},
		'm': {"ian"},
		'n': {"iao"},
		'p': {"ie"},
		'q': {"iu"},
		'r': {"uan", "er"},
		's': {"ong", "iong"},
		't': {"ue", "ve"},
		'w': {"ei"},
		'x': {"ia", "ua"},
		'y': {"un"},
		'z': {"ou"},
	}

	zeroInitials := map[byte][]string{
		'a': {"a", "ai", "an", "ang", "ao"},
		'o': {"o", "ou"},
		'e': {"e", "ei", "en", "eng", "er"},
	}

	return &Scheme{
		ID:              "xiaohe",
		Name:            "小鹤双拼",
		InitialMap:      initials,
		FinalMap:        finals,
		ZeroInitialKeys: zeroInitials,
	}
}

// ziranmaScheme 自然码双拼
func ziranmaScheme() *Scheme {
	initials := defaultInitialMap()
	initials['v'] = "zh"
	initials['i'] = "ch"
	initials['u'] = "sh"

	finals := map[byte][]string{
		'a': {"a"},
		'o': {"uo", "o"},
		'e': {"e"},
		'i': {"i"},
		'u': {"u"},
		'v': {"ui", "v"},
		'b': {"ou"},
		'c': {"iao"},
		'd': {"uang", "iang"},
		'f': {"en"},
		'g': {"eng"},
		'h': {"ang"},
		'j': {"an"},
		'k': {"ao"},
		'l': {"ai"},
		'm': {"ian"},
		'n': {"in"},
		'p': {"un"},
		'q': {"iu"},
		'r': {"uan", "er"},
		's': {"ong", "iong"},
		't': {"ue", "ve"},
		'w': {"ia", "ua"},
		'x': {"ie"},
		'y': {"uai", "ing"},
		'z': {"ei"},
	}

	zeroInitials := map[byte][]string{
		'a': {"a", "ai", "an", "ang", "ao"},
		'o': {"o", "ou"},
		'e': {"e", "ei", "en", "eng", "er"},
	}

	return &Scheme{
		ID:              "ziranma",
		Name:            "自然码",
		InitialMap:      initials,
		FinalMap:        finals,
		ZeroInitialKeys: zeroInitials,
	}
}

// mspyScheme 微软双拼
// 键位来源：RIME double_pinyin_mspy.schema.yaml（iDvel/rime-ice）
func mspyScheme() *Scheme {
	initials := defaultInitialMap()
	initials['v'] = "zh"
	initials['i'] = "ch"
	initials['u'] = "sh"

	finals := map[byte][]string{
		'a': {"a"},
		'o': {"uo", "o"},
		'e': {"e"},
		'i': {"i"},
		'u': {"u"},
		'v': {"ui"},
		'b': {"ou"},
		'c': {"iao"},
		'd': {"uang", "iang"},
		'f': {"en"},
		'g': {"eng"},
		'h': {"ang"},
		'j': {"an"},
		'k': {"ao"},
		'l': {"ai"},
		'm': {"ian"},
		'n': {"in"},
		'p': {"un"},
		'q': {"iu"},
		'r': {"uan", "er"},
		's': {"ong", "iong"},
		't': {"ue", "ve"},
		'w': {"ia", "ua"},
		'x': {"ie"},
		'y': {"uai", "v"},
		'z': {"ei"},
		';': {"ing"},
	}

	zeroInitials := map[byte][]string{
		'a': {"a", "ai", "an", "ang", "ao"},
		'o': {"o", "ou"},
		'e': {"e", "ei", "en", "eng", "er"},
	}

	return &Scheme{
		ID:              "mspy",
		Name:            "微软双拼",
		InitialMap:      initials,
		FinalMap:        finals,
		ZeroInitialKeys: zeroInitials,
	}
}

// sogouScheme 搜狗双拼
// 键位来源：RIME double_pinyin_sogou.schema.yaml（iDvel/rime-ice）
func sogouScheme() *Scheme {
	initials := defaultInitialMap()
	initials['v'] = "zh"
	initials['i'] = "ch"
	initials['u'] = "sh"

	finals := map[byte][]string{
		'a': {"a"},
		'o': {"uo", "o"},
		'e': {"e"},
		'i': {"i"},
		'u': {"u"},
		'v': {"ui"},
		'b': {"ou"},
		'c': {"iao"},
		'd': {"uang", "iang"},
		'f': {"en"},
		'g': {"eng"},
		'h': {"ang"},
		'j': {"an"},
		'k': {"ao"},
		'l': {"ai"},
		'm': {"ian"},
		'n': {"in"},
		'p': {"un"},
		'q': {"iu"},
		'r': {"uan", "er"},
		's': {"ong", "iong"},
		't': {"ue", "ve"},
		'w': {"ia", "ua"},
		'x': {"ie"},
		'y': {"uai", "v"},
		'z': {"ei"},
		';': {"ing"},
	}

	zeroInitials := map[byte][]string{
		'a': {"a", "ai", "an", "ang", "ao"},
		'o': {"o", "ou"},
		'e': {"e", "ei", "en", "eng", "er"},
	}

	return &Scheme{
		ID:              "sogou",
		Name:            "搜狗双拼",
		InitialMap:      initials,
		FinalMap:        finals,
		ZeroInitialKeys: zeroInitials,
	}
}

// abcScheme 智能ABC双拼
// 键位来源：RIME double_pinyin_abc.schema.yaml（iDvel/rime-ice）
func abcScheme() *Scheme {
	initials := defaultInitialMap()
	initials['a'] = "zh"
	initials['e'] = "ch"
	initials['v'] = "sh"

	finals := map[byte][]string{
		'a': {"a"},
		'o': {"uo", "o"},
		'e': {"e"},
		'i': {"i"},
		'u': {"u"},
		'v': {"v"},
		'b': {"ou"},
		'c': {"in", "uai"},
		'd': {"ia", "ua"},
		'f': {"en"},
		'g': {"eng"},
		'h': {"ang"},
		'j': {"an"},
		'k': {"ao"},
		'l': {"ai"},
		'm': {"ui", "ue", "ve"},
		'n': {"un"},
		'p': {"uan"},
		'q': {"ei"},
		'r': {"er", "iu"},
		's': {"ong", "iong"},
		't': {"iang", "uang"},
		'w': {"ian"},
		'x': {"ie"},
		'y': {"ing"},
		'z': {"iao"},
	}

	zeroInitials := map[byte][]string{
		'o': {"o", "ou"},
	}

	return &Scheme{
		ID:              "abc",
		Name:            "智能ABC",
		InitialMap:      initials,
		FinalMap:        finals,
		ZeroInitialKeys: zeroInitials,
	}
}

// ziguangScheme 紫光双拼（华宇双拼）
// 键位来源：RIME double_pinyin_ziguang.schema.yaml（iDvel/rime-ice）
func ziguangScheme() *Scheme {
	initials := defaultInitialMap()
	initials['u'] = "zh"
	initials['i'] = "sh"
	initials['a'] = "ch"

	finals := map[byte][]string{
		'a': {"a"},
		'o': {"uo", "o"},
		'e': {"e"},
		'i': {"i"},
		'u': {"u"},
		'b': {"iao"},
		'd': {"ie"},
		'f': {"ian"},
		'g': {"iang", "uang"},
		'h': {"iong", "ong"},
		'j': {"er", "iu"},
		'k': {"ei"},
		'l': {"uan"},
		';': {"ing"},
		'm': {"un"},
		'n': {"ue", "ui", "ve"},
		'p': {"ai"},
		'q': {"ao"},
		'r': {"an"},
		's': {"ang"},
		't': {"eng"},
		'w': {"en"},
		'x': {"ia", "ua"},
		'y': {"in", "uai"},
		'z': {"ou"},
	}

	// 'a' 是声母 ch 的键，不能同时作零声母前缀（参考 abcScheme 中 a=zh 的处理）
	// 零声母 a/ai/an/ang/ao 通过重复键(aa)或直接拼音输入
	zeroInitials := map[byte][]string{
		'o': {"o", "ou"},
		'e': {"e", "ei", "en", "eng", "er"},
	}

	return &Scheme{
		ID:              "ziguang",
		Name:            "紫光双拼",
		InitialMap:      initials,
		FinalMap:        finals,
		ZeroInitialKeys: zeroInitials,
	}
}
