# WindInput 功能规划文档

## 项目现状分析

### 已实现功能
- TSF 框架集成（C++ DLL）
- Named Pipe IPC 通信
- 基础拼音引擎（音节解析、词库查询）
- 候选窗口 UI（原生 Win32 + gg 渲染）
- 中英文切换（Shift 键）
- 语言栏图标
- 配置系统（YAML）
- 多屏幕和高 DPI 支持

### 待完善架构
- 词库加载器仅支持简单的 `拼音 汉字 权重` 格式
- 引擎接口仅支持拼音输入
- 缺少通用码表支持

---

## 词库格式分析

### 通用码表头格式（参考词库使用）
```
[CODETABLEHEADER]
Name=词库名称
Version=版本号
Author=作者
CodeScheme=编码方案（拼音/五笔86等）
CodeLength=最大码长（五笔为4，拼音可达54）
BWCodeLength=0
SpecialPrefix=特殊前缀（如zz用于反查）
PhraseRule=短语规则
[CODETABLE]
编码	汉字	词频（可选）
```

### 五笔词库特点
- **最大码长**: 4
- **编码规则**: 横竖撇捺折对应 ghjkl/TREWQ 等
- **特殊前缀**: `zz` 用于拼音反查
- **自动上屏策略**: 需要支持四码/唯一候选自动上屏

### 拼音词库特点
- **最大码长**: 较长（全拼可达54）
- **支持简拼**: 如 `bj` 对应 `北京`
- **分词策略**: 需要音节分割

---

## 待实现功能列表

### 第一阶段：核心码表支持（高优先级）✅ 已完成

#### 1. 通用码表加载器 ✅
- [x] 支持 `[CODETABLEHEADER]` 格式解析
- [x] 支持 UTF-8 和 UTF-16 LE 编码
- [x] 解析 CodeLength、CodeScheme 等元数据
- [x] 支持 `编码\t汉字\t词频` 格式（词频可选）

#### 2. 多引擎架构 ✅
- [x] 定义通用 Engine 接口，支持不同输入法
- [x] 拼音引擎（已有，需重构）
- [x] 五笔引擎（基于码表）
- [x] 引擎管理器，支持动态切换

#### 3. 五笔输入特性 ✅
- [x] 四码最大长度限制
- [x] 首选唯一自动上屏选项
- [x] 五码顶字上屏
- [x] 空码处理策略

#### 4. 反查功能 ⏳ 部分完成
- [x] 拼音反查五笔编码（框架已实现）
- [x] 反查功能开关配置
- [ ] 候选词显示编码提示（UI待实现）

---

### 第二阶段：输入体验增强（中优先级）✅ 大部分完成

#### 5. 临时英文模式 ✅ 已完成
- [x] Shift+字母 进入临时英文模式
- [x] 输入完成后自动切回中文
- [x] 支持显示英文候选（可配置）
- [x] 配置项：`input.shift_temp_english.enabled`

#### 6. 自动上屏策略（五笔专用）✅ 已完成
- [x] 不上屏（默认）
- [x] 四码唯一时自动上屏 (`engine.wubi.auto_commit_at_4`)
- [x] 五码顶字上屏 (`engine.wubi.top_code_commit`)
- [x] 标点顶字上屏 (`engine.wubi.punct_commit`)

#### 7. 空码处理策略 ✅ 已完成
- [x] 不清空（继续输入）
- [x] 四码自动清空 (`engine.wubi.clear_on_empty_at_4`)

#### 8. 候选词排序 ⏳ 部分完成
- [x] 按词库顺序
- [x] 按词频排序
- [ ] 按输入次数排序（需用户词库学习）
- [ ] 单字优先选项
- [ ] 用户词优先选项

---

### 第三阶段：快捷操作（中优先级）✅ 已完成

#### 9. 二三候选上屏快捷键 ✅ 已完成
- [x] 分号/引号键（`;` / `'`）- `semicolon_quote`
- [x] 逗号/句号键（`,` / `.`）- `comma_period`
- [x] 左/右 Shift - `lrshift`
- [x] 左/右 Ctrl - `lrctrl`
- [x] 配置项：`input.select_key_groups`（支持多选）

#### 10. 标点符号顶字上屏 ✅ 已完成
- [x] 中文标点顶首选上屏
- [x] 配置项：`engine.wubi.punct_commit`

#### 11. 候选翻页 ✅ 已完成
- [x] PageUp / PageDown - `pageupdown`
- [x] 减号/加号（`-` / `=`）- `minus_equal`
- [x] 左右方括号（`[` / `]`）- `brackets`
- [x] Shift+Tab / Tab - `shift_tab`
- [x] 配置项：`input.page_keys`（支持多选）

---

