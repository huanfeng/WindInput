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
		'v': {"v"},
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
		'v': {"ve", "ui"},
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
		't': {"ue"},
		'w': {"ia", "ua"},
		'x': {"ie"},
		'y': {"uai", "ing"},
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
		'v': {"v"},
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
		ID:              "sogou",
		Name:            "搜狗双拼",
		InitialMap:      initials,
		FinalMap:        finals,
		ZeroInitialKeys: zeroInitials,
	}
}

// abcScheme 智能ABC双拼
func abcScheme() *Scheme {
	initials := defaultInitialMap()
	initials['a'] = "zh"
	initials['e'] = "ch"
	initials['v'] = "sh"

	finals := map[byte][]string{
		'a': {"a"},
		'o': {"o"},
		'e': {"e"},
		'i': {"i"},
		'u': {"u"},
		'v': {"v"},
		'b': {"ou"},
		'c': {"in"},
		'd': {"ia", "ua"},
		'f': {"en"},
		'g': {"eng"},
		'h': {"ang"},
		'j': {"an"},
		'k': {"ao"},
		'l': {"ai"},
		'm': {"iu"},
		'n': {"iao"},
		'p': {"uan"},
		'q': {"ei"},
		'r': {"uan", "er"},
		's': {"ong", "iong"},
		't': {"iang", "uang"},
		'w': {"ian"},
		'x': {"ie"},
		'y': {"ing"},
		'z': {"un"},
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
func ziguangScheme() *Scheme {
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
		'v': {"v"},
		'b': {"iao"},
		'c': {"ing"},
		'd': {"ai"},
		'f': {"en"},
		'g': {"eng"},
		'h': {"ang"},
		'j': {"an"},
		'k': {"ao"},
		'l': {"uang", "iang"},
		'm': {"ian"},
		'n': {"in"},
		'p': {"ou"},
		'q': {"er", "iu"},
		'r': {"uan"},
		's': {"ong", "iong"},
		't': {"ue", "ve"},
		'w': {"ei"},
		'x': {"ia", "ua"},
		'y': {"un"},
		'z': {"uai", "ui"},
	}

	zeroInitials := map[byte][]string{
		'a': {"a", "ai", "an", "ang", "ao"},
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
