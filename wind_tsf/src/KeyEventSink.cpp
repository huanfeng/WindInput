#include "KeyEventSink.h"
#include "TextService.h"
#include "IPCClient.h"
#include "HotkeyManager.h"
#include "BinaryProtocol.h"
#include <cctype>
#include <cstdio>  // for swprintf

namespace
{
    // 纯修饰键：唯一职责就是修饰别的按键，自身按下/抬起对宿主没有独立语义。
    //
    // 这类键**绝不能吃**，哪怕它被配成了中英文切换键。切换判定挂在 keyup 上
    // （见 IsKeyUpHotkey），吃掉 keydown 换不来任何东西，却会让宿主完全看不到修饰键：
    //   · AutoCAD 按住 Shift 是正交模式覆盖，需要在光标移动全程持有 keydown。
    //     吃掉之后 CAD 反复重建输入上下文——实测每次按键放大成约 10 次焦点重建、
    //     每秒近百次，主线程被自己的重建工作压满，表现为「按住 Shift 移光标非常卡」。
    //     实测对照：把切换键从 Shift 换成 Ctrl，Shift 和 Ctrl 都不卡且切换正常；
    //     换回 Shift 立刻复发。Ctrl 不犯病只是因为 CAD 不用它做鼠标移动修饰。
    //   · 同类问题此前已在 Fusion 360 上出现过（见下方 isToggleModeKey 分支的注释），
    //     当时只给「未配置为切换键」的情况加了守卫，配置了的仍然照吃。
    //
    // CapsLock **不**属于此列：它有真实的大写状态副作用，必须吃掉才能压制。
    //
    // ⚠ down 和 up 必须一致放行：只放行一边会让宿主看到「按下但从未松开」的
    // 卡死修饰键，那比吃掉更糟。
    BOOL _IsPureModifierKey(WPARAM vk)
    {
        return vk == VK_SHIFT   || vk == VK_LSHIFT   || vk == VK_RSHIFT
            || vk == VK_CONTROL || vk == VK_LCONTROL || vk == VK_RCONTROL
            || vk == VK_MENU    || vk == VK_LMENU    || vk == VK_RMENU;
    }

    const wchar_t* _HotkeyTypeName(HotkeyType type)
    {
        switch (type)
        {
        case HotkeyType::None: return L"none";
        case HotkeyType::ToggleMode: return L"toggle_mode";
        case HotkeyType::Hotkey: return L"hotkey";
        case HotkeyType::Letter: return L"letter";
        case HotkeyType::Number: return L"number";
        case HotkeyType::Punctuation: return L"punctuation";
        case HotkeyType::Backspace: return L"backspace";
        case HotkeyType::Enter: return L"enter";
        case HotkeyType::Escape: return L"escape";
        case HotkeyType::Space: return L"space";
        case HotkeyType::Tab: return L"tab";
        case HotkeyType::PageKey: return L"page_key";
        case HotkeyType::CursorKey: return L"cursor_key";
        case HotkeyType::SelectKey: return L"select_key";
        }

        return L"unknown";
    }

    void _LogKeyDecision(const wchar_t* phase, ULONGLONG focusSessionId, WPARAM keyCode, uint32_t modifiers,
                         HotkeyType keyType, BOOL chineseMode, BOOL hasComposition, BOOL hasCandidates,
                         BOOL hasInputSession, BOOL eaten, const wchar_t* decision)
    {
        WindLog::OutputFmt(
            5,
            L"compat.key phase=%ls focusSession=%llu vk=0x%02X mods=0x%04X keyType=%ls chinese=%d composing=%d candidates=%d inputSession=%d eaten=%d decision=%ls",
            phase,
            focusSessionId,
            (uint32_t)keyCode,
            modifiers,
            _HotkeyTypeName(keyType),
            chineseMode ? 1 : 0,
            hasComposition ? 1 : 0,
            hasCandidates ? 1 : 0,
            hasInputSession ? 1 : 0,
            eaten ? 1 : 0,
            decision ? decision : L"-"
        );
    }

    // Map VK code + shift state to the actual character for English auto-pair
    wchar_t _MapVkToEnglishPairChar(WPARAM vk, bool hasShift)
    {
        if (hasShift)
        {
            switch (vk)
            {
            case '9':          return L'(';
            case '0':          return L')';
            case VK_OEM_4:     return L'{';  // [ key + Shift = {
            case VK_OEM_6:     return L'}';  // ] key + Shift = }
            case VK_OEM_COMMA: return L'<';  // , key + Shift = <
            case VK_OEM_PERIOD:return L'>';  // . key + Shift = >
            case VK_OEM_7:     return L'"';  // ' key + Shift = "
            }
        }
        else
        {
            switch (vk)
            {
            case VK_OEM_4:     return L'[';
            case VK_OEM_6:     return L']';
            case VK_OEM_7:     return L'\''; // ' key
            }
        }
        return 0;
    }
}

CKeyEventSink::CKeyEventSink(CTextService* pTextService)
    : _refCount(1)
    , _pTextService(pTextService)
    , _dwKeySinkCookie(TF_INVALID_COOKIE)
    , _dwKeyTraceSinkCookie(TF_INVALID_COOKIE)
    , _isComposing(FALSE)
    , _hasCandidates(FALSE)
    , _needsCompositionResync(FALSE)
    , _resyncDeadline(0)
    , _resyncFailStreak(0)
    , _lastPassthroughDigit(0)
    , _pendingKeyUpKey(0)
    , _ctrlSpaceEatenInTest(FALSE)
    , _pendingKeyUpModifiers(0)
    , _pendingKeyDownTime(0)
    , _modsState(0)
    , _eventSeq(0)
    , _nextBarrierSeq(1)
    , _pendingCommit{0, L"", 0, false}
{
    _pTextService->AddRef();

    // Initialize modifier state from current keyboard state
    // This ensures consistency if IME starts while keys are held
    _modsState = GetCurrentModifiers();
}

CKeyEventSink::~CKeyEventSink()
{
    SafeRelease(_pTextService);
}

STDAPI CKeyEventSink::QueryInterface(REFIID riid, void** ppvObj)
{
    if (ppvObj == nullptr)
        return E_INVALIDARG;

    *ppvObj = nullptr;

    if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITfKeyEventSink))
    {
        *ppvObj = (ITfKeyEventSink*)this;
    }
    else if (IsEqualIID(riid, IID_ITfKeyTraceEventSink))
    {
        *ppvObj = (ITfKeyTraceEventSink*)this;
    }

    if (*ppvObj)
    {
        AddRef();
        return S_OK;
    }

    return E_NOINTERFACE;
}

STDAPI_(ULONG) CKeyEventSink::AddRef()
{
    return InterlockedIncrement(&_refCount);
}

STDAPI_(ULONG) CKeyEventSink::Release()
{
    LONG cr = InterlockedDecrement(&_refCount);

    if (cr == 0)
    {
        delete this;
    }

    return cr;
}

// 记录「这个切换键正等着 keyup 触发切换」。
// 由 OnTestKeyDown（放行纯修饰键时）与 OnKeyDown 共同调用；对同一个键重复调用不会
// 重新计时（见函数内说明），因此两处都调、宿主重复发 keydown，都不会影响长按判定。
void CKeyEventSink::_MarkPendingToggleKey(WPARAM wParam, uint32_t modifiers)
{
    // 必须解析出具体的左右键：wParam 可能是笼统的 VK_SHIFT，而热键白名单登记的是
    // VK_LSHIFT / VK_RSHIFT，不解析就匹配不上配置。
    // 优先用 modifiers（双源），降级 GetAsyncKeyState——修复 WebView2 / Wails 等
    // Chromium 宿主下 GetAsyncKeyState 拿不到具体 L/R Shift 的兼容问题。
    uint32_t specificKey = (uint32_t)wParam;
    if (wParam == VK_SHIFT)
    {
        if (modifiers & KEYMOD_LSHIFT)
            specificKey = VK_LSHIFT;
        else if (modifiers & KEYMOD_RSHIFT)
            specificKey = VK_RSHIFT;
        else if (GetAsyncKeyState(VK_LSHIFT) & 0x8000)
            specificKey = VK_LSHIFT;
        else if (GetAsyncKeyState(VK_RSHIFT) & 0x8000)
            specificKey = VK_RSHIFT;
    }
    else if (wParam == VK_CONTROL)
    {
        if (modifiers & KEYMOD_LCTRL)
            specificKey = VK_LCONTROL;
        else if (modifiers & KEYMOD_RCTRL)
            specificKey = VK_RCONTROL;
        else if (GetAsyncKeyState(VK_LCONTROL) & 0x8000)
            specificKey = VK_LCONTROL;
        else if (GetAsyncKeyState(VK_RCONTROL) & 0x8000)
            specificKey = VK_RCONTROL;
    }
    // 同一个键已在 pending 中就**不重新计时**。
    // 宿主会为一次按住的键重复发 keydown：AutoCAD 实测 28 秒内 145 次 test_down，
    // MS Word 2010 也会对单次按键发多次 OnTestKeyDown（Weasel 源码注明）。
    // 每次都重置 _pendingKeyDownTime 的话，_DispatchPendingToggleKeyUp 里的
    // TOGGLE_TAP_THRESHOLD_MS 判定永远只看到「刚按下」，长按 Shift 会被误判成
    // 轻敲而切换中英文——这正是放行 keydown 后在 CAD 暴露出来的回归。
    // 首次按下才起表；重复事件只刷新修饰键位（左右手可能中途变化）。
    if (_pendingKeyUpKey == specificKey)
    {
        _pendingKeyUpModifiers = modifiers;
        return;
    }

    _pendingKeyUpKey = specificKey;
    _pendingKeyUpModifiers = modifiers;
    _pendingKeyDownTime = GetTickCount();
}

// 取消待切换。热键分支（keydown 白名单 / chinese-only / session / Ctrl+Space）都是
// 就地 return，够不着 OnTestKeyDown 下方那段「非 toggle 键取消 pending」的统一处理。
//
// 不在这些分支显式取消的后果：Shift 被配成切换键时（默认 lshift/rshift），Shift 自身的
// keydown 已把 _pendingKeyUpKey 记成待切换；随后的 Shift+Space 若在热键分支被吃掉并
// return，松开 Shift 时 _DispatchPendingToggleKeyUp 仍会命中 —— 一次 Shift+Space 既切了
// 全半角又切了中英文。Ctrl 被配成切换键时的 Ctrl+= / Ctrl+Space 同理。
//
// 命中热键白名单本身就说明这次按键是组合键而非切换键轻敲，故无条件取消。
void CKeyEventSink::_CancelPendingToggle(WPARAM wParam, const wchar_t* reason)
{
    if (_pendingKeyUpKey == 0)
        return;
    // 纯修饰键自身永不在此取消：宿主对按住的键会重复发 keydown（CAD 实测 28 秒 145 次），
    // 若它某天也命中了 keydown 白名单（select_key_groups 支持 lrshift/lrctrl），
    // 「取消→重记」会不断重置 _pendingKeyDownTime，长按被误判成轻敲而误切中英文。
    if (_IsPureModifierKey(wParam))
        return;

    WIND_LOG_DEBUG_FMT(L"Cancel pending toggle (%ls): vk=0x%02X pending=0x%02X\n",
        reason ? reason : L"-", (uint32_t)wParam, _pendingKeyUpKey);
    _pendingKeyUpKey = 0;
    _pendingKeyUpModifiers = 0;
}

// ITfKeyEventSink::OnSetFocus —— 名字像是焦点主回调，实际**很不可靠**，不要往这里挂
// 任何独占职责：AutoCAD 实测整场只触发 2 次，且全是 fForeground=1，从来没有过 0。
// （曾把 focus_lost 挂在它的 else 分支上，结果 focus_lost 完全断供。）
// 应用切入/切出的权威信号是 ITfThreadFocusSink::OnSet/KillThreadFocus——同一份日志里
// 2 / 1 次，与实际切换一一对应。
STDAPI CKeyEventSink::OnSetFocus(BOOL fForeground)
{
    WIND_LOG_INFO_FMT(L"KeyEventSink::OnSetFocus fForeground=%d\n", fForeground ? 1 : 0);
    return S_OK;
}

