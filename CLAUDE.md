* 在一个功能未经过测试前, 不要主动进行 git commit
* 不要主动进行 git push 操作
* 每次修改完, 都需要编译代码, 确定程序可以正常构建
* 每次修改完 go 代码, 都需要调用 go fmt 进行格式化
* 前端代码修改完需要调用格式化
* 不需要提醒我输入法的卸载需要关闭已打开的应用或注销系统, 我们有合适的脚本自动处理
* 提交时不要加入 Constraint/Scope-risk  等所有 AI 附加信息
* INFO 级别日志不得记录用户敏感信息（输入内容、候选词文本、用户词库条目、编码等），仅记录元数据（长度、数量、方案 ID 等）；DEBUG 级别可以记录完整信息用于问题排查
* 修改某目录的对外接口、导出常量或文件结构时，需同步更新该目录的 AGENTS.md（写作模板见 docs/AGENTS-TEMPLATE.md，全局枚举约束见 docs/design/enum-constraint.md）；提交前可运行 scripts/lint_agents_md.ps1 检查 AGENTS.md 引用是否悬空

