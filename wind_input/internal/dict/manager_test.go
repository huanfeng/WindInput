package dict

import (
	"os"
	"path/filepath"
	"testing"
)

func TestDictManager_SwitchSchema(t *testing.T) {
	tmpDir := t.TempDir()

	dm := NewDictManager(tmpDir)
	if err := dm.Initialize(); err != nil {
		t.Fatalf("Initialize 失败: %v", err)
	}
	defer dm.Close()

	// 切换到 wubi86
	dm.SwitchSchema("wubi86", "shadow_wubi86.yaml", "user_words_wubi86.txt")

	if dm.GetActiveSchemaID() != "wubi86" {
		t.Errorf("期望 activeSchemaID=wubi86, 实际=%s", dm.GetActiveSchemaID())
	}
	if dm.GetShadowLayer() == nil {
		t.Error("ShadowLayer 不应为 nil")
	}
	if dm.GetUserDict() == nil {
		t.Error("UserDict 不应为 nil")
	}

	// 添加用户词到 wubi86
	if err := dm.AddUserWord("test", "测试", 100); err != nil {
		t.Fatalf("AddUserWord 失败: %v", err)
	}

	// 切换到 pinyin
	dm.SwitchSchema("pinyin", "shadow_pinyin.yaml", "user_words_pinyin.txt")

	if dm.GetActiveSchemaID() != "pinyin" {
		t.Errorf("期望 activeSchemaID=pinyin, 实际=%s", dm.GetActiveSchemaID())
	}

	// pinyin 的用户词库应该是空的
	if dm.GetUserDict().EntryCount() != 0 {
		t.Errorf("pinyin 用户词库应为空, 实际=%d", dm.GetUserDict().EntryCount())
	}

	// 切换回 wubi86，用户词应该还在
	dm.SwitchSchema("wubi86", "shadow_wubi86.yaml", "user_words_wubi86.txt")
	if dm.GetUserDict().EntryCount() != 1 {
		t.Errorf("wubi86 用户词库应有 1 条, 实际=%d", dm.GetUserDict().EntryCount())
	}
}

func TestDictManager_ShadowIsolation(t *testing.T) {
	tmpDir := t.TempDir()

	dm := NewDictManager(tmpDir)
	dm.Initialize()
	defer dm.Close()

	// 在 wubi86 方案下置顶
	dm.SwitchSchema("wubi86", "shadow_wubi86.yaml", "user_words_wubi86.txt")
	dm.PinWord("abc", "测试", 0)

	wubiShadow := dm.GetShadowLayer()
	rules := wubiShadow.GetShadowRules("abc")
	if rules == nil || len(rules.Pinned) != 1 {
		t.Fatalf("wubi86 应有 1 条 pin 规则")
	}

	// 切换到 pinyin，shadow 应该是独立的
	dm.SwitchSchema("pinyin", "shadow_pinyin.yaml", "user_words_pinyin.txt")
	pinyinShadow := dm.GetShadowLayer()
	pinyinRules := pinyinShadow.GetShadowRules("abc")
	if pinyinRules != nil && (len(pinyinRules.Pinned) > 0 || len(pinyinRules.Deleted) > 0) {
		t.Errorf("pinyin 不应有 shadow 规则")
	}

	// 切换回 wubi86，shadow 规则应该还在
	dm.SwitchSchema("wubi86", "shadow_wubi86.yaml", "user_words_wubi86.txt")
	rules2 := dm.GetShadowLayer().GetShadowRules("abc")
	if rules2 == nil || len(rules2.Pinned) != 1 {
		t.Errorf("wubi86 shadow 规则应还在")
	}
}

func TestDictManager_SameSchemaNoOp(t *testing.T) {
	tmpDir := t.TempDir()

	dm := NewDictManager(tmpDir)
	dm.Initialize()
	defer dm.Close()

	dm.SwitchSchema("wubi86", "shadow_wubi86.yaml", "user_words_wubi86.txt")
	dm.AddUserWord("a", "甲", 100)

	// 再次切换到相同方案应该是 no-op
	dm.SwitchSchema("wubi86", "shadow_wubi86.yaml", "user_words_wubi86.txt")

	if dm.GetUserDict().EntryCount() != 1 {
		t.Errorf("同方案切换不应丢失数据, 实际=%d", dm.GetUserDict().EntryCount())
	}
}

func TestDictManager_SaveAndReload(t *testing.T) {
	tmpDir := t.TempDir()

	// 第一次：创建并保存
	dm := NewDictManager(tmpDir)
	dm.Initialize()
	dm.SwitchSchema("wubi86", "shadow_wubi86.yaml", "user_words_wubi86.txt")
	dm.AddUserWord("test", "保存测试", 200)
	dm.PinWord("test", "保存测试", 0)
	dm.Save()
	dm.Close()

	// 验证文件已生成
	userDictPath := filepath.Join(tmpDir, "user_words_wubi86.txt")
	if _, err := os.Stat(userDictPath); os.IsNotExist(err) {
		t.Error("用户词库文件应已创建")
	}
	shadowPath := filepath.Join(tmpDir, "shadow_wubi86.yaml")
	if _, err := os.Stat(shadowPath); os.IsNotExist(err) {
		t.Error("Shadow 文件应已创建")
	}

	// 第二次：重新加载
	dm2 := NewDictManager(tmpDir)
	dm2.Initialize()
	dm2.SwitchSchema("wubi86", "shadow_wubi86.yaml", "user_words_wubi86.txt")
	defer dm2.Close()

	if dm2.GetUserDict().EntryCount() != 1 {
		t.Errorf("重新加载后应有 1 条用户词, 实际=%d", dm2.GetUserDict().EntryCount())
	}

	rules := dm2.GetShadowLayer().GetShadowRules("test")
	if rules == nil || len(rules.Pinned) != 1 {
		t.Errorf("重新加载后应有 1 条 pin 规则")
	}
}

func TestDictManager_SetActiveEngine_Compat(t *testing.T) {
	tmpDir := t.TempDir()

	dm := NewDictManager(tmpDir)
	dm.Initialize()
	defer dm.Close()

	// 兼容旧调用
	dm.SetActiveEngine("wubi")
	if dm.GetActiveSchemaID() != "wubi86" {
		t.Errorf("SetActiveEngine('wubi') 应映射到 wubi86, 实际=%s", dm.GetActiveSchemaID())
	}

	dm.SetActiveEngine("pinyin")
	if dm.GetActiveSchemaID() != "pinyin" {
		t.Errorf("SetActiveEngine('pinyin') 应映射到 pinyin, 实际=%s", dm.GetActiveSchemaID())
	}
}