STDAPI CKeyEventSink::OnTestKeyDown(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten)
{
    *pfEaten = FALSE;

    // 合成提交触发键：无条件吃下，不流入下面任何按键逻辑（大小写/会话/热键判据……）。
    // 真正的提交在 OnKeyDown 里执行——那才是 TSF 认可的"按键处理期间"。放在最前面，
    // 与状态（_isComposing 等）无关：不管此刻是否在组合中，触发键都必须先被截获。
    if (wParam == VK_ASYNC_COMMIT_TRIGGER && !_pendingAsyncCommits.empty())
    {
        *pfEaten = TRUE;
        return S_OK;
    }

    // Auto-pair: bypass IME for self-generated SendInput keys (VK_LEFT/RIGHT/DELETE/BACK)
    if (_TryConsumeSkipKey(wParam))
    {
        *pfEaten = FALSE; // Let it pass directly to the app
        return S_OK;
    }

    // Ctrl+Shift+F12: Dump TSF ring buffer logs to clipboard (works in AppContainer)
    if (wParam == VK_F12 && (GetKeyState(VK_CONTROL) & 0x8000)
        && (GetKeyState(VK_SHIFT) & 0x8000) && !(GetKeyState(VK_MENU) & 0x8000))
    {
        *pfEaten = TRUE;
        return S_OK;
    }

    // Trace: Log ALL key presses (very high frequency)
    WIND_LOG_TRACE_FMT(L"OnTestKeyDown: wParam=0x%02X\n", (uint32_t)wParam);
    // 每个新按键都重新起算：凭据只对紧随其后的那一次 OnKeyDown 有效。
    _ctrlSpaceEatenInTest = FALSE;

    // Keyboard disabled by system: pass through all keys
    if (_pTextService->IsKeyboardDisabled())
        return S_OK;

    // 密码框强制英文抑制：与上面的 disabled 同类——全部放行，一个键都不吃。
    // 必须在此（吃键决策点）判，不能只靠 core 回 PassThrough：那时 pfEaten 已为 TRUE，
    // 形成 OnTestKeyDown(TRUE)+OnKeyDown(FALSE) 的「吃了再吐」，而 Chrome/Electron 等
    // 严格宿主不回退合成 WM_CHAR → 密码框里字母被整个吞掉（中文模式下字母恒被吃，见下方
    // chinese_letter 分支）。判据镜像 core，见 CTextService::IsPasswordSuppressActive。
    if (_pTextService->IsPasswordSuppressActive())
        return S_OK;

    // First check if the context is read-only (browser non-editable area)
    if (_IsContextReadOnly(pContext))
    {
        _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, 0, HotkeyType::None,
                        _pTextService->IsChineseMode(), _pTextService->HasActiveComposition(), _hasCandidates,
                        _pTextService->HasActiveComposition() || _hasCandidates, FALSE, L"context_readonly");
        return S_OK;
    }

    // Get current modifiers and calculate key hash
    // For function hotkeys (like Ctrl+`), use normalized modifiers (no left/right distinction)
    //
    // 只算归一化这一份：本函数下游的每一处查表都用它。此处原先还并列着一份非归一化的
    // `keyHash`，自「统一走归一化」之后就无人读了——留着会让排查「热键配了没反应」的人
    // 以为这里还有第二条匹配路径，而 hash 失配恰恰是那类问题的头号嫌疑。
    uint32_t modifiers = CHotkeyManager::GetCurrentModifiers();
    uint32_t normalizedMods = CHotkeyManager::NormalizeModifiers(modifiers);
    uint32_t normalizedKeyHash = CHotkeyManager::CalcKeyHash(normalizedMods, (uint32_t)wParam);

    CHotkeyManager* pHotkeyMgr = _pTextService->GetHotkeyManager();

    // Check if this is a KeyDown hotkey from the whitelist
    // Use normalized hash for function hotkeys (Ctrl+`, Shift+Space, etc.)
    if (pHotkeyMgr != nullptr && pHotkeyMgr->IsKeyDownHotkey(normalizedKeyHash))
    {
        _CancelPendingToggle(wParam, L"keydown_hotkey");

        // 「仅注册转发」的键（翻页键组 -=、选词键组 ;'）无会话时放行，并**继续往下走**
        // （不是 return）：引擎对它们只会回 PassThrough，而 WindTerm 等宿主处理不好
        // OnTestKeyDown(TRUE)+OnKeyDown(FALSE) 的翻转会直接吞键；放行后下方
        // ClassifyInputKey 会在中文模式把它们当标点正确处理。
        //
        // ⚠ 闸门只对 FORWARD_ONLY 生效，绝不能按「无 Ctrl/Alt」一刀切扩到真动作热键上。
        // 曾经就是一刀切，于是 shift+space（toggle_full_width）也被放行——而 Space 在下方
        // 只有「有会话」和「已是全角」两条出路，半角 + 空缓冲时无人接手，键直接透传：
        // 严格 TSF 宿主（EverEdit）在 pfEaten=FALSE 后不再回调 OnKeyDown，热键永远送不到
        // 引擎，全半角切换彻底失效；宽松宿主（记事本 / Chromium）照调 OnKeyDown，由那边的
        // 白名单分发兜住才碰巧能用——这正是「记事本行、EverEdit 不行」的由来。
        // 判据须与服务端保持单一真相源：action 为空的登记项才带 FORWARD_ONLY（见 hotkey.rs）。
        BOOL shouldEatHotkey = TRUE;
        if (!(modifiers & (KEYMOD_CTRL | KEYMOD_ALT))
            && pHotkeyMgr->IsKeyDownForwardOnlyHotkey(normalizedKeyHash))
        {
            BOOL hasComp = _pTextService->HasActiveComposition();
            BOOL hasSession = hasComp || _hasCandidates;
            if (!hasSession)
            {
                WIND_LOG_DEBUG_FMT(L"OnTestKeyDown hotkey skipped (forward-only, no input session): vk=0x%02X, hash=0x%08X\n",
                    (uint32_t)wParam, normalizedKeyHash);
                shouldEatHotkey = FALSE;
            }
        }

        if (shouldEatHotkey)
        {
            WIND_LOG_DEBUG_FMT(L"KeyDown hotkey matched: vk=0x%02X, hash=0x%08X\n",
                         (uint32_t)wParam, normalizedKeyHash);
            *pfEaten = TRUE;
            _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::Hotkey,
                            _pTextService->IsChineseMode(), _pTextService->HasActiveComposition(), _hasCandidates,
                            _pTextService->HasActiveComposition() || _hasCandidates, TRUE, L"keydown_hotkey");
            return S_OK;
        }
    }

    // Policy: 仅中文模式吃（AddWord / TogglePunct / ToggleS2T）
    if (pHotkeyMgr != nullptr && pHotkeyMgr->IsKeyDownChineseOnlyHotkey(normalizedKeyHash))
    {
        _CancelPendingToggle(wParam, L"chineseonly_hotkey");
        if (_pTextService->IsChineseMode())
        {
            WIND_LOG_DEBUG_FMT(L"KeyDown chinese-only hotkey matched: vk=0x%02X, hash=0x%08X\n",
                               (uint32_t)wParam, normalizedKeyHash);
            *pfEaten = TRUE;
            _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::Hotkey,
                            TRUE, _pTextService->HasActiveComposition(), _hasCandidates,
                            _pTextService->HasActiveComposition() || _hasCandidates, TRUE, L"chineseonly_hotkey");
            return S_OK;
        }
        // 英文模式 → 透传给宿主，避免干扰宿主原生快捷键 (如 Ctrl+= 放大)
        WIND_LOG_DEBUG_FMT(L"KeyDown chinese-only hotkey skipped (english mode): vk=0x%02X, hash=0x%08X\n",
                           (uint32_t)wParam, normalizedKeyHash);
        *pfEaten = FALSE;
        return S_OK;
    }

    // Policy: 仅中文模式 + session 时吃（PinCandidate / DeleteCandidate，组合键见配置）
    //
    // ⚠️ 这条分支**平时走不到**：同一批键已被 CTextService::_RegisterCandidateHotkeys
    // 用 RegisterHotKey 在系统层拦下（候选可见期间），压根不会派发到 OnTestKeyDown。
    // 它是 RegisterHotKey 失败时（ERROR_HOTKEY_ALREADY_REGISTERED、非前台进程等）的退路。
    // ⇒ 排查「候选热键不生效」先看 RegisterCandidateHotkeys 的 registered=N/M 日志，
    //    别从这里开始查。
    if (pHotkeyMgr != nullptr && pHotkeyMgr->IsKeyDownSessionHotkey(normalizedKeyHash))
    {
        _CancelPendingToggle(wParam, L"session_hotkey");
        BOOL chineseMode = _pTextService->IsChineseMode();
        // resync 期 (上次 IPC 失败后) 视作有会话, 让 ENTER/ESC/Backspace 等 session 热键
        // 也走 Go 重握手, 由 Go 权威响应清旗 + 重建状态。
        BOOL hasSession  = _HasInputSession();
        if (chineseMode && hasSession)
        {
            WIND_LOG_DEBUG_FMT(L"KeyDown session hotkey matched: vk=0x%02X, hash=0x%08X\n",
                               (uint32_t)wParam, normalizedKeyHash);
            *pfEaten = TRUE;
            _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::Hotkey,
                            TRUE, _pTextService->HasActiveComposition(), _hasCandidates,
                            TRUE, TRUE, L"session_hotkey");
            return S_OK;
        }
        // 无 session 或英文模式 → 透传 (e.g., QQ 在无候选时 Ctrl+1 切 tab)
        WIND_LOG_DEBUG_FMT(L"KeyDown session hotkey skipped (chinese=%d session=%d): vk=0x%02X\n",
                           (int)chineseMode, (int)hasSession, (uint32_t)wParam);
        *pfEaten = FALSE;
        return S_OK;
    }

    // Check for KeyUp triggered keys (toggle mode keys) - we still need to intercept KeyDown
    // First try hash-based lookup, then fallback to VK-based detection
    BOOL isToggleModeKey = FALSE;

    // TSF sends generic VK_SHIFT/VK_CONTROL as wParam, but the hotkey whitelist
    // registers specific VK_LSHIFT/VK_RSHIFT/VK_LCONTROL/VK_RCONTROL.
    // Resolve the generic VK to specific left/right variant for proper hash matching.
    // 优先用 modifiers 参数（GetCurrentModifiers 双源），降级 GetAsyncKeyState；
    // WebView2 / Wails / 部分 Chromium 宿主下 GetAsyncKeyState 拿不到 L/R Shift。
    uint32_t resolvedVK = (uint32_t)wParam;
    if (wParam == VK_SHIFT)
    {
        if (modifiers & KEYMOD_LSHIFT)
            resolvedVK = VK_LSHIFT;
        else if (modifiers & KEYMOD_RSHIFT)
            resolvedVK = VK_RSHIFT;
        else if (GetAsyncKeyState(VK_LSHIFT) & 0x8000)
            resolvedVK = VK_LSHIFT;
        else if (GetAsyncKeyState(VK_RSHIFT) & 0x8000)
            resolvedVK = VK_RSHIFT;
    }
    else if (wParam == VK_CONTROL)
    {
        if (modifiers & KEYMOD_LCTRL)
            resolvedVK = VK_LCONTROL;
        else if (modifiers & KEYMOD_RCTRL)
            resolvedVK = VK_RCONTROL;
        else if (GetAsyncKeyState(VK_LCONTROL) & 0x8000)
            resolvedVK = VK_LCONTROL;
        else if (GetAsyncKeyState(VK_RCONTROL) & 0x8000)
            resolvedVK = VK_RCONTROL;
    }
    uint32_t keyUpHash = CHotkeyManager::CalcKeyHash(modifiers, resolvedVK);

    // ⛔ CapsLock 的会话态绑定（`keys.session_actions` 里的 capslock）**不在 TSF 处理**，
    // 别在这里重新加分支——曾加过三版，全部被真机否掉：
    //
    // 1. `pfEaten = TRUE` **压不住**锁定态翻转：那是系统在输入线程状态机里做的，位置在本
    //    回调之前（微软 KB127190 亦称 `SetKeyboardState` 改不了这三个 toggle 键）。
    // 2. 「让它翻转，再 SendInput 回敲复原」在快速连按下有竞态（物理事件与注入事件的相对
    //    顺序无法保证，大写会卡住），且那次真实的状态变化会被厂商 OSD 工具观测到并弹窗。
    //
    // 现改由服务进程的 `WH_KEYBOARD_LL` 钩子在**状态更新之前**拦截（见 wind-keys 的
    // `capslock_hook` 模块）。有会话时钩子吃掉整个 CapsLock，TSF 这里根本收不到；无会话时
    // 钩子放行，走下方原有的状态通知路径。两条路径互补，不重叠。
    if (pHotkeyMgr != nullptr && pHotkeyMgr->IsKeyUpHotkey(keyUpHash))
    {
        isToggleModeKey = TRUE;
    }
    else if ((pHotkeyMgr == nullptr || !pHotkeyMgr->HasHotkeys()) && CHotkeyManager::IsToggleModeKeyByVK(wParam))
    {
        // Fallback: detect toggle mode keys ONLY when hotkey whitelist hasn't been loaded yet.
        // Once the whitelist is loaded, trust it — if a key isn't in the whitelist,
        // it shouldn't be treated as a toggle key. Without this guard, Ctrl/Shift are
        // unconditionally intercepted even when not configured as toggle keys,
        // breaking modifier key usage in apps like Fusion 360.
        isToggleModeKey = TRUE;
    }

    if (isToggleModeKey)
    {
        BOOL hasSession = _pTextService->HasActiveComposition() || _hasCandidates;
        // CapsLock 的会话态绑定不在这里判——它的登记 hash 与本块用的 keyUpHash 不同源，
        // 判据已前移到 isToggleModeKey 判定之前（见那里的 capslock_session_eat 分支）。
        BOOL hasTextCtx = _pTextService->RefreshTextInputContext();
        WIND_LOG_DEBUG_FMT(L"compat.toggle_key test_down: vk=0x%02X resolvedVK=0x%02X mods=0x%04X hasSession=%d hasTextCtx=%d",
            (uint32_t)wParam, resolvedVK, modifiers, (int)hasSession, (int)hasTextCtx);
        if (!hasSession && !hasTextCtx)
        {
            *pfEaten = FALSE;
            _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::ToggleMode,
                            _pTextService->IsChineseMode(), FALSE, _hasCandidates,
                            FALSE, FALSE, L"toggle_no_text_ctx");
            WindLogForegroundProcessInfo(4, L"compat.toggle_no_textctx.host");
            return S_OK;
        }
        // 纯修饰键放行给宿主：切换判定在 keyup，吃掉 keydown 毫无收益却会破坏
        // 宿主的修饰键功能（AutoCAD 正交覆盖卡顿的根因）。详见 _IsPureModifierKey。
        const BOOL eatToggleDown = !_IsPureModifierKey(wParam);

        // ⚠ 放行时必须在这里就记下待切换状态。TSF 在 OnTestKeyDown 返回 pfEaten=FALSE
        // 后通常**不再调用 OnKeyDown**（下方那处 Chrome 的注释正说明它是例外），
        // 而 _pendingKeyUpKey 原本只在 OnKeyDown 里设置——不补这一手，放行 keydown
        // 就等于把中英文切换整个弄没了。
        // OnKeyDown 若仍被调用会再记一次，_MarkPendingToggleKey 对同一个键不会重新计时。
        if (!eatToggleDown)
            _MarkPendingToggleKey(wParam, modifiers);

        *pfEaten = eatToggleDown;
        _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::ToggleMode,
                        _pTextService->IsChineseMode(), _pTextService->HasActiveComposition(), _hasCandidates,
                        hasSession || hasTextCtx, eatToggleDown, L"toggle_mode_key");
        return S_OK;
    }

    // Ctrl+Space（系统 IME 中英切换热键）：吃掉这个键，避免 Space 落进输入。
    // **模式切换不在这里发生**——系统热键会取反 OPENCLOSE compartment，
    // CTextService::OnChange 按值语义直接采纳，不需要按键侧参与。
    //
    // 两条已被实测否掉的设计（勿重蹈）：
    //   1.「吃掉键以阻止系统翻 compartment，自己在 OnKeyDown 切换」——拦不住。
    //      pfEaten=TRUE 对系统 IME 热键无效，msctf 在 keystroke sink 之下就消费了它，
    //      compartment 照样被翻，且**不再回调 OnKeyDown**（该死代码已删）。
    //   2.「在这里打时间戳，供 OnChange 区分『切换请求』与『宿主状态请求』」——
    //      判据的隐含前提是「Space 会经过 keystroke sink」，而 WebView 类宿主
    //      （实测 DBX/msedgewebview2）根本不递 Space，标记永远打不上。
    // 值语义之后这两个问题都不存在了：值本身就是答案，无需知道是谁写的。
    if (wParam == VK_SPACE && (modifiers & KEYMOD_CTRL) && !(modifiers & (KEYMOD_ALT | KEYMOD_SHIFT)))
    {
        _CancelPendingToggle(wParam, L"ctrl_space_intercept");
        // 留下凭据：若 TSF 紧接着仍调用 OnKeyDown，说明 msctf 没把这个键当系统热键，
        // compartment 不会被翻，切换得由我们自己做（见 OnKeyDown 的 ctrl_space_toggle）。
        _ctrlSpaceEatenInTest = TRUE;
        *pfEaten = TRUE;
        _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::None,
                        _pTextService->IsChineseMode(), _pTextService->HasActiveComposition(), _hasCandidates,
                        _pTextService->HasActiveComposition() || _hasCandidates, TRUE, L"ctrl_space_intercept");
        return S_OK;
    }

    // Any non-toggle-mode key cancels pending toggle.
    // IMPORTANT: Must clear here because OnKeyDown may NOT be called
    // if this key is not eaten (e.g., Shift+Enter in English mode).
    // TSF only calls OnKeyDown when OnTestKeyDown sets pfEaten=TRUE.
    if (_pendingKeyUpKey != 0)
    {
        WIND_LOG_DEBUG_FMT(L"OnTestKeyDown: Non-toggle key vk=0x%02X cancels pending toggle\n", (uint32_t)wParam);
        _pendingKeyUpKey = 0;
        _pendingKeyUpModifiers = 0;
    }

    // Check basic input keys based on current state
    // Different handling based on key type:
    // - Letter/number/punctuation keys: intercept in Chinese mode
    // - Backspace/Enter/Escape: only intercept when there's an active composition or input session
    BOOL isChineseMode = _pTextService->IsChineseMode();
    // Use TextService's composition state - this is the source of truth in async architecture
    BOOL hasComposition = _pTextService->HasActiveComposition();
    // 判据收口在 _HasInputSession()（含 defer 真空期，见其定义）。此处不得就地展开：
    // OnTestKeyDown 放行的键根本不会调到 OnKeyDown，两边不一致即「吃了再吐」或直接丢键。
    BOOL hasInputSession = _HasInputSession();

    // English auto-pair: intercept bracket keys in English mode.
    // Ctrl/Alt 组合留给热键/宿主（如 Ctrl+Shift+] open_settings），不做配对。
    // 全角时本地配对让位给 core：全角下 `(` 要出 `（` 并配 `）`，而本引擎只认半角配对表；
    // 若在此吃键本地插入，会出半角配对且与 core 的 pair_tracker 双重处理。全角态统一由
    // core 的 handle_english_full_width 出字+配对（配对表由 english_pairs 过同一条流水线派生）。
    // 全角时本地不判：全角配对由 core 经 english_fullwidth 分支出字（配对表由 english_pairs
    // 过同一条流水线派生），此处只需不重复吃键。
    if (!isChineseMode
        && !_pTextService->IsFullWidth()
        && !(modifiers & (KEYMOD_CTRL | KEYMOD_ALT)))
    {
        bool hasShift = (modifiers & KEYMOD_SHIFT) != 0;
        wchar_t pairChar = _MapVkToEnglishPairChar(wParam, hasShift);
        if (pairChar != 0 && _englishPairEngine.ShouldEat(pairChar))
        {
            // 吃下转发给 core 出字 + 记栈（不再本地插入）。判据与 core 的
            // handle_english_custom_punct 同源，漂移即「吃了再吐」丢键。
            *pfEaten = TRUE;
            _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::None,
                            isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"english_autopair");
            return S_OK;
        }
    }

    // 配对跳出键：**全模式统一闸门**，不再按中英/全半角分岔。
    //
    // `_pairPendingDepth > 0` 本身就蕴含「开了配对、确实插入过、尚未跳出」，故没配对时
    // 一个 Tab/Enter 都不会被吃。陈旧状态由 TTL 挡掉——用户中途用鼠标点走、删掉括号这类
    // 操作输入法感知不到，没有时效的话状态会一直存活到吃掉用户的 Tab。
    //
    // 吃键后一律转发协调器裁决（真相源在那边）。此前英文分支自己持栈本地跳出、中文分支
    // 转发，两套判据互不相认，正是跨模式跳不出的根因。
    if (_pairPendingDepth > 0 && !IsPairStateStale()
        && _IsJumpOutKey((UINT)wParam)
        && !(modifiers & (KEYMOD_CTRL | KEYMOD_ALT | KEYMOD_SHIFT)))
    {
        *pfEaten = TRUE;
        // 部署指纹（勿删）：grep 构建产物 = 编进去了、grep 部署目录 = 换上了、
        // 真机日志出现本行 = 运行时确实加载了新 DLL。C++ 侧「看起来成功但没换二进制」
        // 已让本仓空转过好几轮真机验证。
        WIND_LOG_DEBUG_FMT(L"cross-mode jumpout: vk=0x%02X depth=%d chinese=%d\n",
                           (uint32_t)wParam, _pairPendingDepth, isChineseMode ? 1 : 0);
        _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers,
                        CHotkeyManager::ClassifyInputKey(wParam, modifiers),
                        isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"pair_jumpout_forward");
        return S_OK;
    }

    // 配对状态保活。**必须在上面的陈旧判定之后**，否则每次按键都先把自己刷新掉，TTL 永不触发。
    // 放在这里能覆盖英文模式的普通字母——协调器在英文模式下收不到它们，只有 DLL 看得全，
    // 这也是 TTL 判据必须以 DLL 侧为准的原因之一。
    TouchPairState();

    if (hasInputSession || isChineseMode)
    {
        // Ctrl/Alt combos during active input session: intercept so OnKeyDown can
        // send to Go for state cleanup, then pass through to the host application.
        // This prevents dangling composition state when user presses Ctrl+S, Ctrl+C, etc.
        // Note: registered hotkeys (Ctrl+`, Shift+Space) are already caught above.
        // IMPORTANT: Exclude modifier keys themselves (VK_CONTROL, VK_MENU, etc.) —
        // pressing Ctrl alone should NOT trigger cleanup, otherwise Ctrl+number (pin)
        // and Ctrl+Shift+number (delete) candidate shortcuts break because the
        // composition is cleared before the number key arrives.
        bool isModifierKeyItself = (wParam == VK_CONTROL || wParam == VK_LCONTROL || wParam == VK_RCONTROL ||
                                    wParam == VK_MENU    || wParam == VK_LMENU    || wParam == VK_RMENU ||
                                    wParam == VK_SHIFT   || wParam == VK_LSHIFT   || wParam == VK_RSHIFT);
        if (hasInputSession && (modifiers & (KEYMOD_CTRL | KEYMOD_ALT)) && !isModifierKeyItself)
        {
            WIND_LOG_DEBUG_FMT(L"OnTestKeyDown: Ctrl/Alt during session, eating for cleanup: vk=0x%02X\n",
                         (uint32_t)wParam);
            *pfEaten = TRUE;
            _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::Hotkey,
                            isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"ctrl_alt_cleanup");
            return S_OK;
        }

        HotkeyType keyType = CHotkeyManager::ClassifyInputKey(wParam, modifiers);

        if (keyType == HotkeyType::Backspace || keyType == HotkeyType::Enter ||
            keyType == HotkeyType::Escape || keyType == HotkeyType::Space ||
            keyType == HotkeyType::CursorKey)
        {
            // Only intercept if we have composition or active input session
            // These keys should pass through when there's no active input
            if (hasInputSession)
            {
                *pfEaten = TRUE;
                _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, keyType,
                                isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"session_key");
                return S_OK;
            }
            // 中文+全角：无 input session 时也需拦截 Space，让 core 走全角转换（U+3000）。
            // 与下方 Number 的 chinese_fullwidth_number 例外同源——core 侧空缓冲空格早已
            // 正确走标点流水线，但此处不吃就永远送不到，故全角空格只在恰好有 session /
            // resync 窗口内时才灵，表现为「有时全角有时半角」。
            if (isChineseMode && keyType == HotkeyType::Space && _pTextService->IsFullWidth())
            {
                *pfEaten = TRUE;
                _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, keyType,
                                isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"chinese_fullwidth_space");
                return S_OK;
            }
        }
        else if (keyType == HotkeyType::Number || keyType == HotkeyType::Tab ||
                 keyType == HotkeyType::PageKey || keyType == HotkeyType::SelectKey)
        {
            // Session-only keys: Go returns PassThrough without active input,
            // and some apps (WindTerm) don't handle the OnTestKeyDown(TRUE) +
            // OnKeyDown(FALSE) flip correctly, causing the key to be swallowed.
            if (hasInputSession)
            {
                *pfEaten = TRUE;
                _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, keyType,
                                isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"session_select_or_page");
                return S_OK;
            }
            // 软键盘开着：数字行的键位要能出符号，故无 input session 时也拦。
            // 与下面的全角例外同源——软键盘不组码、不产生 session，不在此拦就永远送不到
            // 协调器，表现为「面板上数字行画着符号，敲下去却出了半角数字」。
            if (isChineseMode && keyType == HotkeyType::Number && _pTextService->IsSoftKeyboard())
            {
                *pfEaten = TRUE;
                _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, keyType,
                                isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"softkeyboard_number");
                return S_OK;
            }
            // 中文+全角：无 input session 时也需拦截 Number, 让 Go 走全角转换。
            // 否则数字直通到应用得到半角, 仅在记事本(IMM32 兼容层)恰好正确,
            // VS Code/Chrome/WPS/Word 等纯 TSF 应用都会出错。
            if (isChineseMode && keyType == HotkeyType::Number && _pTextService->IsFullWidth())
            {
                *pfEaten = TRUE;
                _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, keyType,
                                isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"chinese_fullwidth_number");
                return S_OK;
            }
        }
        else if (keyType == HotkeyType::Letter)
        {
            // 中文 + CapsLock ON + 非全角 + 无 input session：字母走真正的同步透传
            // （与英文模式同构）。不吃键 → 系统按 CapsLock 自然产生大写、Shift 抵消产生
            // 小写，同时保留 WM_KEYDOWN 供 CAD 等依赖原始按键的快捷键使用。
            //
            // 关键：必须在 OnTestKeyDown 阶段就不吃，否则形成 OnTestKeyDown(TRUE)+
            // OnKeyDown(FALSE) 的"吃了再吐"翻转——Chrome/WindTerm/Electron 等宿主不会
            // 回退合成 WM_CHAR，会直接吞掉字母（"部分应用大写下无法输入字母"的根因）。
            // 仅 Go 层返回 PassThrough 不够，因为吃键决策发生在 IPC 之前的本步。
            //
            // 有 composition/candidates 时仍需拦截：让 Go 先提交候选再输出字母。
            // 全角时也需拦截：让 Go 走全角转换。
            if (!hasInputSession && !_pTextService->IsFullWidth() &&
                (GetKeyState(VK_CAPITAL) & 0x0001))
            {
                _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, keyType,
                                isChineseMode, hasComposition, _hasCandidates, hasInputSession, FALSE,
                                L"chinese_capslock_letter_passthrough");
                return S_OK; // pfEaten 保持 FALSE → 同步透传
            }
            // Letters: always eat in Chinese mode (they start composition)
            *pfEaten = TRUE;
            _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, keyType,
                            isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"chinese_letter");
            return S_OK;
        }
        else if (keyType == HotkeyType::Punctuation)
        {
            // 中文 + CapsLock ON + 非全角 + 无 input session：标点与字母键对齐，直接透传。
            // 设计软件（CAD/EDA 等）通过 WM_KEYDOWN 触发快捷功能；CommitText 仅产生
            // WM_CHAR，不会激活依赖原始键值的功能。同时避免输出中文标点。
            // 有 input session 时仍须拦截：让 coordinator 先提交候选再处理标点。
            if (!hasInputSession && !_pTextService->IsFullWidth() &&
                (GetKeyState(VK_CAPITAL) & 0x0001))
            {
                _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, keyType,
                                isChineseMode, hasComposition, _hasCandidates, hasInputSession, FALSE,
                                L"chinese_capslock_punct_passthrough");
                return S_OK; // pfEaten 保持 FALSE → 同步透传
            }
            // Punctuation: always eat in Chinese mode.
            // Go always handles punctuation (returns InsertText), so the
            // OnTestKeyDown(TRUE) + OnKeyDown(TRUE) path is safe.
            *pfEaten = TRUE;
            _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, keyType,
                            isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"chinese_punctuation");
            return S_OK;
        }
    }
    // English mode + full-width: intercept printable characters for full-width conversion
    else if (!isChineseMode && _pTextService->IsFullWidth())
    {
        // Intercept printable ASCII keys (letters, numbers, punctuation, space)
        // so Go can convert them to full-width characters
        HotkeyType keyType = CHotkeyManager::ClassifyInputKey(wParam, modifiers);
        if (keyType == HotkeyType::Letter || keyType == HotkeyType::Number ||
            keyType == HotkeyType::Punctuation || keyType == HotkeyType::Space)
        {
            *pfEaten = TRUE;
            _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, keyType,
                            isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"english_fullwidth");
            return S_OK;
        }
    }
    // 英文模式 + 半角：只吃「英半列有自定义标点映射」的标点键（core 经
    // CONFIG_KEY_CUSTOM_EN_PUNCT 推送字符集合），交给 core 按英半列出字。
    // 不吃的话该键直接透传、core 永远收不到，用户配的英半列就是个打不到的死格。
    // 集合为空（未启用自定义映射）→ 判据立即返回 FALSE，行为与历史完全一致。
    else if (!isChineseMode && _IsCustomEnglishPunctKey(wParam, modifiers))
    {
        *pfEaten = TRUE;
        _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers,
                        CHotkeyManager::ClassifyInputKey(wParam, modifiers),
                        isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE,
                        L"english_custom_punct");
        return S_OK;
    }
    // else: not in Chinese mode and no input session — pass through

    // Track digit pass-through for smart punctuation fallback.
    // When digits pass through without reaching Go (no input session),
    // record them so the next punctuation key sent to Go carries this info via prevChar.
    // This handles editors (e.g., EverEdit) where ITfTextEditSink can't read text.
    //
    // 中文模式空缓冲下数字键（含小键盘，ClassifyInputKey 归 Number）不被吃，必经此处。
    // 判据统一交给 _DigitCharFromVk：非数字键返回 0，正好落进「清零」语义。
    if (*pfEaten == FALSE)
    {
        _lastPassthroughDigit = _DigitCharFromVk(wParam, modifiers);
    }

    return S_OK;
}

