# 按键绑定补充：功能键、撤销上屏、键盘开提示

> 来源：论坛 t372（Shift+Backspace 撤销上屏、→ 键开悬停提示）与维护者提出的「候选中按 F1 切换功能」。
> 全部走既有绑定表，出厂不绑。

## 会话态收 F1–F12

`keys.session_actions` 的键名新增 `f1`…`f12`（可带 `shift+` 前缀）。两份键名表
（`wind_keys::keymap::SESSION_FUNCTION_KEYS` 与 `wind_config::hotkey::session_key_to_vk`）同步，
一致性由 `session_key_tables_agree_across_crates` 守门。仅在有会话时生效，空闲时 F 键仍归宿主。

```toml
[keys.session_actions]
f1 = 'command:ime.toggle("candwin")'   # 候选中按 F1 执行命令（示例）
```

## Shift+Backspace 撤销上屏

无需新代码：组合键绑命令即可，空闲（刚上屏完）与组合中途都命中并吞键。

```toml
[keys.key_actions]
"shift+backspace" = "command:ime.undo_commit()"
```

## `toggle_tip`：键盘开 / 关悬停提示

会话动作，把悬停目标设为高亮项（等同鼠标停在上面），再按一次收起；移动高亮、翻页、候选刷新都会收起。
气泡内容与定位同鼠标悬停。候选没有提示内容时无效果。

```toml
[keys.session_actions]
f2 = "toggle_tip"
```

## 未做

- 空闲态（无会话）的功能键绑定：单键不能进热键表（会被 TSF 当热键吞掉，宿主的 F1 帮助等全失效），需 C++ 侧配合，另案。
- 字母键（u / v / i）作空闲引导键：见看板 C2-117，需方案级设计。
