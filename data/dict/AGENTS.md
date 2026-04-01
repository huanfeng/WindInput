<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-01 -->

# 词库目录 (dict/)

## 用途

存储输入法的字符集和拼音词库。`common_chars.txt` 定义常用字表，`pinyin/` 目录存放拼音及相关数据文件。运行时词库会被加载到内存并用于候选生成。

## 主要文件

| 文件 | 描述 |
|------|------|
| `common_chars.txt` | 常用字表（GB2312 常用 3500 字）。格式：`序号→字符`，用于候选排序权重参考 |

## 子目录

| 目录 | 用途 |
|-----|------|
| `pinyin/` | 拼音词库（见 `pinyin/AGENTS.md`） |
| `wubi86/` | 五笔86版词库 |

## 工作指南

### 字符表格式

`common_chars.txt` 每行格式：
```
N→字
```

其中：
- `N` 是序号（1-3500）
- `→` 是固定分隔符
- `字` 是 GB2312 中的字符

例如：
```
1→一
2→乙
3→二
```

序号越小代表字符越常用，候选排序时可参考此序号给予权重。

### 修改词库

- **词库源文件** 在 `../build/dict/` 目录（编译生成）
- **编辑源文件** 前提是拼音/五笔数据来源已更新
- 修改后需要重新运行 `build_all.ps1` 让词库生效

### 常见操作

```bash
# 查看常用字数量
wc -l dict/common_chars.txt

# 验证字符表格式
head -20 dict/common_chars.txt

# 检查是否有重复序号
awk -F'→' '{print $1}' dict/common_chars.txt | sort | uniq -d
```

## 依赖关系

### 内部

- `../wind_input/` - 加载词库并用于拼音输入
- `../wind_setting/` - 词库管理和自定义词库上传
- `../build/dict/` - 编译产物的词库目录

### 外部

- 拼音词库数据源：[雾凇拼音 rime-ice](https://github.com/iDvel/rime-ice)（rime 生态，自描述 .dict.yaml 格式）
- 五笔词库数据源：rime 生态五笔86词库（.dict.yaml 格式，与 wubi86.schema.yaml 配套）

<!-- MANUAL: -->
