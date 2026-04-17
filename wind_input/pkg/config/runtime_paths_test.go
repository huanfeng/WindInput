package config

import (
	"os"
	"path/filepath"
	"testing"
)

func TestFindPortableRoot(t *testing.T) {
	tmp := t.TempDir()
	root := filepath.Join(tmp, "bundle")
	exeDir := filepath.Join(root, "build")
	if err := os.MkdirAll(exeDir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, PortableMarkerName), []byte("portable=1\n"), 0o644); err != nil {
		t.Fatal(err)
	}

	got, ok := findPortableRoot(exeDir)
	if !ok {
		t.Fatalf("expected portable root, got not found")
	}
	if got != root {
		t.Fatalf("expected %s, got %s", root, got)
	}
}

func TestFindPortableRootNotFound(t *testing.T) {
	tmp := t.TempDir()
	got, ok := findPortableRoot(tmp)
	if ok {
		t.Fatalf("expected not found, got %s", got)
	}
}

func TestFindPortableRootDepthLimit(t *testing.T) {
	// 标记文件在 exeDir 上方 3 层，超过 maxPortableDepth=2 限制
	tmp := t.TempDir()
	root := filepath.Join(tmp, "level1")
	exeDir := filepath.Join(root, "level2", "level3", "level4")
	if err := os.MkdirAll(exeDir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, PortableMarkerName), []byte("portable=1\n"), 0o644); err != nil {
		t.Fatal(err)
	}

	got, ok := findPortableRoot(exeDir)
	if ok {
		t.Fatalf("expected not found due to depth limit, got %s", got)
	}
	_ = got
}

func TestFindPortableRootExactDepth(t *testing.T) {
	// 标记文件恰好在 exeDir 上方 2 层，刚好在 maxPortableDepth 限制内
	tmp := t.TempDir()
	root := filepath.Join(tmp, "level1")
	exeDir := filepath.Join(root, "level2", "level3")
	if err := os.MkdirAll(exeDir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, PortableMarkerName), []byte("portable=1\n"), 0o644); err != nil {
		t.Fatal(err)
	}

	got, ok := findPortableRoot(exeDir)
	if !ok {
		t.Fatalf("expected portable root at depth 2, got not found")
	}
	if got != root {
		t.Fatalf("expected %s, got %s", root, got)
	}
}
