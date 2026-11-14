# 翻页键「翻不动」策略的按键级覆盖（per-key overflow）

> 来源：[GH#249](https://github.com/huanfeng/WindInput/issues/249)（首行首字逗号上屏，看板 A2-93）+
> 维护者意见（2026-10-25）：有人喜欢「明确」——这个按键无效就是无效，不要随意多做事情；
> 行为策略应做成**按键相关**的选项，而不是按键类别一把抓。
> 关联：[keys-overflow.md 的运行时闸门](#) `page_key_overflow_to_punct`、C3-9（t256，`keys.overflow.page_key` 的由来）。

> **2026-11-07 改版**：覆盖表 `page_key_overrides` 已移除，按键级策略改为翻页动作的后缀
> `page_prev:commit_and_input` / `page_prev:ignore`（`page_next` 同），绑在 `keys.session_actions`
> 里——翻页键分组本就折算进这张表，设置端「全局自定义输入中按键」直接可配，外面的全局档不变。
> 下文「方案」一节是 0.129 的覆盖表做法，留作沿革；消费点见 `Coordinator::page_key_overflow_commits`。

## 现状

`[keys.overflow]` 四项（`number_key` / `select_key` / `select_char_key` / `page_key`）都是**按键类别级**的全局档位。以翻页键为例：

- `page_key = "ignore"(出厂)`：翻不动一律吞键；
- `page_key = "commit_and_input"`：翻不动时顶高亮候选并出该键符号（走标点通路，中文标点/全角/自动配对照常）。

闸门在 `coordinator.rs::page_key_overflow_to_punct`：主路 + 本批候选从未翻过页（`!state.paged`）+ 全局档为 `commit_and_input` + 该键能出符号。出厂 `page_keys = ["pageupdown", "minus_equal"]`，用户把 `comma_period` 加进翻页键后，「希望逗号翻不动时上屏、但别的一切照旧」目前做不到——开了全局档，`-` `=` 也跟着变（虽然 `-` 另被 `buffer_symbol_chars="-"` 先接走进缓冲，行为差异被部分掩盖，但 `=` 会真的变）。

## 方案

**全局默认档保留，新增按键级覆盖表**：

```toml
[keys.overflow]
page_key = "ignore"            # 全局默认档（现有字段，语义不变）
# 按键级覆盖：键名（wind-keys KEY_TABLE names）→ 策略。未列出的键回落 page_key。
page_key_overrides = { comma = "commit_and_input" }
```

- 键的取值域：`minus` / `equal` / `comma` / `period` / `lbracket` / `rbracket`——即 `page_keys` 可打印键组的实际成员（`pageupdown` 是物理键，翻不动的场景不存在，不进此表）。
- 策略值域与 `page_key` 一致：`ignore` / `commit_and_input`（没有 `commit`——「只顶不出字」没有诉求，见现有字段注释）。
- 键名走 [`wind-keys` KEY_TABLE] SSOT 的 names；写错键名 / 非法值**启动时 WARN**（配置文件无法当场报错，与 key_actions 写错键名被静默丢弃的教训相对——至少要响）。设置端将来做编辑器时再前置校验。
- 运行时零解析：`Config` 折算阶段把键名解析成 VK 存 `BTreeMap<u32, String>`（或并入现有 session 动作折算处），`page_key_overflow_to_punct` 先查覆盖表、再回落全局档。解析失败（键名不认识）的条目丢弃并 WARN，不进表。

## 边界与不变量

1. **翻过页仍吞键**：`!state.paged` 闸门对覆盖表同样生效——翻页中途永远不出符号，这条红线不因覆盖松动。
2. **`buffer_symbol_chars` 优先级不变**：`-` 出厂先进缓冲；想首页 `-` 顶屏出字，仍需清空 `buffer_symbol_chars`（现有文档已写，配方里复述）。
3. **overlay 不受影响**：本闸门只在主路（`state.active.is_none()`）；临拼 / 快捷输入等各有自己的翻页语义。
4. **展开态（网格）不经过这里**：`grid_page_key` 有自己的行滚动语义。

## 设置端与对账

- `config_schema` 登记为 `Map` 类型（先例：`input.punct.custom_mappings`）。
- `wind_setting_assets` 对账：mockdata / 设置清单需同步补键（邻仓 wind-setting），否则 `crates/wind-rpc` 对账测试红。
- 设置 UI（键 → 策略的小编辑器，先例 custom_mappings 对话框）**另案**；本轮配置文件 + 文档配方即可用。

## 测试计划

- coordinator：逗号覆盖 `commit_and_input` + 减号回落全局 `ignore` **同方案分叉**；覆盖档下翻过页仍吞键；覆盖 `ignore` 显式压制全局 `commit_and_input`（用户能关，正是本条的出发点）。
- config：registry 登记、键名解析（含不认识键名 WARN + 丢弃）、非法值 WARN。
- `wind_setting_assets`：补 mockdata 后全绿。

## 回退

清空 `page_key_overrides`（或整行删除）即回到全局档；全局档出厂 `ignore`，行为与 0.127.0 逐位一致。