STDAPI CKeyEventSink::OnKeyDown(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten)
{
    *pfEaten = FALSE;

    // 合成提交触发键：这里才是真正的提交点——TSF 认可的"按键处理期间"，
    // CommitText 内部据此走 TF_ES_SYNC 而非 nonKeyContext 的异步会话。
    // 见 KeyEventSink.h 的 QueueAsyncCommitViaSyntheticKey 注释与
    // CTextService::CommitTextViaSyntheticKey 的调用点。
    {
        PendingAsyncCommit pending;
        if (_TryConsumeAsyncCommitTrigger(wParam, pending))
        {
            *pfEaten = TRUE;
            WIND_LOG_DEBUG_FMT(L"AsyncCommitTrigger: committing via synchronous edit session, textLen=%zu\n",
                               pending.text.length());
            _pTextService->CommitText(pending.text, /*nonKeyContext=*/FALSE, pending.replacingHeld);
            if (!_pendingAsyncCommits.empty())
            {
                // 队列里还有后续提交（连续快速点选）：再次自注入触发键，让它在自己的
                // OnKeyDown 周期里执行，保持"一次按键一次提交"的语义。
                if (!_SendAsyncCommitTriggerKey())
                {
                    WIND_LOG_WARN(L"AsyncCommitTrigger: chained trigger key injection failed, dropping remaining commits\n");
                    _pendingAsyncCommits.clear();
                }
            }
            return S_OK;
        }
    }

    // Ctrl+Shift+F12: Dump TSF ring buffer logs to clipboard (debug aid for AppContainer)
    if (wParam == VK_F12 && (GetKeyState(VK_CONTROL) & 0x8000)
        && (GetKeyState(VK_SHIFT) & 0x8000) && !(GetKeyState(VK_MENU) & 0x8000))
    {
        *pfEaten = TRUE;
        CFileLogger& lg = CFileLogger::Instance();

        // 顺带重读配置文件。DLL 在宿主进程内常驻、构造函数只跑一次 ⇒ 没有这一步，
        // 改完 mode/level 必须完全退出宿主才生效，而那是取证时最高频的操作。
        lg.ReloadConfig();

        // 环形缓冲为空时**不碰剪贴板**（否则会把用户原有内容清成空串），但提示照给：
        // 这个热键现在同时承担「重读配置」，没有反馈就分不清是没生效还是没日志。
        std::wstring logs = lg.DumpRingBuffer();
        const wchar_t* notice = L"[WindInput 配置已重读 · 环形缓冲为空]";
        if (!logs.empty() && OpenClipboard(nullptr))
        {
            EmptyClipboard();
            size_t cbSize = (logs.size() + 1) * sizeof(wchar_t);
            HGLOBAL hMem = GlobalAlloc(GMEM_MOVEABLE, cbSize);
            if (hMem)
            {
                wchar_t* pDst = (wchar_t*)GlobalLock(hMem);
                if (pDst)
                {
                    memcpy(pDst, logs.c_str(), cbSize);
                    GlobalUnlock(hMem);
                    SetClipboardData(CF_UNICODETEXT, hMem);
                    notice = L"[WindInput 配置已重读 · 日志已复制]";
                }
            }
            CloseClipboard();
        }
        // Brief notification via SendInput so user knows it worked
        _pTextService->InsertText(notice);
        return S_OK;
    }

    // Update modifier state machine for this KeyDown event
    _UpdateModsOnKeyDown(wParam);

    // Check barrier timeout
    _CheckBarrierTimeout();

    // 密码框强制英文抑制：与 OnTestKeyDown 的同款守卫成对。Chrome/QQ 等宿主会无视
    // OnTestKeyDown 的 pfEaten=FALSE 仍调用本函数（见下方 policy 早期闸门的同类注释），
    // 此处不挡则按键仍会流进英文配对引擎 / IPC，抑制形同虚设。
    if (_pTextService->IsPasswordSuppressActive())
        return S_OK;

    // 英文自动配对**不再在此本地处理**：OnTestKeyDown 已按 `PairEngine::ShouldEat` 吃下配对键，
    // 这里直接落到下方的 `_SendKeyToService`，由 core 出字 + 记栈（与「英半自定义标点」同一条路）。
    //
    // 之所以搬走：配对状态原先分散在 core 的 pair_tracker 与这里的英文栈两处，谁也看不见谁，
    // 于是中文里打的配对切到英文跳不出、反之亦然。现在四条建立路径全部入 core 的那一个栈，
    // 本文件只留吃键判据与 `_pairPendingDepth` 闸门。
    //
    // IPC 断连时的兜底见下方 `ipc_failed_*` 分支：那时不能吐成 pfEaten=FALSE（已经吃了），
    // 否则不补发 WM_KEYDOWN 的宿主直接丢字符。

    // For function hotkeys (like Ctrl+`), use normalized modifiers (no left/right distinction)
    //
    // 只算归一化这一份：本函数下游的每一处查表都用它。此处原先还并列着一份非归一化的
    // `keyHash`，自「统一走归一化」之后就无人读了——留着会让排查「热键配了没反应」的人
    // 以为这里还有第二条匹配路径，而 hash 失配恰恰是那类问题的头号嫌疑。
    uint32_t modifiers = CHotkeyManager::GetCurrentModifiers();
    uint32_t normalizedMods = CHotkeyManager::NormalizeModifiers(modifiers);
    uint32_t normalizedKeyHash = CHotkeyManager::CalcKeyHash(normalizedMods, (uint32_t)wParam);

    CHotkeyManager* pHotkeyMgr = _pTextService->GetHotkeyManager();

    // Check if this is a KeyUp triggered key (toggle mode keys like Shift, Ctrl, CapsLock)
    // Use hash-based lookup first, then fallback to VK-based detection
    //
    // TSF sends generic VK_SHIFT/VK_CONTROL as wParam, but the hotkey whitelist
    // registers specific VK_LSHIFT/VK_RSHIFT/VK_LCONTROL/VK_RCONTROL.
    // Resolve the generic VK to specific left/right variant for proper hash matching.
    BOOL isToggleModeKey = FALSE;
    uint32_t resolvedVK = (uint32_t)wParam;
    // 优先用 modifiers 参数解析左右键。modifiers 由 GetCurrentModifiers 计算（使用
    // GetAsyncKeyState OR GetKeyState 双源），更可靠。
    // GetAsyncKeyState 在 WebView2 / Wails / 部分 Chromium 宿主进程里对 VK_LSHIFT/RSHIFT
    // 返回 0，导致解析失败 → Shift 切换中英文无效。modifiers fallback 解决该兼容性问题。
    if (wParam == VK_SHIFT)
    {
        if (modifiers & KEYMOD_LSHIFT)
            resolvedVK = VK_LSHIFT;
        else if (modifiers & KEYMOD_RSHIFT)
            resolvedVK = VK_RSHIFT;
        else if (GetAsyncKeyState(VK_LSHIFT) & 0x8000)
            resolvedVK = VK_LSHIFT;
        else if (GetAsyncKeyState(VK_RSHIFT) & 0x8000)
            resolvedVK = VK_RSHIFT;
    }
    else if (wParam == VK_CONTROL)
    {
        if (modifiers & KEYMOD_LCTRL)
            resolvedVK = VK_LCONTROL;
        else if (modifiers & KEYMOD_RCTRL)
            resolvedVK = VK_RCONTROL;
        else if (GetAsyncKeyState(VK_LCONTROL) & 0x8000)
            resolvedVK = VK_LCONTROL;
        else if (GetAsyncKeyState(VK_RCONTROL) & 0x8000)
            resolvedVK = VK_RCONTROL;
    }
    uint32_t keyUpHash = CHotkeyManager::CalcKeyHash(modifiers, resolvedVK);

    // ⛔ CapsLock 的会话态绑定不在 TSF 处理，改由服务进程的 WH_KEYBOARD_LL 钩子拦截。
    // 三版失败的原因与判据见 OnTestKeyDown 里的同位置注释。
    if (pHotkeyMgr != nullptr && pHotkeyMgr->IsKeyUpHotkey(keyUpHash))
    {
        isToggleModeKey = TRUE;
    }
    else if ((pHotkeyMgr == nullptr || !pHotkeyMgr->HasHotkeys()) && CHotkeyManager::IsToggleModeKeyByVK(wParam))
    {
        // Fallback: only use VK-based detection when hotkey whitelist hasn't been loaded yet
        isToggleModeKey = TRUE;
    }

    if (isToggleModeKey)
    {
        // CapsLock has its own special handling in OnKeyUp, don't set pending here
        if (wParam == VK_CAPITAL)
        {
            // Just consume the KeyDown, let OnKeyUp handle it
            _pTextService->NoteCapsLockKeyActivity(); // 供 OPENCLOSE 联动噪声抑制
            *pfEaten = TRUE;
            return S_OK;
        }

        // Check if this is a key repeat (bit 30 of lParam)
        if (lParam & 0x40000000)
        {
            // Key repeat, ignore
            *pfEaten = TRUE;
            return S_OK;
        }

        // Check if other modifiers are pressed (e.g., Ctrl+Shift is a system shortcut)
        // 用 modifiers 双源参数为主，GetAsyncKeyState 降级；WebView2 等宿主下 GetAsyncKeyState
        // 不可靠，会误判"无其它修饰"，导致 Ctrl+Shift 等系统组合被吞作切换。
        BOOL hasOtherModifier = FALSE;
        if (wParam == VK_SHIFT || wParam == VK_LSHIFT || wParam == VK_RSHIFT)
        {
            hasOtherModifier = (modifiers & (KEYMOD_CTRL | KEYMOD_ALT))
                            || (GetAsyncKeyState(VK_CONTROL) & 0x8000)
                            || (GetAsyncKeyState(VK_MENU) & 0x8000);
        }
        else if (wParam == VK_CONTROL || wParam == VK_LCONTROL || wParam == VK_RCONTROL)
        {
            hasOtherModifier = (modifiers & (KEYMOD_SHIFT | KEYMOD_ALT))
                            || (GetAsyncKeyState(VK_SHIFT) & 0x8000)
                            || (GetAsyncKeyState(VK_MENU) & 0x8000);
        }

        if (hasOtherModifier)
        {
            _pendingKeyUpKey = 0;
            _pendingKeyUpModifiers = 0;
            return S_OK;  // Let system handle it
        }

        // Block toggle when there is no real text input context and no active input session.
        // Chrome calls OnKeyDown even when OnTestKeyDown returned pfEaten=FALSE, so this
        // guard must be repeated here to prevent _pendingKeyUpKey from being set.
        {
            BOOL hasSession = _pTextService->HasActiveComposition() || _hasCandidates;
            BOOL hasTextCtx = _pTextService->RefreshTextInputContext();
            WIND_LOG_DEBUG_FMT(L"compat.toggle_key down: vk=0x%02X resolvedVK=0x%02X mods=0x%04X hasSession=%d hasTextCtx=%d",
                (uint32_t)wParam, resolvedVK, modifiers, (int)hasSession, (int)hasTextCtx);
            if (!hasSession && !hasTextCtx)
            {
                *pfEaten = FALSE;
                _LogKeyDecision(L"down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::ToggleMode,
                                _pTextService->IsChineseMode(), FALSE, _hasCandidates,
                                FALSE, FALSE, L"toggle_no_text_ctx");
                WindLogForegroundProcessInfo(4, L"compat.toggle_no_textctx.host");
                return S_OK;
            }
        }

        // Mark key as pending for KeyUp toggle (Shift/Ctrl only, not CapsLock)
        _MarkPendingToggleKey(wParam, modifiers);

        WIND_LOG_DEBUG(L"OnKeyDown: Toggle mode key pending for KeyUp\n");

        // 同 OnTestKeyDown：纯修饰键放行。待切换状态已记在 _pendingKeyUpKey 上，
        // 放行不影响 keyup 时的切换判定。
        *pfEaten = !_IsPureModifierKey(wParam);
        return S_OK;
    }

    // Any other key cancels pending toggle
    _pendingKeyUpKey = 0;
    _pendingKeyUpModifiers = 0;

    // Check if context is read-only
    if (_IsContextReadOnly(pContext))
    {
        _LogKeyDecision(L"down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::None,
                        _pTextService->IsChineseMode(), _pTextService->HasActiveComposition(), _hasCandidates,
                        _pTextService->HasActiveComposition() || _hasCandidates, FALSE, L"context_readonly");
        return S_OK;
    }

    // Ctrl+Space 兜底切换：只在 OnTestKeyDown 确实吃下过这个键时执行。
    //
    // 这条路径曾以「实测从未执行」为由删除（e152da9b），但那次实测只采样了系统 Ctrl+Space
    // 热键正常的机器——那种机器上 msctf 在 keystroke sink 之下就消费了该键，OnKeyDown 确实
    // 不会被调用。而系统热键实质失效的机器上它会被调用，此时 compartment 永远不翻，
    // OnChange 那条路等不到任何东西，整个功能无人接管。
    // 真机日志指纹（QQ / Maxthon / Totalcmd，2026-08-28）：
    //   test_down eaten=1 decision=ctrl_space_intercept
    //   down      eaten=0 decision=passthrough_not_handled
    //   且全程 0 条 compat.openclose.onchange，State synced 的 mode 恒定不变。
    //
    // ⚠ 双切换为什么不会发生：判据不是时间窗也不是猜测，而是 TSF 契约——OnKeyDown 只在
    // OnTestKeyDown 返回 pfEaten=TRUE 之后才被调用，吃下该键就意味着 msctf 不会再拿它当
    // 热键。_ctrlSpaceEatenInTest 就是这份独占凭据，同时也兑现了吃键集不变量（test 吃了，
    // down 就必须干活）——此前 test 吃下、down 却 passthrough，键既没切换也没落进宿主。
    if (_ctrlSpaceEatenInTest && wParam == VK_SPACE
        && (modifiers & KEYMOD_CTRL) && !(modifiers & (KEYMOD_ALT | KEYMOD_SHIFT)))
    {
        *pfEaten = TRUE;
        // 长按 auto-repeat：吃掉但不切换。判据与上方 toggle_mode_key 分支同源
        // （lParam bit30）——少了它，按住 Ctrl+Space 不放会让中英模式连续翻转。
        if (lParam & 0x40000000)
            return S_OK;
        _ctrlSpaceEatenInTest = FALSE;
        _pTextService->ToggleModeFromKey();
        _LogKeyDecision(L"down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::None,
                        _pTextService->IsChineseMode(), _pTextService->HasActiveComposition(), _hasCandidates,
                        _pTextService->HasActiveComposition() || _hasCandidates, TRUE, L"ctrl_space_toggle");
        return S_OK;
    }

    // Policy 早期闸门：Chrome / QQ 等宿主会无视 OnTestKeyDown 的 pfEaten=FALSE 仍调用
    // OnKeyDown。这里对不满足 policy 的 chineseOnly / session 热键直接 return FALSE，
    // 否则下方 isKeyDownHotkey 命中会把键发给 Go，触发 Go HandleKeyEvent 顶部的 AddWord
    // 匹配（该匹配位于 mode 判定之前）造成英文模式误进 AddWord。
    if (pHotkeyMgr != nullptr)
    {
        BOOL chineseMode = _pTextService->IsChineseMode();
        if (pHotkeyMgr->IsKeyDownChineseOnlyHotkey(normalizedKeyHash) && !chineseMode)
        {
            WIND_LOG_DEBUG_FMT(L"OnKeyDown chinese-only hotkey skipped (english mode): vk=0x%02X\n",
                               (uint32_t)wParam);
            *pfEaten = FALSE;
            return S_OK;
        }
        if (pHotkeyMgr->IsKeyDownSessionHotkey(normalizedKeyHash))
        {
            BOOL hasSession = _pTextService->HasActiveComposition() || _hasCandidates;
            if (!chineseMode || !hasSession)
            {
                WIND_LOG_DEBUG_FMT(L"OnKeyDown session hotkey skipped (chinese=%d session=%d): vk=0x%02X\n",
                                   (int)chineseMode, (int)hasSession, (uint32_t)wParam);
                *pfEaten = FALSE;
                return S_OK;
            }
        }
    }

    // Check if this is a KeyDown hotkey from whitelist
    // Use normalized hash for function hotkeys (Ctrl+`, Shift+Space, etc.)
    // 三个列表统一识别，避免 Ctrl/Alt cleanup 路径把 chinese-only / session 热键当成
    // 无关的 Ctrl 组合键去吃掉。
    BOOL isKeyDownHotkey = (pHotkeyMgr != nullptr && (
                                pHotkeyMgr->IsKeyDownHotkey(normalizedKeyHash) ||
                                pHotkeyMgr->IsKeyDownChineseOnlyHotkey(normalizedKeyHash) ||
                                pHotkeyMgr->IsKeyDownSessionHotkey(normalizedKeyHash)));

    // Check for basic input keys
    // IMPORTANT: Different handling based on key type:
    // - Letter/number/punctuation keys: intercept in Chinese mode (start new composition)
    // - Backspace/Enter/Escape: only intercept when there's an active composition or input session
    //   (otherwise, pass through to application)
    BOOL isInputKey = FALSE;
    BOOL isChineseMode = _pTextService->IsChineseMode();
    // Use TextService's composition state - this is the source of truth in async architecture
    BOOL hasComposition = _pTextService->HasActiveComposition();
    // 与 OnTestKeyDown 同一判据（见 _HasInputSession 定义）：那边吃了键才会调到本函数，
    // 这边若判「无会话」，Backspace/Enter/Escape/CursorKey 会落成 isInputKey=FALSE，
    // 形成「吃了再吐」——严格 TSF 宿主会直接丢键（见 project_fullwidth_eat_flip 的教训）。
    BOOL hasInputSession = _HasInputSession();

    // 与 OnTestKeyDown 对称：中文 + CapsLock ON + 非全角 + 无 session 的字母同步透传
    // （不吃、不发 Go），由系统按 CapsLock 产生大写字母 + 保留 WM_KEYDOWN 供 CAD 快捷键
    // 使用。OnTestKeyDown 已对此场景 pfEaten=FALSE；此处保持一致，避免漏网字母被下方
    // "state_change_letter_consume" 兜底逻辑误吃。
    BOOL capsLockLetterPassthrough =
        isChineseMode && !hasInputSession && !_pTextService->IsFullWidth() &&
        (wParam >= 'A' && wParam <= 'Z') && !(modifiers & (KEYMOD_CTRL | KEYMOD_ALT)) &&
        (GetKeyState(VK_CAPITAL) & 0x0001);

    // 与字母键对称：中文 + CapsLock ON + 非全角 + 无 session 的标点也同步透传。
    // 设计软件依赖原始 WM_KEYDOWN 激活功能，CommitText 只产生 WM_CHAR 无法触发。
    BOOL capsLockPunctPassthrough =
        isChineseMode && !hasInputSession && !_pTextService->IsFullWidth() &&
        !(modifiers & (KEYMOD_CTRL | KEYMOD_ALT)) &&
        (CHotkeyManager::ClassifyInputKey(wParam, modifiers) == HotkeyType::Punctuation) &&
        (GetKeyState(VK_CAPITAL) & 0x0001);

    // Track whether this is a Ctrl/Alt combo that needs cleanup-then-passthrough
    BOOL isCtrlAltCleanup = FALSE;

    // 配对跳出键的统一判据，**必须与 OnTestKeyDown 的 pair_jumpout_forward 逐条一致**。
    //
    // ⚠️ 必须在 `hasInputSession || isChineseMode` 这个门**之外**放行：英文模式没有输入会话，
    // 判据写在门里面就永远进不去 —— 那边吃了、这边不发，键凭空消失。真机实测正是如此：
    // 日志里 `cross-mode jumpout ... depth=1 chinese=0` 连打 45 次（每次都吃），而同一时段
    // core 侧一条日志都没有（根本没收到），用户看到的现象就是「英文下 Tab/Enter 全无反应」。
    BOOL isPairJumpOut = (_pairPendingDepth > 0 && !IsPairStateStale()
                          && _IsJumpOutKey((UINT)wParam)
                          && !(modifiers & (KEYMOD_CTRL | KEYMOD_ALT | KEYMOD_SHIFT)));
    if (isPairJumpOut)
        isInputKey = TRUE;

    if (hasInputSession || isChineseMode)
    {
        // Ctrl/Alt combos during active input session: mark as input key so we can
        // send to Go for state cleanup. After response, we'll override pfEaten=FALSE.
        // Note: registered hotkeys are already caught by isKeyDownHotkey above.
        // IMPORTANT: Exclude modifier keys themselves — pressing Ctrl/Alt alone should
        // not trigger cleanup, to preserve Ctrl+number and Ctrl+Shift+number shortcuts.
        bool isModifierKeyItself = (wParam == VK_CONTROL || wParam == VK_LCONTROL || wParam == VK_RCONTROL ||
                                    wParam == VK_MENU    || wParam == VK_LMENU    || wParam == VK_RMENU ||
                                    wParam == VK_SHIFT   || wParam == VK_LSHIFT   || wParam == VK_RSHIFT);
        if (hasInputSession && (modifiers & (KEYMOD_CTRL | KEYMOD_ALT)) && !isKeyDownHotkey && !isModifierKeyItself)
        {
            isInputKey = TRUE;
            isCtrlAltCleanup = TRUE;
            WIND_LOG_DEBUG_FMT(L"OnKeyDown: Ctrl/Alt during session, sending to Go for cleanup: vk=0x%02X\n",
                         (uint32_t)wParam);
        }
        else
        {
            HotkeyType keyType = CHotkeyManager::ClassifyInputKey(wParam, modifiers);

            // 配对跳出键复用门外算好的判据。**必须留在本链首位**：否则中文模式无会话时
            // 会被下面那条 `isInputKey = hasInputSession` 覆盖回 FALSE，又变成吃了不发。
            if (isPairJumpOut)
            {
                isInputKey = TRUE;
            }
            // Backspace, Enter, Escape, CursorKey should only be intercepted when there's an active composition or input session
            // Otherwise they should pass through to the application
            else if (keyType == HotkeyType::Backspace || keyType == HotkeyType::Enter ||
                keyType == HotkeyType::Escape || keyType == HotkeyType::CursorKey)
            {
                isInputKey = hasInputSession;  // Only intercept if we have composition or input session
            }
            else
            {
                // CapsLock 字母/标点透传场景不视为输入键（保持 pfEaten=FALSE 同步透传，不发 Go）
                isInputKey = (capsLockLetterPassthrough || capsLockPunctPassthrough) ? FALSE : (keyType != HotkeyType::None);
            }
        }
    }
    // English mode + full-width: intercept printable characters for full-width conversion
    else if (!isChineseMode && _pTextService->IsFullWidth())
    {
        HotkeyType keyType = CHotkeyManager::ClassifyInputKey(wParam, modifiers);
        if (keyType == HotkeyType::Letter || keyType == HotkeyType::Number ||
            keyType == HotkeyType::Punctuation || keyType == HotkeyType::Space)
        {
            isInputKey = TRUE;
        }
    }
    // 英文模式 + 半角 + 该键配了英半列：与 OnTestKeyDown 的 english_custom_punct 分支成对，
    // 必须同条件放行转发，否则那边吃了、这边不发 → 键彻底丢失。
    else if (!isChineseMode && _IsCustomEnglishPunctKey(wParam, modifiers))
    {
        isInputKey = TRUE;
    }
    // 英文模式 + 半角 + 该键是配对字符：与 OnTestKeyDown 的 english_autopair 分支成对，
    // 同理必须同条件放行。英文配对改由 core 出字 + 记栈后，这条放行就是它唯一的转发出口——
    // 少了它就是「OnTestKeyDown 吃下、OnKeyDown 不发」，键彻底消失。
    else if (!isChineseMode && !_pTextService->IsFullWidth()
             && !(modifiers & (KEYMOD_CTRL | KEYMOD_ALT))
             && _englishPairEngine.ShouldEat(
                    _MapVkToEnglishPairChar(wParam, (modifiers & KEYMOD_SHIFT) != 0)))
    {
        isInputKey = TRUE;
    }

    if (!isKeyDownHotkey && !isInputKey)
    {
        // CRITICAL FIX: If OnTestKeyDown decided to eat this key (based on the state
        // at that time), but now the state has changed (e.g., _isComposing became FALSE
        // after a commit), we STILL need to consume the key to maintain consistency.
        // Otherwise, the key will be passed to the application unexpectedly.
        //
        // This can happen during fast typing: "d<space>d" where:
        // 1. OnTestKeyDown('d') sees _isComposing=TRUE, returns pfEaten=TRUE
        // 2. Space key IPC returns, sets _isComposing=FALSE
        // 3. OnKeyDown('d') now sees _isComposing=FALSE, but must still consume 'd'
        //
        // We detect this by checking if we're in Chinese mode and this is a letter key.
        // 但 CapsLock 透传字母例外：OnTestKeyDown 已主动不吃它，这里不能反过来强制消费，
        // 否则字母既不发 Go 也被吃掉 → 彻底丢失。
        if (isChineseMode && wParam >= 'A' && wParam <= 'Z' && !(modifiers & (KEYMOD_CTRL | KEYMOD_ALT))
            && !capsLockLetterPassthrough)
        {
            // Letter key in Chinese mode slipped through due to state change - consume it
            *pfEaten = TRUE;
            _LogKeyDecision(L"down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::Letter,
                            isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"state_change_letter_consume");
        }
        else
        {
            _LogKeyDecision(L"down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::None,
                            isChineseMode, hasComposition, _hasCandidates, hasInputSession, FALSE, L"passthrough_not_handled");
        }
        return S_OK;
    }

    // 有待重开的余码组合而用户已按下新键（快打）：先把余码组合开出来，避免与后续输入错序。
    if (_pTextService->HasDeferredComposition())
    {
        _pTextService->StartDeferredCompositionIfPending();
    }

    // Update caret position before sending key event
    // This ensures the candidate window appears at the correct position
    _pTextService->SendCaretPositionUpdate();

    // Send key to Go Service using binary protocol (SYNC mode)
    if (!_SendKeyToService((uint32_t)wParam, modifiers, KEY_EVENT_DOWN))
    {
        WIND_LOG_ERROR(L"Failed to send key to service");
        WIND_LOG_DEBUG_FMT(
            L"compat.ipc_send_failed focusSession=%llu vk=0x%02X mods=0x%04X chinese=%d composing=%d candidates=%d",
            _pTextService->GetFocusSessionId(), (uint32_t)wParam, modifiers,
            isChineseMode ? 1 : 0, hasComposition ? 1 : 0, _hasCandidates ? 1 : 0
        );
        WindLogForegroundProcessInfo(4, L"compat.ipc_send_failed.foreground_host");

        // Service not available - pass through letters directly
        if (wParam >= 'A' && wParam <= 'Z' && !(modifiers & (KEYMOD_CTRL | KEYMOD_ALT)))
        {
            std::wstring ch;
            if (modifiers & KEYMOD_SHIFT)
                ch = (wchar_t)wParam;                      // Shift held: uppercase
            else
                ch = (wchar_t)towlower((wint_t)wParam);    // No Shift: lowercase
            _pTextService->InsertText(ch);
            *pfEaten = TRUE;
            _LogKeyDecision(L"down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::Letter,
                            isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"ipc_failed_fallback_insert");
        }
        else
        {
            // 英文配对键的断连兜底：OnTestKeyDown 已按 ShouldEat 吃下它等 core 出字，此处
            // 若吐成 pfEaten=FALSE 就是「吃了再吐」——记事本/Chromium 会补发 WM_KEYDOWN，
            // EverEdit 这类不补发的宿主直接丢字符。故降级为本地插入**单个字符**：
            // 不配对、不记栈（也就不会留下需要清理的状态），断连期间「无配对但不丢字」。
            bool hasShift = (modifiers & KEYMOD_SHIFT) != 0;
            wchar_t pairChar = _MapVkToEnglishPairChar(wParam, hasShift);
            if (!isChineseMode && !_pTextService->IsFullWidth()
                && !(modifiers & (KEYMOD_CTRL | KEYMOD_ALT))
                && pairChar != 0 && _englishPairEngine.ShouldEat(pairChar))
            {
                std::wstring ch(1, pairChar);
                _pTextService->InsertText(ch);
                *pfEaten = TRUE;
                _LogKeyDecision(L"down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::None,
                                isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"ipc_failed_pair_insert");
            }
            else
            {
                // 其余非字母按键（符号、标点等）：放行给应用程序处理
                *pfEaten = FALSE;
                _LogKeyDecision(L"down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::None,
                                isChineseMode, hasComposition, _hasCandidates, hasInputSession, FALSE, L"ipc_failed_passthrough");
            }
        }
        return S_OK;
    }

    // 智能符号 hold 预览态须**在**响应处理前采样：PassThrough 分支会把组合收口掉，
    // 之后 IsHoldCompositionActive() 就查不到了。
    BOOL holdActiveBeforeResponse = _pTextService->IsHoldCompositionActive();

    // SYNC: Wait for response and handle it directly
    // This is simpler and matches Weasel's architecture
    // 先清零重放标志：只有本次响应置的位才算数（见其声明处说明）。
    _pendingReplayToHost = FALSE;
    *pfEaten = _HandleServiceResponse();

    // ── 联想态回车/退格透传：组合已收口，把这一键还给宿主 ──────────────────────
    // 置位来自 ResponseType::ClearCompositionThenPassThrough（那里解释了为何必须重放而
    // 不是吐 FALSE）。放在 hold 重放之前：本条是服务端**显式声明**的意图，而 hold 那条
    // 是本地按「PassThrough + hold 活跃」推断出来的；两者实际互斥（联想态没有 hold）。
    if (_pendingReplayToHost)
    {
        _pendingReplayToHost = FALSE;
        _ReplayKeyToHost((WORD)wParam);
        *pfEaten = TRUE;
        _LogKeyDecision(L"down", _pTextService->GetFocusSessionId(), wParam, modifiers,
                        CHotkeyManager::ClassifyInputKey(wParam, modifiers),
                        isChineseMode, hasComposition, _hasCandidates, hasInputSession,
                        TRUE, L"assoc_clear_then_replay");
        return S_OK;
    }

    // ── hold 预览态 + 无法代劳的键：吃键 → 收口 → 重放 ─────────────────────────
    // 走到这里意味着服务端回了 PassThrough（缓冲为空），且 _HandleServiceResponse 已在
    // OnKeyDown 这个**合法的文档修改上下文**里把符号同步收口（实测日志
    // `CommitText: TSF atomic commit succeeded`）。
    //
    // 为什么不能直接 return FALSE 让键透传（那是本改动前的行为）：
    //   1. OnTestKeyDown 此前已按「有会话」吃了这个键，这里再吐成 FALSE 就是「吃了再吐」
    //      翻转——记事本/Chromium 会补发，EverEdit 这类不补发的宿主直接丢键（实测
    //      vk=0x0D：test_down eaten=1 → down eaten=0，符号上屏而回车消失）。
    //   2. 就算宿主补发，**组合态活着时回车是宿主的通用语义「确认输入」而非换行**——
    //      任何 IME 都一样。我们的 hold 是预览态，用户并不认为自己在输入中，于是回车
    //      被静默吞掉（实测：只提交符号、不换行）。
    //   3. 也不能靠更早收口来规避：曾在 OnTestKeyDown 里 Flush，写入同样成功、选区也
    //      Collapse 到末尾了，真机却打出 `\n。`——宿主处理「TSF 文档变更」与「WM_KEYDOWN」
    //      是两条独立路径，不保证前者先落地。我们没有任何 TSF 手段能强制它先消化写入。
    //
    // 故把两个动作都收进我们控制的顺序：吃掉原键（与 OnTestKeyDown 的决定一致，无翻转），
    // 再用 SendInput 重放。宿主先看到收口后的文档，再看到一个与组合无关的普通按键。
    if (holdActiveBeforeResponse && !(*pfEaten) && _IsHoldReplayKey(wParam))
    {
        _ReplayKeyToHost((WORD)wParam);
        *pfEaten = TRUE;
        _LogKeyDecision(L"down", _pTextService->GetFocusSessionId(), wParam, modifiers,
                        CHotkeyManager::ClassifyInputKey(wParam, modifiers),
                        isChineseMode, hasComposition, _hasCandidates, hasInputSession,
                        TRUE, L"hold_commit_then_replay");
        return S_OK;
    }

    // ── 配对跳出转发的 desync 兜底 ──────────────────────────────────────────────
    // 本次按 isPairJumpOut 把键吃了，core 却回 PassThrough：说明它那边的配对栈已经空了
    // （失焦清栈 / 归属校验清栈 / 右符号不匹配清栈等），而本地 depth 还挂着 —— 两侧对
    // 「还有没有待跳出的配对」给出了相反答案。
    //
    // 此时直接吐成 FALSE 就是「吃了再吐」，不补发 WM_KEYDOWN 的宿主会丢掉这个 Tab/Enter。
    // 处理：**以 core 为准**把本地 depth 归零（下次不再吃，desync 自愈），并把键原样重放
    // 给宿主，让用户拿到正常的缩进/换行。
    if (isPairJumpOut && !(*pfEaten))
    {
        WIND_LOG_DEBUG_FMT(L"pair jumpout desync: core 未跳出，depth %d -> 0，重放 vk=0x%02X\n",
                           _pairPendingDepth, (uint32_t)wParam);
        _pairPendingDepth = 0;
        _pairLastActivityTick = 0;
        _ReplayKeyToHost((WORD)wParam);
        *pfEaten = TRUE;
        _LogKeyDecision(L"down", _pTextService->GetFocusSessionId(), wParam, modifiers,
                        CHotkeyManager::ClassifyInputKey(wParam, modifiers),
                        isChineseMode, hasComposition, _hasCandidates, hasInputSession,
                        TRUE, L"pair_jumpout_desync_replay");
        return S_OK;
    }

    // 数字键"吃了又吐"缺口修复：有候选时数字键被 TSF eat（pfEaten=TRUE）发给 coordinator，
    // coordinator 返回 PassThrough → OnKeyDown 返 FALSE。此时 OnTestKeyDown 里
    // _lastPassthroughDigit 未设置（因彼时 pfEaten=TRUE 跳过了设置代码）。
    // 在此补设，确保数字后智能标点备用路径在 OnEndEdit 不触发的应用中仍能正确获取 prevChar。
    // 这里只补设不清零：非数字键的清零由 _SendKeyToService 那处负责（它能看到所有
    // 送往服务端的键，包括本处 pfEaten 为真、根本不进这个分支的那些）。
    if (!(*pfEaten))
    {
        if (WCHAR digit = _DigitCharFromVk(wParam, modifiers))
        {
            _lastPassthroughDigit = digit;
        }
    }

    // Ctrl/Alt combo during active session: decide pass-through based on Go's response.
    // If Go handled the key as a candidate operation (pin/delete) and the composition
    // is still active, respect Go's decision and eat the key. Only override to FALSE
    // when Go actually cleared the composition (e.g., Ctrl+S cleanup).
    if (isCtrlAltCleanup && *pfEaten)
    {
        if (_hasCandidates || _isComposing)
        {
            // Go handled it as a candidate action (e.g., Ctrl+number pin/delete),
            // composition still active — keep pfEaten=TRUE to prevent app from seeing the key.
            WIND_LOG_DEBUG(L"OnKeyDown: Ctrl/Alt key handled by Go (session still active), eating key\n");
        }
        else
        {
            // Go cleared composition (cleanup) — pass key through to the host application.
            WIND_LOG_DEBUG(L"OnKeyDown: Ctrl/Alt cleanup done, overriding to pass-through\n");
            *pfEaten = FALSE;
        }
    }

    _LogKeyDecision(L"down", _pTextService->GetFocusSessionId(), wParam, modifiers,
                    isKeyDownHotkey ? HotkeyType::Hotkey : CHotkeyManager::ClassifyInputKey(wParam, modifiers),
                    isChineseMode, hasComposition, _hasCandidates, hasInputSession, *pfEaten,
                    isCtrlAltCleanup && !*pfEaten ? L"ctrl_alt_cleanup_passthrough" : L"service_response");

    return S_OK;
}

