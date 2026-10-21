# 第三方资源声明

清风输入法 (WindInput) 使用了以下第三方资源，在此表示感谢并声明其许可证信息。

## 词库与数据资源

### 已包含在本仓库中的资源

#### 五笔86拆字数据库 (wubi86_chaizi.txt)

- **用途**: 五笔字根拆字数据，用于悬停提示中显示候选字的拆字信息
- **文件**: `data/schemas/wubi86/wubi86_chaizi.txt`
- **来源**: 来自五笔输入法资源网盘，原始来源及作者不详；2026-09 起由社区用户
  （论坛 t222）补全 8105 通规字、CJK、CJK-A 并修订部分字根与编码
- **许可证**: 未附带任何版权声明或许可证信息。如您是该资源的权利人且认为
  本项目的使用不当，请通过 Issue 联系我们，我们将及时处理

#### 黑体字根字体 (HeiTiZiGen.ttf)

- **用途**: 渲染拆字提示中 PUA 私用区的五笔字根字符
- **文件**: `data/schemas/wubi86/HeiTiZiGen.ttf`
- **来源**: 来自五笔输入法资源网盘，原始来源及作者不详
- **许可证**: 未附带任何版权声明或许可证信息。处理方式同上

#### 英文词频名次表 (en_freq.txt)

