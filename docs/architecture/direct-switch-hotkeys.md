# DirectSwitchHotkeys（activate_ime 热键）

`activate_ime` 热键靠 Windows TSF 的「直接切换热键」表实现，代码在
`wind_input/crates/wind-coordinator/src/direct_switch.rs`。本文记录这张表的宿主行为。

## 机制

`HKCU\Software\Microsoft\CTF\DirectSwitchHotkeys` 下每个子键是一条热键。ctfmon 监听这张表，
按下热键时由系统把当前前台应用的输入法切到指定 TIP（per-app，不波及整个会话）。
本进程不参与该热键的按键分发。

ctfmon 的注册表监听在它启动时建立：根键此前不存在（用户从未配置过系统输入法热键）时，
本次新建后写入的条目可能要注销重登后才生效。代码在新建根键时记 WARN。

## 值格式

全部 `REG_SZ`（与系统设置 UI 写入一致；DWORD 不被识别）：

| 值名 | 内容 |
|---|---|
| `CLSID` | 本输入法 TIP CLSID（随构建变体） |
| `Profile` | 本输入法 guidProfile（随构建变体） |
| `PreservedKeyId` | 条目标识 GUID，Win10 必需 |
| `LangId` | `00000804`（简体中文） |
| `Modifiers` | `0000c0XX`：0xC000 固定高位 \| TF 修饰位（Alt=0x01 / Ctrl=0x02 / Shift=0x04） |
| `VirtualKey` | `000000XX`：Windows 虚拟键码 |

子键名是 8 位十六进制 slot 编号。

## PreservedKeyId：Win10 / Win11 分水岭

- Win10（msctf.dll 10.0.19041）：msctf.dll 与 input.dll 都读该值。不写则热键完全不响应，
  系统「高级键设置」里该行显示「无」。
- Win11（msctf.dll 10.0.26100）：两个 DLL 都不含该字符串，写了只是被忽略。

故无条件写入。系统 UI 每次配置都用 `UuidCreate()` 现场生成新的 v1 GUID（同一 profile 反复
禁用/启用得到的值各不相同），说明它只是条目唯一标识、不承载语义，也不要求 TIP 经
`ITfKeystrokeMgr::PreserveKey` 注册过对应的保留键。本仓用每变体一个固定常量（变体 GUID 系列的
`x5`，`x0..x4` 已被 wind_tsf `Globals.cpp` 占用），使条目可预测、可反认自家残留。

## 写入策略

- 清理旧条目按 CLSID、Profile、PreservedKeyId 任一匹配：值缺失/损坏的自家旧条目也能清掉。
- 新条目取 ≥ `0x1000` 的最小空闲 slot；取 max+1 会让反复保存配置时编号无限增长。
- 成功与清理记 INFO，以便用户反馈「配置了但不生效」时有据可查。注册表失败只记日志，
  不影响输入法主流程。
