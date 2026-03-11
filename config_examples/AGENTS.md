<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# 配置示例目录 (config_examples/)

## 用途

提供拼音和五笔两种输入法的配置文件示例。用户可以参考这些示例创建自己的 `config.yaml`，放在 `%APPDATA%\WindInput\` 目录下。

## 主要文件

| 文件 | 描述 |
|------|------|
| `pinyin_config.yaml` | 拼音模式配置示例 |
| `wubi_config.yaml` | 五笔模式配置示例 |

## 子目录

无

## 配置项说明

### pinyin_config.yaml

```yaml
general:
  start_in_chinese_mode: true    # 启动时是否中文模式（false=英文）
  log_level: info                # 日志级别：debug/info/warn/error

dictionary:
  system_dict: dict/pinyin/base.txt  # 拼音词库路径
  user_dict: user_dict.txt           # 用户自定义词库
  pinyin_dict: ""                    # 拼音词库（可选）

engine:
  type: pinyin                   # 输入法类型：pinyin/wubi

hotkeys:
  toggle_mode: shift             # 切换中英文的快捷键
  switch_engine: ctrl+`          # 切换拼音/五笔引擎

ui:
  font_size: 18                  # 候选窗口字号
  candidates_per_page: 9         # 每页显示候选数
  font_path: ""                  # 自定义字体路径（空=系统默认）
```

### wubi_config.yaml

```yaml
engine:
  wubi:
    auto_commit: unique_at_4     # 自动上屏模式
      # - none:                   # 不自动上屏
      # - unique:                 # 候选唯一时上屏
      # - unique_at_4:            # 四码唯一时上屏（推荐）
      # - unique_full_match:      # 编码完整匹配且唯一

    empty_code: clear_at_4       # 空码处理
      # - none:                   # 继续输入
      # - clear:                  # 清空编码
      # - clear_at_4:             # 四码时清空（推荐）
      # - to_english:             # 转为英文上屏

    top_code_commit: true        # 五码顶字上屏
    punct_commit: true           # 标点顶字上屏
    show_pinyin_hint: false      # 显示拼音反查提示
```

## 工作指南

### 添加新配置选项

1. 更新 `pinyin_config.yaml` 或 `wubi_config.yaml` 示例文件
2. 在此文档中补充说明
3. 更新 `wind_input/config/` 中的配置定义代码
4. 更新 `wind_setting/frontend/` 的设置界面表单

### 用户使用配置

用户应：
1. 复制相应示例文件为 `%APPDATA%\WindInput\config.yaml`
2. 根据需要修改配置项
3. 重启或通过 `wind_setting` 重新加载配置

### 配置验证

修改配置后运行：
```bash
# 验证 YAML 格式
go test ./wind_input/config -v

# 检查配置加载
wind_input.exe --config-check
```

## 依赖关系

### 内部

- `../wind_input/config/` - 配置定义和加载逻辑
- `../wind_setting/frontend/` - 配置 UI 表单
- `../wind_input/` - 配置的消费者

### 外部

- YAML 规范标准
- 无其他外部依赖

<!-- MANUAL: -->
