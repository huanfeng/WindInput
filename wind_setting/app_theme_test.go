package main

import (
	"os"
	"path/filepath"
	"testing"
)

// minimalV3YAML 构造最小合法 v3 主题 YAML（含 colors 块即满足 HasV3Schema）
func minimalV3YAML(name string) string {
	return `meta:
  name: "` + name + `"
  version: "1.0"
colors:
  bg: { light: "#FFFFFF", dark: "#2D2D2D" }
  text: { light: "#1E1E1E", dark: "#E0E0E0" }
`
}

// setupUserThemesDir 创建临时用户主题目录，调用方负责 defer cleanup()。
func setupUserThemesDir(t *testing.T) (dir string, cleanup func()) {
	t.Helper()
	dir, err := os.MkdirTemp("", "windinput-user-themes-*")
	if err != nil {
		t.Fatalf("创建临时目录失败: %v", err)
	}
	return dir, func() { os.RemoveAll(dir) }
}

// plantTheme 在 userThemesDir 下创建 dirName 子目录，写入 meta.name=themeName 的主题。
func plantTheme(t *testing.T, userThemesDir, dirName, themeName string) {
	t.Helper()
	d := filepath.Join(userThemesDir, dirName)
	if err := os.MkdirAll(d, 0o755); err != nil {
		t.Fatalf("plantTheme MkdirAll: %v", err)
	}
	if err := os.WriteFile(filepath.Join(d, "theme.yaml"), []byte(minimalV3YAML(themeName)), 0o644); err != nil {
		t.Fatalf("plantTheme WriteFile: %v", err)
	}
}

// TestFindUserThemeDirByName_Found 能通过 meta.name 找到目录名不同的主题。
func TestFindUserThemeDirByName_Found(t *testing.T) {
	dir, cleanup := setupUserThemesDir(t)
	defer cleanup()

	plantTheme(t, dir, "custom_dir_name", "我的主题")

	got := findUserThemeDirByName(dir, "我的主题")
	want := filepath.Join(dir, "custom_dir_name")
	if got != want {
		t.Errorf("got %q, want %q", got, want)
	}
}

// TestFindUserThemeDirByName_NotFound 不存在时返回空串。
func TestFindUserThemeDirByName_NotFound(t *testing.T) {
	dir, cleanup := setupUserThemesDir(t)
	defer cleanup()

	plantTheme(t, dir, "some_theme", "其他主题")

	if got := findUserThemeDirByName(dir, "我的主题"); got != "" {
		t.Errorf("期望空串，got %q", got)
	}
}

// TestImportThemeToDir_NewTheme 导入全新主题时写入 slug 目录。
func TestImportThemeToDir_NewTheme(t *testing.T) {
	dir, cleanup := setupUserThemesDir(t)
	defer cleanup()

	result := importThemeToDir([]byte(minimalV3YAML("brand-new")), false, dir)
	if !result.Success {
		t.Fatalf("期望成功，got error: %q", result.ErrorMsg)
	}
	slug := sanitizeThemeSlug("brand-new")
	if _, err := os.Stat(filepath.Join(dir, slug, "theme.yaml")); err != nil {
		t.Errorf("期望文件存在于 slug 目录 %q: %v", slug, err)
	}
}

// TestImportThemeToDir_ConflictByName 同名主题存于不同目录名时触发 Conflict。
func TestImportThemeToDir_ConflictByName(t *testing.T) {
	dir, cleanup := setupUserThemesDir(t)
	defer cleanup()

	plantTheme(t, dir, "renamed_dir", "重名主题")

	result := importThemeToDir([]byte(minimalV3YAML("重名主题")), false, dir)
	if !result.Conflict {
		t.Fatalf("期望 Conflict=true，got: %+v", result)
	}
	if result.ThemeName != "重名主题" {
		t.Errorf("ThemeName 期望 %q，got %q", "重名主题", result.ThemeName)
	}
}

// TestImportThemeToDir_ForceOverwriteExistingDir force=true 时原地覆盖，不新建 slug 目录。
func TestImportThemeToDir_ForceOverwriteExistingDir(t *testing.T) {
	dir, cleanup := setupUserThemesDir(t)
	defer cleanup()

	plantTheme(t, dir, "old_dir_name", "覆盖主题")

	updated := minimalV3YAML("覆盖主题") + "# updated\n"
	result := importThemeToDir([]byte(updated), true, dir)
	if !result.Success {
		t.Fatalf("期望成功，got error: %q", result.ErrorMsg)
	}

	data, err := os.ReadFile(filepath.Join(dir, "old_dir_name", "theme.yaml"))
	if err != nil {
		t.Fatalf("原目录文件不存在: %v", err)
	}
	if string(data) != updated {
		t.Errorf("文件内容未更新")
	}

	slug := sanitizeThemeSlug("覆盖主题")
	if _, err := os.Stat(filepath.Join(dir, slug)); err == nil {
		t.Errorf("不应新建 slug 目录 %q", slug)
	}
}

func TestParseThemePreviewMeta(t *testing.T) {
	yaml := "meta:\n  name: 暗夜\n  author: 张三\n  version: \"1.2\"\n"
	name, author, version := parseThemePreviewMeta([]byte(yaml))
	if name != "暗夜" || author != "张三" || version != "1.2" {
		t.Errorf("got %q/%q/%q", name, author, version)
	}
}
