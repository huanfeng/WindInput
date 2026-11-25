# TSF 上屏编辑会话：同步被拒、孤儿组合与 SendInput 兜底

> 涉及 `wind_tsf/src/TextService.cpp` 的 `CommitText` / `CCommitTextEditSession`。
> 智能符号 `DeleteReplace` 方案的范围替换问题见 [smart-symbol-compat-notes.md](smart-symbol-compat-notes.md)。

## 1. 同步会话会被拒

- MSDN 限定 `TF_ES_SYNC` 只在按键处理等场景下合法。Word 严格照此校验：非按键上下文（裸 `WM_TIMER`、窗口
  消息、COM 回调）请求同步会话时返回 `hrSession=TS_E_SYNCHRONOUS`（0x80040208），`DoEditSession` 根本不执行。
- 按键上下文里 `TF_ES_SYNC` 也只是「can be expected to succeed」，宿主忙时照样拒，表现为偶发。
- `TF_ES_ASYNCDONTCARE` 在能立即给锁的宿主（如 Tabby）上同步执行，Word 则延后到可授予锁时执行；异步下编辑
  会话对象由 TSF 保活至 `DoEditSession` 运行，交给它的组合随之正确收尾。

## 2. 孤儿组合会被宿主 finalize 成正文

组合未经 `EndComposition` 就被 Release（孤儿），Word / 记事本 / EverEdit 都会把组合区当前内容 finalize 成正文
（Word 还会套用自动更正的首字母大写）。若此刻组合里显示的是原码，随后的 `SendInput` 兜底又把正文追加在后面，
得到 `Sfge杜甫`、`fffb土地` 之类的结果。已知受害路径：

- 智能符号 HoldComposition 超时收口（`WM_TIMER` 回调里提交）：符号重复上屏；
- 鼠标点候选（`WM_COMMIT_TEXT`）；
- 按键上下文里偶发的同步拒锁。

故同步会话没跑过时，`CommitText` 退到异步会话，由 TSF 在可授予锁时原地 `SetText` + `EndComposition`，绝不
`SendInput`；非按键上下文直接发异步。代价是用户可能多看到原码若干毫秒，比文档里多一串编码轻。

## 3. 外层返回码不可信

部分宿主编辑其实已执行，外层 `RequestEditSession` 的 `hr` / `hrSession` 仍报失败。只信会话内部的
`GetSuccess()`：它在 `SetText` / `EndComposition` 真正执行完后才置 `TRUE`（详见 smart-symbol-compat-notes.md
第 1 节）。

## 4. 判据是「会话跑过没有」

退异步还是落 `SendInput`，判据是 `WasExecuted()`（`DoEditSession` 第一句置位，与成败无关）：

- 没跑过（宿主拒发锁）⇒ 退异步；
- 跑过但没写成（组合已收尾、`SetText` 与 `InsertTextAtSelection` 都失败）⇒ 再请求只会重复出字，落 `SendInput`。

不能用 `HasPendingComposition()` 代替：无组合的提交（空缓冲直接上屏符号、鼠标上屏）里组合指针从一开始就是空，
它恒为 FALSE，同步被拒也会被当成已执行而直接落 `SendInput`。`SendInput` 是最差的出路：受 UIPI（被拦时静默
失败）与物理修饰键污染，Tk 一类非 TSF-aware 宿主还会把注入的字符码当虚拟键码解释。

同步与异步都被拒时，context 已不可用，组合只能孤儿释放、编码泄漏无从避免，落 `SendInput` 至少不丢字；日志
`sync AND async both rejected` 出现即说明宿主侧硬故障，值得单独查。
