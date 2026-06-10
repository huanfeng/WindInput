// Package config — TOML 桥接编解码层
//
// 本包的配置文件格式为 TOML（config.toml / state.toml / compat.toml /
// schema_overrides.toml），但 struct ↔ map 的转换统一走既有 yaml tag 管线
// （yaml.v3），TOML 仅作为磁盘表面格式。这种"桥接式"设计的动机：
//   - 三层配置合并依赖 yaml.v3 "Unmarshal 到已填充 struct 只覆盖文档中
//     出现的键" 的部分覆盖语义，桥接后该语义原样保留；
//   - 配置自愈依赖 yaml.TypeError 的"部分解码+收集全部错误"语义，桥接后
//     类型错误仍在 yaml 解码阶段产生，自愈分支无需改动（TOML 阶段只会
//     产生语法错误，按"文件损坏"处理）；
//   - 无需给所有配置 struct 维护第二套 toml tag。
//
// 三态字段（*bool/*string + omitempty）以"键缺失"表达未设置，TOML 与
// YAML 在此语义上等价，故无 null 表达需求；marshalTOML 仍会防御性剔除
// nil 值（TOML 无法编码 nil）。
package config

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	toml "github.com/pelletier/go-toml/v2"
	"gopkg.in/yaml.v3"
)

// MigratedBackupSuffix 旧版 YAML 配置文件迁移为 TOML 后的备份后缀。
// 迁移完成后旧文件改名为 <原名>.migrated.bak 保留，便于回滚到旧版本程序。
const MigratedBackupSuffix = ".migrated.bak"

// IsTOMLPath 判断路径扩展名是否为 .toml（不区分大小写）。
func IsTOMLPath(path string) bool {
	return strings.EqualFold(filepath.Ext(path), ".toml")
}

// LegacyYAMLPath 返回 .toml 路径对应的旧版 .yaml 同名路径
// （如 config.toml → config.yaml），用于双读回退与一次性迁移。
func LegacyYAMLPath(tomlPath string) string {
	return strings.TrimSuffix(tomlPath, filepath.Ext(tomlPath)) + ".yaml"
}

// normalizeToYAML 把配置文件原始字节归一化为 YAML 字节流：
// .toml 文件经 TOML→map→YAML 桥接转换，其余格式原样返回。
// 仅做语法层转换；类型校验留给后续 yaml.Unmarshal，以保留
// yaml.TypeError 的部分解码自愈语义。
func normalizeToYAML(path string, data []byte) ([]byte, error) {
	if !IsTOMLPath(path) {
		return data, nil
	}
	if len(data) == 0 {
		return nil, nil
	}
	var m map[string]interface{}
	if err := toml.Unmarshal(data, &m); err != nil {
		return nil, fmt.Errorf("invalid TOML: %w", err)
	}
	return yaml.Marshal(m)
}

// marshalTOML 把任意值（struct 或 map）序列化为 TOML 字节流。
// 值先经 YAML 往返归一化为 map（字段命名沿用 yaml tag），再剔除 TOML
// 无法表达的 nil 值后编码。空 map 序列化为空文件。
func marshalTOML(v interface{}) ([]byte, error) {
	m, err := toYAMLMap(v)
	if err != nil {
		return nil, err
	}
	stripNils(m)
	if len(m) == 0 {
		return []byte{}, nil
	}
	return toml.Marshal(m)
}

// marshalForPath 按目标路径扩展名选择序列化格式（.toml → TOML，其余 → YAML）。
func marshalForPath(path string, v interface{}) ([]byte, error) {
	if IsTOMLPath(path) {
		return marshalTOML(v)
	}
	return yaml.Marshal(v)
}

// stripNils 递归剔除 map 中的 nil 值与切片中的 nil 元素（原地修改）。
// TOML 没有 null，三态字段的"未设置"以键缺失表达。
func stripNils(m map[string]interface{}) {
	for k, v := range m {
		switch val := v.(type) {
		case nil:
			delete(m, k)
		case map[string]interface{}:
			stripNils(val)
		case []interface{}:
			m[k] = stripNilsSlice(val)
		}
	}
}

func stripNilsSlice(s []interface{}) []interface{} {
	out := s[:0]
	for _, v := range s {
		switch val := v.(type) {
		case nil:
			continue
		case map[string]interface{}:
			stripNils(val)
		case []interface{}:
			v = stripNilsSlice(val)
		}
		out = append(out, v)
	}
	return out
}

// readFileWithLegacyFallback 读取 path；若 path 为 .toml 且不存在，则回退
// 读取同名旧版 .yaml 文件。返回实际读到的数据、实际读取路径，以及当发生
// 旧版回退时的旧文件路径（migratedFrom 非空表示调用方应在成功持久化新格式
// 后调用 renameLegacyFile 完成迁移收尾）。
func readFileWithLegacyFallback(path string) (data []byte, readPath string, migratedFrom string, err error) {
	data, err = os.ReadFile(path)
	if err == nil || !os.IsNotExist(err) || !IsTOMLPath(path) {
		return data, path, "", err
	}
	legacy := LegacyYAMLPath(path)
	legacyData, legacyErr := os.ReadFile(legacy)
	if legacyErr != nil {
		// 旧文件也不存在（或读取失败）：维持原始的 not-exist 错误语义
		return nil, path, "", err
	}
	return legacyData, legacy, legacy, nil
}

// renameLegacyFile 把已完成迁移的旧版 YAML 文件改名为 *.migrated.bak。
// 已有同名备份时先删除（保留最近一次）。失败仅打印警告：旧文件残留是
// 无害的——加载方此后总是优先读到新格式文件。
func renameLegacyFile(legacyPath string) {
	bakPath := legacyPath + MigratedBackupSuffix
	_ = os.Remove(bakPath)
	if err := os.Rename(legacyPath, bakPath); err != nil {
		fmt.Fprintf(os.Stderr, "[config] warning: 旧配置文件改名备份失败 path=%s err=%v\n", legacyPath, err)
	}
}
