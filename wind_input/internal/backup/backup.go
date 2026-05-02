package backup

import (
	"archive/zip"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"time"

	"gopkg.in/yaml.v3"
)

// Manifest 备份包元数据
type Manifest struct {
	Version     string `yaml:"version"`
	AppVersion  string `yaml:"app_version"`
	CreatedAt   string `yaml:"created_at"`
	DataDirMode string `yaml:"data_dir_mode"`
}

// WriteManifest 将 manifest 写入 YAML 文件
func WriteManifest(path string, m *Manifest) error {
	f, err := os.Create(path)
	if err != nil {
		return err
	}
	defer f.Close()
	enc := yaml.NewEncoder(f)
	enc.SetIndent(2)
	return enc.Encode(m)
}

// ReadManifest 从 YAML 文件读取 manifest
func ReadManifest(path string) (*Manifest, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	var m Manifest
	if err := yaml.NewDecoder(f).Decode(&m); err != nil {
		return nil, err
	}
	return &m, nil
}

// ReadManifestFromZip 从 ZIP 文件中读取 manifest.yaml
func ReadManifestFromZip(zipPath string) (*Manifest, error) {
	r, err := zip.OpenReader(zipPath)
	if err != nil {
		return nil, fmt.Errorf("open zip: %w", err)
	}
	defer r.Close()
	for _, f := range r.File {
		if f.Name == "manifest.yaml" {
			rc, err := f.Open()
			if err != nil {
				return nil, err
			}
			var m Manifest
			if err := yaml.NewDecoder(rc).Decode(&m); err != nil {
				rc.Close()
				return nil, err
			}
			rc.Close()
			return &m, nil
		}
	}
	return nil, fmt.Errorf("manifest.yaml not found in zip")
}

// WriteManifestToZip 将 manifest 写入已打开的 ZIP writer
func WriteManifestToZip(zw *zip.Writer, m *Manifest) error {
	fw, err := zw.Create("manifest.yaml")
	if err != nil {
		return err
	}
	enc := yaml.NewEncoder(fw)
	enc.SetIndent(2)
	return enc.Encode(m)
}

// CopyDirToZip 递归将 srcDir 下所有文件写入 ZIP 的 zipPrefix 路径
// excludeTopNames：跳过的顶层文件/目录名（如 "user_data.db"）
func CopyDirToZip(zw *zip.Writer, srcDir, zipPrefix string, excludeTopNames []string) error {
	excludeSet := make(map[string]bool, len(excludeTopNames))
	for _, n := range excludeTopNames {
		excludeSet[n] = true
	}
	return filepath.WalkDir(srcDir, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if path == srcDir {
			return nil
		}
		rel, _ := filepath.Rel(srcDir, path)
		topName := strings.SplitN(rel, string(filepath.Separator), 2)[0]
		if excludeSet[topName] {
			if d.IsDir() {
				return fs.SkipDir
			}
			return nil
		}
		if d.IsDir() {
			return nil
		}
		zipPath := zipPrefix + filepath.ToSlash(rel)
		fw, err := zw.Create(zipPath)
		if err != nil {
			return err
		}
		f, err := os.Open(path)
		if err != nil {
			return err
		}
		_, err = io.Copy(fw, f)
		f.Close()
		return err
	})
}

// ExtractZipPrefix 将 ZIP 中以 prefix 开头的条目解压到 destDir
func ExtractZipPrefix(r *zip.Reader, prefix, destDir string) error {
	absRoot, err := filepath.Abs(destDir)
	if err != nil {
		return err
	}
	for _, f := range r.File {
		if !strings.HasPrefix(f.Name, prefix) {
			continue
		}
		rel := strings.TrimPrefix(f.Name, prefix)
		if rel == "" || strings.HasSuffix(f.Name, "/") {
			continue
		}
		dest := filepath.Join(destDir, filepath.FromSlash(rel))
		// zip-slip 防护
		absDest, err := filepath.Abs(dest)
		if err != nil {
			return err
		}
		if !strings.HasPrefix(absDest, absRoot+string(filepath.Separator)) {
			return fmt.Errorf("invalid zip entry path: %s", f.Name)
		}
		if err := os.MkdirAll(filepath.Dir(dest), 0755); err != nil {
			return err
		}
		rc, err := f.Open()
		if err != nil {
			return err
		}
		outf, err := os.Create(dest)
		if err != nil {
			rc.Close()
			return err
		}
		_, copyErr := io.Copy(outf, rc)
		rc.Close()
		closeErr := outf.Close()
		if copyErr != nil {
			os.Remove(dest)
			return copyErr
		}
		if closeErr != nil {
			os.Remove(dest)
			return closeErr
		}
	}
	return nil
}

// ExtractSchemaIDsFromZip 从 ZIP 的 db/schemas/ 路径中枚举方案 ID
func ExtractSchemaIDsFromZip(r *zip.Reader) []string {
	seen := make(map[string]bool)
	const prefix = "db/schemas/"
	for _, f := range r.File {
		if !strings.HasPrefix(f.Name, prefix) {
			continue
		}
		rest := strings.TrimPrefix(f.Name, prefix)
		parts := strings.SplitN(rest, "/", 2)
		if len(parts) >= 1 && parts[0] != "" {
			seen[parts[0]] = true
		}
	}
	ids := make([]string, 0, len(seen))
	for id := range seen {
		ids = append(ids, id)
	}
	return ids
}

// CountThemes 统计主题目录下的文件数量
func CountThemes(themesDir string) int {
	entries, err := os.ReadDir(themesDir)
	if err != nil {
		return 0
	}
	count := 0
	for _, e := range entries {
		if !e.IsDir() {
			count++
		}
	}
	return count
}

// DefaultBackupFilename 生成默认备份文件名
func DefaultBackupFilename() string {
	return "WindInput_backup_" + time.Now().Format("2006-01-02_150405") + ".zip"
}

// AtomicReplaceDir 将 srcDir 的所有内容覆盖到 destDir
// srcDir 中不存在的 destDir 顶层条目会被删除
func AtomicReplaceDir(srcDir, destDir string) error {
	destEntries, err := os.ReadDir(destDir)
	if err != nil && !os.IsNotExist(err) {
		return err
	}
	srcNames := make(map[string]bool)
	if srcEntries, err := os.ReadDir(srcDir); err == nil {
		for _, e := range srcEntries {
			srcNames[e.Name()] = true
		}
	}
	for _, e := range destEntries {
		if !srcNames[e.Name()] {
			_ = os.RemoveAll(filepath.Join(destDir, e.Name()))
		}
	}
	return filepath.WalkDir(srcDir, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		rel, _ := filepath.Rel(srcDir, path)
		dest := filepath.Join(destDir, rel)
		if d.IsDir() {
			return os.MkdirAll(dest, 0755)
		}
		if err := os.MkdirAll(filepath.Dir(dest), 0755); err != nil {
			return err
		}
		if err := copyFile(path, dest); err != nil {
			return err
		}
		return os.Remove(path)
	})
}

func copyFile(src, dst string) error {
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()
	out, err := os.Create(dst)
	if err != nil {
		return err
	}
	if _, err := io.Copy(out, in); err != nil {
		out.Close()
		os.Remove(dst)
		return err
	}
	return out.Close()
}
