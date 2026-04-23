package config

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/huanfeng/wind_input/pkg/buildvariant"
)

func TestFindPortableRootSameDir(t *testing.T) {
	// 标记文件与 exeDir 同级——应检测到
	tmp := t.TempDir()
	exeDir := filepath.Join(tmp, "bundle")
	if err := os.MkdirAll(exeDir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(exeDir, PortableMarkerName), []byte("portable=1\n"), 0o644); err != nil {
		t.Fatal(err)
	}

	got, ok := findPortableRoot(exeDir)
	if !ok {
		t.Fatalf("expected portable root, got not found")
	}
	if got != exeDir {
		t.Fatalf("expected %s, got %s", exeDir, got)
	}
}

func TestFindPortableRootNotFound(t *testing.T) {
	// 目录中没有标记文件——应返回 not found
	tmp := t.TempDir()
	got, ok := findPortableRoot(tmp)
	if ok {
		t.Fatalf("expected not found, got %s", got)
	}
}

func TestFindPortableRootParentIgnored(t *testing.T) {
	// 标记文件在父目录而非 exeDir 同级——不应检测到
	tmp := t.TempDir()
	root := filepath.Join(tmp, "bundle")
	exeDir := filepath.Join(root, "build")
	if err := os.MkdirAll(exeDir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, PortableMarkerName), []byte("portable=1\n"), 0o644); err != nil {
		t.Fatal(err)
	}

	_, ok := findPortableRoot(exeDir)
	if ok {
		t.Fatalf("expected not found — marker in parent dir should be ignored")
	}
}

func TestResolveUserDataDir_Default(t *testing.T) {
	// 非便携模式下，无 datadir.conf，应返回包含 AppName 的路径
	dir, err := ResolveUserDataDir()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if dir == "" {
		t.Fatal("expected non-empty dir")
	}
	if !strings.Contains(dir, buildvariant.AppName()) {
		t.Fatalf("expected dir to contain %s, got %s", buildvariant.AppName(), dir)
	}
}
