# 配对插入：宿主丢字符与按键注入提交

本文记录「配对符号（括号/引号成对上屏）在个别宿主里静默丢失」的排查结论与最终方案。
2026-11 一轮靶机实测里，在 TSF 侧试了五种写法全部无效，最终只有按键注入有效——
这条路绕了很久，记下来是为了下次不再重走。

## 一、症状

AutoCAD（`acad.exe`）表格在位编辑器里：

| 输入 | 结果 |
|---|---|
| 半角配对 `(` `)` `[` `]`（开自动配对） | **不出现** |
| 全角（）（中文模式配对） | 正常 |
| 关闭自动配对后单发 `(` `)` `[` | 正常 |
| 其它非配对字符 | 正常 |

DLL 日志全程显示成功（`InsertTextAtSelection` 返回 S_OK、事后读回宿主 store 也含 `()`），
**屏幕上却没有字符**——宿主 store 与显示脱节，谎报成功。

另有一例同族症状：cc-remote-desktop（Chromium 系 WebView2/Tauri）输入框里直写后约 700ms
被宿主回滚（日志有回滚铁证）。该宿主未在按键注入方案下复测。

## 二、关键误导：关配对时单发 `(` 正常

直觉上「同一个字符，关配对能显示，开配对不能」像是配对流程的问题（内容、两字符、光标回移）。
实际上**关配对时 `(` 根本没经 TSF 上屏**：DLL 没有吃这个键，放行给宿主自己处理。
所以它不是「TSF 上屏 ASCII 能成功」的证据。全角（）能显示，是因为宿主只丢弃 ASCII。

## 三、试过且无效的 TSF 写法

靶机上逐个实测，宿主读回都含字符、屏幕全无：

| 写法 | 结论 |
|---|---|
| 直写两字符（`InsertTextAtSelection` 一次插 `()`） | 无 |
| 同会话开组合 → `SetText` → `EndComposition` → `SetSelection` | 无 |
| 同会话逐字符 `InsertTextAtSelection` | 无 |
| 每字符一个独立同步会话 | 无 |
| 只插 `(`（与单发同内容、无光标回移） | 无 |
| 直接调用现成 `CommitText`（与单发 ASCII 同一函数） | 无 |

最后两条是决定性的：内容、字符数、光标回移都排除后，问题仍在，说明**不是会话写法的问题，
而是 acad 对 TSF 注入的 ASCII 一律不认**。

## 四、有效方案：Unicode 按键注入

`SendInput` + `KEYEVENTF_UNICODE`（`VK_PACKET`）产生真实 `WM_CHAR`，宿主能收到。

- `CTextService::InjectUnicodeText`：逐字符注入，注入前 `MarkSyntheticKey(VK_PACKET)`，
  避免被自己的 `OnTestKeyDown` 当真实按键二次处理。
- 光标回移走既有的 `_SimulatePairKey(VK_LEFT, n)`。**不能自己直接注入 VK_LEFT**：物理 Shift 还按着
  时（`(` 要 Shift），注入的 VK_LEFT 变成 Shift+Left，右括号被选中；`_SimulatePairKey` 在修饰键
  按住时会延后到松开再发。
- **被吃下的右符号同理**：`)` 在未开右符号跳出（`jump_out_on_right_symbol`，默认只有 Tab 跳出）
  时是普通标点提交，走 `CommitText` 的 TSF 路径照样被丢。开关开着时，空闲态（非组字、非
  `replacingHeld`）的纯 ASCII `CommitText` 响应也走注入。
- 失败回退老路径（`CommitText` + `_SimulatePairKey`）。

## 五、开关与分层

- 全局 `input.auto_pair.commit_via_composition`（出厂**关**，键名沿用最初的组合语义设计，语义已是
  「按键注入」），compat `pair_via_comp` 三态按应用覆盖。靶机 `compat.toml` 里只有 `acad.exe` 开。
- 服务端经 `CONFIG_KEY_PAIR_VIA_COMP` 按客户端现算推送（`push_pair_via_comp_config`），
  不走焦点槽——后台进程会收到错误值。
- 出厂不全局翻转：TSF 直写在多数宿主已工作多年。

## 六、验证状态

| 宿主 | 状态 |
|---|---|
| AutoCAD 表格在位编辑器 | 开关开：`()` `[]` 与右符号均正常，光标落在括号中间，Shift 不再选中 |
| WPS、Chrome | 用户验证无异常 |
| cc-remote-desktop（WebView2） | 原始回滚症状未在按键注入方案下复测 |

## 七、排查方法（日志判据）

- DLL 日志 `wind_tsf.<进程>.<pid>.log`：`InjectUnicodeText: sent=N of N`、
  `Auto-pair: deferred vk=0x25 x1 (modifiers held)` / `executing deferred ...`。
- 配对未生效时先看服务端日志 `Compat rule for process=... matched=` 与 DLL 的
  `Pair via composition updated: enabled=`，确认开关真的下发到了该进程。
- **手工改靶机 `compat.toml` 必须重启服务**（`wind_input_dev.exe restart`）：规则表只在启动与
  `reload_app_compat` 时加载，没有文件监听，漏掉是**静默**的（`matched=false`）。
- 目录大小显示 0 是假象，用 `Get-Content`/`Select-String` 读日志。
- 改 DLL 后宿主进程要重启才加载新版。

## 八、推送路径（直通 `ime.pair`）必须挪进按键上下文

`ime.pair` 词条的上屏经 `WM_PAIR_COMMIT` 窗口消息到达（`CKeyEventSink::HandlePairCommitPush`），
不在 `OnKeyDown` 调用栈里。在这里当场 `CommitText` 再注入 `VK_LEFT`，WPS 文字里文本位置正确，但
随后的左移会把光标打到文档最开头：在宿主输入处理链路之外改文档，宿主自己的插入点没跟着更新，注入的
方向键却走它的常规编辑路径。对照：直接敲「（」（整套动作在 `OnKeyDown` 里）、或关掉 auto_pair 让
词条退化为纯上屏（走 `WM_COMMIT_TEXT` 的合成提交键）都正常。

所以提交与左移一并排进合成提交键（`QueueAsyncCommitViaSyntheticKey`），在触发键的 `OnKeyDown` 里
执行。合成键注入失败时才退回非按键上下文直接提交，此时左移可能抢在文本落定之前，是已知残留竞态。

配对深度按「这一条是不是配对上屏」记，不按 `moveLeft > 0`：`ime.pair(..., jump="0")` 合法，core 侧
照样压一层配对栈，按格数记会漏掉，depth 停在 0，中文模式下 Enter 不再转发，表现为「Tab 跳得出、
Enter 毫无反应」。丢弃待提交队列里的配对条目同样会留下 DLL < core 的失配，这个方向没有自愈路径，
所以 `_DropPendingAsyncCommits` 必须留 WARN。