STDAPI CKeyEventSink::OnTestKeyUp(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten)
{
    *pfEaten = FALSE;

    // 合成提交触发键的 keyup 半程：down 已经在 OnTestKeyDown/OnKeyDown 里处理完毕，
    // 这里只需干净吃掉，不流入下面任何 keyup 逻辑（toggle/CapsLock 判据……）。
    if (wParam == VK_ASYNC_COMMIT_TRIGGER)
    {
        *pfEaten = TRUE;
        return S_OK;
    }

    // Auto-pair: bypass IME for self-generated SendInput key releases
    if (_TryConsumeSkipKey(wParam))
    {
        *pfEaten = FALSE;
        return S_OK;
    }

    // Keyboard disabled by system: pass through all keys
    if (_pTextService->IsKeyboardDisabled())
        return S_OK;

    // direct_commit 顶码：余码新组合在触发键 keyup 才开（下一个 keyup 即触发键 keyup）。
    // 先到者开组合，另一处 HasDeferredComposition()==FALSE 后自然 no-op。
    if (_pTextService->HasDeferredComposition())
    {
        _pTextService->StartDeferredCompositionIfPending();
        _isComposing = TRUE;
        _hasCandidates = TRUE;
        _pTextService->NotifyCandidatesVisibilityChanged(TRUE);
    }

    // Intercept modifier release if we have a pending auto-pair action
    if (_pendingPairAction.active)
    {
        if (wParam == VK_SHIFT || wParam == VK_LSHIFT || wParam == VK_RSHIFT ||
            wParam == VK_CONTROL || wParam == VK_LCONTROL || wParam == VK_RCONTROL ||
            wParam == VK_MENU || wParam == VK_LMENU || wParam == VK_RMENU)
        {
            *pfEaten = TRUE;
            return S_OK;
        }
    }

    // Handle pending toggle key release.
    // Dispatch here so apps like mintty (which call OnTestKeyUp but NOT OnKeyUp) still toggle.
    // _DispatchPendingToggleKeyUp clears _pendingKeyUpKey, making the OnKeyUp call a no-op.
    if (_DispatchPendingToggleKeyUp(wParam))
    {
        // 纯修饰键必须与 keydown 一致放行：只放行 down 而吃掉 up，宿主就会停在
        // 「Shift 按下且从未松开」的状态，比两边都吃更糟。
        *pfEaten = !_IsPureModifierKey(wParam);
        return S_OK;
    }

    // Also handle Caps Lock for indicator
    if (wParam == VK_CAPITAL)
    {
        _pTextService->NoteCapsLockKeyActivity(); // 供 OPENCLOSE 联动噪声抑制
        *pfEaten = TRUE;
        return S_OK;
    }

    return S_OK;
}

