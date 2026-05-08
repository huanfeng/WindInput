package updater

import (
	"strconv"
	"strings"
)

// Version 表示解析后的语义版本。
type Version struct {
	Major      int
	Minor      int
	Patch      int
	PreRelease string // 如 "alpha"、"beta"，稳定版为 ""
}

// ParseVersion 解析 "v0.1.2"、"0.1.2-alpha" 等格式。
// 解析失败时返回零值 Version。
func ParseVersion(s string) Version {
	s = strings.TrimPrefix(s, "v")
	base, pre, _ := strings.Cut(s, "-")
	parts := strings.SplitN(base, ".", 3)
	v := Version{PreRelease: pre}
	if len(parts) >= 1 {
		v.Major, _ = strconv.Atoi(parts[0])
	}
	if len(parts) >= 2 {
		v.Minor, _ = strconv.Atoi(parts[1])
	}
	if len(parts) >= 3 {
		v.Patch, _ = strconv.Atoi(parts[2])
	}
	return v
}

// IsNewerThan 判断 v 是否严格新于 other。
// 同一基础版本下，稳定版（无 PreRelease）比预发布版更新。
func (v Version) IsNewerThan(other Version) bool {
	if v.Major != other.Major {
		return v.Major > other.Major
	}
	if v.Minor != other.Minor {
		return v.Minor > other.Minor
	}
	if v.Patch != other.Patch {
		return v.Patch > other.Patch
	}
	if v.PreRelease == "" && other.PreRelease != "" {
		return true
	}
	return false
}
