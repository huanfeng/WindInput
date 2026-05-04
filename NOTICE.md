# 第三方资源声明

清风输入法 (WindInput) 使用了以下第三方资源，在此表示感谢并声明其许可证信息。

## 词库资源

### 雾凇拼音 (rime-ice)

- **用途**: 拼音词库数据源（单字词库、基础词库），用于生成拼音 unigram 语言模型
- **仓库**: https://github.com/iDvel/rime-ice
- **许可证**: GPL-3.0
- **使用的文件**:
  - `cn_dicts/8105.dict.yaml` — 通用规范汉字单字词库
  - `cn_dicts/base.dict.yaml` — 基础词库
  - `rime_ice.dict.yaml` — 词库入口描述文件

### 极点五笔 for Rime (rime-wubi86-jidian)

- **用途**: 五笔 86 版码表数据源
- **仓库**: https://github.com/KyleBing/rime-wubi86-jidian
- **许可证**: Apache-2.0
- **使用的文件**:
  - `wubi86_jidian.dict.yaml` — 主码表
  - `wubi86_jidian_extra.dict.yaml` — 扩展词库
  - `wubi86_jidian_extra_district.dict.yaml` — 行政区域词库

### pinyin-data

- **用途**: 汉字现代普通话读音数据，用于悬停提示中的拼音显示
- **仓库**: https://github.com/mozillazg/pinyin-data
- **许可证**: MIT
- **使用的文件**:
  - `kXHC1983.txt` — 现代新华字典多音字读音
  - `kTGHZ2013.txt` — 通用规范汉字多音字读音
  - `kMandarin_8105.txt` — 8105 标准汉字首音
- **说明**: 数据通过 `cmd/gen_pinyin_data` 工具生成为 `internal/tooltip/pinyin_data_generated.go`，已排除 kHanyuPinyin（汉语大字典古音）

### 五笔86拆字数据库 (wubi86_chaizi.txt)

- **用途**: 五笔字根拆字数据，用于悬停提示中显示候选字的拆字信息
- **文件**: `data/schemas/wubi86/wubi86_chaizi.txt`
- **来源**: 来自五笔输入法资源网盘，原始来源及作者不详
- **许可证**: 未附带任何版权声明或许可证信息

### 黑体字根字体 (HeiTiZiGen.ttf)

- **用途**: 渲染拆字提示中 PUA 私用区的五笔字根字符
- **文件**: `data/schemas/wubi86/HeiTiZiGen.ttf`
- **来源**: 来自五笔输入法资源网盘，原始来源及作者不详
- **许可证**: 未附带任何版权声明或许可证信息

### 腾讯词向量

- **用途**: 词频数据参考，用于 unigram 语言模型的词频权重
- **来源**: 腾讯 AI Lab 中文词向量数据集

## 技术参考

### Windows TSF 官方文档

- **来源**: https://docs.microsoft.com/en-us/windows/win32/tsf/text-services-framework
- **用途**: TSF 框架接口实现参考

### Windows Classic Samples

- **仓库**: https://github.com/microsoft/Windows-classic-samples
- **许可证**: MIT
- **用途**: TSF 输入法示例代码参考

## 许可证兼容性说明

本项目源代码采用 [MIT 许可证](LICENSE)。

词库数据文件来源于上述第三方项目，其各自适用原项目的许可证条款。构建过程中会从原始仓库下载这些词库文件，它们不包含在本项目的源代码中，而是作为构建时的外部依赖获取。
