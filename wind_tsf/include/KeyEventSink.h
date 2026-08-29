#pragma once

#include "Globals.h"
#include "IPCClient.h"
#include <string>
#include <cstdint>
#include <deque>
#include <map>
#include <set>
#include <vector>
#include <utility>

class CTextService;

// 英文自动配对的**查表**（不再持有栈）。
//
// 配对状态的唯一真相源是协调器的 `pair_tracker`，四条建立路径（中文标点 / 英文全角 /
// 英半自定义 / 英半普通）全部入那一个栈；DLL 这边只回答「这个键该不该吃下转发」，
// 并用 `_pairPendingDepth` 作吃键闸门。
//
// 此前这里另有一个栈，与协调器的栈互相看不见，于是「中文里打的配对切到英文跳不出、
// 反之亦然」——那是三处记账、三个互不相认的判定入口造成的，删栈即消除该类根因。
class PairEngine {
public:
    void SetEnabled(bool enabled) { _enabled = enabled; }
    bool IsEnabled() const { return _enabled; }

    void SetPairs(const std::vector<std::pair<wchar_t, wchar_t>>& pairs) {
        _pairMap.clear();
        _rightSet.clear();
        for (auto& p : pairs) {
            _pairMap[p.first] = p.second;
            _rightSet.insert(p.second);
        }
    }

    bool IsLeft(wchar_t ch) const { return _pairMap.count(ch) > 0; }
    bool IsRight(wchar_t ch) const { return _rightSet.count(ch) > 0; }
    wchar_t GetRight(wchar_t left) const {
        auto it = _pairMap.find(left);
        return it != _pairMap.end() ? it->second : 0;
    }

    // 吃键判据：开关已开 且 该字符在生效配对表内。**吃键面不得超出此判据**——
    // 协调器侧 `handle_english_custom_punct` 用同源判据出字，漂移即「吃了再吐」丢键。
    bool ShouldEat(wchar_t ch) const { return _enabled && (IsLeft(ch) || IsRight(ch)); }

private:
    std::map<wchar_t, wchar_t> _pairMap;
    std::set<wchar_t> _rightSet;
    bool _enabled = false;
};

