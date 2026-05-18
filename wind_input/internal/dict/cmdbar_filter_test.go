package dict

import "testing"

// TestHasCmdbarMarker_SSValueExcluded 验证顶层 $SS marker 的 value 不会因为
// 内嵌 $CC 元素而被误识别为 cmdbar 命令短语 (历史 bug: SearchPrefix +
// ValueExpander.Expand 子串扫描漏判, 让 $SS 整段流入 cmdbarPhraseHook,
// Evaluate(ArrayPhrase) 立即报错, 产生错误候选 + 噪音日志)。
func TestHasCmdbarMarker_SSValueExcluded(t *testing.T) {
	cases := []struct {
		name  string
		value string
		want  bool
	}{
		// 纯 $CC / $CC1 → cmdbar marker
		{"plain CC", `$CC("打开", open("https://x"))`, true},
		{"plain CC1", `$CC1("打开", open("https://x"))`, true},

		// $SS 内含 $CC → 仍是 SS, 不应识别为 cmdbar marker
		{"SS with embedded CC", `$SS("百度", $CC("打开", open("https://x")), "https://x")`, false},
		{"SS with multi CC1", `$SS("g", $CC1("a", open("a")), $CC1("b", open("b")))`, false},
		{"SS leading whitespace", `  $SS("g", $CC("x", open("y")))`, false},

		// 字面量
		{"plain literal", "你好", false},
		{"empty", "", false},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if got := HasCmdbarMarker(c.value); got != c.want {
				t.Errorf("HasCmdbarMarker(%q) = %v, want %v", c.value, got, c.want)
			}
		})
	}
}

// TestIsExactOnly_TemplateVars 验证含已知 $X 模板变量的动态短语被识别为
// "仅精确匹配", 不污染前缀候选; 不含变量的字面量短语允许前缀展开。
func TestIsExactOnly_TemplateVars(t *testing.T) {
	cases := []struct {
		name  string
		value string
		want  bool
	}{
		// 模板变量 → exact-only
		{"date Y MM DD", "$Y-$MM-$DD", true},
		{"date YYYY", "$YYYY/$MM/$DD", true},
		{"time HH mm ss", "$HH:$mm:$ss", true},
		{"weekday WC", "今天 $WC", true},
		{"chinese YC", "$YC年", true},
		{"uuid", "id=$uuid", true},
		{"timestamp ts", "now=$ts", true},
		{"timestamp tsms", "ms=$tsms", true},
		{"brace form", "${Y}-${MM}", true},

		// CC marker
		{"CC exact", `$CC("打开", open("https://x"))`, true},
		{"CC1 prefix-visible", `$CC1("打开", open("https://x"))`, false},
		{"AA char group", `$AA("aa", "abc")`, false},

		// 字面量 / 无变量
		{"plain literal", "你好世界", false},
		{"dollar without var", "价格 $100", false},
		{"escape dollar", "literal $$Y is $$ not a var", false},

		// 混合: $CC1 优先于 $CC
		{"CC1 wins over CC", `$CC1("a", open("b")) $CC("c", run("d"))`, false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := IsExactOnly(tc.value)
			if got != tc.want {
				t.Fatalf("IsExactOnly(%q) = %v, want %v", tc.value, got, tc.want)
			}
		})
	}
}

// TestHasTemplateVar_NoMarkerCollision 验证 $CC(/$CC1(/$AA( 等 marker 形式
// 不会被误判为模板变量 (即使去除 marker 名仍合法时)。
func TestHasTemplateVar_NoMarkerCollision(t *testing.T) {
	cases := []struct {
		name  string
		value string
		want  bool
	}{
		{"CC marker alone", `$CC("x", open("y"))`, false},
		{"CC1 marker alone", `$CC1("x", open("y"))`, false},
		{"AA marker alone", `$AA("n", "abc")`, false},
		{"escape only", "$$", false},
		{"trailing dollar", "abc$", false},
		{"unknown var", "$XYZ", false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := hasTemplateVar(tc.value)
			if got != tc.want {
				t.Fatalf("hasTemplateVar(%q) = %v, want %v", tc.value, got, tc.want)
			}
		})
	}
}

// TestIsCmdbarExactOnly_BackCompat 验证旧名 IsCmdbarExactOnly 与 IsExactOnly
// 行为一致 (转调实现)。
func TestIsCmdbarExactOnly_BackCompat(t *testing.T) {
	samples := []string{
		"$Y-$MM-$DD",
		`$CC("a", open("b"))`,
		`$CC1("a", open("b"))`,
		"plain",
	}
	for _, v := range samples {
		if IsCmdbarExactOnly(v) != IsExactOnly(v) {
			t.Fatalf("IsCmdbarExactOnly diverges from IsExactOnly for %q", v)
		}
	}
}