### 第四阶段：模式切换（中优先级）✅ 已完成

#### 12. 中英文切换热键 ✅ 已完成
- [x] 左 Shift (`lshift`)
- [x] 右 Shift (`rshift`)
- [x] 左 Ctrl (`lctrl`)
- [x] 右 Ctrl (`rctrl`)
- [x] CapsLock (`capslock`)
- [x] 配置项：`hotkeys.toggle_mode_keys`（支持多选）

#### 13. 中文切换到英文时处理 ✅ 已完成
- [x] 已有编码上屏
- [x] 配置项：`hotkeys.commit_on_switch`

#### 14. 全半角切换 ✅ 已完成
- [x] 无（禁用）
- [x] Shift+空格
- [x] 配置项：`hotkeys.toggle_full_width`

#### 15. 中英文标点切换 ✅ 已完成
- [x] 无（禁用）
- [x] Ctrl+句号
- [x] 配置项：`hotkeys.toggle_punct`

#### 16. 标点状态独立 ✅ 已完成
- [x] 标点随中英文切换（可配置）
- [x] 配置项：`input.punct_follow_mode`

---

### 第五阶段：高级功能（低优先级）

#### 17. 用户词库
- [ ] 自动学习用户输入
- [ ] 词频自动调整
- [ ] 用户词库导入导出

#### 18. 自定义短语
- [ ] 支持自定义缩写
- [ ] 如 `yx` -> `邮箱：xxx@example.com`

#### 19. 模糊音
- [ ] z/zh, c/ch, s/sh
- [ ] n/l, r/l
- [ ] an/ang, en/eng, in/ing
- [ ] 可选配置

#### 20. 云词库同步
- [ ] 用户词库云端备份
- [ ] 多设备同步

---

## 配置结构（当前实现）

```yaml
startup:
  remember_last_state: false          # 记忆前次状态
  default_chinese_mode: true          # 默认中文模式
  default_full_width: false           # 默认半角
  default_chinese_punct: true         # 默认中文标点

dictionary:
  system_dict: dict/pinyin/pinyin.txt
  user_dict: user_dict.txt
  pinyin_dict: dict/pinyin/pinyin.txt  # 用于反查

engine:
  type: pinyin                        # pinyin / wubi
  filter_mode: smart                  # smart / general / gb18030

  pinyin:
    show_wubi_hint: true              # 显示五笔编码提示

  wubi:
    auto_commit_at_4: false           # 四码唯一自动上屏
    clear_on_empty_at_4: false        # 四码为空时清空
    top_code_commit: true             # 五码顶字上屏
    punct_commit: true                # 标点顶字上屏

hotkeys:
  toggle_mode_keys: [lshift, rshift]  # 中英切换键（多选）
  commit_on_switch: true              # 切换时编码上屏
  switch_engine: "ctrl+`"             # 切换引擎
  toggle_full_width: shift+space      # 全半角切换
  toggle_punct: "ctrl+."              # 中英标点切换

input:
  punct_follow_mode: false            # 标点随中英文切换
  select_key_groups: [semicolon_quote] # 2/3候选键组（多选）
  page_keys: [pageupdown, minus_equal] # 翻页键（多选）
  shift_temp_english:
    enabled: true                     # 临时英文模式
    show_english_candidates: true     # 显示英文候选
  capslock_behavior:
    cancel_on_mode_switch: false      # 切换时取消 CapsLock

toolbar:
  visible: false                      # 显示工具栏
  position_x: 0
  position_y: 0

ui:
  font_size: 18
  candidates_per_page: 9
  font_path: ""
  inline_preedit: true                # 嵌入式编码行

advanced:
  log_level: info                     # debug/info/warn/error
```

---

## 实现优先级排序

### P0 - ✅ 已完成
1. 通用码表加载器
2. 五笔引擎基础实现
3. 基础输入测试
4. 二进制 IPC 协议

### P1 - ✅ 已完成
5. 自动上屏策略
6. 空码处理
7. 反查功能框架
8. 二三候选上屏

### P2 - ✅ 已完成
9. 临时英文模式
10. 候选翻页配置
11. 中英切换热键配置
12. 标点相关配置
13. 工具栏 UI

### P3 - 待实现
14. 用户词库学习
15. 自定义短语
16. 模糊音
17. 云同步

---

## 下一步行动

### 当前进度
第一至第四阶段的核心功能已基本完成，可投入日常使用。

### 下一步计划
1. **用户词库学习** - 记录用户输入习惯，自动调整词频
2. **自定义短语** - 支持用户自定义缩写
3. **模糊音支持** - 拼音引擎的容错输入
4. **设置界面完善** - wind_setting 功能开发
5. **稳定性优化** - 修复边界情况和潜在问题
