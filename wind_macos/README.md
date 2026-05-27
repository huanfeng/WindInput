# WindInput macOS

WindInput 输入法的 macOS 端工程. 当前为 PR-A M1 骨架.

## 快速开始

```bash
# 1. 单测 (协议帧 roundtrip)
swift test

# 2. 启动 Go 服务 (另一终端)
cd ../wind_input && go run ./cmd/service

# 3. Smoke: 连 bridge.sock 发 KeyEvent + 订阅 push 10 秒
swift run wind-smoke
```

文档:

- `AGENTS.md` — 目录结构 / 协议同步铁律
- `../docs/design/macos-imkit-plan.md` — PR-A 全里程碑计划
- `../docs/wire-protocol-reference.md` — 协议字段速查
- `../docs/macos-build.md` — Go 服务端构建与调试