class CKeyEventSink : public ITfKeyEventSink,
                      public ITfKeyTraceEventSink
{
public:
    CKeyEventSink(CTextService* pTextService);
    ~CKeyEventSink();

    // IUnknown
    STDMETHODIMP QueryInterface(REFIID riid, void** ppvObj);
    STDMETHODIMP_(ULONG) AddRef();
    STDMETHODIMP_(ULONG) Release();

    // ITfKeyEventSink
    STDMETHODIMP OnSetFocus(BOOL fForeground);
    STDMETHODIMP OnTestKeyDown(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten);
    STDMETHODIMP OnKeyDown(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten);
    STDMETHODIMP OnTestKeyUp(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten);
    STDMETHODIMP OnKeyUp(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten);
    STDMETHODIMP OnPreservedKey(ITfContext* pContext, REFGUID rguid, BOOL* pfEaten);

    // ITfKeyTraceEventSink
    STDMETHODIMP OnKeyTraceDown(WPARAM wParam, LPARAM lParam);
    STDMETHODIMP OnKeyTraceUp(WPARAM wParam, LPARAM lParam);

    // Initialize/Uninitialize
    BOOL Initialize();
    void Uninitialize();

    // Reset composing state (called when focus is lost or input field changes)
    // 注意: _lastPassthroughDigit 不在此清零。它是跨 IME 会话的上下文信号，
    // 用于 Excel/WPS cell-select(按数字直通) → cell-edit(按标点) 这种焦点切换
    // 场景的数字后智能标点判断。残留由按键事件路径（_SendKeyToService 非智能
    // 标点目标键清零）和光标 Y 跨行检测兜底，不应在 IME 会话状态重置时一起清。
    // keepPairState=TRUE：保留自动配对状态（`_englishPairEngine` 与 `_pairPendingDepth`）。
    // 配对状态的前提是「光标紧贴一个已插入的右符号」。中英模式切换与上屏都不移动光标、
    // 也不消除那个右符号，前提仍成立，清掉只会让 Tab/Enter 跳不出去（切走再切回、或
    // 配对里打完字再跳出）。真正让前提失效的是焦点/文档切换与组合被清，那些路径按默认值全清。
    void ResetComposingState(BOOL keepPairState = FALSE) {
        _isComposing = FALSE; _hasCandidates = FALSE; _needsCompositionResync = FALSE; _resyncDeadline = 0; _resyncFailStreak = 0; _skipKeyCount = 0; _pendingPairAction = {};
        if (!keepPairState) { _pairPendingDepth = 0; _pairLastActivityTick = 0; }
    }

    // 配对状态保活：所有按键都应调用（含英文模式的普通字母——协调器在英文模式下收不到
    // 它们，只有这里能看全）。栈空（depth==0）时不记，避免空状态攒出活动时间。
    void TouchPairState() { if (_pairPendingDepth > 0) _pairLastActivityTick = GetTickCount64(); }

    // 直通 ime.pair 的推送落点：上屏 text 后左移 moveLeft 格，并记一层待跳出深度。
    //
    // 与 OnKeyDown 里 InsertTextWithCursor 响应分支**同源**——那条走按键响应，这条走 push
    // 通道（命令动作在协调器的独立线程执行，按键响应早已返回）。深度这一层不能省：它是
    // 中文模式下 Enter 能否被转发给协调器的闸门，漏掉的症状是「Tab 跳得出、Enter 毫无反应」。
    void HandlePairCommitPush(const std::wstring& text, uint32_t moveLeft);

    // 配对状态是否已陈旧。TTL=0 表示不过期。
    //
    // **判据必须留在 DLL 侧**：吃键闸门在这里，若只有协调器过期而这边照吃跳出键，
    // 协调器回 PassThrough 已太晚——OnTestKeyDown 已经吃了，再吐成 FALSE 就是「吃了再吐」，
    // 不补发 WM_KEYDOWN 的宿主（EverEdit 等）直接丢键。
    BOOL IsPairStateStale() const {
        if (_pairStateTtlMs == 0 || _pairPendingDepth <= 0 || _pairLastActivityTick == 0)
            return FALSE;
        return (GetTickCount64() - _pairLastActivityTick) >= _pairStateTtlMs;
    }

    // Flush pending English pass-through stats before focus/mode teardown.
    void FlushEnglishStats();

    // Status queries
    BOOL IsStatsTrackingEnglish() const { return _statsEnabled && _statsTrackEnglish; }
    BOOL IsEnglishAutoPairEnabled() const { return _englishPairEngine.IsEnabled(); }

    // Handle config sync from Go service (called from async reader thread)
    void OnSyncConfig(const std::string& key, const std::vector<uint8_t>& value);

    // Called when composition is unexpectedly terminated by the application
    // This resets state and notifies Go service to clear input buffer
    void OnCompositionUnexpectedlyTerminated();

    // 从 RegisterHotKey/WM_HOTKEY 路径派发一个键事件给 Go 服务。
    // 用于 Pin/Delete 候选热键被系统级 RegisterHotKey 拦截后，转发给我们的常规处理流程。
    // vk: 虚拟键码（Win32 VK_*）；mods: 内部 KEYMOD_* 修饰位（不是 TSF MOD_*）。
    BOOL DispatchHotkey(uint32_t vk, uint32_t mods);

    // 供 CTextService 的 SendInput 兜底路径（CommitText/InsertText/ReplacePrecedingChars）
    // 调用：把即将注入的按键标记为"自生成"，OnTestKeyDown/OnTestKeyUp 见到后直接放行，
    // 不会被本 IME 自己的按键逻辑当成真实用户按键二次处理。用于 TSF EditSession 兜底
    // 到 SendInput 的宿主（部分终端模拟器/微信/纯文本编辑器）——否则注入的退格/字符键
    // 会被自己的钩子截获重新处理，导致文字重复上屏（同 _SimulatePairKey 用的机制）。
    void MarkSyntheticKey(WORD vk) { _PushSkipKey(vk); }

    // 供 CTextService::CommitTextViaSyntheticKey 调用：把待提交文本缓冲起来，自注入一个
    // 保留 VK（VK_ASYNC_COMMIT_TRIGGER），让真正的 CommitText 挪到 OnKeyDown 里执行——
    // 那才是 TSF 认可的"按键处理期间"，TF_ES_SYNC 合法，宿主自身的输入时处理链路
    // （AutoCorrect……）才会被触发，且规避了 Word 对非按键上下文同步会话的拒绝
    // （nonKeyContext 的已知问题面）。
    //
    // 内嵌 `\n` 的文本无需在这里做任何特殊处理：它由宿主按「输入法上屏」语义自行
    // 规范化成分段。Word/WPS 里换行不生效的那类现象，根因在**上游有没有活跃 composition**
    // （无 composition 时 TSF 只能退到裸插入，换行就只是普通字符），已在协调器侧
    // 让纯文本 `$CC` 命令走回同步上屏路径解决，不属于本层的职责。
    //
    // 不能复用 `_skipKeys`：那套是"识别出自生成键→直接放行不处理"，用于让宿主看到一个
    // "干净"的按键；这里要的是相反方向——"识别出触发键→吃掉→转入我们自己的同步提交"，
    // 必须是独立的判据与独立的队列，见 `_TryConsumeAsyncCommitTrigger`。
    //
    // 返回 FALSE＝合成按键注入失败（SendInput 出错，极罕见），调用方应回退到旧的
    // `CommitText(text, TRUE)` 直接异步提交，保证至少不丢字。
    BOOL QueueAsyncCommitViaSyntheticKey(const std::wstring& text, BOOL replacingHeld);

private:
    static constexpr uint32_t ENGLISH_STATS_REPORT_COUNT = 5;
    static constexpr ULONGLONG ENGLISH_STATS_REPORT_INTERVAL_MS = 5000;

    LONG _refCount;
    CTextService* _pTextService;
    DWORD _dwKeySinkCookie;
    DWORD _dwKeyTraceSinkCookie;
    bool _statsEnabled = true;
    bool _statsTrackEnglish = true;

    // State
    // 本次 OnKeyDown 的响应是 ClearCompositionThenPassThrough：组合已在响应处理里收掉，
    // 还欠宿主一次按键重放。**只在 OnKeyDown 里置位并当场消费**——`_HandleServiceResponse`
    // 另有三个非按键调用点（同步握手等），它们拿不到 wParam，标志若残留会在下一次按键上
    // 误重放。故 OnKeyDown 在调用响应处理**之前**先清零。
    BOOL _pendingReplayToHost = FALSE;
    BOOL _isComposing;
    BOOL _hasCandidates;         // True if there are candidates to select
    // 配对跳出键（VK 码集合，由 core 经 CONFIG_KEY_JUMP_OUT_KEYS 推送）。英文模式配对直接
    // 据此跳出；中文模式仅用于「有待跳出配对」时放行 Enter 等被会话门控的键转发给协调器。
    std::set<UINT> _jumpOutKeys;
    // 待跳出配对深度：收到 InsertTextWithCursor(配对插入)时 +1，MoveCursorRight(跳出)时 -1，
    // 会话/焦点复位归零（见 ResetComposingState）。中文模式据此判断 Enter 等键是否该转发。
    int _pairPendingDepth = 0;
    // 配对状态最后一次活动时刻（GetTickCount64，0=无）与时效阈值（毫秒，0=不过期）。
    // 阈值由协调器经 CONFIG_KEY_PAIR_STATE_TTL 推送，见 IsPairStateStale。
    ULONGLONG _pairLastActivityTick = 0;
    ULONGLONG _pairStateTtlMs = 0;
    bool _IsJumpOutKey(UINT vk) const { return _jumpOutKeys.count(vk) > 0; }

    // 软键盘总闸的**唯一判据**。`OnTestKeyDown`（吃）与 `OnKeyDown`（转发）都调它。
    //
    // ★★★ 两处必须用同一个函数，不许各写一份 switch：那边吃了、这边不发，键就凭空
    // 消失（core 侧一条日志都没有）。同一文件里 pair_jumpout / english_custom_punct /
    // english_autopair 三条注释写的都是这句话，软键盘还是栽了第四次——物理 Esc
    // 关不掉面板，查了两轮。收成一个函数，让「两边一致」由编译器而不是纪律来保证。
    //
    // 键盘面（send_keys）只接管 Esc 与翻页：字母/数字/标点落回常规判定链，
    // 与没开面板时完全一致，于是这一面上能正常组码打中文。
    bool _IsSoftKeyboardEatenKey(WPARAM vk, uint32_t modifiers) const;
    // 输入右符号本身是否跳出（配置 jump_out_keys 里的 `right_symbol` 特殊值）。
    // **本 DLL 已不再消费它**——右符号跳出统一由协调器裁决（配对栈在那边，需要比对具体是
    // 哪一对）。但仍须解析：它占 payload 首字节，不读就会算错后面 VK 列表的偏移。
    bool _jumpOutOnRightSymbol = false;
    // 「英半列有自定义标点映射」的源字符集合（core 经 CONFIG_KEY_CUSTOM_EN_PUNCT 推送）。
    // 英文模式（非全角）本 DLL 默认透传标点键 → core 收不到 → 英半列打不到；据此**精确**吃下
    // 集合内的键转发。空集合（默认）= 与历史行为完全一致，不配的键一律不受影响。
    std::set<wchar_t> _customEnPunctChars;
    // 该键是否属于「英半自定义标点」：英文模式吃键判据，同时也是英文本地配对的让位判据
    // （吃下的键要交给 core 出字，本地配对若抢先 CommitText 就把转发吞了）。
    // 判据必须与 core 的 `wind_punct::custom_english_punct_chars` 同源，漂移即「吃了再吐」丢键。
    BOOL _IsCustomEnglishPunctKey(WPARAM vk, uint32_t modifiers) const;
    // IPC 失败后置位：本地 composition 已强制复位，但 Go 侧可能仍持有活跃会话状态。
    // 下一次按键前提下视作"有会话"，让 ENTER/ESC 也能发给 Go 走重握手；
    // 任何一次成功 ReceiveResponse 之后清旗，状态由响应处理路径自然重建。
    BOOL _needsCompositionResync;
    // resync 自愈窗口：deadline 到期或连续失败超限后自动放弃，避免 Go/IPC 长时间不可用
    // 时把 ENTER/ESC/Ctrl+Alt 等键永久吃掉。失败 streak 在响应成功后清零。
    DWORD _resyncDeadline;        // GetTickCount() 时间戳，0 表示无 deadline
    int   _resyncFailStreak;      // 连续 IPC 失败次数，超过 RESYNC_MAX_RETRIES 强制降级 passthrough
    static constexpr DWORD RESYNC_WINDOW_MS = 3000;
    static constexpr int   RESYNC_MAX_RETRIES = 3;
    BOOL _IsResyncActive();       // 读旗+过期检查；过期会自动清旗

    // 「当前是否有活跃输入会话」的**唯一判据**——决定各类键归本输入法还是透传宿主。
    // 曾以三份等价表达式散落在 OnTestKeyDown / OnKeyDown / session 热键三处，direct_commit
    // 顶码新增 defer 真空期时只有部分被更新，导致该窗口内空格/退格直落宿主（见
    // project_top_commit_mode）。任何新的「算不算有会话」的状态，只加到这里。
    BOOL _HasInputSession();

    // 智能符号 hold 预览态下「我们无法代劳、必须交给宿主」的键：走「吃键 → 收口 → 重放」，
    // 见 OnKeyDown 里的调用点注释。回车已真机验证，其余同族键机制相同。
    //
    // 判据是「会被吃键门控捕获、且服务端在空缓冲下回 PassThrough」的键，**不是**按键语义。
    // 全角态下空格/数字改走 CommitText（字符由我们输出），那时 pfEaten 为真，重放分支的
    // `!pfEaten` 守卫直接挡住——故此处无须、也不该按全半角区分，列全即可。
    // 数字键必须在列：hold 期间无候选，它被 session_select_or_page 吃掉后服务端回
    // PassThrough，漏列就退回「吃了再吐」，EverEdit 这类宿主下数字会丢。
    //
    // 仍未覆盖 Ctrl/Alt 组合（Ctrl+S 等宿主快捷键）。此处曾断言它们「走 isCtrlAltCleanup、
    // 响应为 Ack、会被吃掉」——**实测证伪**：hold 期间缓冲为空，服务端对 Ctrl+S / Ctrl+C
    // 一律回 PassThrough，故 pfEaten 为假、`isCtrlAltCleanup && *pfEaten` 那段压根不执行，
    // 符号也已在 PassThrough 分支的 FlushHoldCompositionIfActive 里收口。
    // 真实症状与本函数治的是同一个：OnTestKeyDown 吃了、OnKeyDown 吐成 FALSE 的「吃了再吐」
    // ——记事本/Chromium 补发所以正常，EverEdit 这类严格宿主丢键。
    // 修法也同构：把重放条件放宽为 `_IsHoldReplayKey(vk) || (modifiers & (KEYMOD_CTRL|KEYMOD_ALT))`
    // 即可（重放时物理修饰键仍按着，宿主 GetKeyState 能还原 Ctrl+S 语义）。暂未实施——触及面
    // 小：只在「hold 的 500ms 窗口内」+「严格 TSF 宿主」同时成立时才丢那一次快捷键，符号本身
    // 不丢。完整背景、实测探测方法与「普通输入会话下同类翻转尚未验证」的提醒见
    // docs/architecture/smart-symbol-compat-notes.md 的「HoldComposition 方案」一节。
    BOOL _IsHoldReplayKey(WPARAM wParam) const
    {
        if (wParam >= '0' && wParam <= '9')                 return TRUE; // 主键盘数字
        if (wParam >= VK_NUMPAD0 && wParam <= VK_NUMPAD9)   return TRUE; // 小键盘数字
        switch (wParam)
        {
        case VK_RETURN: case VK_SPACE:  case VK_BACK: case VK_DELETE:
        case VK_ESCAPE: case VK_TAB:
        case VK_LEFT:   case VK_RIGHT:  case VK_UP:   case VK_DOWN:
        case VK_HOME:   case VK_END:    case VK_PRIOR: case VK_NEXT:
            return TRUE;
        default:
            return FALSE;
        }
    }
    // 把一个已被我们吃掉的键原样重放给宿主（skip 表标记，避免自己的钩子二次处理）。
    void _ReplayKeyToHost(WORD vk);

    // ── 数字后智能标点的备用 prevChar 通路 ──────────────────────────────────────
    //
    // prevChar 主路径是 TSF 现读文档（CTextService::ConsumeCachedPrevChar）；EverEdit
    // 这类宿主读不回文档、恒为 0，只能靠 _lastPassthroughDigit 记住「刚打出去的数字」。
    // 服务端只认 prevChar 的**值**（wind-punct 判 0x30..=0x39），它不知道数字是怎么打的，
    // 所以「哪些键**产出数字**」这个判据完全落在本文件——那些键透传出去了，服务端看不到。
    //
    // ⛔ 反过来，「哪些**标点键**该带上这个值」不是本文件的事，别再往消费点写符号白名单。
    // 那等于把服务端的 `input.punct.smart_list` 抄一份过来，两处必然漂移（历史教训：消费点
    // 硬编码 `.`/`,` 时，出厂默认列表里的 `:` 从设计上就拿不到备用值）。消费点如实上报所有
    // 标点键，要不要用由 wind-punct 的 `is_smart_punct_after_digit` 决定。
    //
    // ★ 主键盘与小键盘必须都认。判据只写 '0'..'9'（VK 0x30-0x39）时小键盘数字
    // （VK_NUMPAD0-9 = 0x60-0x69）不但记不上，还会落进记录点的 else 分支把已记的值清零
    // ——症状是「小键盘打的数字后面标点仍出中文」，且只在读不回文档的宿主暴露（能读的
    // 宿主主路径兜住了，看起来像是随机时灵时不灵）。同族的 _IsHoldReplayKey 一直是两种
    // 都列的，这里是遗漏。NumLock 关闭时小键盘发的是 VK_END/VK_INSERT 等，语义本就不是
    // 数字，天然不命中；NumLock 开着按 Shift+小键盘时 Windows 临时取消 NumLock，届时
    // vk 已是方向键，同样到不了这里。
    //
    // 返回 0 表示「这一键不产出数字」，记录点据此清零——把「没记到」与「明确不是数字」
    // 统一成一个出口，避免两处判据各写一份而漂移。
    static WCHAR _DigitCharFromVk(WPARAM vk, uint32_t modifiers)
    {
        // Shift+主键盘数字产出的是符号（!@#…）而非数字，不能当数字记。
        if (vk >= '0' && vk <= '9')
            return (modifiers & KEYMOD_SHIFT) ? 0 : (WCHAR)vk;
        if (vk >= VK_NUMPAD0 && vk <= VK_NUMPAD9)
            return (WCHAR)(L'0' + (vk - VK_NUMPAD0));
        return 0;
    }

    // 经引擎上屏的文本也要更新备用 prevChar。全角数字、小键盘 direct「顶屏候选再追加
    // 数字」、候选文本本身带数字这些路径，pfEaten 为真且响应不是 PassThrough，两个按键侧
    // 记录点（OnTestKeyDown 透传臂 / OnKeyDown 补设）都不覆盖——能读文档的宿主靠主路径
    // 兜住，读不回的宿主里这些场景现在是全丢的。
    //
    // 记的是「已落进文档、光标紧邻的那个字符」，与 prevChar 语义严格一致：末位是 ASCII
    // 数字则记，否则清零（上屏汉字/标点后不该再继承数字状态）。全角数字（U+FF10-FF19）
    // 刻意不记——服务端只认 ASCII 0x30-0x39，记了也不会命中，徒增误判面。
    // 空文本不动状态：没有东西写进文档，光标前是什么并没有改变。
    void _TrackCommittedTextForSmartPunct(const std::wstring& text)
    {
        if (text.empty())
            return;
        WCHAR last = text.back();
        _lastPassthroughDigit = (last >= L'0' && last <= L'9') ? last : 0;
    }

    // ⛔ 这里曾有 CapsLock 的「回敲复原」机制（_RestoreCapsLockToggle / _capsSessionEaten /
    // 自注入放行窗口），2026-08-11 全部移除——TSF 压不住 CapsLock 的锁定态翻转，而事后
    // 回敲在快速连按下有竞态且会触发厂商 OSD 弹窗。CapsLock 的会话态绑定改由服务进程的
    // WH_KEYBOARD_LL 钩子在状态更新前拦截，见 wind-keys 的 capslock_hook 模块。
    // **不要在 TSF 侧重新实现它。**

    WCHAR _lastPassthroughDigit; // Last digit key that passed through (for smart punct fallback in apps where TSF can't read text)
    uint32_t _pendingKeyUpKey;   // Key code of pending KeyUp toggle key
    uint32_t _pendingKeyUpModifiers; // Modifiers when KeyDown was pressed
    DWORD    _pendingKeyDownTime;    // GetTickCount() when toggle key was pressed down
    // OnTestKeyDown 是否刚吃下一个 Ctrl+Space。它是「我们独占了这个键」的凭据：
    // TSF 只在 pfEaten=TRUE 后才调 OnKeyDown，吃下就意味着 msctf 不会再拿它当 IME
    // 热键、OPENCLOSE compartment 不会被翻，因此按键侧兜底切换与 compartment 路径
    // 天然互斥。QQ 那类「不调 OnTestKeyDown 却调 OnKeyDown」的宿主此标志恒为 FALSE，
    // 兜底不触发——宁可不修，也不拿双切换去赌。
    BOOL     _ctrlSpaceEatenInTest;

    // Maximum duration (ms) for a toggle key press to count as a "tap"
    // Long presses beyond this threshold will NOT trigger mode toggle
    static constexpr DWORD TOGGLE_TAP_THRESHOLD_MS = 500;

    // ========================================================================
    // Modifier key state machine (replaces GetAsyncKeyState for consistency)
    // ========================================================================
    uint32_t _modsState;         // Current modifier state (maintained by KeyDown/KeyUp)
    uint16_t _eventSeq;          // Monotonic event sequence number

    // State machine update methods
    void _UpdateModsOnKeyDown(WPARAM vk);
    void _UpdateModsOnKeyUp(WPARAM vk);
    uint32_t _GetModsSnapshot() const { return _modsState; }
    uint8_t _GetTogglesSnapshot() const;
    uint16_t _GetNextEventSeq() { return _eventSeq++; }

    // Sync state from Go response
    void _SyncStateFromResponse(uint32_t statusFlags);

    // ========================================================================
    // Barrier mechanism for async commit requests
    // ========================================================================
    struct PendingBarrier
    {
        uint16_t barrierSeq;
        std::wstring composition;  // Composition at request time
        DWORD requestTime;         // GetTickCount() at request
        bool waiting;
    };

    uint16_t _nextBarrierSeq;
    PendingBarrier _pendingCommit;

    // Barrier timeout (if Go doesn't respond, fallback handling)
    static constexpr DWORD BARRIER_TIMEOUT_MS = 500;

    BOOL _SendCommitRequest(uint16_t barrierSeq, uint16_t triggerKey, uint32_t mods, const std::string& inputBuffer);
    void _HandleCommitResult(uint16_t barrierSeq, const std::wstring& text, const std::wstring& newComp, bool modeChanged, bool chineseMode);
    void _CheckBarrierTimeout();

    // ========================================================================
    // Helper methods
    // ========================================================================
    BOOL _IsMatchingKeyUp(WPARAM wParam, uint32_t pendingKey);
    // Dispatch the pending toggle key (Shift/Ctrl) to Go service and clear _pendingKeyUpKey.
    // Called from both OnTestKeyUp and OnKeyUp; clearing in the first caller makes the second a no-op.
    // Returns TRUE if a toggle was matched (caller should set pfEaten=TRUE).
    BOOL _DispatchPendingToggleKeyUp(WPARAM wParam);

    // 记录「该切换键正等 keyup 触发切换」。OnTestKeyDown 与 OnKeyDown 都会调用：
    // 纯修饰键放行后 TSF 未必再调 OnKeyDown，只在后者记录会让切换失灵。幂等。
    void _MarkPendingToggleKey(WPARAM wParam, uint32_t modifiers);

    // 取消待切换。给命中热键白名单的组合键用：那些分支都是就地 return，够不着
    // OnTestKeyDown 下方那段统一取消，不显式调用就会「按了热键还顺带切了中英文」。
    void _CancelPendingToggle(WPARAM wParam, const wchar_t* reason);
    BOOL _SendKeyToService(uint32_t keyCode, uint32_t modifiers, uint8_t eventType);
    BOOL _HandleServiceResponse(); // Returns TRUE if key was handled, FALSE to pass through

    // Context state checking (for browser non-editable area detection)
    BOOL _IsContextReadOnly(ITfContext* pContext);

    // ========================================================================
    // Auto-pair key simulation (deferred + skip list approach)
    // ========================================================================
    void _SimulatePairKey(WORD vk);
    static bool _AreModifiersHeld();

    // Pending auto-pair action (deferred until modifiers released)
    struct PendingPairAction {
        WORD vk = 0;
        int count = 0;
        bool active = false;
    };
    PendingPairAction _pendingPairAction;

    // English auto-pair engine (handles pairing in English mode)
    PairEngine _englishPairEngine;

    // Skip list: SendInput keys generated by auto-pair / CommitText·InsertText·
    // ReplacePrecedingChars 兜底路径的，应该绕过 IME 处理。后者可能一次注入一整段
    // 文本（每字符一个 skip 条目），容量比原来只服务 auto-pair 时留宽一些。
    static constexpr int MAX_SKIP_KEYS = 64;
    WORD _skipKeys[MAX_SKIP_KEYS] = {};
    int _skipKeyCount = 0;
    void _PushSkipKey(WORD vk);
    BOOL _TryConsumeSkipKey(WPARAM wParam);

    // ========================================================================
    // Async commit via synthetic key (nonKeyContext → key-context 提交)
    // ========================================================================
    // Win32 虚拟键码表中的保留/未分配值（见 MSDN Virtual-Key Codes），真实键盘永远不会
    // 产生，专供本类内部识别、不会与任何真实按键或热键组合冲突。
    static constexpr WORD VK_ASYNC_COMMIT_TRIGGER = 0xE8;

    // 一个待执行的提交：文本原样保留（含内嵌换行），到 OnKeyDown 里一次性同步提交。
    struct PendingAsyncCommit {
        std::wstring text;
        BOOL replacingHeld = FALSE;
    };
    // 队列而非单槽：鼠标可能连续快速点选，多个 push 可能在上一个触发键送达前叠加。
    // 上限只是防御性上界，避免目标窗口已经失焦、触发键永远送不达时无限增长。
    static constexpr size_t MAX_PENDING_ASYNC_COMMITS = 16;
    std::deque<PendingAsyncCommit> _pendingAsyncCommits;

    // OnTestKeyDown/OnKeyDown 最前面调用：命中触发键就弹出队首、返回 TRUE。
    BOOL _TryConsumeAsyncCommitTrigger(WPARAM wParam, PendingAsyncCommit& out);

    // 自注入一次触发键（down+up）。返回 FALSE＝SendInput 失败。
    BOOL _SendAsyncCommitTriggerKey();

    // English mode input stats counter
    struct EnglishStatsCounter {
        uint32_t chars = 0;   // a-z, A-Z
        uint32_t digits = 0;  // 0-9
        uint32_t puncts = 0;  // punctuation/symbols
        uint32_t spaces = 0;  // spaces
        ULONGLONG lastReportTick = 0;

        uint32_t Total() const { return chars + digits + puncts + spaces; }

        void StartIfIdle() {
            if (Total() == 0 || lastReportTick == 0)
                lastReportTick = GetTickCount64();
        }

        uint32_t ElapsedMs() const {
            if (lastReportTick == 0)
                return 0;
            ULONGLONG elapsed = GetTickCount64() - lastReportTick;
            return elapsed > UINT32_MAX ? UINT32_MAX : (uint32_t)elapsed;
        }

        void RecordChar(char ch) {
            StartIfIdle();
            if ((ch >= 'a' && ch <= 'z') || (ch >= 'A' && ch <= 'Z')) chars++;
            else if (ch >= '0' && ch <= '9') digits++;
            else if (ch == ' ') spaces++;
            else if (ch >= 0x21 && ch <= 0x7E) puncts++;
        }

        bool ShouldReport() const {
            uint32_t total = Total();
            return total >= ENGLISH_STATS_REPORT_COUNT ||
                   (total > 0 && lastReportTick != 0 && GetTickCount64() - lastReportTick >= ENGLISH_STATS_REPORT_INTERVAL_MS);
        }

        void Reset() {
            chars = digits = puncts = spaces = 0;
            lastReportTick = 0;
        }
    };
    EnglishStatsCounter _englishStats;
    void _RecordEnglishKeyTrace(WPARAM wParam, uint32_t modifiers);
    void _ReportEnglishStats();
};