STDAPI CKeyEventSink::OnKeyUp(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten)
{
    *pfEaten = FALSE;

    // 合成提交键（VK_ASYNC_COMMIT_TRIGGER）的 keyup 半程：已在 OnTestKeyUp 里吃过，这里
    // 直接放行返回，避免它被下面 direct_commit / auto-pair / 修饰键状态机等逻辑误当成
    // 真实按键处理——注意与下一行注释里的「顶码触发键」不是同一个概念，只是恰好同名字。
    if (wParam == VK_ASYNC_COMMIT_TRIGGER)
    {
        *pfEaten = TRUE;
        return S_OK;
    }

    // direct_commit 顶码：余码新组合在触发键 keyup 才开（下一个 keyup 即触发键 keyup）。
    // 先到者开组合，另一处 HasDeferredComposition()==FALSE 后自然 no-op。
    if (_pTextService->HasDeferredComposition())
    {
        _pTextService->StartDeferredCompositionIfPending();
        _isComposing = TRUE;
        _hasCandidates = TRUE;
        _pTextService->NotifyCandidatesVisibilityChanged(TRUE);
    }

    // Update modifier state machine for this KeyUp event
    _UpdateModsOnKeyUp(wParam);

    // Execute pending auto-pair action when all modifiers are released
    if (_pendingPairAction.active && !_AreModifiersHeld())
    {
        WIND_LOG_DEBUG_FMT(L"Auto-pair: executing deferred vk=0x%02X x%d (modifiers released)\n",
            (WORD)_pendingPairAction.vk, _pendingPairAction.count);
        for (int i = 0; i < _pendingPairAction.count; i++)
        {
            _PushSkipKey(_pendingPairAction.vk);
            INPUT inputs[2] = {};
            inputs[0].type = INPUT_KEYBOARD;
            inputs[0].ki.wVk = _pendingPairAction.vk;
            inputs[1].type = INPUT_KEYBOARD;
            inputs[1].ki.wVk = _pendingPairAction.vk;
            inputs[1].ki.dwFlags = KEYEVENTF_KEYUP;
            SendInput(2, inputs, sizeof(INPUT));
        }
        _pendingPairAction = {};
        // Consume the modifier key-up to prevent mode toggle.
        // The user pressed Shift for a shifted character (e.g., parenthesis),
        // not for toggling input mode.
        *pfEaten = TRUE;
        return S_OK;
    }

    // Handle toggle key release for mode toggle.
    // _pendingKeyUpKey may already be 0 if OnTestKeyUp already dispatched it
    // (apps like mintty call OnTestKeyUp but skip OnKeyUp — dispatch happens there).
    if (_DispatchPendingToggleKeyUp(wParam))
    {
        // 与 keydown 一致放行纯修饰键，理由同 OnTestKeyUp。
        *pfEaten = !_IsPureModifierKey(wParam);
        return S_OK;
    }

    // Handle Caps Lock key release
    if (wParam == VK_CAPITAL)
    {
        _pTextService->NoteCapsLockKeyActivity(); // 供 OPENCLOSE 联动噪声抑制

        CHotkeyManager* pHotkeyMgr = _pTextService->GetHotkeyManager();

        // Calculate hash for CapsLock
        uint32_t keyHash = CHotkeyManager::CalcKeyHash(KEYMOD_CAPSLOCK, VK_CAPITAL);

        // Check if CapsLock is configured as toggle key (for Chinese/English switching)
        //
        // ⚠ 必须排掉「只有会话语义」的登记：`keys.session_actions` 里的 CapsLock 绑定同样
        // 落在 _keyUpHotkeys 里（那是转发白名单，两类语义共用），但它不是切中英文的配置。
        // 不排会让「只配了 CapsLock 打字时翻页」的用户丢掉 0x8000 状态通知标记，服务端
        // 收到的就成了「这是一次模式切换请求」。
        BOOL isConfiguredAsToggle = (pHotkeyMgr != nullptr && pHotkeyMgr->IsKeyUpHotkey(keyHash)
                                     && !pHotkeyMgr->IsKeyUpSessionOnlyHotkey(keyHash));

        // Get current Caps Lock state
        BOOL capsLockOn = (GetKeyState(VK_CAPITAL) & 0x0001) != 0;

        // Always send CapsLock event to Go service for:
        // 1. Mode toggle (if configured)
        // 2. CapsLock indicator display (A/a prompt)
        // 3. Toolbar state update
        // Use a special modifier to indicate whether this is for mode toggle
        uint32_t mods = KEYMOD_CAPSLOCK;
        if (!isConfiguredAsToggle)
        {
            // Add a marker to indicate this is just for CapsLock state notification, not mode toggle
            // Go side will check this to decide whether to toggle mode
            mods |= 0x8000; // High bit as "state notification only" marker
        }

        // Update caret position before sending CapsLock event
        _pTextService->SendCaretPositionUpdate();

        // SYNC: Send key event and wait for response
        // Go service will push state update followed by CMD_CONSUMED response
        // _HandleServiceResponse will process both and update the language bar
        if (_SendKeyToService(VK_CAPITAL, mods, KEY_EVENT_UP))
        {
            _HandleServiceResponse();
        }
        else
        {
            // IPC failed, fall back to local update
            WIND_LOG_ERROR(L"IPC failed for CapsLock, updating locally");
            _pTextService->UpdateCapsLockState(capsLockOn);
        }

        *pfEaten = TRUE;
        return S_OK;
    }

    return S_OK;
}

STDAPI CKeyEventSink::OnPreservedKey(ITfContext* pContext, REFGUID rguid, BOOL* pfEaten)
{
    *pfEaten = FALSE;
    return S_OK;
}

STDAPI CKeyEventSink::OnKeyTraceDown(WPARAM wParam, LPARAM lParam)
{
    if (_pTextService == nullptr || _pTextService->IsKeyboardDisabled())
        return S_OK;

    // 中文模式下统计由 Go recordCommit 负责；但 CapsLock 透传键（无 input session、
    // 非全角）直接透传给系统，不经过 Go，需在此统计为英文输入。
    bool capsLockPassthrough = false;
    if (_pTextService->IsChineseMode())
    {
        bool capsLockOn = (GetKeyState(VK_CAPITAL) & 0x0001) != 0;
        bool hasSession = _pTextService->HasActiveComposition() || _hasCandidates;
        if (!capsLockOn || hasSession || _pTextService->IsFullWidth())
            return S_OK;
        capsLockPassthrough = true;
    }

    // Check if stats are enabled
    if (!_statsEnabled || !_statsTrackEnglish)
        return S_OK;

    bool isPrintableTraceKey =
        (wParam >= 'A' && wParam <= 'Z') ||
        (wParam >= '0' && wParam <= '9') ||
        (wParam >= VK_NUMPAD0 && wParam <= VK_NUMPAD9) ||
        wParam == VK_MULTIPLY || wParam == VK_ADD || wParam == VK_SUBTRACT ||
        wParam == VK_DECIMAL || wParam == VK_DIVIDE ||
        wParam == VK_SPACE ||
        CHotkeyManager::IsPunctuationKey(wParam);
    if (!isPrintableTraceKey)
        return S_OK;

    uint32_t modifiers = CHotkeyManager::GetCurrentModifiers();

    // Optimization: avoid double counting.
    // If a key is intercepted by OnTestKeyDown in English mode (for full-width or auto-pair),
    // it will be sent to Go and recorded there. We should not count it here.
    // CapsLock 透传键直接到系统、不经 Go recordCommit，跳过下方去重检查。
    if (!capsLockPassthrough)
    {
        // 1. English auto-pair check（与 OnTestKeyDown/OnKeyDown 一致：Ctrl/Alt 不走配对）
        if (_englishPairEngine.IsEnabled()
            && !(modifiers & (KEYMOD_CTRL | KEYMOD_ALT)))
        {
            bool hasShift = (modifiers & KEYMOD_SHIFT) != 0;
            wchar_t pairChar = _MapVkToEnglishPairChar(wParam, hasShift);
            if (pairChar != 0 && (_englishPairEngine.IsLeft(pairChar) || _englishPairEngine.IsRight(pairChar)))
            {
                // This key will be eaten by OnTestKeyDown for auto-pairing.
                return S_OK;
            }
        }

        // 2. Full-width mode check
        if (_pTextService->IsFullWidth())
        {
            HotkeyType keyType = CHotkeyManager::ClassifyInputKey(wParam, modifiers);
            if (keyType == HotkeyType::Letter || keyType == HotkeyType::Number ||
                keyType == HotkeyType::Punctuation || keyType == HotkeyType::Space)
            {
                // This key will be eaten by OnTestKeyDown for full-width conversion.
                return S_OK;
            }
        }
    }

    _RecordEnglishKeyTrace(wParam, modifiers);
    return S_OK;
}

STDAPI CKeyEventSink::OnKeyTraceUp(WPARAM wParam, LPARAM lParam)
{
    return S_OK;
}

BOOL CKeyEventSink::Initialize()
{
    WIND_LOG_INFO(L"KeyEventSink::Initialize\n");

    ITfThreadMgr* pThreadMgr = _pTextService->GetThreadMgr();
    if (pThreadMgr == nullptr)
    {
        WIND_LOG_ERROR(L"ThreadMgr is null");
        return FALSE;
    }

    ITfKeystrokeMgr* pKeystrokeMgr = nullptr;
    HRESULT hr = pThreadMgr->QueryInterface(IID_ITfKeystrokeMgr, (void**)&pKeystrokeMgr);

    if (FAILED(hr) || pKeystrokeMgr == nullptr)
    {
        WIND_LOG_ERROR(L"Failed to get ITfKeystrokeMgr");
        return FALSE;
    }

    hr = pKeystrokeMgr->AdviseKeyEventSink(_pTextService->GetClientId(), this, TRUE);
    pKeystrokeMgr->Release();

    if (FAILED(hr))
    {
        WIND_LOG_ERROR(L"AdviseKeyEventSink failed");
        return FALSE;
    }

    ITfSource* pSource = nullptr;
    hr = pThreadMgr->QueryInterface(IID_ITfSource, (void**)&pSource);
    if (SUCCEEDED(hr) && pSource != nullptr)
    {
        hr = pSource->AdviseSink(IID_ITfKeyTraceEventSink, (ITfKeyTraceEventSink*)this, &_dwKeyTraceSinkCookie);
        pSource->Release();

        if (FAILED(hr))
        {
            _dwKeyTraceSinkCookie = TF_INVALID_COOKIE;
            WIND_LOG_ERROR_FMT(L"AdviseKeyTraceEventSink failed: hr=0x%08X\n", (uint32_t)hr);
        }
        else
        {
            WIND_LOG_INFO(L"KeyTraceEventSink initialized successfully\n");
        }
    }
    else
    {
        WIND_LOG_ERROR(L"Failed to get ITfSource for key trace sink");
    }

    WIND_LOG_INFO(L"KeyEventSink initialized successfully\n");
    return TRUE;
}

void CKeyEventSink::Uninitialize()
{
    WIND_LOG_INFO(L"KeyEventSink::Uninitialize\n");

    ITfThreadMgr* pThreadMgr = _pTextService->GetThreadMgr();
    if (pThreadMgr == nullptr)
        return;

    ITfKeystrokeMgr* pKeystrokeMgr = nullptr;
    if (SUCCEEDED(pThreadMgr->QueryInterface(IID_ITfKeystrokeMgr, (void**)&pKeystrokeMgr)))
    {
        pKeystrokeMgr->UnadviseKeyEventSink(_pTextService->GetClientId());
        pKeystrokeMgr->Release();
    }

    if (_dwKeyTraceSinkCookie != TF_INVALID_COOKIE)
    {
        ITfSource* pSource = nullptr;
        if (SUCCEEDED(pThreadMgr->QueryInterface(IID_ITfSource, (void**)&pSource)) && pSource != nullptr)
        {
            pSource->UnadviseSink(_dwKeyTraceSinkCookie);
            pSource->Release();
        }
        _dwKeyTraceSinkCookie = TF_INVALID_COOKIE;
    }
}