- **用途**: 给英文词库里没写权重的词条按常用程度赋权，英文候选按词频而非字母序排列
- **文件**: `data/schemas/english/en_freq.txt`
- **来源**: [google-10000-english](https://github.com/first20hours/google-10000-english) 的
  `20k.txt`（提交 `d0736d4`），出厂英文主库 `en.dict.yaml` 的词表即取自同一份数据
- **加工方式**: 原样保留行序，删去约 70 个成人 / 粗俗词（清单见文件头）
- **许可证**: 上游未采用开源许可证。其 `LICENSE.md` 说明数据源自 Google Web Trillion Word
  Corpus（经 LDC 发布）、Peter Norvig 整理的子集（MIT），允许教育、个人与研究用途，并**不建议
  未经 LDC 授权的商业使用**。原文抄录于文件头。如您是权利人且认为本项目的使用不当，请通过
  Issue 联系我们

### 构建时下载的资源（不包含在本仓库中）

以下资源在构建过程中由构建脚本从原始仓库下载（缓存于 `.cache/`，已被
gitignore），用于生成词库数据文件，其各自适用原项目的许可证条款。

#### 极点五笔 for Rime (rime-wubi86-jidian)

- **用途**: 五笔 86 版码表数据源
- **仓库**: https://github.com/KyleBing/rime-wubi86-jidian
- **许可证**: Apache-2.0
- **使用的文件**: `wubi86_jidian.dict.yaml`（主码表）、
  `wubi86_jidian_extra.dict.yaml`（扩展词库）、
  `wubi86_jidian_extra_district.dict.yaml`（行政区域词库）
- **加工方式**: 由 `wind-tools/gen_dict` 处理后写入构建产物 `data/schemas/wubi86/`：
  主码表按 unigram 词频重新赋权排序、单字提权，并按简码级别分层；扩展词库按字符
  类型拆分为 extra / emoji / english / symbols 四个文件；行政区域词库原样透传
  （仅清理头部的 librime `sort:` 键）。条目文本本身未作增删改

#### 白霜拼音 (rime-frost)

- **用途**: 拼音词库数据源（单字词库、基础词库、扩展词库、英文词库），
  用于生成拼音 unigram 语言模型
- **仓库**: https://github.com/gaboolic/rime-frost
- **许可证**: GPL-3.0
- **使用的文件**: `rime_frost.dict.yaml`、`cn_dicts/`（8105 / 41448 / base /
  ext / others / corrections / tencent）、`en_dicts/`（en / en_ext）、`cn_dicts_cell/` 中的 16 张
  小型细胞词库（作扩展词库随包，出厂关，清单见 `scripts/dev.sh` 的 `FROST_CELLS`）、
  `opencc/emoji.txt` 与 `opencc/others.txt`（emoji 附加模块；`others.txt` 构建期按分组剔除
  `FROST_OTHERS_EXCLUDE` 所列的组后分发）
- **另用于**: 笔画输入方案词库的权重（取 `cn_dicts/8105`、`41448` 的单字字频，见 rime-stroke 条目）

#### pinyin-data

- **用途**: 汉字现代普通话读音数据，用于生成拼音映射与悬停提示中的拼音显示
- **仓库**: https://github.com/mozillazg/pinyin-data
- **许可证**: MIT

#### OpenCC

- **用途**: 简繁转换词典数据
- **仓库**: https://github.com/BYVoid/OpenCC
- **许可证**: Apache-2.0

#### Unicode CLDR / Unicode emoji 数据（研究用，未纳入发行版）

- **用途**: emoji 中文名称，供 `wind-tools/gen_emoji_names` 研究「按语义用五笔
  编码检索 emoji」（输入 `khgf` 出 ⚽）。**当前功能未启用**，其产物既不入库
  也不进发行版，仅在本机 `.cache/` 下用于评估
- **仓库/来源**:
  - https://github.com/unicode-org/cldr — `common/annotations/zh.xml`、
    `common/annotationsDerived/zh.xml`
  - https://unicode.org/Public/emoji/latest/emoji-test.txt
- **许可证**: **Unicode-3.0**（Unicode License V3）。允许自由使用、修改、再分发
  与商业使用，仅要求保留版权与许可声明。`Copyright © 1991-2025 Unicode, Inc.`，
  完整条款见 https://www.unicode.org/license.txt。若将来启用该功能，其产物可
  直接入库——本许可证无 copyleft 传染性

#### rime-emoji（候选 emoji 扩展）

- **用途**: 「打出词组后在候选里追加对应 emoji」的映射表（`词 → emoji…`），出厂关闭。
  设计见 `docs/design/emoji-suggestion.md`
- **仓库**: https://github.com/rime/rime-emoji
- **许可证**: **LGPL-3.0**
- **使用的文件**: `opencc/emoji_word.txt`（4668 行）、`opencc/emoji_category.txt`（165 行）、
  `LICENSE`
- **加工方式**: **不加工**。三个文件由 `scripts/dev.*` 的 gen-data 下载到 `.cache/rime-emoji/`，
  再**原样**复制进发行版 `data/emoji/`，逐字节未改
- **★ 与 rime-stroke 的处理刻意不同**: 笔画表在构建期转换（剥 YAML 头、裁剪字集），产物是
  衍生作品；本表则原样分发，我们交付的是**逐字副本**，LGPL 关于修改版的义务（标注修改、
  提供对应源）因此不触发。繁→简归一、同键合并与二进制化全部推迟到**用户本机**首次启用时
  进行，其产物是本机缓存，从不离开用户机器 ⇒ 不构成 conveying
- **⛔ 不得嵌入可执行文件**: 一旦 `include_bytes!` 进 exe 即构成 LGPL §4 的 Combined Work，
  需另行提供让用户替换该部分的机制；保持独立数据文件则该节整节不适用

#### rime-stroke（笔画辅助码 / 笔画输入方案）

- **用途**: 辅助码功能的笔画码表（拼音候选的字形二次筛选，出厂关闭）；「笔画」输入方案的码表
  （装着但不在出厂切换列表里）。全拼默认辅助码即引用该方案（`schema:stroke`）
- **仓库**: https://github.com/rime/rime-stroke
- **许可证**: **LGPL-3.0**
- **使用的文件**: `stroke.dict.yaml`（`字<TAB>笔画码`，h/s/p/n/z 表横竖撇捺折）
- **加工方式**: 由 `wind-tools/gen_aux_code` 按常用字集裁剪后写出两份构建产物，
  字集与每字的码序逐一相同。**码本身不作任何改动**（同字多码保留上游行序 = 优先级）。
  裁剪字集见下方 hanzi-chars 条目
  - `data/schemas/aux_code/stroke.txt`：剥去 YAML 头、`<TAB>` 转 `=`（辅助码表格式）
  - `data/schemas/stroke/stroke.dict.yaml`（笔画输入方案词库）：保留 librime 词库格式，
    另加一列权重。上游不带权重，权重取自 rime-frost（GPL-3.0，见上方条目）单字表
    `cn_dicts/8105` 与 `41448` 的字频（同字多音取最大，按对数压进 0~10000，无字频为 0）

#### rime-lua-aux-code（小鹤 / 自然码形码）

- **用途**: 辅助码功能的字形码表（双拼用户的形码筛选，出厂关闭）
- **仓库**: https://github.com/HowcanoeWang/rime-lua-aux-code
- **许可证**: MIT
- **使用的文件**: `aux_code/flypy_full.txt`（小鹤音形）、`aux_code/ZRM-wanxiang.txt`（自然码）
- **加工方式**: 上游已是 `字=码` 行格式，**逐行原样透传**，仅在文件首部补
  `# name/source/license` 元数据头（运行时据首行显示码表名）

#### hanzi-chars（汉字字表）

- **用途**: 裁剪笔画码表的字集依据。上游笔画表覆盖 11 万字（含扩展 B/C/…），
  全量载入对一个默认关闭的功能过重
- **仓库**: https://github.com/zispace/hanzi-chars
- **使用的文件**: `data-charset/GB 18030-2000.txt`、
  `data-charlist/《通用规范汉字表》（2013年）.txt`、`data-unicode/Unicode-CJK 〇.txt`
  （取并集，共 27733 字；清单在 `gen_aux_code::CHARSET_FILES`）
- **说明**: 这些是字表（事实数据汇编），本项目只用其中的汉字集合作过滤条件，
  不分发字表文件本身

#### 腾讯词向量

- **用途**: 词频数据参考（经由 rime-frost 的 `tencent.dict.yaml`），
  用于 unigram 语言模型的词频权重
- **来源**: 腾讯 AI Lab 中文词向量数据集

#### Wayland 协议描述（wind_linux/protocols/）

- **用途**: Linux 版 Wayland 候选窗（输入法 popup surface）生成客户端绑定所需的协议 XML，
  构建时由 wayland-scanner 生成代码；因发行版的 wayland-protocols 包不一定带
  `input-method-unstable-v2`，随仓库原样收录
- **文件**: `input-method-unstable-v2.xml`、`viewporter.xml`、`xdg-output-unstable-v1.xml`、
  `xdg-shell.xml`、`relative-pointer-unstable-v1.xml`
- **仓库**: https://gitlab.freedesktop.org/wayland/wayland-protocols
- **许可证**: MIT（各文件头部保留原版权声明）
- **另收**: `wlr-layer-shell-unstable-v1.xml`（屏幕锚定的浮层用），出自
  https://gitlab.freedesktop.org/wlroots/wlr-protocols ，HPND 式宽松许可（文件头部保留原版权与许可声明）

## 技术参考

以下项目/文档作为实现参考，本项目未复制其代码：

### Windows TSF 官方文档

- **来源**: https://learn.microsoft.com/en-us/windows/win32/tsf/text-services-framework
- **用途**: TSF 框架接口实现参考

### Windows Classic Samples

- **仓库**: https://github.com/microsoft/Windows-classic-samples
- **许可证**: MIT
- **用途**: TSF 输入法示例代码参考

### 鼠须管 (Squirrel)

- **仓库**: https://github.com/rime/squirrel
- **许可证**: GPL-3.0
- **用途**: macOS IMKit 输入法架构参考

### RIME / librime

- **仓库**: https://github.com/rime/librime
- **许可证**: BSD-3-Clause
- **用途**: 方案与词库格式（`.dict.yaml` 列序与元数据语义）、候选排序层级、
  整句解码与补全策略（`enable_completion`）、词频权重模型的设计参考

### 小狼毫 (Weasel)

- **仓库**: https://github.com/rime/weasel
- **许可证**: GPL-3.0
- **用途**: Windows TSF 焦点语义、编辑会话与候选窗定位行为的对照参考

### Fcitx5 / libime

- **仓库**: https://github.com/fcitx/fcitx5 、https://github.com/fcitx/libime
  （实际查阅的是 https://github.com/fcitx5-android/fcitx5-android 内置的 libime 源码）
- **许可证**: LGPL-2.1
- **用途**: 拼音解码（不完整拼音、`overLengthCost` 超长词惩罚、`partialLongWordLimit`）、
  用户词频模型（`HistoryBigram` / `UserLanguageModel`）的设计参考

### fcitx5-macos

- **用途**: macOS 端应用打包、代码签名与输入源注册细节的对照参考

### 极点五笔

- **用途**: 五笔输入的交互习惯与功能参考。
  其码表数据经由 Rime 移植版 rime-wubi86-jidian 引入，见上文「词库与数据资源」

## 许可证兼容性说明

本项目源代码采用 [GPL-3.0-or-later](LICENSE)。依赖的第三方 Rust crate 均为与 GPL-3.0
兼容的许可证（MIT、Apache-2.0、BSD、ISC、Zlib、MPL-2.0 等）。

词库数据文件来源于上述第三方项目，其各自适用原项目的许可证条款。
GPL-3.0 许可的词库数据（rime-frost）与 LGPL-3.0 许可的笔画码表（rime-stroke）
不包含在本仓库中，而是在构建过程中作为外部数据依赖从原始仓库下载；发行版中包含
由其生成的数据文件，该部分数据分别适用 GPL-3.0 / LGPL-3.0 条款。

LGPL-3.0 许可的 emoji 映射表（rime-emoji）同样不入库，但发行版中包含的是其
**未经修改的原始文件**（`data/emoji/`），适用 LGPL-3.0 条款，并随附上游 LICENSE 全文。

发行版的 `licenses/` 目录含本文件与本项目自身的 LICENSE，`data/emoji/LICENSE` 为
rime-emoji 的 LGPL-3.0 全文。该目录为履行「向每一位接收者提供许可证副本」
（GPL-3.0 §4、LGPL-3.0、Apache-2.0 §4(a)）而设，打包时不得省略。

⚠️ **尚未补全**：除 rime-emoji 外，其余 copyleft 上游（rime-frost 的 GPL-3.0、
rime-stroke 的 LGPL-3.0）与 Apache-2.0 上游的许可证**全文**目前未随包分发——本文件
的逐条声明不等于许可证副本。补齐方式是在 `scripts/dev.*` 的 gen-data 里逐个下载上游
LICENSE 并复制进 `licenses/`。

Apache-2.0（极点五笔码表）与 Unicode-3.0（Unicode CLDR，当前仅研究用）
均为宽松许可证，允许修改后再分发，其加工产物可直接包含在本仓库中，
无 copyleft 传染性。

## 本项目自身的名称与标识

本项目的 logo（`pic/logo.png`）与应用图标为原创图形作品，不在 GPL 许可证的
授权范围内。依据 GPL-3.0 第 7 条 (e) 款，本项目的许可证不授予使用项目名称
「清风输入法」「WindInput」及其标识的权利，详见 [BRANDING.md](BRANDING.md)。
