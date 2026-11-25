#pragma once

#include "Globals.h"
#include "IPCClient.h"
#include "SkipKeyTable.h"
#include "PassthroughNote.h"
#include "InputSequenceChar.h"
#include "ObservedKeyGate.h"
#include "ToggleTapPolicy.h"
#include "CtrlRawPolicy.h"
#include "CtrlSpacePolicy.h"
#include "PairCommitPolicy.h"
#include <string>
#include <cstdint>
#include <atomic>
#include <deque>
#include <map>
#include <set>
#include <vector>
#include <utility>

class CTextService;

// 英文自动配对的查表，不持有栈。
//
// 配对状态的唯一真相源是协调器的 `pair_tracker`，四条建立路径都入那一个栈；DLL 只回答「这个键
// 该不该吃下转发」，并用 `_pairPendingDepth` 作吃键闸门。别在 DLL 侧另建栈：两个栈互相看不见，
// 中文里打的配对切到英文就跳不出。
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

    // 吃键判据：开关已开且该字符在生效配对表内。吃键面不得超出此判据：协调器侧
    // `handle_english_custom_punct` 用同源判据出字，漂移即「吃了再吐」丢键。
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
    // _lastPassthroughDigit 不在此清零：它服务于 Excel/WPS 选格按数字 → 编辑格按标点这种跨焦点
    // 的数字后智能标点。残留由按键路径消费兜底；鼠标挪光标看不见，是已知代价（见 SmartPunctPrevChar.h）。
    // keepPairState=TRUE：保留自动配对状态（`_englishPairEngine` 与 `_pairPendingDepth`）。
    // 中英切换与上屏不移动光标，「光标紧贴已插入的右符号」仍成立，清掉会让 Tab/Enter 跳不出；
    // 焦点/文档切换与组合被清才让它失效，按默认值全清。
    void ResetComposingState(BOOL keepPairState = FALSE) {
        _EndAssocBareSession();
        _isComposing = FALSE; _hasCandidates = FALSE; _needsCompositionResync = FALSE; _resyncDeadline = 0; _resyncFailStreak = 0; _skipKeys.Clear(); _pendingPairAction = {};
        _passthrough.Reset();  // 与 skip 表同处清：跨文档带一个陈旧的透传事实过去没有意义
        // seqChar 同配对状态：中英切换不清（切换后紧接着打的仍算「英文后」），焦点/文档切换清。
        if (!keepPairState) { _pairPendingDepth = 0; _pairLastActivityTick = 0; _seqChar = WindInputSeq::kUnknown; }
    }

    // 配对状态保活：所有按键都应调用（含英文模式的普通字母——协调器在英文模式下收不到
    // 它们，只有这里能看全）。栈空（depth==0）时不记，避免空状态攒出活动时间。
    void TouchPairState() { if (_pairPendingDepth > 0) _pairLastActivityTick = GetTickCount64(); }

    // 直通 ime.pair 的推送落点：上屏 text 后左移 moveLeft 格，并记一层待跳出深度
    //（经合成提交键延后到 OnKeyDown 周期执行，不在本函数返回时就已发生）。
    //
    // 与 OnKeyDown 里 InsertTextWithCursor 响应分支同源，这条走 push 通道。深度这一层不能省：
    // 它是中文模式下 Enter 能否转发给协调器的闸门，漏了 Tab 跳得出、Enter 无反应。
    //
    // 主路只排队；合成键注入失败时就地直接提交并自己补记账。调用方在 WM_PAIR_COMMIT 的
    // 窗口消息回调里，不是按键上下文。
    void HandlePairCommitPush(const std::wstring& text, uint32_t moveLeft);

    // 配对状态是否已陈旧。TTL=0 表示不过期。
    //
    // 判据必须留在 DLL 侧：吃键闸门在这里，只有协调器过期而这边照吃跳出键，就是「吃了再吐」，
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

    // Handle config sync from the service (called from async reader thread)
    void OnSyncConfig(const std::string& key, const std::vector<uint8_t>& value);

    // Called when composition is unexpectedly terminated by the application
    // This resets state and notifies the service to clear input buffer
    void OnCompositionUnexpectedlyTerminated();

    // 从 RegisterHotKey/WM_HOTKEY 路径派发一个键事件给服务端。
    // 用于 Pin/Delete 候选热键被系统级 RegisterHotKey 拦截后，转发给我们的常规处理流程。
    // vk: 虚拟键码（Win32 VK_*）；mods: 内部 KEYMOD_* 修饰位（不是 TSF MOD_*）。
    // 返回 TRUE ＝ 服务端要这个键交还宿主（由调用方注入，见实现处）。
    BOOL DispatchHotkey(uint32_t vk, uint32_t mods);
    /// 收到 WM_HOTKEY（不论后面是否分发：前台竞态丢弃、vk 解不出也算）：作废待判单击与
    /// Ctrl 如实上报的单击——这一键不经按键回调，那里的「别的键取消待判」够不着。
    void NoteHotkeyMessage();
    /// 线程失焦：Ctrl 如实上报的记账作废（松开多半送不到了）。
    void ResetCtrlRaw();
    // 失焦：吃掉过按下的功能键补报松开并清记账（见实现）。
    void ResetEatenFnKey();

    // 把一个已被吃掉的键重放给宿主（_ReplayKeyToHost 的公开入口，给 WM_HOTKEY 派发用）。
    void ReplayKeyToHost(WORD vk) { _ReplayKeyToHost(vk); }

    /// 无占位联想会话中，文档焦点反弹（WPS 点单元格）时问一句：鼠标刚按过吗？是 ⇒ 用户点了别处，
    /// 调用方应让服务端收联想（宿主没有组合，也不一定发选区变化，没有别的信号）。
    /// 只在会话期间读鼠标状态；会话开始时已丢弃一次旧的「按下过」位。TSF 线程调用。
    BOOL BareAssocMouseClicked();
    /// 「热键模式会话」镜像位的写入口。服务端是唯一权威，这里只镜像、不推断。
    ///
    /// 权威写入（level-triggered，每条状态响应都带完整值，漏一次由下一次纠正）：
    /// `CTextService::_SyncStateFromResponse`、StatePush 回调、`_HandleServiceResponse` 的
    /// `ResponseType::StatusUpdate` 分支。另有 `WM_HOTKEY` 加词 id 段的乐观置位，只为盖住
    /// `DispatchHotkey` 同步 IPC 的空窗；它过宽，但会被随后的推送纠正。
    ///
    /// DLL 侧不得加本地清位逻辑：边沿驱动漏一个事件就永久错（如软键盘开关不产候选也不发
    /// `ClearComposition`，位卡在 TRUE 后 Backspace/Enter/Escape 被吃了再吐）。
    ///
    /// 例外是无占位联想会话（`_assocBareSession`）：本函数可在 AsyncReader 线程被调，FALSE 只记
    /// 「待收」，由 TSF 线程补一次候选收起。
    void SetCandidateSessionActive(BOOL active)
    {
        if (active)
        {
            // 后到的 TRUE 撤销前面迟到的 FALSE（服务端联想活着时每个 keydown 都会重推 TRUE）。
            _hotkeyModeSession = TRUE;
            _assocBareEndPending = FALSE;
            return;
        }
        if (_assocBareSession)
        {
            // 无占位联想会话的 FALSE 只记下「待收」，由 TSF 线程在下一次 OnTestKeyDown 入口落位：
            // 本函数在 AsyncReader 线程，直接清位会让同一键的 Test / Key 两阶段判定翻转，
            // 也可能把刚被上屏应答同步置起的新一轮会话清掉。
            _assocBareEndPending = TRUE;
            return;
        }
        _hotkeyModeSession = FALSE;
    }

    /// 服务端接管了会话（加词等热键会话在 WM_HOTKEY 上乐观置位）：无占位联想的归属标记交还，
    /// 之后 `ResetComposingState` 不会再把这一位清掉（那是加词会话的位，只能由服务端推送清）。
    void ReleaseAssocBareOwnership()
    {
        _assocBareSession = FALSE;
        _assocBareEndPending = FALSE;
    }

    /// 宿主报了选区变化（`OnEndEdit`）：若此刻有 Shift/Ctrl 正等着 keyup 触发切换，且鼠标键按着
    /// 或按住期间按下过，说明是鼠标点选，这次不算单击。选区变化本身不等于鼠标（输入法与宿主
    /// 的编辑也会报），必须与鼠标状态相与，否则误吞真单击。无 pending 时空操作。
    void NoteSelectionChangedDuringToggle();

    // 供 CTextService 的 SendInput 兜底路径（CommitText/InsertText/ReplacePrecedingChars）调用：
    // 把即将注入的按键标记为自生成，OnTestKeyDown/OnTestKeyUp 见到后直接放行，以免注入的
    // 退格/字符被当成用户按键二次处理（文字重复上屏）。压两条：每个键都注入 down+up，都该绕过 IME。
    void MarkSyntheticKey(WORD vk) { _PushSkipKey(vk, FALSE); _PushSkipKey(vk, TRUE); }

    // 供 CTextService::CommitTextViaSyntheticKey 调用：缓冲待提交文本并自注入保留 VK
    // （VK_ASYNC_COMMIT_TRIGGER），让 CommitText 挪到 OnKeyDown 里执行。那才是按键处理期间：
    // TF_ES_SYNC 合法、宿主的输入时处理（AutoCorrect 等）会触发，也避开 Word 对非按键上下文
    // 同步会话的拒绝。内嵌 `\n` 无需特殊处理，由宿主按上屏语义规范化。
    //
    // 不能复用 `_skipKeys`：那套是识别自生成键后放行，这里相反，识别触发键后吃掉并转入同步提交，
    // 见 `_TryConsumeAsyncCommitTrigger`。
    //
    // `pairCommit=TRUE` 时左移与深度记账一并排队，在同一 OnKeyDown 周期紧跟提交执行。
    //
    // 返回 FALSE = 合成按键注入失败，调用方应回退到 `CommitText(text, TRUE)` 保证不丢字；配对那条
    // 还必须自己补一次 `_ApplyPairCommitTail`，否则 DLL 少记一层、core 那层成孤儿。
    BOOL QueueAsyncCommitViaSyntheticKey(const std::wstring& text, BOOL replacingHeld,
                                         BOOL pairCommit = FALSE, uint32_t moveLeft = 0);

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
    // 本次按键的响应是 ClearCompositionThenPassThrough：组合已收掉，还欠宿主一次按键重放。
    // 只在 OnKeyDown / DispatchHotkey 里消费，两者都在调用响应处理之前先清零：
    // `_HandleServiceResponse` 的非按键调用点拿不到 wParam，残留会在下一次按键上误重放。
    BOOL _pendingReplayToHost = FALSE;
    // 正在 DispatchHotkey（WM_HOTKEY 回调）里处理响应：不在按键上下文，上屏要按
    // nonKeyContext 走异步会话。只在 DispatchHotkey 里置位、处理完恢复原值。
    BOOL _inHotkeyDispatch = FALSE;
    BOOL _isComposing;
    BOOL _hasCandidates;         // True if there are candidates to select
    /// 热键激活的模式（加词 / 临拼 / 特殊 / 生僻字）当前活着：服务端 `STATUS_HOTKEY_SESSION` 的
    /// 本地镜像，独立于 composition 与焦点变更。写入口只有 `SetCandidateSessionActive`。
    ///
    /// 不能复用 `_hasCandidates`，也不进 `ResetComposingState`：加词窗弹出会让宿主丢焦点并清掉
    /// `_hasCandidates`，Backspace/Enter/Escape 就又透传给宿主了。
    ///
    /// 可从 AsyncReader 线程写（StatePush 回调），TSF 线程在 `_HasInputSession()` 里读；对齐的
    /// 32 位写不会撕裂。
    BOOL _hotkeyModeSession = FALSE;
    /// 「无占位的联想会话」由本地同步置位的标记（`COMMIT_FLAG_ASSOC_SESSION`）。宿主里没有组合，
    /// 转发资格只能靠 `_hotkeyModeSession`，而服务端的异步推送赢不了下一次 OnTestKeyDown，故上屏
    /// 应答里同步先置一次并记下「这一位是我置的」，让本地结束点能清位又不误清加词等服务端会话。
    /// 只在 TSF 线程置位；服务端推 FALSE 的处理见 SetCandidateSessionActive。
    BOOL _assocBareSession = FALSE;
    /// 服务端推 FALSE 清掉了无占位联想会话，TSF 线程还欠一次候选收起。
    BOOL _assocBareEndPending = FALSE;
    /// 本地结束点：清位（不动加词等服务端会话）。TSF 线程调用。
    void _EndAssocBareSession()
    {
        if (_assocBareSession)
        {
            _assocBareSession = FALSE;
            _hotkeyModeSession = FALSE;
        }
    }
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

    // 软键盘布局之外的键：小键盘（docs/design/soft-keyboard.md §4.1，透传）。
    //
    // 软键盘态下不得吃它们：Rust 侧 `vk_to_slot()` 只含主键区 47 键，吃了转发过去查不到键位，
    // 就是「吃了再吐」，严格宿主里数字凭空消失。不变量是 C++ 吃键集 ⊆ Rust 出字集。
    // NumLock 关闭时小键盘发 VK_PRIOR/VK_NEXT 等，天然不命中，按翻页/编辑键语义走。
    // 代价：`numpad_behavior = "follow_main"` 下小键盘直接出数字而非面板符号；面板上没画小键盘，
    // 出数字不丢键，可接受。
    static BOOL _IsSoftKeyboardOutOfLayoutKey(WPARAM vk)
    {
        if (vk >= VK_NUMPAD0 && vk <= VK_NUMPAD9)
            return TRUE;
        return vk == VK_MULTIPLY || vk == VK_ADD || vk == VK_SUBTRACT
            || vk == VK_DECIMAL || vk == VK_DIVIDE;
    }
    // 软键盘总闸的唯一判据，`OnTestKeyDown`（吃）与 `OnKeyDown`（转发）都调它。两处不许各写
    // 一份条件：那边吃了、这边不发，键就凭空消失。
    // 键盘面（send_keys）只接管 Esc 与翻页，字母/数字/标点落回常规判定链，能正常组码。
    bool _IsSoftKeyboardEatenKey(WPARAM vk, uint32_t modifiers) const;
    // Ctrl+Shift+F12（环形日志导出）的唯一判据，`OnTestKeyDown`（吃）与 `OnKeyDown`（导出）都调它，
    // 纪律同上。默认关（`dump_hotkey`，见 FileLogger.h）：两个调用点排在所有闸门之前，开着时
    // 这个 VS / JetBrains 常用组合键永远到不了宿主。
    static bool _IsLogDumpHotkey(WPARAM vk);
    // 输入右符号本身是否跳出（配置 jump_out_keys 里的 `right_symbol` 特殊值）。
    // 本 DLL 不消费它（右符号跳出由协调器裁决），但仍须解析：它占 payload 首字节。
    bool _jumpOutOnRightSymbol = false;
    // 「英半列有自定义标点映射」的源字符集合（core 经 CONFIG_KEY_CUSTOM_EN_PUNCT 推送）。
    // 英文半角默认透传标点键，core 收不到；据此精确吃下集合内的键转发。空集合 = 全部透传。
    std::set<wchar_t> _customEnPunctChars;
    // 该键是否属于「英半自定义标点」：英文模式吃键判据，同时也是英文本地配对的让位判据
    // （吃下的键要交给 core 出字，本地配对若抢先 CommitText 就把转发吞了）。
    // 判据必须与 core 的 `wind_punct::custom_english_punct_chars` 同源，漂移即「吃了再吐」丢键。
    BOOL _IsCustomEnglishPunctKey(WPARAM vk, uint32_t modifiers) const;
    // 英文半角会吃下转发 core 的标点键（英半自定义 ∪ 英文配对键）。「中文 + CapsLock」的
    // 标点透传按它开例外，与「英文 + CapsLock」对齐。
    BOOL _IsEnglishHalfEatenPunctKey(WPARAM vk, uint32_t modifiers) const;
    // 「中文模式下该透传不吃」的标点集合（core 经 CONFIG_KEY_CN_PASSTHROUGH_PUNCT 推送）。
    // 空集合（默认）= 全部按常规处理。
    std::set<wchar_t> _cnPassthroughPunctChars;
    // 同上，英文标点态那份（core 经 CONFIG_KEY_EN_PASSTHROUGH_PUNCT 推送）。
    // 是上一份的超集：那个态不走中文标点表。按当下标点态二选一。
    std::set<wchar_t> _enPassthroughPunctChars;
    // 自定义标点「空格」行配了值的列（位掩码，core 经 CONFIG_KEY_SPACE_CUSTOM_COLS 推送）。
    // 0（默认）= 中文模式空缓冲只在全角态吃空格。判据见 SpaceKeyPolicy.h。
    uint8_t _spaceCustomCols = 0;
    // 该键在中文输入模式下是否该透传（不吃），按当下标点态在两份集合间二选一。
    // 仅答「配置上该不该」，调用方须自行叠上 `!hasInputSession`（组码中按标点是顶码）与
    // `!IsFullWidth()`（全角下 `#` 要出 `＃`）两道动态闸门。
    // 判据必须与 core 的 `wind_punct::{chinese,english}_passthrough_punct_chars` 同源。
    BOOL _IsPassthroughPunctKey(WPARAM vk, uint32_t modifiers) const;
    // IPC 失败后置位：本地 composition 已复位，但服务端可能仍持有会话。期间视作有会话，让
    // ENTER/ESC 也发给服务端重握手；任何一次成功 ReceiveResponse 后清旗。
    BOOL _needsCompositionResync;
    // resync 自愈窗口：deadline 到期或连续失败超限后放弃，避免服务长时间不可用时把
    // ENTER/ESC/Ctrl+Alt 等键永久吃掉。失败 streak 在响应成功后清零。
    DWORD _resyncDeadline;        // GetTickCount() 时间戳，0 表示无 deadline
    int   _resyncFailStreak;      // 连续 IPC 失败次数，超过 RESYNC_MAX_RETRIES 强制降级 passthrough
    static constexpr DWORD RESYNC_WINDOW_MS = 3000;
    static constexpr int   RESYNC_MAX_RETRIES = 3;
    BOOL _IsResyncActive();       // 读旗+过期检查；过期会自动清旗

    // 「当前是否有活跃输入会话」的唯一判据，决定各类键归本输入法还是透传宿主。
    // 任何新的「算不算有会话」的状态只加到这里，别在调用点另写等价表达式。
    BOOL _HasInputSession();

    // 智能符号 hold 预览态下「我们无法代劳、必须交给宿主」的键：走「吃键 → 收口 → 重放」，
    // 见 OnKeyDown 里的调用点注释。回车已真机验证，其余同族键机制相同。
    //
    // 判据是「会被吃键门控捕获、且服务端在空缓冲下回 PassThrough」的键，不是按键语义；全角态
    // 由重放分支的 `!pfEaten` 守卫挡住，故不按全半角区分。数字键必须在列：hold 期间它被
    // session_select_or_page 吃掉后服务端回 PassThrough，漏列即「吃了再吐」。
    // Ctrl/Alt 组合由重放判据的 isCtrlAltCleanup 项覆盖（HoldReplayPolicy.h）。背景见
    // docs/architecture/smart-symbol-compat-notes.md 的「HoldComposition 方案」。
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

    // ── 透传上报（TOGGLE_PASSTHROUGH_KEY）────────────────────────────────────────
    //
    // 事实：某个 keydown 没被我们吃下，直接进了宿主。判据与失效方向见 `PassthroughNote.h`；
    // 这里只把 VK 值与 `<winuser.h>` 钉死，并把守卫接到两个入口上。
    static_assert(WindPassthrough::kVkShift == VK_SHIFT, "VK 漂移");
    static_assert(WindPassthrough::kVkControl == VK_CONTROL, "VK 漂移");
    static_assert(WindPassthrough::kVkMenu == VK_MENU, "VK 漂移");
    static_assert(WindPassthrough::kVkLWin == VK_LWIN, "VK 漂移");
    static_assert(WindPassthrough::kVkRWin == VK_RWIN, "VK 漂移");
    static_assert(WindPassthrough::kVkLShift == VK_LSHIFT, "VK 漂移");
    static_assert(WindPassthrough::kVkRShift == VK_RSHIFT, "VK 漂移");
    static_assert(WindPassthrough::kVkLControl == VK_LCONTROL, "VK 漂移");
    static_assert(WindPassthrough::kVkRControl == VK_RCONTROL, "VK 漂移");
    static_assert(WindPassthrough::kVkLMenu == VK_LMENU, "VK 漂移");
    static_assert(WindPassthrough::kVkRMenu == VK_RMENU, "VK 漂移");
    static_assert(WindPassthrough::kVkPacket == VK_PACKET, "VK 漂移");
    static_assert(WindInputSeq::kVkSpace == VK_SPACE, "VK 漂移");
    static_assert(WindInputSeq::kVkNumpad0 == VK_NUMPAD0, "VK 漂移");
    static_assert(WindInputSeq::kVkNumpad9 == VK_NUMPAD9, "VK 漂移");
    static_assert(WindInputSeq::kVkMultiply == VK_MULTIPLY, "VK 漂移");
    static_assert(WindInputSeq::kVkDivide == VK_DIVIDE, "VK 漂移");
    static_assert(WindInputSeq::kVkOem1 == VK_OEM_1, "VK 漂移");
    static_assert(WindInputSeq::kVkOem3 == VK_OEM_3, "VK 漂移");
    static_assert(WindInputSeq::kVkOem4 == VK_OEM_4, "VK 漂移");
    static_assert(WindInputSeq::kVkOem7 == VK_OEM_7, "VK 漂移");
    static_assert(WindInputSeq::kVkOem102 == VK_OEM_102, "VK 漂移");

    // `OnTestKeyDown` / `OnKeyDown` 有 40+ 个 return，逐个接线必漏。用作用域守卫在函数出口
    // 统一判：析构时 `*pfEaten` 已是最终结论。同款收口见 Rust 侧的 `SoftKeyboardPushOnDrop`。
    struct PassthroughNoter
    {
        CKeyEventSink* sink;
        WPARAM vk;
        BOOL* eaten;
        LPARAM lParam;
        // 挂在 OnTestKeyDown（true）还是 OnKeyDown（false）上：扩展旁路上报按此去重。
        bool fromTest;
        // skip 表命中（我们自己注入的键）时置位，见 `PassthroughNote.h` 的排除项论证。
        bool suppressed = false;
        void Suppress() { suppressed = true; }
        ~PassthroughNoter()
        {
            if (sink != nullptr && eaten != nullptr)
            {
                sink->_passthrough.NoteKeyDown((uint32_t)vk, *eaten != FALSE, suppressed);
                sink->_NoteInputSequenceAtExit(vk, *eaten != FALSE, suppressed);
                sink->_ReportObservedKeyAtExit(vk, lParam, fromTest, *eaten != FALSE, suppressed);
            }
        }
    };

    // ── 扩展旁路上报（CMD_KEY_OBSERVED）──────────────────────────────────────────
    //
    // 放行给宿主的用户按键核心看不见；有扩展订阅按键时（core 推开 CONFIG_KEY_EXTENSION_KEY_OBSERVE）
    // 在上面那个守卫的出口异步报一份，供按键音这类扩展用。去重规则在 `ObservedKeyGate.h`；
    // 密码框与键盘禁用时一个都不报。只读通知，核心不拿它做任何按键处理。
    void _ReportObservedKeyAtExit(WPARAM vk, LPARAM lParam, bool fromTest, bool eaten, bool suppressed);
    WindObserve::ObservedKeyGate _observeGate;
    // config 回调在 IPC 读线程写、TSF 线程读。
    std::atomic<bool> _observeKeys{false};

    // skip 表放行的键一律不上报，哪怕它们 `pfEaten = FALSE` 进了宿主：那些是我们自己注入的
    // （`MarkSyntheticKey` / `_SimulatePairKey` / `_ReplayKeyToHost`），记账会让智能符号在走
    // SendInput 兜底的宿主上整条失效。见 `PassthroughNote.h`。

    // ── 数字后智能标点的输入序列记账 ────────────────────────────────────────────
    //
    // 「数字后」只认 _lastPassthroughDigit 记下的刚打出去的数字，不读文档（见 SmartPunctPrevChar.h）。
    // 服务端只认 prevChar 的值，「哪些键产出数字」的判据完全在本文件。
    //
    // 反过来「哪些标点键该带上这个值」不是本文件的事，别在消费点写符号白名单（会与服务端的
    // `input.punct.smart_list` 漂移）；消费点如实上报所有标点键，由 wind-punct 的
    // `is_smart_punct_after_digit` 决定。
    //
    // 主键盘与小键盘必须都认，漏了小键盘数字会把已记的值清零。NumLock 关闭或 Shift+小键盘时
    // vk 已是方向/编辑键，天然不命中。返回 0 表示这一键不产出数字，记录点据此清零。
    static WCHAR _DigitCharFromVk(WPARAM vk, uint32_t modifiers)
    {
        // Ctrl/Alt+数字是宿主快捷键，不产出数字（与 Linux `DigitTracker::digitChar` 同口径）。
        if (modifiers & (KEYMOD_CTRL | KEYMOD_ALT))
            return 0;
        // Shift+主键盘数字产出的是符号（!@#…）而非数字，不能当数字记。
        if (vk >= '0' && vk <= '9')
            return (modifiers & KEYMOD_SHIFT) ? 0 : (WCHAR)vk;
        if (vk >= VK_NUMPAD0 && vk <= VK_NUMPAD9)
            return (WCHAR)(L'0' + (vk - VK_NUMPAD0));
        return 0;
    }

    // 按键侧记录点（同时更新 _seqChar，见 InputSequenceChar.h）：挂在 PassthroughNoter 的出口，
    // 按最终 `*pfEaten` 判。键进了宿主（判据同 `WindPassthrough::IsUserKeyIntoHost`）就记是不是
    // 数字，非数字即清零。同时作废 CTextService 缓存的 prevChar（按键前的快照），否则不触发
    // OnEndEdit 的宿主里智能符号 press2 守卫会拿旧字比对。
    void _NoteInputSequenceAtExit(WPARAM vk, bool eaten, bool suppressed);

    // 经引擎上屏的文本也要更新输入序列记账（全角数字、候选带数字等路径按键侧记录点覆盖不到）。
    // 末位是 ASCII 数字则记，否则清零；全角数字不记（服务端只认 ASCII）；空文本不动状态。
    // 同时更新 _seqChar（末尾码点，见 InputSequenceChar.h）。
    void _TrackCommittedTextForSmartPunct(const std::wstring& text);