// Dispatch the pending toggle key (Shift/Ctrl) to Go service.
// Clears _pendingKeyUpKey so a subsequent OnKeyUp call is a no-op (prevents double dispatch).
// This is called from BOTH OnTestKeyUp and OnKeyUp because some apps (e.g. mintty) call
// OnTestKeyUp but never call OnKeyUp; others skip OnTestKeyUp and go straight to OnKeyUp.
BOOL CKeyEventSink::_DispatchPendingToggleKeyUp(WPARAM wParam)
{
    if (_pendingKeyUpKey == 0)
        return FALSE;
    if (!_IsMatchingKeyUp(wParam, _pendingKeyUpKey))
        return FALSE;

    uint32_t pendingKey = _pendingKeyUpKey;
    DWORD pressDuration = GetTickCount() - _pendingKeyDownTime;
    _pendingKeyUpKey = 0;
    _pendingKeyUpModifiers = 0;
    _pendingKeyDownTime = 0;

    if (pressDuration > TOGGLE_TAP_THRESHOLD_MS)
    {
        WIND_LOG_DEBUG_FMT(L"Toggle key held too long (%lu ms > %lu ms), ignoring\n",
            pressDuration, TOGGLE_TAP_THRESHOLD_MS);
        return TRUE;
    }

    if (pendingKey != VK_CAPITAL)
    {
        WIND_LOG_DEBUG_FMT(L"Sending toggle key KeyUp to Go: vk=0x%02X\n", pendingKey);

        uint32_t mods = 0;
        if (pendingKey == VK_LSHIFT)
            mods = KEYMOD_SHIFT | KEYMOD_LSHIFT;
        else if (pendingKey == VK_RSHIFT)
            mods = KEYMOD_SHIFT | KEYMOD_RSHIFT;
        else if (pendingKey == VK_LCONTROL)
            mods = KEYMOD_CTRL | KEYMOD_LCTRL;
        else if (pendingKey == VK_RCONTROL)
            mods = KEYMOD_CTRL | KEYMOD_RCTRL;

        _pTextService->SendCaretPositionUpdate();

        if (_SendKeyToService(pendingKey, mods, KEY_EVENT_UP))
            _HandleServiceResponse();
        else
            WIND_LOG_ERROR(L"IPC failed for toggle key, not toggling locally");
    }

    return TRUE;
}

BOOL CKeyEventSink::_IsMatchingKeyUp(WPARAM wParam, uint32_t pendingKey)
{
    if (pendingKey == 0)
        return FALSE;

    // Direct match
    if (wParam == pendingKey)
        return TRUE;

    // Handle generic VK_SHIFT -> need to check if the pending specific key was released
    if (wParam == VK_SHIFT)
    {
        // pendingKey is specific (VK_LSHIFT or VK_RSHIFT)
        // Check if that specific key is no longer pressed
        SHORT lshiftState = GetAsyncKeyState(VK_LSHIFT);
        SHORT rshiftState = GetAsyncKeyState(VK_RSHIFT);
        WIND_LOG_DEBUG_FMT(L"compat.keyup_match: pendingKey=0x%02X lshift_async=0x%04X rshift_async=0x%04X",
            pendingKey, (uint16_t)lshiftState, (uint16_t)rshiftState);
        if (pendingKey == VK_LSHIFT && !(lshiftState & 0x8000))
        {
            return TRUE;
        }
        if (pendingKey == VK_RSHIFT && !(rshiftState & 0x8000))
        {
            return TRUE;
        }
        WIND_LOG_DEBUG_FMT(L"compat.keyup_match: VK_SHIFT->pendingKey=0x%02X not matched (key still held?)", pendingKey);
        return FALSE;
    }

    // Handle generic VK_CONTROL -> need to check if the pending specific key was released
    if (wParam == VK_CONTROL)
    {
        if (pendingKey == VK_LCONTROL && !(GetAsyncKeyState(VK_LCONTROL) & 0x8000))
        {
            return TRUE;
        }
        if (pendingKey == VK_RCONTROL && !(GetAsyncKeyState(VK_RCONTROL) & 0x8000))
        {
            return TRUE;
        }
        return FALSE;
    }

    // Handle specific VK matching specific pending
    // E.g., if pendingKey is VK_LSHIFT and wParam is VK_LSHIFT -> already matched above
    // But if pendingKey is VK_LSHIFT and wParam is VK_RSHIFT -> don't match (different keys)

    return FALSE;
}

// Send key to Go Service using binary protocol
BOOL CKeyEventSink::DispatchHotkey(uint32_t vk, uint32_t mods)
{
    // 走与 OnKeyDown 同一通路：send + handle response。
    // 用于经 WM_HOTKEY（RegisterHotKey 全局拦截）到达的热键：
    //   - Pin/Delete 候选热键（组合键取自配置）：操作已显示候选，不依赖 caret。
    //   - AddWord（Ctrl+= 等）：中文+文本框时经全局拦截规避 Chromium 宿主双处理。
    //     composition 由 _HandleServiceResponse 处理 UpdateComposition 建立；caret 定位
    //     由 WM_HOTKEY 分发处先行 SendCaretPositionUpdate 补齐（见 TextService WndProc）。
    if (!_SendKeyToService(vk, mods, KEY_EVENT_DOWN))
    {
        WIND_LOG_ERROR_FMT(L"DispatchHotkey: _SendKeyToService failed vk=0x%02X mods=0x%04X\n", vk, mods);
        return FALSE;
    }
    return _HandleServiceResponse();
}

BOOL CKeyEventSink::_SendKeyToService(uint32_t keyCode, uint32_t modifiers, uint8_t eventType)
{
    DWORD startTime = GetTickCount();

    CIPCClient* pIPCClient = _pTextService->GetIPCClient();
    if (pIPCClient == nullptr)
    {
        WIND_LOG_ERROR(L"IPCClient is null");
        return FALSE;
    }

    // If a new connection was established (e.g., service started after TSF loaded),
    // perform a full state sync before processing key events.
    // This covers the edge case where service becomes available between focus events.
    if (pIPCClient->NeedsStateSync())
    {
        if (!pIPCClient->IsConnected() && !pIPCClient->Connect())
        {
            WIND_LOG_WARN(L"State sync needed but reconnect failed before key send");
            return FALSE;
        }

        if (_pTextService->HasActiveComposition())
        {
            // Composition is active — do NOT send CMD_IME_ACTIVATED here.
            // HandleIMEActivated on the Go side clears inputBuffer if non-empty,
            // which would destroy the in-progress composition.
            // WM_SERVICE_READY will handle the sync after composition ends.
            WIND_LOG_INFO(L"NeedsStateSync: composition active, clearing flag without sync\n");
            pIPCClient->ClearNeedsSyncFlag();
        }
        else
        {
            _pTextService->_DoFullStateSync();

            // Re-send caret position after reconnection/state sync so the Go side has
            // a valid anchor before it processes the first post-restart key event.
            _pTextService->SendCaretPositionUpdate();
        }
    }

    _pTextService->TryRecoverFocusState();

    // Get scan code from virtual key (optional, set to 0 if not needed)
    uint32_t scanCode = MapVirtualKeyW(keyCode, MAPVK_VK_TO_VSC);

    // Get toggles and event sequence
    uint8_t toggles = _GetTogglesSnapshot();
    uint16_t eventSeq = _GetNextEventSeq();

    // IMPORTANT: Always use the passed-in modifiers from CHotkeyManager::GetCurrentModifiers()
    // which calls GetAsyncKeyState(). The _modsState state machine can get out of sync
    // when we pass keys through to the system (e.g., Ctrl+S for save).
    // Using stale _modsState causes all subsequent keys to appear as having Ctrl held.

    // Get character before caret for smart punctuation:
    // 1. Prefer ITfTextEditSink::OnEndEdit cache (works in Notepad, browsers, etc.)
    //    Consume (clear) to prevent stale values in apps where OnEndEdit fires late (e.g., WeChat)
    // 2. Fallback to digit pass-through tracking (for editors like EverEdit where TSF text access fails)
    uint16_t prevChar = (uint16_t)_pTextService->ConsumeCachedPrevChar();
    // 备用通路的消费判据：**任何标点键**都如实带上 prevChar，不在这里挑「哪些标点算数」。
    //
    // ★ 这里曾硬编码 `keyCode == VK_OEM_PERIOD || VK_OEM_COMMA`，等于把服务端的
    // `input.punct.smart_list` 抄了一份到 DLL——而出厂默认就是 ".,:"，冒号从设计上就
    // 拿不到备用值，且会落进下面的 else 把已记的数字**清零**（连带毁掉紧随其后的句号）。
    // 用户自定义 `?` `!` 等更是全军覆没。同一语义判据写在两处，必然漂移。
    //
    // 分工边界：DLL 只上报**事实**（光标前一个字符是什么），服务端 wind-punct 的
    // `is_smart_punct_after_digit` 持有全部**策略**（smart_after_digit 总开关 +
    // smart_list 成员判定 + 0x30..=0x39 数字判定）。多报一个标点键的 prevChar 不会
    // 有副作用：不在 smart_list 里的标点，服务端自己会判 false。
    // （对称的另一半——「哪些键**产出数字**」——只能留在本文件，见头文件
    //  _DigitCharFromVk 注释：那些键透传出去了，服务端根本看不到。）
    //
    // 用 ClassifyInputKey 而非 IsPunctuationKey：前者额外覆盖 Shift+主键盘数字
    // （`!` `@` `#`…，见 HotkeyManager.cpp 的 Number 分支），后者只列 11 个 OEM 键。
    const BOOL isPunctKey =
        (CHotkeyManager::ClassifyInputKey(keyCode, modifiers) == HotkeyType::Punctuation);
    if (prevChar == 0 && _lastPassthroughDigit != 0 && isPunctKey)
    {
        prevChar = (uint16_t)_lastPassthroughDigit;
        _lastPassthroughDigit = 0;  // 已消费，清除以避免后续标点误判
        // 真机验证锚点：读不回文档的宿主（EverEdit 等）走到这里才说明备用通路生效。
        // 主路径能读回时本分支不执行——两者症状相同、成因不同，只有这条日志分得开。
        // 刻意不打字符内容（那是用户输入），只标记路径与键码。
        WIND_LOG_DEBUG_FMT(L"smart_punct_digit_fallback: prevChar from passthrough digit, vk=0x%02X\n",
                           keyCode);
    }
    // Clear stale digit passthrough when any non-punctuation key is sent to the service.
    // Without this, _lastPassthroughDigit persists through eaten keys (composition,
    // candidate selection, etc.), causing e.g. "58的。" to incorrectly use digit
    // fallback and output "." instead of "。" in non-TSF apps.
    else if (_lastPassthroughDigit != 0 && !isPunctKey)
    {
        _lastPassthroughDigit = 0;
    }

    BOOL result = pIPCClient->SendKeyEvent(keyCode, scanCode, modifiers, eventType, toggles, eventSeq, prevChar);

    WIND_LOG_DEBUG_FMT(L"_SendKeyToService: vk=0x%02X, mods=0x%04X, elapsed=%dms\n",
                 keyCode, modifiers, GetTickCount() - startTime);

    return result;
}

BOOL CKeyEventSink::_HandleServiceResponse()
{
    LARGE_INTEGER startTime, midTime, freq;
    QueryPerformanceCounter(&startTime);
    QueryPerformanceFrequency(&freq);

    CIPCClient* pIPCClient = _pTextService->GetIPCClient();
    if (pIPCClient == nullptr)
        return TRUE; // Default to eating the key if no IPC

    ServiceResponse response;

    // Bridge pipe 上的响应直接读一次即可。state push 已迁到独立 push pipe (由 async
    // reader 处理), 不会再夹在 bridge response 之前; 而 StatusUpdate 现在是 lshift/
    // OnClick/SystemModeSwitch 等同步操作的正式响应类型, 必须返回给外层 switch 走
    // case StatusUpdate 分支 (UpdateFullStatus + 同步 TSF compartments)。
    // 旧版本这里有一个吃掉 StatusUpdate 并 continue 的 loop, 是历史遗留: 早期
    // state push 借 bridge pipe 道, 现在已废弃。继续保留会导致 lshift 响应被吃掉,
    // 后续 ReceiveResponse 等不到下一条而 200ms timeout 断连 (Ctrl+Space 失效根因)。
    if (!pIPCClient->ReceiveResponse(response))
    {
        // 本地 composition 强制复位 + 置 resync 标志：
        // 失败丢响应后 C++ 与 Go 状态会失同步 (例如 Shift+字母 起合成响应丢失 →
        // _isComposing 一直为 FALSE → 后续 ENTER/ESC 被判 hasInputSession=FALSE
        // 而直接放行给宿主, 候选窗失控)。本地清干净 + 置 resync, 让下一次按键
        // 强行走"有会话"路径发给 Go, 由 Go 权威响应自然重建状态。
        WIND_LOG_ERROR(L"Failed to receive response from service, performing local composition reset");
        if (_pTextService->HasActiveComposition())
        {
            _pTextService->EndComposition();
        }
        _isComposing = FALSE;
        _hasCandidates = FALSE;
        _pTextService->NotifyCandidatesVisibilityChanged(FALSE);

        // resync 自愈：累计连续失败，到上限就放弃自愈、走 passthrough，
        // 避免 Go 服务长时间挂掉时 ENTER/ESC/Ctrl+Alt 被永久吃。
        // 任一次响应成功 (下方 _resyncFailStreak=0) 即清零计数。
        _resyncFailStreak++;
        if (_resyncFailStreak >= RESYNC_MAX_RETRIES)
        {
            WIND_LOG_WARN_FMT(L"Resync fail streak=%d reached limit, dropping to passthrough mode",
                              _resyncFailStreak);
            _needsCompositionResync = FALSE;
            _resyncDeadline = 0;
        }
        else
        {
            _needsCompositionResync = TRUE;
            _resyncDeadline = GetTickCount() + RESYNC_WINDOW_MS;
        }
        return TRUE; // Default to eating the key on error
    }

    // 响应成功 → 状态由下方 switch 各分支按权威重建, 清 resync 旗 + 失败计数。
    _needsCompositionResync = FALSE;
    _resyncDeadline = 0;
    _resyncFailStreak = 0;

    QueryPerformanceCounter(&midTime);
    int ipcMs = (int)((midTime.QuadPart - startTime.QuadPart) * 1000 / freq.QuadPart);
    WIND_LOG_DEBUG_FMT(L"_HandleServiceResponse: IPC receive took %dms, responseType=%d\n",
                 ipcMs, (int)response.type);

    switch (response.type)
    {
    case ResponseType::Ack:
        // ACK means key was handled (consumed without output)
        return TRUE;

    case ResponseType::PassThrough:
        // PassThrough means key was NOT handled, pass to system
        WIND_LOG_DEBUG(L"PassThrough: key not handled, passing to system\n");
        _pTextService->FlushHoldCompositionIfActive();
        if (_pTextService->HasDeferredComposition())
            _pTextService->StartDeferredCompositionIfPending();
        return FALSE;

    case ResponseType::CommitText:
        {
            LARGE_INTEGER ctStart, ctMid1, ctEnd;
            QueryPerformanceCounter(&ctStart);

            WIND_LOG_DEBUG(L"Processing CommitText response\n");

            // Handle new composition if needed (top code / non-inline restart)
            // restartComposition=true: both inline (newComposition has text) and non-inline (newComposition empty, uses placeholder)
            if (response.restartComposition)
            {
                WIND_LOG_TRACE_FMT(L"CommitText with restart composition: textLen=%zu, newCompLen=%zu\n",
                             response.text.length(), response.newComposition.length());

                _pTextService->InsertTextAndStartComposition(response.text, response.newComposition);
                _isComposing = TRUE;
                _hasCandidates = TRUE;
                _pTextService->NotifyCandidatesVisibilityChanged(TRUE);

                // Re-send caret position after composition change
                _pTextService->SendCaretPositionUpdate();
            }
            else
            {
                // No new composition, commit text atomically (end composition + insert in one EditSession)
                // replacingHeld：智能符号 press2 要覆盖 hold 预览态里的中文符号；其余提交
                // 路径为 FALSE，held 符号并入前缀一起上屏（见 CTextService::CommitText）。
                _pTextService->CommitText(response.text, FALSE, response.replacingHeld ? TRUE : FALSE);
                QueryPerformanceCounter(&ctMid1);

                // 上屏文本末位即新的「光标前一字符」，据此维护备用 prevChar（见 header 注释）。
                // 只在这条「提交后不留组合」的分支记：restartComposition 分支提交后立刻又起了
                // 组合，光标前是组合内容而非 response.text 末位，记了就是错的。
                _TrackCommittedTextForSmartPunct(response.text);

                _isComposing = FALSE;
                _hasCandidates = FALSE;
                _pTextService->NotifyCandidatesVisibilityChanged(FALSE);

                int commitMs = (int)((ctMid1.QuadPart - ctStart.QuadPart) * 1000 / freq.QuadPart);
                WIND_LOG_TRACE_FMT(L"CommitText: atomic commit=%dms\n", commitMs);
            }

            // Handle mode change if present
            if (response.modeChanged)
            {
                _pTextService->SetInputMode(response.chineseMode);
            }

            QueryPerformanceCounter(&ctEnd);
            int ctMs = (int)((ctEnd.QuadPart - ctStart.QuadPart) * 1000 / freq.QuadPart);
            WIND_LOG_DEBUG_FMT(L"CommitText total took %dms\n", ctMs);
        }
        return TRUE;

    case ResponseType::UpdateComposition:
        {
            LARGE_INTEGER ucStart, ucEnd;
            QueryPerformanceCounter(&ucStart);

            WIND_LOG_TRACE(L"Received UpdateComposition from service\n");
            // 若 HoldComposition 计时器活跃（中文符号待提交），把符号定格并入 prefix，
            // 与新组合内容在同一次 UpdateComposition 内显示（符号无下划线、新内容有，
            // 复用顶码聚合分段）。曾用 Flush（CommitText+立即开新组合）——WPS/微信下
            // 该模式被误读成替换，符号被新输入顶掉（与顶码双写同根）。
            _pTextService->AbsorbHeldIntoPrefix();
            _isComposing = TRUE;
            _hasCandidates = TRUE;
            _pTextService->NotifyCandidatesVisibilityChanged(TRUE);
            _pTextService->UpdateComposition(response.composition, response.caretPos);

            // Re-send caret position after composition update so Go can
            // reposition the candidate window with the up-to-date coordinates.
            _pTextService->SendCaretPositionUpdate();

            QueryPerformanceCounter(&ucEnd);
            int ucMs = (int)((ucEnd.QuadPart - ucStart.QuadPart) * 1000 / freq.QuadPart);
            WIND_LOG_DEBUG_FMT(L"UpdateComposition total took %dms\n", ucMs);
        }
        return TRUE;

    case ResponseType::ClearComposition:
        WIND_LOG_DEBUG(L"Received ClearComposition from service\n");
        _isComposing = FALSE;
        _hasCandidates = FALSE;
        _pTextService->NotifyCandidatesVisibilityChanged(FALSE);
        _pTextService->EndComposition();
        return TRUE;

    case ResponseType::ClearCompositionThenPassThrough:
        // 与上一分支同样收组合，区别只在**这一键要还给宿主**（联想态回车/退格透传）。
        //
        // 这里仍返回 TRUE（吃掉原键）：OnTestKeyDown 已按「有会话」吃了它，此处吐 FALSE
        // 就是「吃了再吐」翻转，EverEdit 这类不补发 WM_KEYDOWN 的宿主会直接丢键（实测
        // vk=0x0D）。改由 OnKeyDown 在收口完成后 SendInput 重放一个干净的按键——宿主先
        // 看到收口后的文档，再看到一个与组合无关的普通回车/退格。同 hold / 配对跳出。
        WIND_LOG_DEBUG(L"Received ClearCompositionThenPassThrough from service\n");
        _isComposing = FALSE;
        _hasCandidates = FALSE;
        _pTextService->NotifyCandidatesVisibilityChanged(FALSE);
        _pTextService->EndComposition();
        _pendingReplayToHost = TRUE;
        return TRUE;

    case ResponseType::StatusUpdate:
        // StatusUpdate 是 lshift/SystemModeSwitch/FocusGained/IMEActivated 等同步操作
        // 的标准响应类型 (自包含 mode + iconLabel + hotkeys), 走 UpdateFullStatus 一并
        // 同步 _bChineseMode mirror + TSF compartments + LangBar UI。
        WIND_LOG_DEBUG(L"Received StatusUpdate as final response\n");
        _pTextService->UpdateFullStatus(
            response.IsChineseMode(),
            response.IsFullWidth(),
            response.IsChinesePunct(),
            response.IsToolbarVisible(),
            response.IsCapsLock(),
            response.iconLabel.empty() ? nullptr : response.iconLabel.c_str()
        );
        return TRUE;

    case ResponseType::Consumed:
        // Key was consumed by a hotkey
        WIND_LOG_DEBUG(L"Key consumed by hotkey\n");
        return TRUE;

    case ResponseType::InsertTextWithCursor:
        {
            WIND_LOG_DEBUG(L"Processing InsertTextWithCursor response\n");
            _pTextService->CommitText(response.text);
            // 备用 prevChar：cursorOffset==0 时末位就是光标前一字符；cursorOffset>0（配对
            // 插入后光标回退到中间）时光标前其实是左符号，末位取的是右符号——但两者都是
            // 配对符号、都不是数字，结论同为清零，故无须为配对单独分支。
            _TrackCommittedTextForSmartPunct(response.text);
            _isComposing = FALSE;
            _hasCandidates = FALSE;
            for (int i = 0; i < response.cursorOffset; i++)
                _SimulatePairKey(VK_LEFT);
            // 配对插入：记一层待跳出深度。它是 core 侧 pair_tracker 的镜像计数，也是本文件
            // 唯一的吃键闸门——Enter 等被会话门控的键靠它放行转发（Tab 本就无条件转发）。
            // 四条配对建立路径（中文标点 / 英文全角 / 英半自定义 / 英半普通）都经此响应。
            if (response.cursorOffset > 0)
            {
                _pairPendingDepth++;
                TouchPairState(); // 起算时效
            }
        }
        return TRUE;

    case ResponseType::MoveCursorRight:
        {
            WIND_LOG_DEBUG_FMT(L"Processing MoveCursorRight response (smart skip), count=%u\n",
                               response.moveCount);
            // 一次跳出 = 越过一层配对的右段，可能是多格（直通 ime.pair 的多字符右段）。
            // 深度只减 1：格数是「这一层有多宽」，与「弹掉几层」无关。
            for (uint32_t i = 0; i < response.moveCount; i++)
                _SimulatePairKey(VK_RIGHT);
            if (_pairPendingDepth > 0)
                _pairPendingDepth--;
        }
        return TRUE;

    case ResponseType::DeletePair:
        {
            WIND_LOG_DEBUG(L"Processing DeletePair response (smart delete)\n");
            _SimulatePairKey(VK_DELETE);
            _SimulatePairKey(VK_BACK);
        }
        return TRUE;

    case ResponseType::ReplaceBackward:
        {
            // 智能符号：删除光标前 N 个字符并插入替换文本。默认走 TSF 同步范围替换
            // （见 TextService::ReplacePrecedingChars），不发合成按键——不依赖修饰键
            // 是否松开，用户按住 Shift 连续输入多个符号也不受影响；只有 TSF 失败时
            // 才回退到 SendInput，那种情况下才可能受 Shift 类标点的发键抑制问题影响
            // （已知 Chromium/Qt 部分宿主的 TSFTextStore 会谎报替换成功，届时若仍需要
            // 兼容，走按宿主进程名特判，而不是默认全局改用合成按键——见
            // ReplacePrecedingChars 里 kTryTsfRangeReplace 的取舍说明）。
            WIND_LOG_DEBUG(L"Processing ReplaceBackward response (smart symbol)\n");
            _pTextService->ReplacePrecedingChars(response.replaceCount, response.text);
            _isComposing = FALSE;
            _hasCandidates = FALSE;
        }
        return TRUE;

    case ResponseType::HoldComposition:
        {
            // 智能符号 HoldComposition 方案：press1 将中文符号放入 TSF 组合态，
            // 由 TextService 启动 500ms 计时器自动提交；press2 将直接发 CommitText。
            WIND_LOG_DEBUG_FMT(L"Processing HoldComposition response: text=%ls timeoutMs=%u\n",
                               response.text.c_str(), response.holdTimeoutMs);
            _pTextService->HoldComposition(response.text, response.holdTimeoutMs);
            _isComposing = TRUE;
            _hasCandidates = FALSE;
        }
        return TRUE;

    case ResponseType::CommitAndHold:
        {
            // 标点顶屏 + 智能符号 HoldComposition：**聚合式**——候选文本并入 _pendingCommitPrefix
            // （不真提交、留组合内），中文符号作为 held 放入同一组合，最终一次 CommitText 收口
            // （超时→可能。/ press2→可能.）。曾用「CommitText 候选 + 立即 HoldComposition」——
            // 真提交+同位置重开组合被 diff 式宿主（微信 Qt / Tabby·终端 Chromium 内嵌）误读成
            // 替换而吞掉已提交候选（TSF 全程报成功、渲染却丢字）。改聚合后全程只有
            // compositionupdate、无中途 EndComposition，与顶码 pre_confirm / 连续符号同一套路。
            // 只把候选（承诺提交、不可撤回）并入 prefix；中文符号仍作 held——press2 要替换它成
            // 英文，进 prefix 会中英都上屏（见 project_tsf_desync_analysis 教训）。
            WIND_LOG_DEBUG_FMT(L"Processing CommitAndHold(aggregate): commit=%ls hold=%ls timeoutMs=%u\n",
                               response.text.c_str(), response.newComposition.c_str(),
                               response.holdTimeoutMs);
            _pTextService->PinCommitTextToPrefix(response.text);
            _hasCandidates = FALSE;
            _pTextService->NotifyCandidatesVisibilityChanged(FALSE);
            _pTextService->HoldComposition(response.newComposition, response.holdTimeoutMs);
            _isComposing = TRUE;
        }
        return TRUE;

    case ResponseType::CommitThenDefer:
        {
            // direct_commit 顶码：立即真提交顶出文本（keydown 内，对齐 compositionend@keydown），
            // 余码新组合暂存、延迟到本键 keyup（或兜底定时器）才开——隔一拍消息泵躲开
            // diff 式宿主整锁合并。见 top-commit-mode 设计文档。
            WIND_LOG_DEBUG_FMT(L"Processing CommitThenDefer: commit=%ls defer=%ls timeoutMs=%u\n",
                               response.text.c_str(), response.newComposition.c_str(),
                               response.holdTimeoutMs);
            _pTextService->CommitText(response.text);
            _isComposing = FALSE;
            _hasCandidates = FALSE;
            _pTextService->NotifyCandidatesVisibilityChanged(FALSE);
            _pTextService->StashDeferredComposition(response.newComposition, response.holdTimeoutMs);
        }
        return TRUE;

    default:
        WIND_LOG_ERROR(L"Unknown response type from service");
        return TRUE;
    }

    return TRUE; // Default: key was handled
}

