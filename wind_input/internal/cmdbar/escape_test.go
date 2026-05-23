package cmdbar

import "testing"

func TestDecodeEscapes(t *testing.T) {
	cases := []struct {
		name string
		in   string
		want string
	}{
		{"no backslash fast path", "第一行第二行", "第一行第二行"},
		{"newline", `第一行\n第二行`, "第一行\n第二行"},
		{"tab", `a\tb`, "a\tb"},
		{"carriage return", `a\rb`, "a\rb"},
		{"literal backslash", `a\\b`, `a\b`},
		{"windows path unknown backslash-U", `C:\Users`, `C:\Users`},
		{"unknown escape with hit-letters", `C:\new`, "C:" + "\n" + "ew"}, // \n 解码为 LF, \e/\w 未知转义原样保留
		{"empty string", "", ""},
		{"only backslash", `\`, `\`},
		{"trailing lone backslash", `abc\`, `abc\`},
		{"mixed", `行1\n行2\\tail`, "行1\n行2\\tail"},
		{"consecutive", `\n\n`, "\n\n"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := DecodeEscapes(tc.in)
			if got != tc.want {
				t.Fatalf("DecodeEscapes(%q) = %q, want %q", tc.in, got, tc.want)
			}
		})
	}
}