public:
    /// 只更新 seqChar（数字记账不动）：「上屏后紧接着开组合」与 HoldComposition 落定这类
    /// 数字记账刻意不记、但文档里确实多了一段的上屏点用。空文本不动。
    void NoteCommittedSeqChar(const std::wstring& text)
    {
        if (!text.empty())
            _seqChar = WindInputSeq::FromCommittedText(text.c_str(), text.size());
    }

    /// 鼠标挪过光标（判据见 `WindInputSeq::CursorMovedByMouse`）：输入序列记账作废。
    void NoteCursorMovedByMouse() { _seqChar = WindInputSeq::kUnknown; }

private:

    // 不要在 TSF 侧实现 CapsLock 的「回敲复原」：TSF 压不住锁定态翻转，事后回敲在快速连按下
    // 有竞态且触发厂商 OSD。CapsLock 的会话态绑定由服务进程的 WH_KEYBOARD_LL 钩子在状态更新前
    // 拦截，见 wind-keys 的 capslock_hook 模块。

    WCHAR _lastPassthroughDigit; // 最近一笔输入若是 ASCII 数字则记之，否则 0——数字后智能标点的唯一依据
    // 输入序列里最近进文档的字符代表值（KeyPayload.seqChar，取值规则见 InputSequenceChar.h）。
    // 与 _lastPassthroughDigit 同两个记录点，但发送时不清零：服务端只在组合开始那一键锁存它。
    uint16_t _seqChar;
    // 中数间补空格开着、且本应用未关（core 经 CONFIG_KEY_SPACE_BEFORE_DIGIT 按客户端现算推来）。
    // 只决定「汉字后的数字吃不吃」，见 NumberKeyPolicy.h 的 SpaceBeforeDigit。
    bool _spaceBeforeDigit = false;
    bool _SpaceBeforeDigitFor(WPARAM vk, uint32_t modifiers) const;
    // 「自上一个 keydown 送达服务端以来有键进了宿主」的记账。规则与理由见 PassthroughNote.h；
    // 消费点在 _SendKeyToService（仅 keydown 那一发），清理点在 ResetComposingState。
    WindPassthrough::PassthroughState _passthrough;
    uint32_t _pendingKeyUpKey;   // Key code of pending KeyUp toggle key
    uint32_t _pendingKeyUpModifiers; // Modifiers when KeyDown was pressed
    DWORD    _pendingKeyDownTime;    // GetTickCount() when toggle key was pressed down
    // 待切换键按住期间鼠标操作的记账（判据见 ToggleTapPolicy.h）。
    // 首次记 pending 时置 mouseDownAtPress 并清 GetAsyncKeyState 的「按下过」位；
    // selectionChanged 由 TextService::OnEndEdit 经 NoteSelectionChangedDuringToggle 置位。
    wind::toggletap::TapSignals _pendingTapSignals;
    // 候选显示时 Ctrl 如实上报的这次按住（判据见 CtrlRawPolicy.h）。与上面的待判单击互斥：
    // 一次按住在按下那一刻定走哪条路。鼠标记账与待判单击同一套判据，各记各的。
    wind::ctrlraw::Press _ctrlRaw;
    wind::toggletap::TapSignals _ctrlRawTapSignals;
    // 服务端吃掉过按下的那个功能键（F1–F12，不带 Ctrl / Alt），0 = 没有。它的松开也要送给
    // 服务端（注释切换键选功能键 + 按住档时靠松开收起详细态），见 _DispatchEatenFnKeyUp。
    UINT _eatenFnKeyVk = 0;
    BOOL _eatenFnKeyUpSent = FALSE;
    // OnTestKeyDown 是否刚吃下一个 Ctrl+Space，即「我们独占了这个键」的凭据：吃下后 msctf 不会
    // 翻 OPENCLOSE compartment，按键侧兜底切换与 compartment 路径天然互斥。不调 OnTestKeyDown
    // 的宿主（QQ）此标志恒 FALSE、兜底不触发，宁可不修也不冒双切换的险。
    BOOL     _ctrlSpaceEatenInTest;
    // Ctrl+空格 是否当中英切换键（服务端经 CONFIG_KEY_CTRL_SPACE_TOGGLE 下发）。关闭时不吃，键交给
    // 宿主（IDEA 代码提示），兜底切换也随之不走。每次重连从默认起步，握手时服务端必推。
    // 判据见 CtrlSpacePolicy.h。写在 IPC 读线程、读在 TSF 线程，故 atomic。
    std::atomic<bool> _ctrlSpaceToggleEnabled{wind::ctrlspace::kDefaultEnabled};

    // 配对插入走按键注入提交（服务端经 CONFIG_KEY_PAIR_VIA_COMP 下发，compat
    // `pair_via_comp` 覆盖全局 `input.auto_pair.commit_via_composition`，默认关 = 直写）。
    // 开启时 InsertTextWithCursor 响应与空闲态 ASCII 提交走 InjectUnicodeText：AutoCAD 表格在位
    // 编辑器丢弃 TSF 直写的 ASCII 却报 S_OK。写在 IPC 读线程、读在 TSF 线程，故 atomic。
    std::atomic<bool> _pairViaComposition{wind::paircommit::kDefaultPairViaComp};

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

    // Sync state from the service response
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

    // Barrier timeout (if the service doesn't respond, fallback handling)
    static constexpr DWORD BARRIER_TIMEOUT_MS = 500;

    BOOL _SendCommitRequest(uint16_t barrierSeq, uint16_t triggerKey, uint32_t mods, const std::string& inputBuffer);
    void _HandleCommitResult(uint16_t barrierSeq, const std::wstring& text, const std::wstring& newComp, bool modeChanged, bool chineseMode);
    void _CheckBarrierTimeout();

    // ========================================================================
    // Helper methods
    // ========================================================================
    BOOL _IsMatchingKeyUp(WPARAM wParam, uint32_t pendingKey);
    // Dispatch the pending toggle key (Shift/Ctrl) to the service and clear _pendingKeyUpKey.
    // Called from both OnTestKeyUp and OnKeyUp; clearing in the first caller makes the second a no-op.
    // Returns TRUE if a toggle was matched (caller should set pfEaten=TRUE).
    BOOL _DispatchPendingToggleKeyUp(WPARAM wParam);

    // 候选显示时 Ctrl 的如实上报（CtrlRawPolicy.h）。
    // _NoteKeyDownForCtrlRaw：任意非 Ctrl 的 keydown 记「按住期间按过别的键」。
    // _TryCtrlRawDown：Ctrl 的 keydown；返回 TRUE = 这次按住走如实上报（已发或重复），调用方放行并返回。
    // _DispatchCtrlRawUp：Ctrl 的 keyup；是如实上报那次按住就发 MOD_UP。不吃键，调用方照常往下走。
    void _NoteKeyDownForCtrlRaw(WPARAM wParam);
    BOOL _TryCtrlRawDown(WPARAM wParam, LPARAM lParam, uint32_t modifiers);
    void _DispatchCtrlRawUp(WPARAM wParam);
    BOOL _DispatchEatenFnKeyUp(WPARAM wParam, BOOL finalize);
    static uint32_t _ResolveCtrlVk(WPARAM wParam, uint32_t modifiers);

    // 记录「该切换键正等 keyup 触发切换」。OnTestKeyDown 与 OnKeyDown 都会调用：
    // 纯修饰键放行后 TSF 未必再调 OnKeyDown，只在后者记录会让切换失灵。幂等。
    void _MarkPendingToggleKey(WPARAM wParam, uint32_t modifiers);

    // 取消待切换。给命中热键白名单的组合键用：那些分支就地 return，不显式调用就会按了热键还切中英。
    void _CancelPendingToggle(WPARAM wParam, const wchar_t* reason);
    BOOL _SendKeyToService(uint32_t keyCode, uint32_t modifiers, uint8_t eventType, uint8_t extraToggles = 0);
    // Returns TRUE if key was handled, FALSE to pass through.
    // outType：收包成功时写入响应类型，失败时不动（热键派发据此判是否交还宿主）。
    BOOL _HandleServiceResponse(ResponseType* outType = nullptr);
    // 正在处理其应答的 keydown 的 VK（仅 OnKeyDown 主路径置位，其余路径为 0）。
    // CommitThenDefer 用它登记延迟组合的触发键，见 DeferredCompositionPolicy.h。
    UINT _responseKeyVk = 0;

    // Context state checking (for browser non-editable area detection)
    BOOL _IsContextReadOnly(ITfContext* pContext);

    // ========================================================================
    // Auto-pair key simulation (deferred + skip list approach)
    // ========================================================================
    // count = 这一次配对定位要走的格数（多字符右段如 `-->` 就是 3）。无修饰键时 count 个
    // down+up 一次 SendInput 提交；有修饰键时整条 defer，到 keyup 再按 count 发。
    void _SimulatePairKey(WORD vk, int count = 1);
    static bool _AreModifiersHeld();

    // 配对上屏的收尾：左移 moveLeft 格回到两段之间，并记一层待跳出深度。
    // 两个调用点（合成提交键的正路、注入失败的兜底）共用，避免记账规则分裂。
    // 记账判据是 pairCommit 而不是 moveLeft——理由见 .cpp 的实现注释，写错两边都会失配。
    void _ApplyPairCommitTail(BOOL pairCommit, uint32_t moveLeft);

    // 丢弃全部待提交并对被丢掉的配对层留痕。队列清空只走这一个出口。
    void _DropPendingAsyncCommits();

    // Pending auto-pair action (deferred until modifiers released)
    struct PendingPairAction {
        WORD vk = 0;
        int count = 0;
        bool active = false;
    };
    PendingPairAction _pendingPairAction;

    // English auto-pair engine (handles pairing in English mode)
    PairEngine _englishPairEngine;

    // ── Skip 表：我们自己经 SendInput 注入的键，应绕过 IME 处理 ──────────────
    //
    // 表的语义与 `forKeyUp` 两类压入者见 `SkipKeyTable.h`。下面 6 处接线单测盖不到，接反了照样
    // 全绿，只能靠人工核对 + 真机：
    //
    //   压入 · 键是我们凭空造的 ⇒ 两条：`MarkSyntheticKey`（本文件上方）
    //   压入 · 键是用户真按的 ⇒ 一条(FALSE)：`_ReplayKeyToHost`、`_SimulatePairKey`、
    //                                        OnKeyUp 里的 defer 执行
    //   消费 · `OnTestKeyDown` 传 FALSE / `OnTestKeyUp` 传 TRUE
    //
    // 三个薄包装只传时钟、把过期与溢出打成日志、转换 BOOL/bool。判定逻辑一律在 SkipKeyTable 里，
    // 别在这层加分支。
    SkipKeyTable _skipKeys;
    void _PushSkipKey(WORD vk, BOOL forKeyUp);
    BOOL _TryConsumeSkipKey(WPARAM wParam, BOOL forKeyUp);
    // 把表里攒下的过期条目打成 WARN。这是注入键丢失（残条）唯一的可观测信号：残条切个窗就自愈，
    // 报障形态只是「时灵时不灵」。
    void _LogExpiredSkipKeys();

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
        // 本次是配对上屏（直通 ime.pair）：提交完成后要记一层待跳出深度。不能按 moveLeft 判：
        // `jump="0"` 时 core 照样压栈而 moveLeft 为 0，普通提交的 moveLeft 也是 0。
        BOOL pairCommit = FALSE;
        // 提交后向左退回两段之间的格数（多字符右段按 ime.pair 的 jump 取值）。
        // 跟着提交一起排队，保证左移与提交落在同一个按键上下文里。
        uint32_t moveLeft = 0;
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