// Check if the current context is read-only
BOOL CKeyEventSink::_IsContextReadOnly(ITfContext* pContext)
{
    if (!pContext)
    {
        WIND_LOG_DEBUG_FMT(L"compat.context_status focusSession=%llu context=null", _pTextService->GetFocusSessionId());
        return TRUE;
    }

    TF_STATUS tfStatus = {};
    HRESULT hr = pContext->GetStatus(&tfStatus);

    if (SUCCEEDED(hr))
    {
        if (tfStatus.dwDynamicFlags & TF_SD_READONLY)
        {
            WIND_LOG_DEBUG_FMT(
                L"compat.context_status focusSession=%llu flags=0x%08X readonly=1 loading=0",
                _pTextService->GetFocusSessionId(), tfStatus.dwDynamicFlags
            );
            return TRUE;
        }

        if (tfStatus.dwDynamicFlags & TF_SD_LOADING)
        {
            WIND_LOG_DEBUG_FMT(
                L"compat.context_status focusSession=%llu flags=0x%08X readonly=0 loading=1",
                _pTextService->GetFocusSessionId(), tfStatus.dwDynamicFlags
            );
            return TRUE;
        }

        WIND_LOG_TRACE_FMT(
            L"compat.context_status focusSession=%llu flags=0x%08X readonly=0 loading=0",
            _pTextService->GetFocusSessionId(), tfStatus.dwDynamicFlags
        );
    }
    else
    {
        WIND_LOG_WARN_FMT(
            L"compat.context_status focusSession=%llu get_status_failed hr=0x%08X",
            _pTextService->GetFocusSessionId(), hr
        );
    }

    return FALSE;
}

// Called when composition is unexpectedly terminated by the application
// This typically happens when:
// 1. Fast typing: new composition starts before previous InsertText completes
// 2. User clicks in input field to change cursor position
// 3. Application forcefully terminates composition
void CKeyEventSink::OnCompositionUnexpectedlyTerminated()
{
    WIND_LOG_INFO(L"OnCompositionUnexpectedlyTerminated: Resetting state and notifying service\n");

    // Reset local state
    _isComposing = FALSE;
    _hasCandidates = FALSE;

    // Notify Go service to clear input buffer and hide candidate window
    // Use CompositionTerminated instead of FocusLost so that the toolbar stays visible
    // (FocusLost would hide toolbar, but composition termination should not)
    CIPCClient* pIPCClient = _pTextService->GetIPCClient();
    if (pIPCClient != nullptr && pIPCClient->IsConnected())
    {
        pIPCClient->SendCompositionTerminated();
        WIND_LOG_DEBUG(L"OnCompositionUnexpectedlyTerminated: Sent CompositionTerminated to service\n");
    }
}

// ============================================================================
// Modifier key state machine implementation
// ============================================================================

void CKeyEventSink::_UpdateModsOnKeyDown(WPARAM vk)
{
    switch (vk)
    {
    case VK_SHIFT:
        // Generic shift - set generic flag, actual L/R determined by GetAsyncKeyState
        _modsState |= KEYMOD_SHIFT;
        if (GetAsyncKeyState(VK_LSHIFT) & 0x8000) _modsState |= KEYMOD_LSHIFT;
        if (GetAsyncKeyState(VK_RSHIFT) & 0x8000) _modsState |= KEYMOD_RSHIFT;
        break;
    case VK_LSHIFT:
        _modsState |= (KEYMOD_SHIFT | KEYMOD_LSHIFT);
        break;
    case VK_RSHIFT:
        _modsState |= (KEYMOD_SHIFT | KEYMOD_RSHIFT);
        break;

    case VK_CONTROL:
        _modsState |= KEYMOD_CTRL;
        if (GetAsyncKeyState(VK_LCONTROL) & 0x8000) _modsState |= KEYMOD_LCTRL;
        if (GetAsyncKeyState(VK_RCONTROL) & 0x8000) _modsState |= KEYMOD_RCTRL;
        break;
    case VK_LCONTROL:
        _modsState |= (KEYMOD_CTRL | KEYMOD_LCTRL);
        break;
    case VK_RCONTROL:
        _modsState |= (KEYMOD_CTRL | KEYMOD_RCTRL);
        break;

    case VK_MENU:
    case VK_LMENU:
    case VK_RMENU:
        _modsState |= KEYMOD_ALT;
        break;

    case VK_LWIN:
    case VK_RWIN:
        _modsState |= KEYMOD_WIN;
        break;
    }
}

void CKeyEventSink::_UpdateModsOnKeyUp(WPARAM vk)
{
    switch (vk)
    {
    case VK_SHIFT:
        // Clear all shift flags when generic VK_SHIFT is released
        _modsState &= ~(KEYMOD_SHIFT | KEYMOD_LSHIFT | KEYMOD_RSHIFT);
        break;
    case VK_LSHIFT:
        _modsState &= ~KEYMOD_LSHIFT;
        // Only clear generic shift if right shift is also not held
        if (!(_modsState & KEYMOD_RSHIFT))
            _modsState &= ~KEYMOD_SHIFT;
        break;
    case VK_RSHIFT:
        _modsState &= ~KEYMOD_RSHIFT;
        if (!(_modsState & KEYMOD_LSHIFT))
            _modsState &= ~KEYMOD_SHIFT;
        break;

    case VK_CONTROL:
        _modsState &= ~(KEYMOD_CTRL | KEYMOD_LCTRL | KEYMOD_RCTRL);
        break;
    case VK_LCONTROL:
        _modsState &= ~KEYMOD_LCTRL;
        if (!(_modsState & KEYMOD_RCTRL))
            _modsState &= ~KEYMOD_CTRL;
        break;
    case VK_RCONTROL:
        _modsState &= ~KEYMOD_RCTRL;
        if (!(_modsState & KEYMOD_LCTRL))
            _modsState &= ~KEYMOD_CTRL;
        break;

    case VK_MENU:
    case VK_LMENU:
    case VK_RMENU:
        _modsState &= ~KEYMOD_ALT;
        break;

    case VK_LWIN:
    case VK_RWIN:
        _modsState &= ~KEYMOD_WIN;
        break;
    }
}

uint8_t CKeyEventSink::_GetTogglesSnapshot() const
{
    uint8_t toggles = 0;
    if (GetKeyState(VK_CAPITAL) & 0x01) toggles |= TOGGLE_CAPSLOCK;
    if (GetKeyState(VK_NUMLOCK) & 0x01) toggles |= TOGGLE_NUMLOCK;
    if (GetKeyState(VK_SCROLL) & 0x01)  toggles |= TOGGLE_SCROLLLOCK;
    return toggles;
}

void CKeyEventSink::_SyncStateFromResponse(uint32_t statusFlags)
{
    // Sync mode from Go response
    bool chineseMode = (statusFlags & STATUS_CHINESE_MODE) != 0;
    _pTextService->SetInputMode(chineseMode);
}

// ============================================================================
// Config sync handler
// ============================================================================

// 该键是否配了「英文半角」列的自定义标点映射（core 推送的字符集合）。
// 空集合时零开销返回 FALSE —— 未启用自定义映射的用户完全不受本机制影响。
BOOL CKeyEventSink::_IsCustomEnglishPunctKey(WPARAM vk, uint32_t modifiers) const
{
    if (_customEnPunctChars.empty())
        return FALSE;
    // Ctrl/Alt 组合是功能热键，不参与出字（与 ClassifyInputKey 对其返回 None 保持一致）。
    if (modifiers & (KEYMOD_CTRL | KEYMOD_ALT))
        return FALSE;
    if (!CHotkeyManager::IsPunctuationKey(vk))
        return FALSE;
    wchar_t ch = CHotkeyManager::VirtualKeyToPunctuation(vk, (modifiers & KEYMOD_SHIFT) != 0);
    return ch != 0 && _customEnPunctChars.count(ch) > 0;
}

void CKeyEventSink::OnSyncConfig(const std::string& key, const std::vector<uint8_t>& value)
{
    if (key == CONFIG_KEY_ENGLISH_PAIRS)
    {
        if (value.size() < 2) return;
        bool enabled = value[0] != 0;
        uint8_t count = value[1];

        std::vector<std::pair<wchar_t, wchar_t>> pairs;
        for (size_t i = 0; i < count && (2 + i * 4 + 4) <= value.size(); i++)
        {
            uint16_t left = *reinterpret_cast<const uint16_t*>(value.data() + 2 + i * 4);
            uint16_t right = *reinterpret_cast<const uint16_t*>(value.data() + 2 + i * 4 + 2);
            pairs.push_back({(wchar_t)left, (wchar_t)right});
        }

        _englishPairEngine.SetPairs(pairs);
        _englishPairEngine.SetEnabled(enabled);

        WIND_LOG_INFO_FMT(L"English pair config updated: enabled=%d, pairs=%d\n", enabled, (int)pairs.size());
    }
    else if (key == CONFIG_KEY_JUMP_OUT_KEYS)
    {
        // 格式：right_symbol(u8) + count(u8) + [vk:u16(LE)]...（对齐 Rust encode_jump_out_keys_value）
        _jumpOutKeys.clear();
        _jumpOutOnRightSymbol = false;
        if (value.size() < 2) return;
        _jumpOutOnRightSymbol = value[0] != 0;
        uint8_t count = value[1];
        for (size_t i = 0; i < count && (2 + i * 2 + 2) <= value.size(); i++)
        {
            uint16_t vk = *reinterpret_cast<const uint16_t*>(value.data() + 2 + i * 2);
            _jumpOutKeys.insert((UINT)vk);
        }
        WIND_LOG_INFO_FMT(L"Jump-out keys config updated: count=%d, right_symbol=%d\n",
                          (int)_jumpOutKeys.size(), (int)_jumpOutOnRightSymbol);
    }
    else if (key == CONFIG_KEY_CUSTOM_EN_PUNCT)
    {
        // 格式：count(u8) + [ch:u16(LE)]...（对齐 Rust encode_custom_en_punct_value）
        _customEnPunctChars.clear();
        if (value.empty()) return;
        uint8_t count = value[0];
        for (size_t i = 0; i < count && (1 + i * 2 + 2) <= value.size(); i++)
        {
            uint16_t ch = *reinterpret_cast<const uint16_t*>(value.data() + 1 + i * 2);
            _customEnPunctChars.insert((wchar_t)ch);
        }
        WIND_LOG_INFO_FMT(L"Custom english punct chars updated: count=%d\n", (int)_customEnPunctChars.size());
    }
    else if (key == CONFIG_KEY_PAIR_STATE_TTL)
    {
        // 格式：secs(u16 LE)（对齐 Rust encode_pair_state_ttl_value）
        if (value.size() < 2) return;
        uint16_t secs = *reinterpret_cast<const uint16_t*>(value.data());
        _pairStateTtlMs = (ULONGLONG)secs * 1000ULL;
        WIND_LOG_INFO_FMT(L"Pair state TTL updated: %d s\n", (int)secs);
    }
    else if (key == CONFIG_KEY_PASSWORD_SUPPRESS)
    {
        // 格式：enabled(u8)（对齐 Rust encode_password_suppress_value）
        if (value.empty()) return;
        BOOL enabled = value[0] != 0;
        _pTextService->SetPasswordSuppressEnabled(enabled);
        WIND_LOG_INFO_FMT(L"Password suppress policy updated: enabled=%d\n", enabled);
    }
    else if (key == CONFIG_KEY_LANGBAR_TOOLTIP)
    {
        // 格式：[ch:u16(LE)]...（对齐 Rust push_langbar_tooltip）。value 即整段文本。
        // 空 value 是合法的：服务端从不发空串，但真收到就当作「没有文本」，
        // GetTooltipString 会回落到本地默认文案。
        std::wstring text(reinterpret_cast<const wchar_t*>(value.data()), value.size() / 2);
        _pTextService->SetLangBarTooltip(text);
        WIND_LOG_DEBUG_FMT(L"LangBar tooltip updated: %ls\n", text.c_str());
    }
    else if (key == CONFIG_KEY_DIAG_SNAPSHOT)
    {
        // 格式：enabled(u8)（对齐 Rust encode_diag_snapshot_value）
        if (value.empty()) return;
        BOOL enabled = value[0] != 0;
        _pTextService->SetDiagSnapshotEnabled(enabled);
        WIND_LOG_INFO_FMT(L"Diag snapshot collection updated: enabled=%d\n", enabled);
    }
    else if (key == CONFIG_KEY_STATS)
    {
        if (value.size() < 2) return;

        _statsEnabled = value[0] != 0;
        _statsTrackEnglish = value[1] != 0;
        if (!_statsEnabled || !_statsTrackEnglish)
        {
            _englishStats.Reset();
        }

        WIND_LOG_INFO_FMT(L"Stats config updated: enabled=%d, trackEnglish=%d\n",
            _statsEnabled ? 1 : 0, _statsTrackEnglish ? 1 : 0);
    }
}

