// Package uicmd 定义"UI 命令/事件"的平台无关数据模型。
//
// # 设计目的
//
// 历史上 internal/ui 包同时承担两件事:
//  1. 命令模型 (UICommand + cmdCh 异步队列)
//  2. Windows 原生渲染 (gg/DirectWrite/GDI + LayeredWindow)
//
// 这导致 coordinator 等"应当平台无关的核心"在 import 路径上间接依赖 Win32 API。
// uicmd 包把"命令模型"从 internal/ui 抽出来, 形成 Win/macOS 共享的命令字典:
//
//	coordinator ──> uicmd.Command ──> ui.Manager.cmdCh ──> Win 端本地渲染
//	                                  bridge forwarder  ──> macOS IMKit .app
//
// # 命令 vs 事件
//
//   - Command (下行): Go 服务 → 渲染后端 (Win 上 ui 包消费, macOS 上 IMKit 消费)
//   - Event   (上行): 渲染后端 → Go 服务 (鼠标点选、菜单点击、快捷键触发等)
//
// 二者均通过二进制协议序列化, codec 与 internal/ipc 风格一致 (LittleEndian + uint32 长度前缀)。
//
// # cmd id 段位分配
//
//   - 0x06xx: 下行命令 (Command)
//   - 0x07xx: 上行事件 (Event)
//
// 不与 internal/ipc 现有 cmd id (0x01xx ~ 0x05xx, 0x0Fxx) 冲突。
//
// # 序列化约定
//
// 每个 payload 自定义二进制布局, 字符串/字节切片统一用 uint32 长度前缀。
// 复杂枚举类型 (CandidateLayout, ThemeStyle 等) 序列化为对应 string 字面值,
// 在反序列化端做受限解析。
package uicmd
