package dict

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/internal/cmdbar"
)

// TestPhraseLayerSSGroup_ExactExpands 验证 $SS 短语精确码命中时由 ArrayHook
// 展开为 N 个 candidate, 共享 group weight 与 NaturalOrder=index。
func TestPhraseLayerSSGroup_ExactExpands(t *testing.T) {
	tmpDir := t.TempDir()
	systemFile := filepath.Join(tmpDir, "system.phrases.yaml")
	content := `phrases:
  - code: "url"
    text: '$SS("常用网址", "https://google.com", "https://github.com", "https://baidu.com")'
    weight: 1000
`
	if err := os.WriteFile(systemFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
	pl := loadPhraseLayerFromYAML(t, systemFile, "")

	// 注入 mock ArrayHook 返回三个 string lit 元素 (display=字面量, actions=nil)。
	pl.SetCmdbarArrayHook(func(value string) (string, []CmdbarArrayElement, map[string]any, bool, error) {
		return "常用网址", []CmdbarArrayElement{
			{Display: "https://google.com"},
			{Display: "https://github.com"},
			{Display: "https://baidu.com"},
		}, map[string]any{"prefix": true, "expand": "exact", "nav": true}, true, nil
	})

	results := pl.SearchCommand("url", 10)
	if len(results) != 3 {
		t.Fatalf("expected 3 candidates, got %d", len(results))
	}
	wantDisplays := []string{"https://google.com", "https://github.com", "https://baidu.com"}
	for i, want := range wantDisplays {
		if results[i].Text != want {
			t.Errorf("idx %d: text = %q, want %q", i, results[i].Text, want)
		}
		if results[i].Weight != 1000 {
			t.Errorf("idx %d: weight = %d, want 1000", i, results[i].Weight)
		}
		if results[i].NaturalOrder != i {
			t.Errorf("idx %d: NaturalOrder = %d, want %d", i, results[i].NaturalOrder, i)
		}
		if !results[i].IsCommand || !results[i].IsPhrase {
			t.Errorf("idx %d: should be marked IsCommand & IsPhrase", i)
		}
		// IsGroupMember=true: $SS 元素候选右键菜单全 disable (2026-05-17)
		if !results[i].IsGroupMember {
			t.Errorf("idx %d: $SS expanded element should have IsGroupMember=true", i)
		}
	}
}

// TestPhraseLayerSSGroup_MixedWithCC 验证 $SS 元素混用 string lit 与嵌入
// $CC: string lit 元素 Actions 为 nil; 嵌入 $CC 元素带 Actions 闭包。
func TestPhraseLayerSSGroup_MixedWithCC(t *testing.T) {
	tmpDir := t.TempDir()
	systemFile := filepath.Join(tmpDir, "system.phrases.yaml")
	content := `phrases:
  - code: "bd"
    text: '$SS("百度", $CC("打开百度", open("x")), "https://baidu.com")'
    weight: 500
`
	if err := os.WriteFile(systemFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
	pl := loadPhraseLayerFromYAML(t, systemFile, "")

	pl.SetCmdbarArrayHook(func(value string) (string, []CmdbarArrayElement, map[string]any, bool, error) {
		return "百度", []CmdbarArrayElement{
			{
				Display: "打开百度",
				Actions: []cmdbar.ResolvedAction{
					{Kind: cmdbar.ActionEffect, Run: func() (string, error) { return "", nil }},
				},
			},
			{Display: "https://baidu.com"}, // 纯 string lit, 无 actions
		}, map[string]any{"prefix": true}, true, nil
	})

	results := pl.SearchCommand("bd", 10)
	if len(results) != 2 {
		t.Fatalf("expected 2 candidates, got %d", len(results))
	}
	if results[0].Text != "打开百度" {
		t.Errorf("idx 0: text = %q, want %q", results[0].Text, "打开百度")
	}
	if len(results[0].Actions) != 1 {
		t.Errorf("idx 0: actions count = %d, want 1 (embedded $CC)", len(results[0].Actions))
	}
	if results[1].Text != "https://baidu.com" {
		t.Errorf("idx 1: text = %q, want %q", results[1].Text, "https://baidu.com")
	}
	if len(results[1].Actions) != 0 {
		t.Errorf("idx 1: actions count = %d, want 0 (string lit element)", len(results[1].Actions))
	}
}

// TestPhraseLayerSSGroup_PrefixNav 验证 $SS 短语在前缀输入时出 1 个 nav 候选
// 而非展开 elements。
func TestPhraseLayerSSGroup_PrefixNav(t *testing.T) {
	tmpDir := t.TempDir()
	systemFile := filepath.Join(tmpDir, "system.phrases.yaml")
	content := `phrases:
  - code: "url1"
    text: '$SS("常用网址", "a", "b")'
    weight: 1000
`
	if err := os.WriteFile(systemFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
	pl := loadPhraseLayerFromYAML(t, systemFile, "")
	// ArrayHook 装上但不应被调用 (前缀场景走 nav)
	hookCalled := 0
	pl.SetCmdbarArrayHook(func(value string) (string, []CmdbarArrayElement, map[string]any, bool, error) {
		hookCalled++
		return "", nil, nil, true, nil
	})

	// 输入 "url" 是 "url1" 的前缀, len >= 2 (避免短前缀过滤)
	results := pl.SearchCommand("url", 10)
	if hookCalled != 0 {
		t.Errorf("ArrayHook must NOT be invoked for prefix search, got %d calls", hookCalled)
	}
	if len(results) != 1 {
		t.Fatalf("expected 1 nav candidate for prefix 'url', got %d", len(results))
	}
	c := results[0]
	if !c.IsGroup {
		t.Errorf("nav candidate must have IsGroup=true, got false")
	}
	if c.GroupCode != "url1" {
		t.Errorf("nav candidate GroupCode = %q, want url1", c.GroupCode)
	}
	if c.Text != "常用网址" {
		t.Errorf("nav candidate text = %q, want 常用网址 (group display name)", c.Text)
	}
}

// TestPhraseLayerSSGroup_NoHookDegradesGracefully 验证未装 ArrayHook 时,
// $SS 精确码命中不 panic, 返回空 candidate slice。
func TestPhraseLayerSSGroup_NoHookDegradesGracefully(t *testing.T) {
	tmpDir := t.TempDir()
	systemFile := filepath.Join(tmpDir, "system.phrases.yaml")
	content := `phrases:
  - code: "url"
    text: '$SS("常用网址", "a", "b")'
    weight: 1000
`
	if err := os.WriteFile(systemFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
	pl := loadPhraseLayerFromYAML(t, systemFile, "")
	// 不装 ArrayHook

	results := pl.SearchCommand("url", 10)
	// 期望空 slice 而非 panic
	if len(results) != 0 {
		t.Errorf("expected empty results when no ArrayHook, got %d", len(results))
	}
}

// TestPhraseLayerSSGroup_PhraseGroupKind 验证 LoadFromStore 正确把 $SS 短语
// 注册为 PhraseGroupKindSS, $AA 短语注册为 PhraseGroupKindAA。
func TestPhraseLayerSSGroup_PhraseGroupKind(t *testing.T) {
	tmpDir := t.TempDir()
	systemFile := filepath.Join(tmpDir, "system.phrases.yaml")
	content := `phrases:
  - code: "ssgrp"
    text: '$SS("ss-name", "a")'
  - code: "aagrp"
    text: '$AA("aa-name", "ab")'
`
	if err := os.WriteFile(systemFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
	pl := loadPhraseLayerFromYAML(t, systemFile, "")

	ss, ok := pl.phraseGroups["ssgrp"]
	if !ok {
		t.Fatal("ssgrp not in phraseGroups")
	}
	if ss.Kind != PhraseGroupKindSS {
		t.Errorf("ssgrp Kind = %q, want %q", ss.Kind, PhraseGroupKindSS)
	}
	if ss.Name != "ss-name" {
		t.Errorf("ssgrp Name = %q, want %q", ss.Name, "ss-name")
	}

	aa, ok := pl.phraseGroups["aagrp"]
	if !ok {
		t.Fatal("aagrp not in phraseGroups")
	}
	if aa.Kind != PhraseGroupKindAA {
		t.Errorf("aagrp Kind = %q, want %q", aa.Kind, PhraseGroupKindAA)
	}
	if aa.Name != "aa-name" {
		t.Errorf("aagrp Name = %q, want %q", aa.Name, "aa-name")
	}
}