// ============================================================================
// Barrier mechanism implementation
// ============================================================================

BOOL CKeyEventSink::_SendCommitRequest(uint16_t barrierSeq, uint16_t triggerKey, uint32_t mods, const std::string& inputBuffer)
{
    CIPCClient* pIPCClient = _pTextService->GetIPCClient();
    if (pIPCClient == nullptr || !pIPCClient->IsConnected())
    {
        return FALSE;
    }

    // Build CommitRequestPayload
    //
    // 尺寸跟着结构体走，不写字面量 12：`sizeof` 有 BinaryProtocol.h 里的 static_assert
    // 兜底，结构体一旦改动这里自动跟随；写死的 12 只会在改动那天悄悄少写几个字节。
    // （此前两者并存——算了 payloadSize 却用字面量开 vector，数值恰好相等而已。）
    size_t payloadSize = sizeof(CommitRequestPayload) + inputBuffer.size();
    std::vector<uint8_t> payload(payloadSize);

    // Header fields
    payload[0] = barrierSeq & 0xFF;
    payload[1] = (barrierSeq >> 8) & 0xFF;
    payload[2] = triggerKey & 0xFF;
    payload[3] = (triggerKey >> 8) & 0xFF;
    payload[4] = mods & 0xFF;
    payload[5] = (mods >> 8) & 0xFF;
    payload[6] = (mods >> 16) & 0xFF;
    payload[7] = (mods >> 24) & 0xFF;
    uint32_t inputLen = (uint32_t)inputBuffer.size();
    payload[8] = inputLen & 0xFF;
    payload[9] = (inputLen >> 8) & 0xFF;
    payload[10] = (inputLen >> 16) & 0xFF;
    payload[11] = (inputLen >> 24) & 0xFF;

    // Copy input buffer
    if (!inputBuffer.empty())
    {
        memcpy(payload.data() + 12, inputBuffer.data(), inputBuffer.size());
    }

    return pIPCClient->SendCommitRequest(payload.data(), (uint32_t)payload.size());
}

void CKeyEventSink::_HandleCommitResult(uint16_t barrierSeq, const std::wstring& text, const std::wstring& newComp, bool modeChanged, bool chineseMode)
{
    if (!_pendingCommit.waiting || _pendingCommit.barrierSeq != barrierSeq)
    {
        // Barrier mismatch, log warning
        WIND_LOG_TRACE(L"CommitResult barrier mismatch, ignoring\n");
        return;
    }

    // Clear pending state
    _pendingCommit.waiting = false;

    // Commit the text and handle composition atomically
    if (!newComp.empty())
    {
        // Has new composition: use InsertTextAndStartComposition (now handles end old composition internally)
        _pTextService->InsertTextAndStartComposition(text, newComp);
        _isComposing = TRUE;
    }
    else
    {
        // No new composition: atomic commit (end composition + insert text)
        _pTextService->CommitText(text);
        _isComposing = FALSE;
        _hasCandidates = FALSE;
    }

    // Handle mode change
    if (modeChanged)
    {
        _pTextService->SetInputMode(chineseMode);
    }
}

// 读 resync 旗 + 过期检查。deadline 到期立即清旗，保证只读处不需要关心时间窗口。
// 注意：_resyncFailStreak 在此不清零——streak 仅由"响应成功"清零，否则失败计数被
// 时间衰减抹掉就失去了"连续失败 → 降级"的语义。
BOOL CKeyEventSink::_IsResyncActive()
{
    if (!_needsCompositionResync)
        return FALSE;
    if (GetTickCount() >= _resyncDeadline)
    {
        WIND_LOG_DEBUG_FMT(L"Resync window expired (streak=%d), auto-clearing flag",
                           _resyncFailStreak);
        _needsCompositionResync = FALSE;
        _resyncDeadline = 0;
        return FALSE;
    }
    return TRUE;
}

// 「有活跃输入会话」的唯一判据（声明处有为何收口的来龙去脉）。四个分量：
//   HasActiveComposition : TSF 组合活跃 —— 常规主判据。
//   _hasCandidates       : 候选窗有内容而组合为空（非 app_inline 时 core 发空组合串）。
//   _IsResyncActive      : 上次 IPC 失败后的自愈窗口，强行视作有会话以便重握手。
//   HasDeferredComposition: CommitThenDefer（direct_commit 顶码）已 commit、余码组合尚未
//     重开的真空期。coordinator 缓冲里确有余码，该段内的键理应归本输入法；漏掉会让空格
//     插入字面空格、退格删掉刚上屏的字（真机复现：skce 顶码后快打 h + 空格）。
BOOL CKeyEventSink::_HasInputSession()
{
    return _pTextService->HasActiveComposition()
        || _hasCandidates
        || _IsResyncActive()
        || _pTextService->HasDeferredComposition();
}

void CKeyEventSink::_CheckBarrierTimeout()
{
    if (!_pendingCommit.waiting)
        return;

    DWORD elapsed = GetTickCount() - _pendingCommit.requestTime;
    if (elapsed > BARRIER_TIMEOUT_MS)
    {
        WIND_LOG_ERROR(L"Barrier timeout, falling back to local handling");

        // Timeout - clear pending state and try to recover
        _pendingCommit.waiting = false;

        // Fallback: just clear the composition
        _pTextService->EndComposition();
        _isComposing = FALSE;
        _hasCandidates = FALSE;
    }
}

// ============================================================================
// Auto-pair key simulation (deferred + skip list approach)
//
// When modifiers are held (e.g., Shift for "("), we defer the cursor key
// until modifiers are released. This avoids the fundamental flaw of the
// "release modifiers via SendInput" approach: releasing and restoring Shift
// via SendInput causes the OS to generate additional Shift key-down events
// (with repeat bit 0), which re-arms _pendingKeyUpKey and triggers mode
// toggle when the physical Shift is released.
// ============================================================================

void CKeyEventSink::HandlePairCommitPush(const std::wstring& text, uint32_t moveLeft)
{
    WIND_LOG_DEBUG_FMT(L"HandlePairCommitPush: textLen=%zu, moveLeft=%u\n",
                       text.length(), moveLeft);
    _pTextService->CommitText(text);
    _isComposing = FALSE;
    _hasCandidates = FALSE;
    for (uint32_t i = 0; i < moveLeft; i++)
        _SimulatePairKey(VK_LEFT);
    // moveLeft==0 说明协调器判定为「退化纯上屏」，那侧也没压栈，此处同样不记账——
    // 深度与 core 的 pair_tracker 必须严格同步，宁可两边都没有，不要一边有。
    if (moveLeft > 0)
    {
        _pairPendingDepth++;
        TouchPairState();
    }
}

void CKeyEventSink::_SimulatePairKey(WORD vk)
{
    if (_AreModifiersHeld())
    {
        // Defer: save action, execute when modifiers released
        if (!_pendingPairAction.active)
        {
            _pendingPairAction.vk = vk;
            _pendingPairAction.count = 1;
            _pendingPairAction.active = true;
        }
        else if (_pendingPairAction.vk == vk)
        {
            // Same key deferred again (e.g., Shift+< pressed multiple times)
            // Only the last pair's cursor positioning matters, keep count = 1
        }
        else
        {
            // Different key — replace pending action
            _pendingPairAction.vk = vk;
            _pendingPairAction.count = 1;
        }
        WIND_LOG_DEBUG_FMT(L"Auto-pair: deferred vk=0x%02X x%d (modifiers held)\n",
            (WORD)vk, _pendingPairAction.count);
        return;
    }

    // No modifiers: execute immediately via skip list
    _PushSkipKey(vk);

    INPUT inputs[2] = {};
    inputs[0].type = INPUT_KEYBOARD;
    inputs[0].ki.wVk = vk;
    inputs[1].type = INPUT_KEYBOARD;
    inputs[1].ki.wVk = vk;
    inputs[1].ki.dwFlags = KEYEVENTF_KEYUP;
    SendInput(2, inputs, sizeof(INPUT));
}

// 把一个已被我们吃掉的键原样重放给宿主。
// 与 _SimulatePairKey 的区别：**不做修饰键 defer**。那边注入的是 VK_RIGHT（我们自己
// 造的光标移动），物理按住的 Shift 叠上去会变成「Shift+→ 选中」，语义被污染；这里重放的
// 是用户真按下的那个键，Shift/Ctrl 叠加恰恰还原用户本意（Shift+Enter 就该是 Shift+Enter）。
void CKeyEventSink::_ReplayKeyToHost(WORD vk)
{
    // skip 条目**只压一个**（与 _SimulatePairKey 一致）：down 消费掉它，注入的 keyup 走
    // 正常路径。看似该压两个（down/up 都是合成的），但那样更危险——若宿主不调
    // OnTestKeyUp（本仓已知 mintty 类宿主有此怪癖），多出的条目会残留，把用户**下一次
    // 真实按下的同一个键**静默吃掉。重放的都不是 toggle 键，keyup 走正常路径无副作用。
    _PushSkipKey(vk);

    INPUT inputs[2] = {};
    inputs[0].type = INPUT_KEYBOARD;
    inputs[0].ki.wVk = vk;
    inputs[1].type = INPUT_KEYBOARD;
    inputs[1].ki.wVk = vk;
    inputs[1].ki.dwFlags = KEYEVENTF_KEYUP;
    UINT sent = SendInput(2, inputs, sizeof(INPUT));
    if (sent != 2)
    {
        // 注入失败＝这个键彻底消失（原键已被我们吃掉），且用户毫无感知。
        // 必须留痕：否则现象是「hold 后按某键偶尔没反应」，无从诊断。
        WIND_LOG_WARN_FMT(L"HoldReplay: SendInput sent %u of 2 for vk=0x%02X, key lost\n",
                          sent, (uint32_t)vk);
        return;
    }
    WIND_LOG_DEBUG_FMT(L"HoldReplay: replayed vk=0x%02X to host after commit\n", (uint32_t)vk);
}

bool CKeyEventSink::_AreModifiersHeld()
{
    return (GetAsyncKeyState(VK_SHIFT) & 0x8000) != 0 ||
           (GetAsyncKeyState(VK_CONTROL) & 0x8000) != 0 ||
           (GetAsyncKeyState(VK_MENU) & 0x8000) != 0;
}

void CKeyEventSink::_PushSkipKey(WORD vk)
{
    if (_skipKeyCount < MAX_SKIP_KEYS)
    {
        _skipKeys[_skipKeyCount++] = vk;
    }
}

BOOL CKeyEventSink::_TryConsumeSkipKey(WPARAM wParam)
{
    if (_skipKeyCount > 0 && _skipKeys[0] == (WORD)wParam)
    {
        // Shift remaining entries left
        for (int i = 1; i < _skipKeyCount; i++)
            _skipKeys[i - 1] = _skipKeys[i];
        _skipKeyCount--;
        WIND_LOG_DEBUG_FMT(L"Auto-pair: skip key 0x%02X bypassed IME, remaining=%d\n", (WORD)wParam, _skipKeyCount);
        return TRUE;
    }
    return FALSE;
}

BOOL CKeyEventSink::_SendAsyncCommitTriggerKey()
{
    // down+up 一次性提交（同 _SimulatePairKey/_ReplayKeyToHost 的批量提交先例），避免
    // 被并发的真实键鼠输入从中间穿插。VK_ASYNC_COMMIT_TRIGGER 是保留/未分配值，无需
    // 走 _PushSkipKey——它不会被当成真实按键，也不该被"直接放行"，而是要被
    // OnTestKeyDown/OnKeyDown 的专门分支吃掉、转入我们自己的同步提交。
    INPUT inputs[2] = {};
    inputs[0].type = INPUT_KEYBOARD;
    inputs[0].ki.wVk = VK_ASYNC_COMMIT_TRIGGER;
    inputs[1].type = INPUT_KEYBOARD;
    inputs[1].ki.wVk = VK_ASYNC_COMMIT_TRIGGER;
    inputs[1].ki.dwFlags = KEYEVENTF_KEYUP;
    UINT sent = SendInput(2, inputs, sizeof(INPUT));
    if (sent != 2)
    {
        WIND_LOG_WARN_FMT(L"_SendAsyncCommitTriggerKey: SendInput sent %u of 2\n", sent);
        return FALSE;
    }
    return TRUE;
}

BOOL CKeyEventSink::QueueAsyncCommitViaSyntheticKey(const std::wstring& text, BOOL replacingHeld)
{
    if (text.empty())
        return TRUE; // 无事可做，不必绕一圈合成按键。

    if (_pendingAsyncCommits.size() >= MAX_PENDING_ASYNC_COMMITS)
    {
        // 队列积压＝上一批触发键没有正常送达/消费（多半是目标窗口已经失焦）。继续囤积
        // 只会让文本越堆越多、之后乱序上屏，故整体清空并留痕——好过无界增长或半截乱序。
        WIND_LOG_WARN(L"QueueAsyncCommitViaSyntheticKey: pending queue full, dropping all pending commits\n");
        _pendingAsyncCommits.clear();
    }
    _pendingAsyncCommits.push_back(PendingAsyncCommit{text, replacingHeld});

    if (!_SendAsyncCommitTriggerKey())
    {
        WIND_LOG_WARN_FMT(L"QueueAsyncCommitViaSyntheticKey: trigger key injection failed, textLen=%zu\n",
                          text.length());
        _pendingAsyncCommits.pop_back();
        return FALSE;
    }
    WIND_LOG_DEBUG_FMT(L"QueueAsyncCommitViaSyntheticKey: queued textLen=%zu, pending=%zu\n",
                       text.length(), _pendingAsyncCommits.size());
    return TRUE;
}

BOOL CKeyEventSink::_TryConsumeAsyncCommitTrigger(WPARAM wParam, PendingAsyncCommit& out)
{
    if (wParam != VK_ASYNC_COMMIT_TRIGGER || _pendingAsyncCommits.empty())
        return FALSE;
    out = _pendingAsyncCommits.front();
    _pendingAsyncCommits.pop_front();
    return TRUE;
}

void CKeyEventSink::_RecordEnglishKeyTrace(WPARAM wParam, uint32_t modifiers)
{
    if (!_statsEnabled || !_statsTrackEnglish)
        return;

    if (_pTextService->IsChineseMode())
        return;

    // Count source keystrokes only. Ctrl/Alt combinations are shortcuts, not text input.
    if (modifiers & (KEYMOD_CTRL | KEYMOD_ALT))
        return;

    bool counted = false;
    if (wParam >= 'A' && wParam <= 'Z')
    {
        _englishStats.chars++;
        counted = true;
    }
    else if (wParam >= '0' && wParam <= '9')
    {
        if (modifiers & KEYMOD_SHIFT)
            _englishStats.puncts++; // Shift+digit produces a symbol.
        else
            _englishStats.digits++;
        counted = true;
    }
    else if (wParam >= VK_NUMPAD0 && wParam <= VK_NUMPAD9)
    {
        _englishStats.digits++;
        counted = true;
    }
    else if (wParam == VK_MULTIPLY || wParam == VK_ADD || wParam == VK_SUBTRACT ||
             wParam == VK_DECIMAL || wParam == VK_DIVIDE)
    {
        _englishStats.puncts++;
        counted = true;
    }
    else if (wParam == VK_SPACE)
    {
        _englishStats.spaces++;
        counted = true;
    }
    else
    {
        HotkeyType keyType = CHotkeyManager::ClassifyInputKey(wParam, modifiers);
        if (keyType == HotkeyType::Punctuation ||
            keyType == HotkeyType::PageKey ||
            keyType == HotkeyType::SelectKey)
        {
            _englishStats.puncts++;
            counted = true;
        }
    }

    if (!counted)
        return;

    _englishStats.StartIfIdle();
    // openclose 是回读的**真实** compartment 值，用于验证「英文态收键是否依赖 compartment=1」。
    // 门控不可省：日志宏的实参在调用点即求值，不门控就等于在每个英文字符上做两次 COM 调用。
    if (CFileLogger::Instance().IsEnabled(CFileLogger::LogLevel::Debug))
    {
        WIND_LOG_DEBUG_FMT(L"EnglishStats counted from key trace: vk=0x%02X total=%u shouldReport=%d openclose=%d\n",
            (uint32_t)wParam, _englishStats.Total(), _englishStats.ShouldReport() ? 1 : 0,
            _pTextService->GetOpenCloseCompartmentValue());
    }

    if (_englishStats.ShouldReport())
        _ReportEnglishStats();
}

void CKeyEventSink::_ReportEnglishStats()
{
    if (!_statsEnabled || !_statsTrackEnglish)
    {
        _englishStats.Reset();
        return;
    }

    if (_englishStats.Total() == 0)
        return;

    CIPCClient* pIPCClient = _pTextService->GetIPCClient();
    if (pIPCClient == nullptr || !pIPCClient->IsConnected())
    {
        _englishStats.Reset();
        return;
    }

    InputStatsPayload payload = {};
    payload.englishChars = _englishStats.chars;
    payload.englishDigits = _englishStats.digits;
    payload.englishPuncts = _englishStats.puncts;
    payload.englishSpaces = _englishStats.spaces;
    payload.elapsedMs = _englishStats.ElapsedMs();

    pIPCClient->SendAsync(CMD_INPUT_STATS, &payload, sizeof(payload));

    WIND_LOG_INFO_FMT(L"InputStats reported: chars=%u digits=%u puncts=%u spaces=%u elapsedMs=%u\n",
        _englishStats.chars, _englishStats.digits, _englishStats.puncts, _englishStats.spaces, payload.elapsedMs);

    _englishStats.Reset();
}

void CKeyEventSink::FlushEnglishStats()
{
    _ReportEnglishStats();
}

