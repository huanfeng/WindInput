package coordinator

import "testing"

func TestIsCalcExpression(t *testing.T) {
	tests := []struct {
		input string
		want  bool
	}{
		{"123+456", true},
		{"10-3", true},
		{"2*3", true},
		{"10/2", true},
		{"1+2*3", true},
		{"12345", false},
		{"abc+def", false},
		{"+123", false},
		{"123+", true},
		{"1+2+3+4", true},
		{"(1+2)*3", true},
		{"(10-2)/4", true},
		{"2*(3+4)", true},
		{"(3)", false}, // 只有括号无运算符，不算计算表达式
		{"(1+2)*(3+4)", true},
	}
	for _, tt := range tests {
		got := isCalcExpression(tt.input)
		if got != tt.want {
			t.Errorf("isCalcExpression(%q) = %v, want %v", tt.input, got, tt.want)
		}
	}
}

func TestEvaluateExpression(t *testing.T) {
	tests := []struct {
		expr    string
		want    float64
		wantErr bool
	}{
		{"1+2", 3, false},
		{"10-3", 7, false},
		{"2*3", 6, false},
		{"10/2", 5, false},
		{"1+2*3", 7, false},
		{"2+3*4-1", 13, false},
		{"10/3", 3.3333333333333335, false},
		{"100+200*3", 700, false},
		{"10/0", 0, true},
		{"123+456", 579, false},
		{"0*999", 0, false},
		{"(1+2)*3", 9, false},
		{"2*(3+4)", 14, false},
		{"(10-2)/4", 2, false},
		{"(1+2)*(3+4)", 21, false},
		{"100/(2+3)", 20, false},
		{"(2+3)*(4-1)", 15, false},
	}
	for _, tt := range tests {
		got, err := evaluateExpression(tt.expr)
		if tt.wantErr {
			if err == nil {
				t.Errorf("evaluateExpression(%q) expected error, got %v", tt.expr, got)
			}
			continue
		}
		if err != nil {
			t.Errorf("evaluateExpression(%q) unexpected error: %v", tt.expr, err)
			continue
		}
		if got != tt.want {
			t.Errorf("evaluateExpression(%q) = %v, want %v", tt.expr, got, tt.want)
		}
	}
}

func TestFormatCalcResult(t *testing.T) {
	tests := []struct {
		val  float64
		want string
	}{
		{579, "579"},
		{3.14, "3.14"},
		{100.0, "100"},
		{0.5, "0.5"},
		{1234567.89, "1234567.89"},
		{1.0 / 3.0, "0.333333"}, // 默认保留6位
	}
	for _, tt := range tests {
		got := formatCalcResult(tt.val)
		if got != tt.want {
			t.Errorf("formatCalcResult(%v) = %q, want %q", tt.val, got, tt.want)
		}
	}
}

func TestFormatCalcResultPrec(t *testing.T) {
	tests := []struct {
		val           float64
		decimalPlaces int
		want          string
	}{
		{579, 6, "579"},
		{3.14, 6, "3.14"},
		{3.1415926535, 6, "3.141593"},
		{100.0, 6, "100"},
		{0.5, 6, "0.5"},
		{3.14, 0, "3"},             // 0 表示取整
		{1.0 / 3.0, 6, "0.333333"}, // 保留6位
		{1.0 / 3.0, 0, "0"},        // 0 表示取整
		{3.5, 0, "4"},              // 四舍五入
		{2.4, 0, "2"},              // 四舍五入
	}
	for _, tt := range tests {
		got := formatCalcResultPrec(tt.val, tt.decimalPlaces)
		if got != tt.want {
			t.Errorf("formatCalcResultPrec(%v, %d) = %q, want %q", tt.val, tt.decimalPlaces, got, tt.want)
		}
	}
}

func TestGenerateCalcCandidates(t *testing.T) {
	candidates := generateCalcCandidates("123+456", 6)
	if len(candidates) < 2 {
		t.Fatalf("expected at least 2 candidates, got %d", len(candidates))
	}
	if candidates[0] != "123+456=579" {
		t.Errorf("candidate[0] = %q, want %q", candidates[0], "123+456=579")
	}
	if candidates[1] != "579" {
		t.Errorf("candidate[1] = %q, want %q", candidates[1], "579")
	}

	// 123.23+ 不应生成候选（去掉尾部运算符后无运算符）
	noCalc := generateCalcCandidates("123.23+", 6)
	if noCalc != nil {
		t.Errorf("generateCalcCandidates(\"123.23+\", 6) should return nil, got %v", noCalc)
	}

	// 小数计算结果应包含中文读法
	decCalc := generateCalcCandidates("10/3", 6)
	if len(decCalc) < 4 {
		t.Fatalf("expected at least 4 candidates for 10/3, got %d", len(decCalc))
	}
	if decCalc[0] != "10/3=3.333333" {
		t.Errorf("candidate[0] = %q, want %q", decCalc[0], "10/3=3.333333")
	}
}
