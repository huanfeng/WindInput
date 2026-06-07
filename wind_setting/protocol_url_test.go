package main

import "testing"

func TestParseProtocolURL(t *testing.T) {
	t.Run("合法主题链接", func(t *testing.T) {
		req, err := ParseProtocolURL("windinput://import/theme?url=https%3A%2F%2Fsite.com%2Fa.yaml&name=Dark")
		if err != nil {
			t.Fatalf("unexpected err: %v", err)
		}
		if req.Kind != "theme" {
			t.Errorf("kind = %q, want theme", req.Kind)
		}
		if req.URL != "https://site.com/a.yaml" {
			t.Errorf("url = %q", req.URL)
		}
		if req.Name != "Dark" {
			t.Errorf("name = %q", req.Name)
		}
	})
	t.Run("scheme 错误", func(t *testing.T) {
		if _, err := ParseProtocolURL("http://import/theme?url=https://x"); err == nil {
			t.Error("want error for wrong scheme")
		}
	})
	t.Run("未知 kind", func(t *testing.T) {
		if _, err := ParseProtocolURL("windinput://import/unknown?url=https://x"); err == nil {
			t.Error("want error for unknown kind")
		}
	})
	t.Run("缺 url 参数", func(t *testing.T) {
		if _, err := ParseProtocolURL("windinput://import/theme"); err == nil {
			t.Error("want error for missing url")
		}
	})
	t.Run("非 https 拒绝", func(t *testing.T) {
		if _, err := ParseProtocolURL("windinput://import/theme?url=http://x/a.yaml"); err == nil {
			t.Error("want error for non-https url")
		}
	})
	t.Run("预留 kind 可解析", func(t *testing.T) {
		for _, k := range []string{"schema", "dict", "extdict"} {
			if _, err := ParseProtocolURL("windinput://import/" + k + "?url=https://x/a"); err != nil {
				t.Errorf("kind %s should parse: %v", k, err)
			}
		}
	})
}
