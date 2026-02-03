# 码表过滤功能设计文档

> **状态**: ✅ 已完成实现
> **最后更新**: 2026-02-03

## 1. 功能目标

实现输入法候选词的过滤功能，避免过多生僻字影响日常使用体验。

### 核心需求
- 基于《通用规范汉字表》(8105字) 过滤生僻字
- 支持三种检索模式，满足不同使用场景
- 对拼音引擎和五笔引擎统一生效

## 2. 三种检索模式

| 模式 | 名称 | 说明 |
|------|------|------|
| `general` | 通用模式 | 只显示通用规范汉字，过滤所有生僻字 |
| `gb18030` | 全字符模式 | 显示所有字符，不过滤 |
| `smart` | 智能模式（默认） | 优先显示通用字；若结果为空则回退到全字符 |

## 3. 需要修改的文件

### 3.1 新增文件

| 文件路径 | 说明 |
|----------|------|
| `wind_input/internal/dict/common_chars.go` | 通用汉字判断模块 |
| `dict/common_chars.txt` | 通用规范汉字表数据文件 (8105字) |

### 3.2 修改文件

| 文件路径 | 修改内容 |
|----------|----------|
| `wind_input/internal/candidate/candidate.go` | 添加 `IsCommon bool` 字段 |
| `wind_input/internal/config/config.go` | 添加 `FilterMode string` 配置项 |
| `wind_input/internal/dict/codetable.go` | 加载时标记 IsCommon |
| `wind_input/internal/dict/loader.go` | 加载时标记 IsCommon |
| `wind_input/internal/engine/wubi/wubi.go` | Convert 时应用过滤 |
| `wind_input/internal/engine/pinyin/pinyin.go` | Convert 时应用过滤 |

## 4. 详细设计

### 4.1 common_chars.go 模块

```go
package dict

var commonCharMap = make(map[rune]bool)

// InitCommonChars 从文件或内置数据初始化通用汉字表
func InitCommonChars()

// IsCommonChar 判断单个字符是否为通用字
func IsCommonChar(char rune) bool

// IsStringCommon 判断字符串是否全部为通用字（一票否决）
func IsStringCommon(text string) bool
```

### 4.2 Candidate 结构体修改

```go
// candidate/candidate.go
type Candidate struct {
    Text     string
    Pinyin   string
    Code     string
    Weight   int
    Hint     string
    IsCommon bool  // 新增：是否为通用规范汉字
}
```

### 4.3 配置项修改

```go
// config/config.go
type EngineConfig struct {
    Type       string       `yaml:"type"`
    FilterMode string       `yaml:"filter_mode"` // 新增：general/gb18030/smart
    Pinyin     PinyinConfig `yaml:"pinyin"`
    Wubi       WubiConfig   `yaml:"wubi"`
}
```

默认值：`filter_mode: "smart"`

### 4.4 码表加载时标记

在 `codetable.go` 的 `parseEntryLine` 和 `loader.go` 的 `Load` 中：

```go
cand := candidate.Candidate{
    Text:     text,
    Code:     code,
    Weight:   weight,
    IsCommon: dict.IsStringCommon(text), // 新增
}
```

### 4.5 过滤逻辑实现

在引擎的 Convert 方法返回前应用过滤：

```go
// FilterCandidates 根据模式过滤候选词
func FilterCandidates(candidates []Candidate, mode string) []Candidate {
    switch mode {
    case "general":
        // 只返回通用字
        return filterCommonOnly(candidates)
    case "gb18030":
        // 不过滤
        return candidates
    case "smart":
        // 智能模式：优先通用字，为空则回退
        filtered := filterCommonOnly(candidates)
        if len(filtered) == 0 {
            return candidates
        }
        return filtered
    default:
        return candidates
    }
}

func filterCommonOnly(candidates []Candidate) []Candidate {
    var result []Candidate
    for _, c := range candidates {
        if c.IsCommon {
            result = append(result, c)
        }
    }
    return result
}
```

## 5. 通用规范汉字表数据

### 5.1 数据来源
- 《通用规范汉字表》(2013年国务院发布)
- 共 8105 字，分三级

### 5.2 文件格式 (common_chars.txt)

```
# 通用规范汉字表 (8105字)
# 一级字表：3500字（常用字）
# 二级字表：3000字（次常用字）
# 三级字表：1605字（专业用字）

的一是在不了有和人这中大为上个国我以要他...
（每行存放多个汉字，无分隔符）
```

### 5.3 加载方式
1. 程序启动时从 `dict/common_chars.txt` 加载
2. 若文件不存在，使用内置的核心常用字（约2500字）
3. 使用 `sync.Once` 确保只加载一次

## 6. 实现步骤

1. ✅ **创建 common_chars.txt** - 包含通用规范汉字（`dict/common_chars.txt`）
2. ✅ **创建 common_chars.go** - 实现加载和判断函数（`dict/common_chars.go`）
3. ✅ **修改 candidate.go** - 添加 IsCommon 字段（`candidate/candidate.go`）
4. ✅ **修改 config.go** - 添加 FilterMode 配置（`engine.filter_mode`）
5. ✅ **修改 codetable.go** - 加载时调用 IsStringCommon 标记
6. ✅ **修改 loader.go** - 加载时调用 IsStringCommon 标记
7. ✅ **创建 filter.go** - 实现候选词过滤逻辑（`candidate/filter.go`）
8. ✅ **集成到引擎** - 五笔和拼音引擎使用过滤功能
9. ✅ **编译测试** - 功能验证通过

## 7. 配置示例

```yaml
# config.yaml
engine:
  type: pinyin
  filter_mode: smart  # general / gb18030 / smart
  pinyin:
    show_wubi_hint: true
  wubi:
    auto_commit: unique_at_4
```

## 8. 用户界面

在设置界面中添加"字符过滤"选项：
- 通用字优先（智能模式）- 默认
- 仅通用字（严格模式）
- 全部字符（不过滤）
