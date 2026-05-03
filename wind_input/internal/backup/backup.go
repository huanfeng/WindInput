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

// EstimateFilesSize 递归累加 dataDir 下所有文件的字节数，跳过 excludeTopNames 中的顶层条目。
// 用于备份大小预估，错误尽量忽略。
func EstimateFilesSize(dataDir string, excludeTopNames []string) int64 {
	excludeSet := make(map[string]bool, len(excludeTopNames))
	for _, n := range excludeTopNames {
		excludeSet[n] = true
	}
	var total int64
	_ = filepath.WalkDir(dataDir, func(path string, d fs.DirEntry, err error) error {
		if err != nil || path == dataDir {
			return nil
		}
		rel, _ := filepath.Rel(dataDir, path)
		top := strings.SplitN(rel, string(filepath.Separator), 2)[0]
		if excludeSet[top] {
			if d.IsDir() {
				return fs.SkipDir
			}
			return nil
		}
		if d.IsDir() {
			return nil
		}
		if info, err := d.Info(); err == nil {
			total += info.Size()
		}
		return nil
	})
	return total
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

// AtomicReplaceDir 用 srcDir 的内容原子地替换 destDir：
//  1. destDir → destDir+".bak"（保留旧数据）
//  2. srcDir  → destDir       （生效新数据）
//  3. 删除 .bak（任意失败可忽略）
//
// 任一阶段失败都会尝试回滚到原始状态，dataDir 始终处于一致状态。
// srcDir 与 destDir 必须位于同一文件系统（同一卷）。
func AtomicReplaceDir(srcDir, destDir string) error {
	backupDir := destDir + ".bak"
	// 清理上一次失败遗留的 .bak（若存在）
	_ = os.RemoveAll(backupDir)

	destExists := true
	if _, err := os.Stat(destDir); err != nil {
		if !os.IsNotExist(err) {
			return fmt.Errorf("stat dest: %w", err)
		}
		destExists = false
	}

	if destExists {
		if err := os.Rename(destDir, backupDir); err != nil {
			return fmt.Errorf("backup current dir: %w", err)
		}
	}

	if err := os.Rename(srcDir, destDir); err != nil {
		// 安装失败：回滚到原始状态
		if destExists {
			if rbErr := os.Rename(backupDir, destDir); rbErr != nil {
				return fmt.Errorf("install new dir: %w (rollback failed: %v)", err, rbErr)
			}
		}
		return fmt.Errorf("install new dir: %w", err)
	}

	if destExists {
		_ = os.RemoveAll(backupDir)
	}
	return nil
}
