#include "KeyEventSink.h"
#include "DeferredCompositionPolicy.h"
#include "CtrlAltCleanupPolicy.h"
#include "CandidateHotkeyPolicy.h"
#include "HoldReplayPolicy.h"
#include "NumberKeyPolicy.h"
#include "PairCommitPolicy.h"
#include "SpaceKeyPolicy.h"
#include "SmartPunctPrevChar.h"
#include "TextService.h"
#include "IPCClient.h"
#include "HotkeyManager.h"
#include "BinaryProtocol.h"
#include "WindowTitlePolicy.h" // CONFIG_KEY_COMPAT_TITLE_MATCH 的值解析
#include <cctype>
#include <cstdio>  // for swprintf

// CtrlSpacePolicy.h 不含 Win32 头、自带一份修饰位与 VK 常量（好让 g++ 单测），这里钉住两边一致。
static_assert(wind::ctrlspace::kModShift == KEYMOD_SHIFT, "CtrlSpacePolicy kModShift != KEYMOD_SHIFT");
static_assert(wind::ctrlspace::kModCtrl == KEYMOD_CTRL, "CtrlSpacePolicy kModCtrl != KEYMOD_CTRL");
static_assert(wind::ctrlspace::kModAlt == KEYMOD_ALT, "CtrlSpacePolicy kModAlt != KEYMOD_ALT");
static_assert(wind::ctrlspace::kVkSpace == VK_SPACE, "CtrlSpacePolicy kVkSpace != VK_SPACE");

namespace
{
    // 纯修饰键（Shift/Ctrl/Alt）绝不能吃，哪怕被配成中英切换键：切换判定挂在 keyup 上，
    // 吃 keydown 换不来任何东西，却让宿主看不到修饰键（AutoCAD、Fusion 360 会因此卡顿，
    // 见 docs/architecture/tsf-key-sink-host-notes.md）。CapsLock 不在此列：要吃掉才能压住大写副作用。
    // down 和 up 必须一致放行，只放行一边会让宿主看到卡死的修饰键。
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
    , _seqChar(WindInputSeq::kUnknown)
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

// 鼠标键（左/右/中）的异步状态：此刻是否按着、自上次调用以来是否被按下过。
// 左右键都查：系统可能交换了主副键，VK_LBUTTON 反映的是物理键。
// 两位必须一次调用取：GetAsyncKeyState 每次调用都清「按下过」位，分两次调第二次恒为 0。
// 「按下过」位微软明说不可依赖（别的进程调用也会清），只作补充信号，见 ToggleTapPolicy.h。
struct MouseAsyncState
{
    bool down = false;
    bool pressedSinceLastCall = false;
};

static MouseAsyncState ReadMouseAsyncState()
{
    const SHORT bits = GetAsyncKeyState(VK_LBUTTON) | GetAsyncKeyState(VK_RBUTTON) | GetAsyncKeyState(VK_MBUTTON);
    MouseAsyncState st;
    st.down = (bits & 0x8000) != 0;
    st.pressedSinceLastCall = (bits & 0x0001) != 0;
    return st;
}

BOOL CKeyEventSink::BareAssocMouseClicked()
{
    if (!_assocBareSession)
        return FALSE;
    const MouseAsyncState mouse = ReadMouseAsyncState();
    return (mouse.down || mouse.pressedSinceLastCall) ? TRUE : FALSE;
}

void CKeyEventSink::NoteSelectionChangedDuringToggle()
{
    if (_pendingKeyUpKey == 0 && _ctrlRaw.vk == 0)
        return;
    // OnEndEdit 是异步通知，快速单击时鼠标键可能已经抬起，故除「此刻按着」外还要看
    // 「按下过」位；读位会清它，命中就并入 mousePressedSincePress，keyup 时不丢。
    // 待判单击与 Ctrl 如实上报各记一份（两者可能同时在按住中，如 Ctrl 按着再按 Shift）。
    const MouseAsyncState mouse = ReadMouseAsyncState();
    wind::toggletap::TapSignals* const sigs[] = {&_pendingTapSignals, &_ctrlRawTapSignals};
    for (wind::toggletap::TapSignals* sig : sigs)
    {
        if (mouse.pressedSinceLastCall)
            sig->mousePressedSincePress = true;
        if (mouse.down || mouse.pressedSinceLastCall)
            sig->selectionChanged = true;
    }
}

// 记录「这个切换键正等着 keyup 触发切换」。
// 由 OnTestKeyDown（放行纯修饰键时）与 OnKeyDown 共同调用；对同一个键重复调用不会
// 重新计时（见函数内说明），因此两处都调、宿主重复发 keydown，都不会影响长按判定。
void CKeyEventSink::_MarkPendingToggleKey(WPARAM wParam, uint32_t modifiers)
{
    // 必须解析出具体左右键（白名单登记的是 VK_LSHIFT / VK_RSHIFT）。优先用 modifiers，
    // 降级 GetAsyncKeyState：Chromium 系宿主（WebView2 等）下后者拿不到具体左右键。
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
    // 同一个键已在 pending 中就不重新计时：宿主会为按住的键重复发 keydown（AutoCAD、Word），
    // 每次重置会让长按被误判成轻敲而误切中英。重复事件只刷新修饰键位。
    if (_pendingKeyUpKey == specificKey)
    {
        _pendingKeyUpModifiers = modifiers;
        return;
    }

    _pendingKeyUpKey = specificKey;
    _pendingKeyUpModifiers = modifiers;
    _pendingKeyDownTime = GetTickCount();

    // 鼠标记账：首次记 pending 时重置，读一次异步状态顺带清掉「按下过」位，
    // 好让 _DispatchPendingToggleKeyUp 读到的只是按住期间发生的点击。
    _pendingTapSignals = {};
    _pendingTapSignals.mouseDownAtPress = ReadMouseAsyncState().down;
}

// 取消待切换。热键分支（keydown 白名单 / chinese-only / session / Ctrl+Space）都就地 return，
// 够不着 OnTestKeyDown 下方的统一取消，必须显式调；否则 Shift+Space 松开 Shift 时还会再切一次中英。
// 命中热键白名单即说明是组合键而非轻敲，故无条件取消。
void CKeyEventSink::_CancelPendingToggle(WPARAM wParam, const wchar_t* reason)
{
    if (_pendingKeyUpKey == 0)
        return;
    // 纯修饰键自身永不在此取消：宿主对按住的键重复发 keydown，若它也命中 keydown 白名单
    // （select_key_groups 支持 lrshift/lrctrl），「取消→重记」会让长按被误判成轻敲。
    if (_IsPureModifierKey(wParam))
        return;

    WIND_LOG_DEBUG_FMT(L"Cancel pending toggle (%ls): vk=0x%02X pending=0x%02X\n",
        reason ? reason : L"-", (uint32_t)wParam, _pendingKeyUpKey);
    _pendingKeyUpKey = 0;
    _pendingKeyUpModifiers = 0;
}

// Ctrl 的左右：优先 modifiers（双源），降级 GetAsyncKeyState；同 _MarkPendingToggleKey。认不出给 0。
uint32_t CKeyEventSink::_ResolveCtrlVk(WPARAM wParam, uint32_t modifiers)
{
    if (wParam == VK_LCONTROL || wParam == VK_RCONTROL)
        return (uint32_t)wParam;
    if (wParam != VK_CONTROL)
        return 0;
    if (modifiers & KEYMOD_LCTRL)
        return VK_LCONTROL;
    if (modifiers & KEYMOD_RCTRL)
        return VK_RCONTROL;
    if (GetAsyncKeyState(VK_LCONTROL) & 0x8000)
        return VK_LCONTROL;
    if (GetAsyncKeyState(VK_RCONTROL) & 0x8000)
        return VK_RCONTROL;
    return 0;
}

void CKeyEventSink::_NoteKeyDownForCtrlRaw(WPARAM wParam)
{
    if (wParam == VK_CONTROL || wParam == VK_LCONTROL || wParam == VK_RCONTROL)
        return; // Ctrl 自己归 _TryCtrlRawDown
    wind::ctrlraw::OnOtherKey(_ctrlRaw);
}

// 候选显示时 Ctrl 如实上报的按下（规则见 CtrlRawPolicy.h）。放在一切白名单判定之前：这条路
// 与切换键、切换方式无关，服务端替中英切换 / 选词 / 会话绑定 / 注释切换判单击。
BOOL CKeyEventSink::_TryCtrlRawDown(WPARAM wParam, LPARAM lParam, uint32_t modifiers)
{
    if (!(wParam == VK_CONTROL || wParam == VK_LCONTROL || wParam == VK_RCONTROL))
        return FALSE;
    const uint32_t vk = _ResolveCtrlVk(wParam, modifiers);
    const bool autoRepeat = (lParam & 0x40000000) != 0;
    switch (wind::ctrlraw::OnCtrlDown(_ctrlRaw, vk, _HasInputSession() != FALSE, autoRepeat, _pendingKeyUpKey))
    {
    case wind::ctrlraw::Down::Legacy:
        return FALSE;
    case wind::ctrlraw::Down::Repeat:
        return TRUE;
    case wind::ctrlraw::Down::Start:
        break;
    }
    // 别的键的待判单击（如 Shift 按着再按 Ctrl）就此作废，与旧路「别的键取消待判」一致。
    _pendingKeyUpKey = 0;
    _pendingKeyUpModifiers = 0;
    _ctrlRawTapSignals = {};
    _ctrlRawTapSignals.mouseDownAtPress = ReadMouseAsyncState().down;
    WIND_LOG_DEBUG_FMT(L"Ctrl 如实上报 按下: vk=0x%02X mods=0x%04X\n", vk, modifiers);
    if (_SendKeyToService(vk, modifiers, KEY_EVENT_MOD_DOWN))
        _HandleServiceResponse();
    else
        WIND_LOG_ERROR(L"IPC failed for Ctrl raw down\n");
    return TRUE;
}

// 候选显示时 Ctrl 如实上报的松开：是那次按住就发 MOD_UP（按住翻转靠它收起，故不看此刻有没有会话）。
void CKeyEventSink::_DispatchCtrlRawUp(WPARAM wParam)
{
    if (_ctrlRaw.vk == 0)
        return;
    if (!(wParam == VK_CONTROL || wParam == VK_LCONTROL || wParam == VK_RCONTROL))
        return;
    if (!_IsMatchingKeyUp(wParam, _ctrlRaw.vk))
        return;
    wind::toggletap::TapSignals signals = _ctrlRawTapSignals;
    const MouseAsyncState mouse = ReadMouseAsyncState();
    signals.mouseDownAtRelease = mouse.down;
    signals.mousePressedSincePress = signals.mousePressedSincePress || mouse.pressedSinceLastCall;
    _ctrlRawTapSignals = {};
    const wind::ctrlraw::Up up =
        wind::ctrlraw::OnCtrlUp(_ctrlRaw, true, wind::toggletap::IsTapCancelledByMouse(signals));
    if (!up.report)
        return;
    const uint32_t mods = KEYMOD_CTRL | (up.vk == VK_LCONTROL ? KEYMOD_LCTRL : KEYMOD_RCTRL);
    WIND_LOG_DEBUG_FMT(L"Ctrl 如实上报 松开: vk=0x%02X notTap=%d\n", up.vk, (int)up.notTap);
    // 判出单击可能切中英（状态气泡要锚到当前插入点），同旧路。
    _pTextService->SendCaretPositionUpdate();
    if (_SendKeyToService(up.vk, mods, KEY_EVENT_MOD_UP, up.notTap ? TOGGLE_MOD_NOT_TAP : 0))
        _HandleServiceResponse();
    else
        WIND_LOG_ERROR(L"IPC failed for Ctrl raw up\n");
}

// 服务端吃掉过按下的功能键：把松开也送过去（按住档的注释切换键靠它收起详细态）。按下吃了，
// 松开同样吃掉，宿主两边都看不到这个键。OnTestKeyUp 先到就由它发、OnKeyUp 只收尾
// （finalize）；只调 OnTestKeyUp 的宿主（mintty）留下的记账在下一次按下时覆盖。
// 返回 TRUE = 这次松开属于那个键，调用方吃掉。
BOOL CKeyEventSink::_DispatchEatenFnKeyUp(WPARAM wParam, BOOL finalize)
{
    if (_eatenFnKeyVk == 0 || (UINT)wParam != _eatenFnKeyVk)
        return FALSE;
    if (!_eatenFnKeyUpSent)
    {
        _eatenFnKeyUpSent = TRUE;
        if (_SendKeyToService((uint32_t)wParam, 0, KEY_EVENT_UP))
            _HandleServiceResponse();
        else
            WIND_LOG_ERROR(L"IPC failed for function key up\n");
    }
    if (finalize)
    {
        _eatenFnKeyVk = 0;
        _eatenFnKeyUpSent = FALSE;
    }
    return TRUE;
}

// ITfKeyEventSink::OnSetFocus 很不可靠（有的宿主从不给 fForeground=0），别往这里挂职责。
// 应用切入/切出的权威信号是 ITfThreadFocusSink::OnSet/KillThreadFocus。
STDAPI CKeyEventSink::OnSetFocus(BOOL fForeground)
{
    WIND_LOG_INFO_FMT(L"KeyEventSink::OnSetFocus fForeground=%d\n", fForeground ? 1 : 0);
    return S_OK;
}

// 软键盘总闸的唯一判据，`OnTestKeyDown`（吃）与 `OnKeyDown`（转发）必须都调它：两边各写一份，
// 那边吃了这边不发，键就凭空消失。
// 键盘面（send_keys）只接管 Esc 与翻页，其余落回常规链路，所以能组码打中文；符号面接管整个主键区。
// 两种面都不吃 Space / Tab / Enter / Backspace / 方向键（auto-repeat 由系统提供），Ctrl/Alt 组合一律放行。
bool CKeyEventSink::_IsSoftKeyboardEatenKey(WPARAM vk, uint32_t modifiers) const
{
    if (_pTextService == nullptr || !_pTextService->IsSoftKeyboard())
        return false;
    if (modifiers & (KEYMOD_CTRL | KEYMOD_ALT))
        return false;

    // 布局之外的键（小键盘）一律不吃，见 _IsSoftKeyboardOutOfLayoutKey。
    // 必须排在 ClassifyInputKey 之前：那里把小键盘归成 Number，落到下面的 switch 就被吃了。
    if (_IsSoftKeyboardOutOfLayoutKey(vk))
        return false;

    HotkeyType t = CHotkeyManager::ClassifyInputKey(vk, modifiers);
    // 面板控制键：两种面都归面板。
    if (t == HotkeyType::Escape || t == HotkeyType::PageKey)
        return true;
    // 键盘面：其余一律交还常规链路。
    if (_pTextService->IsSoftKeyboardKeys())
        return false;
    switch (t)
    {
    case HotkeyType::Letter:
    case HotkeyType::Number:
    case HotkeyType::Punctuation:
    case HotkeyType::SelectKey:
        return true;
    default:
        return false;
    }
}

// Ctrl+Shift+F12 日志导出热键的唯一判据（理由见声明处）。每个按键最前面都调两次，
// 所以先比 vk、再看开关、最后才 GetKeyState，常态下只有一次比较。
bool CKeyEventSink::_IsLogDumpHotkey(WPARAM vk)
{
    if (vk != VK_F12)
        return false;
    if (!CFileLogger::Instance().IsDumpHotkeyEnabled())
        return false;
    return (GetKeyState(VK_CONTROL) & 0x8000) && (GetKeyState(VK_SHIFT) & 0x8000)
        && !(GetKeyState(VK_MENU) & 0x8000);
}

STDAPI CKeyEventSink::OnTestKeyDown(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten)
{
    // 服务端在异步线程宣布无占位联想会话结束（超时隐藏等）：候选收起欠在 TSF 线程补。
    // 期间若又起了组合/候选就不收。
    if (_assocBareEndPending)
    {
        _assocBareEndPending = FALSE;
        _EndAssocBareSession();
        if (!_pTextService->HasActiveComposition() && !_hasCandidates)
            _pTextService->NotifyCandidatesVisibilityChanged(FALSE);
    }
    *pfEaten = FALSE;

    // 服务进程 Alt 钩子注入的屏蔽键：宿主要看到它才不激活菜单栏，一律放行、不转发、不记账。
    if (wParam == WindPassthrough::kVkMenuMask)
        return S_OK;

    // 透传上报：本函数 return 太多，守卫在出口统一按最终的 `*pfEaten` 判（没吃下 = 服务端
    // 看不见这次输入，智能符号的 press2 判定要知道）。见 KeyEventSink.h 的 _NotePassthroughKeyDown。
    PassthroughNoter _ptNote{this, wParam, pfEaten, lParam, true};

    // 合成提交触发键：无条件吃下，不进下面任何按键逻辑；真正的提交在 OnKeyDown 里执行
    // （TSF 认可的按键处理期间）。放在最前面，与组合状态无关。
    if (wParam == VK_ASYNC_COMMIT_TRIGGER && !_pendingAsyncCommits.empty())
    {
        *pfEaten = TRUE;
        return S_OK;
    }

    // 自生成键放行：auto-pair 的合成键与 _ReplayKeyToHost 的重放键都压在 skip 表里。
    // 传 FALSE = 只认「给 keydown 的」条目，见 _PushSkipKey 处的通道说明。
    if (_TryConsumeSkipKey(wParam, FALSE))
    {
        *pfEaten = FALSE; // Let it pass directly to the app
        // 自生成键是服务端让我们注入的，不算透传，见 PassthroughNoter::Suppress。
        _ptNote.Suppress();
        return S_OK;
    }

    // Ctrl 如实上报按住期间的别的键（含此后各分支放行给宿主、服务端看不见的那些）。
    _NoteKeyDownForCtrlRaw(wParam);

    // Ctrl+Shift+F12: Dump TSF ring buffer logs to clipboard (works in AppContainer)
    // 默认关，且判据与 OnKeyDown 共用同一个函数——见 _IsLogDumpHotkey。
    if (_IsLogDumpHotkey(wParam))
    {
        *pfEaten = TRUE;
        return S_OK;
    }

    // Trace: Log ALL key presses (very high frequency)
    WIND_LOG_TRACE_FMT(L"OnTestKeyDown: wParam=0x%02X\n", (uint32_t)wParam);
    // 外来的 VK_PACKET（不是 skip 表里我们注入的）：中文标点态下的 `,` `.` 以这种形态到达会被
    // 原样放行成 ASCII。记下物理逗号/句号键是否按着，区分「硬件键被别层转成 Unicode 包」与「纯软件注入」。
    if (wParam == VK_PACKET)
    {
        WIND_LOG_DEBUG_FMT(
            L"compat.vk_packet.external lParam=0x%08X scan=0x%02X extraInfo=0x%llX physComma=%d physPeriod=%d shift=%d",
            (uint32_t)lParam, (uint32_t)((lParam >> 16) & 0xFF), (unsigned long long)GetMessageExtraInfo(),
            (GetAsyncKeyState(VK_OEM_COMMA) & 0x8000) ? 1 : 0, (GetAsyncKeyState(VK_OEM_PERIOD) & 0x8000) ? 1 : 0,
            (GetAsyncKeyState(VK_SHIFT) & 0x8000) ? 1 : 0);
        WindLogForegroundProcessInfo(4, L"compat.vk_packet.external.foreground_host");
    }
    // 每个新按键都重新起算：凭据只对紧随其后的那一次 OnKeyDown 有效。
    _ctrlSpaceEatenInTest = FALSE;

    // Keyboard disabled by system: pass through all keys
    if (_pTextService->IsKeyboardDisabled())
        return S_OK;

    // 密码框强制英文抑制：全部放行。必须在吃键决策点判，只靠 core 回 PassThrough 会形成
    // 「吃了再吐」，Chrome/Electron 等宿主不补 WM_CHAR，字母被吞。判据见 CTextService::IsPasswordSuppressActive。
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
    // 下游 keydown 查表都用归一化 hash（不分左右）；切换键另用 keyUpHash，见下方。
    uint32_t modifiers = CHotkeyManager::GetCurrentModifiers();
    uint32_t normalizedMods = CHotkeyManager::NormalizeModifiers(modifiers);
    uint32_t normalizedKeyHash = CHotkeyManager::CalcKeyHash(normalizedMods, (uint32_t)wParam);

    CHotkeyManager* pHotkeyMgr = _pTextService->GetHotkeyManager();

    // 候选显示时 Ctrl 如实上报：放行给宿主，单击由服务端判（见 CtrlRawPolicy.h）。
    if (_TryCtrlRawDown(wParam, lParam, modifiers))
    {
        *pfEaten = FALSE;
        _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::ToggleMode,
                        _pTextService->IsChineseMode(), _pTextService->HasActiveComposition(), _hasCandidates,
                        TRUE, FALSE, L"ctrl_raw_report");
        return S_OK;
    }

    // Check if this is a KeyDown hotkey from the whitelist
    // Use normalized hash for function hotkeys (Ctrl+`, Shift+Space, etc.)
    if (pHotkeyMgr != nullptr && pHotkeyMgr->IsKeyDownHotkey(normalizedKeyHash))
    {
        _CancelPendingToggle(wParam, L"keydown_hotkey");

        // 「仅注册转发」的键（翻页键组 -=、选词键组 ;'）无会话时不吃并继续往下走：引擎只会回
        // PassThrough，WindTerm 等宿主处理不好「吃了再吐」；下方 ClassifyInputKey 会按标点处理。
        // 闸门只对 FORWARD_ONLY 生效，别扩到真动作热键（如 shift+space）：严格宿主（EverEdit）
        // 不吃就不再调 OnKeyDown，热键永远送不到。FORWARD_ONLY 由服务端标（action 为空，见 hotkey.rs）。
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
    // 平时走不到：候选可见期间这批键已被 CTextService::_RegisterCandidateHotkeys 用 RegisterHotKey
    // 拦下，这里是注册失败时的退路。排查候选热键先看 registered=N/M 日志。
    if (pHotkeyMgr != nullptr && pHotkeyMgr->IsKeyDownSessionHotkey(normalizedKeyHash))
    {
        _CancelPendingToggle(wParam, L"session_hotkey");
        BOOL chineseMode = _pTextService->IsChineseMode();
        // resync 期（上次 IPC 失败后）视作有会话，让 session 热键也送服务端重握手、由其响应清旗重建状态。
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
    // 优先用 modifiers，降级 GetAsyncKeyState（同 _MarkPendingToggleKey）。
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

    // CapsLock 的会话态绑定（`keys.session_actions` 里的 capslock）不在 TSF 处理，别在这里加分支：
    // pfEaten=TRUE 压不住锁定态翻转（系统在本回调之前就翻了），翻转后回敲复原有竞态且会触发厂商 OSD。
    // 由服务进程的 `WH_KEYBOARD_LL` 钩子在状态更新前拦截（wind-keys 的 `capslock_hook`），有会话时
    // 这里收不到；无会话时走下方的状态通知路径。
    if (pHotkeyMgr != nullptr && pHotkeyMgr->IsKeyUpHotkey(keyUpHash))
    {
        isToggleModeKey = TRUE;
    }
    else if ((pHotkeyMgr == nullptr || !pHotkeyMgr->HasHotkeys()) && CHotkeyManager::IsToggleModeKeyByVK(wParam))
    {
        // 只在白名单尚未加载时按 VK 兜底；加载后以白名单为准，否则未配成切换键的 Ctrl/Shift
        // 也会被拦（Fusion 360 的修饰键因此失效）。
        isToggleModeKey = TRUE;
    }

    if (isToggleModeKey)
    {
        BOOL hasSession = _pTextService->HasActiveComposition() || _hasCandidates;
        // CapsLock 的会话态绑定不在 TSF 判，见上方 isToggleModeKey 判定前的说明。
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
        // 纯修饰键放行给宿主，见 _IsPureModifierKey。
        const BOOL eatToggleDown = !_IsPureModifierKey(wParam);

        // 放行时必须在这里就记下待切换：pfEaten=FALSE 后 TSF 通常不再调 OnKeyDown。
        // OnKeyDown 若仍被调用会再记一次，_MarkPendingToggleKey 对同一个键不重新计时。
        if (!eatToggleDown)
            _MarkPendingToggleKey(wParam, modifiers);

        *pfEaten = eatToggleDown;
        _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::ToggleMode,
                        _pTextService->IsChineseMode(), _pTextService->HasActiveComposition(), _hasCandidates,
                        hasSession || hasTextCtx, eatToggleDown, L"toggle_mode_key");
        return S_OK;
    }

    // Ctrl+Space（系统 IME 中英切换热键）：吃掉以免 Space 落进输入。模式切换不在这里发生：
    // 系统热键取反 OPENCLOSE compartment，CTextService::OnChange 按值语义采纳。否定过的做法
    // 见 docs/architecture/tsf-openclose-compartment-semantics.md。
    // 用户可关（`keys.ctrl_space_toggle`）：关闭时不吃、凭据不置位，键落到常规流程（IDE 代码提示要用）；
    // 系统热键那条由服务端拒绝翻转，见 CtrlSpacePolicy.h。
    if (wind::ctrlspace::ShouldInterceptCtrlSpace(_ctrlSpaceToggleEnabled.load(), (uint32_t)wParam, modifiers))
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
    // 判据收口在 _HasInputSession()（含 defer 真空期），不得就地展开：与 OnKeyDown 不一致即
    // 「吃了再吐」或丢键。
    BOOL hasInputSession = _HasInputSession();

    // English auto-pair: intercept bracket keys in English mode.
    // Ctrl/Alt 组合留给热键/宿主（如 Ctrl+Shift+] open_settings），不做配对。
    // 全角时本地不判：全角配对由 core 的 english_fullwidth 分支出字，此处吃键会重复处理。
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

    // 配对跳出键：全模式统一闸门。`_pairPendingDepth > 0` 即「插入过配对、尚未跳出」；鼠标点走、
    // 删括号这类操作感知不到，靠 TTL 挡掉陈旧状态。吃键后一律转发协调器裁决（真相源在那边）。
    if (_pairPendingDepth > 0 && !IsPairStateStale()
        && _IsJumpOutKey((UINT)wParam)
        && !(modifiers & (KEYMOD_CTRL | KEYMOD_ALT | KEYMOD_SHIFT)))
    {
        *pfEaten = TRUE;
        // 部署指纹（勿删）：可 grep 构建产物 / 部署目录 / 真机日志，确认新 DLL 确实被加载。
        WIND_LOG_DEBUG_FMT(L"cross-mode jumpout: vk=0x%02X depth=%d chinese=%d\n",
                           (uint32_t)wParam, _pairPendingDepth, isChineseMode ? 1 : 0);
        _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers,
                        CHotkeyManager::ClassifyInputKey(wParam, modifiers),
                        isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"pair_jumpout_forward");
        return S_OK;
    }

    // 配对状态保活。必须在上面的陈旧判定之后，否则 TTL 永不触发；放在这里也覆盖英文模式的
    // 普通字母（协调器收不到），所以 TTL 以 DLL 侧为准。
    TouchPairState();

    // 软键盘总闸，与中英文模式无关（判据见 _IsSoftKeyboardEatenKey）。必须排在下面
    // `hasInputSession || isChineseMode` 块之外：软键盘不组码，英文模式下那块整个跳过。
    if (_IsSoftKeyboardEatenKey(wParam, modifiers))
    {
        *pfEaten = TRUE;
        _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers,
                        CHotkeyManager::ClassifyInputKey(wParam, modifiers),
                        isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"softkeyboard");
        return S_OK;
    }

    if (hasInputSession || isChineseMode)
    {
        // 会话中的 Ctrl/Alt 组合：吃下让 OnKeyDown 送服务端清理状态，再交给宿主（免得 Ctrl+S 等
        // 留下悬空组合）。修饰键自身要排除，否则 Ctrl+数字（钉住）等候选快捷键到达前组合已被清掉。
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
            // 中文模式无会话时：全角要吃 Space 让 core 出 U+3000；半角看自定义标点「空格」行
            // 当前列有无映射。判据见 SpaceKeyPolicy.h。
            if (isChineseMode && keyType == HotkeyType::Space &&
                wind::spacekey::ShouldEatIdleSpace(_pTextService->IsChinesePunct() != FALSE,
                                                   _pTextService->IsFullWidth() != FALSE, _spaceCustomCols))
            {
                *pfEaten = TRUE;
                _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, keyType,
                                isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE,
                                _pTextService->IsFullWidth() ? L"chinese_fullwidth_space" : L"chinese_custom_space");
                return S_OK;
            }
        }
        else if (keyType == HotkeyType::Number || keyType == HotkeyType::Tab ||
                 keyType == HotkeyType::PageKey || keyType == HotkeyType::SelectKey)
        {
            // Session-only keys：无会话时服务端只会回 PassThrough，WindTerm 等宿主处理不好
            // 「吃了再吐」会吞键，所以只在有会话时吃。
            if (hasInputSession)
            {
                *pfEaten = TRUE;
                _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, keyType,
                                isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"session_select_or_page");
                return S_OK;
            }
            // 中文+全角无会话时也要吃数字让服务端出全角。判据与 OnKeyDown 的 numberPassthrough
            // 同一个函数（NumberKeyPolicy.h）。
            if (keyType == HotkeyType::Number &&
                wind::numberkey::ShouldEatNumberKey(hasInputSession != FALSE, isChineseMode != FALSE,
                                                    _pTextService->IsFullWidth() != FALSE,
                                                    _SpaceBeforeDigitFor(wParam, modifiers)))
            {
                *pfEaten = TRUE;
                _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, keyType,
                                isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"chinese_fullwidth_number");
                return S_OK;
            }
        }
        else if (keyType == HotkeyType::Letter)
        {
            // 中文 + CapsLock ON + 非全角 + 无会话：字母同步透传，系统自然产生大小写，CAD 等也拿得到
            // WM_KEYDOWN。必须在这里就不吃，「吃了再吐」时 Chrome/Electron 等不补 WM_CHAR 会吞字母。
            // 有会话（先提交候选）或全角（全角转换）时仍要吃。
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
            // 中文 + CapsLock ON + 非全角 + 无会话：标点与字母对齐，直接透传（CAD 等靠 WM_KEYDOWN 触发功能）。
            // 例外：英文半角会吃的键（配对 / 英半自定义）照英文模式吃下转发，否则「中文 + 大写」下配对失效。
            if (!hasInputSession && !_pTextService->IsFullWidth() &&
                (GetKeyState(VK_CAPITAL) & 0x0001) &&
                !_IsEnglishHalfEatenPunctKey(wParam, modifiers))
            {
                _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, keyType,
                                isChineseMode, hasComposition, _hasCandidates, hasInputSession, FALSE,
                                L"chinese_capslock_punct_passthrough");
                return S_OK; // pfEaten 保持 FALSE → 同步透传
            }
            // 中文 + 该标点产物就是原样半角 ASCII + 无会话 + 非全角 → 透传：吃了再吐回去，
            // 非 TSF-aware 宿主会把字符码当虚拟键码（`#`→VK_END），见 smart-symbol-compat-notes.md
            // 「产物不变的标点要透传」。会话与全角两道闸门是动态的，不在推送集合里。
            if (!hasInputSession && !_pTextService->IsFullWidth() &&
                _IsPassthroughPunctKey(wParam, modifiers))
            {
                _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, keyType,
                                isChineseMode, hasComposition, _hasCandidates, hasInputSession, FALSE,
                                L"chinese_punct_passthrough");
                // 透传的是符号，输入序列由出口守卫清零（_NoteInputSequenceAtExit）。
                return S_OK; // pfEaten 保持 FALSE → 同步透传
            }
            // Punctuation: always eat in Chinese mode（服务端总会出字，不会吃了再吐）。
            *pfEaten = TRUE;
            _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, keyType,
                            isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"chinese_punctuation");
            return S_OK;
        }
    }
    // English mode + full-width: intercept printable characters for full-width conversion
    else if (!isChineseMode && _pTextService->IsFullWidth())
    {
        // Intercept printable ASCII keys so the service can convert them to full-width
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
    // 英文模式 + 半角：只吃「英半列有自定义标点映射」的标点键（集合由 core 经
    // CONFIG_KEY_CUSTOM_EN_PUNCT 推送），不吃 core 就收不到；集合为空时恒不吃。
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

    // 中文模式空缓冲下数字键（含小键盘，ClassifyInputKey 归 Number）不被吃，从这里透传；
    // 输入序列记账在出口守卫里做（_NoteInputSequenceAtExit）。
    return S_OK;
}

bool CKeyEventSink::_SpaceBeforeDigitFor(WPARAM vk, uint32_t modifiers) const
{
    return wind::numberkey::SpaceBeforeDigit(
        _spaceBeforeDigit, WindInputSeq::IsCjk(_seqChar), (unsigned)vk,
        (modifiers & (KEYMOD_SHIFT | KEYMOD_CTRL | KEYMOD_ALT | KEYMOD_WIN)) != 0,
        (GetKeyState(VK_CAPITAL) & 0x0001) != 0);
}

void CKeyEventSink::_TrackCommittedTextForSmartPunct(const std::wstring& text)
{
    if (text.empty())
        return;
    WCHAR last = text.back();
    _lastPassthroughDigit = (last >= L'0' && last <= L'9') ? last : 0;
    _seqChar = WindInputSeq::FromCommittedText(text.c_str(), text.size());
}

void CKeyEventSink::_NoteInputSequenceAtExit(WPARAM vk, bool eaten, bool suppressed)
{
    if (!WindPassthrough::IsUserKeyIntoHost((uint32_t)vk, eaten, suppressed))
        return;
    // 非数字键 _DigitCharFromVk 返回 0，即清零。修饰键用 GetKeyState（与这条按键消息同步），
    // 不用 GetCurrentModifiers：它掺了实时值，用户可能已按下下一个字符的 Shift（「3:」的 3 被判成 Shift+3）。
    uint32_t mods = 0;
    if (GetKeyState(VK_SHIFT) & 0x8000) mods |= KEYMOD_SHIFT;
    if (GetKeyState(VK_CONTROL) & 0x8000) mods |= KEYMOD_CTRL;
    if (GetKeyState(VK_MENU) & 0x8000) mods |= KEYMOD_ALT;
    _lastPassthroughDigit = _DigitCharFromVk(vk, mods);
    _seqChar = WindInputSeq::FromVk((uint32_t)vk, (mods & KEYMOD_SHIFT) != 0,
                                    (mods & (KEYMOD_CTRL | KEYMOD_ALT)) != 0);
    _pTextService->InvalidateCachedPrevChar();
}

void CKeyEventSink::_ReportObservedKeyAtExit(WPARAM vk, LPARAM lParam, bool fromTest, bool eaten,
                                             bool suppressed)
{
    // 未推开时只这一次原子读。密码框、键盘禁用时一个都不报：那是用户数据，与扩展授权无关。
    const bool enabled = _observeKeys.load(std::memory_order_relaxed) && !_pTextService->IsKeyboardDisabled()
                         && !_pTextService->IsPasswordScope();
    const bool repeat = ((lParam >> 30) & 1) != 0;
    const bool report =
        fromTest ? _observeGate.OnTestExit((uint32_t)vk, eaten, suppressed, repeat, enabled)
                 : _observeGate.OnDownExit((uint32_t)vk, eaten, suppressed, repeat, enabled);
    if (!report)
        return;
    // 修饰键取法同 _NoteInputSequenceAtExit：GetKeyState 与正在处理的这条按键消息同步。
    uint32_t mods = 0;
    if (GetKeyState(VK_SHIFT) & 0x8000) mods |= KEYMOD_SHIFT;
    if (GetKeyState(VK_CONTROL) & 0x8000) mods |= KEYMOD_CTRL;
    if (GetKeyState(VK_MENU) & 0x8000) mods |= KEYMOD_ALT;
    if ((GetKeyState(VK_LWIN) & 0x8000) || (GetKeyState(VK_RWIN) & 0x8000)) mods |= KEYMOD_WIN;
    if (CIPCClient* ipc = _pTextService->GetIPCClient())
        ipc->SendKeyObserved((uint32_t)vk, mods);
}

STDAPI CKeyEventSink::OnKeyDown(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten)
{
    *pfEaten = FALSE;

    // 屏蔽键一律放行（见 OnTestKeyDown）。
    if (wParam == WindPassthrough::kVkMenuMask)
        return S_OK;

    // 同 OnTestKeyDown 的守卫：Chrome / QQ 等宿主无视 test 的 pfEaten=FALSE 仍调本函数，skip 条目
    // 已被 test 消费，这里无从 Suppress()，自生成键改由 `IsSelfInjectedVk` 按 VK 值挡（见 PassthroughNote.h）。
    // 两处都记无妨，记的是幂等的布尔。
    PassthroughNoter _ptNote{this, wParam, pfEaten, lParam, false};

    // 合成提交触发键：这里才是真正的提交点（按键处理期间，CommitText 走 TF_ES_SYNC）。
    // 见 KeyEventSink.h 的 QueueAsyncCommitViaSyntheticKey。
    {
        PendingAsyncCommit pending;
        if (_TryConsumeAsyncCommitTrigger(wParam, pending))
        {
            *pfEaten = TRUE;
            WIND_LOG_DEBUG_FMT(L"AsyncCommitTrigger: committing via synchronous edit session, textLen=%zu\n",
                               pending.text.length());
            BOOL asyncDeferred = FALSE;
            _pTextService->CommitText(pending.text, /*nonKeyContext=*/FALSE, pending.replacingHeld,
                                      &asyncDeferred);

            // 提交真正发生在这一刻，清组合态、输入序列记账、配对收尾都必须跟在它后面而非入队时：
            // 入队到触发键抵达之间用户可能又敲了键，提前做会与文档实际状态错位。
            _TrackCommittedTextForSmartPunct(pending.text);
            _isComposing = FALSE;
            _hasCandidates = FALSE;

            // 按键上下文里宿主忙时也可能拒发 TF_ES_SYNC，组合还活着就退到异步会话，而左移是立刻注入的，
            // 存在残留竞态。这条 WARN 是它唯一的可观测信号，别删。
            if (asyncDeferred && pending.moveLeft > 0)
            {
                WIND_LOG_WARN_FMT(L"AsyncCommitTrigger: 宿主拒发同步锁, 提交退到异步会话; "
                                  L"随后的 %u 格左移可能抢在文本落定之前\n",
                                  pending.moveLeft);
            }
            _ApplyPairCommitTail(pending.pairCommit, pending.moveLeft);
            if (!_pendingAsyncCommits.empty())
            {
                // 队列里还有后续提交（连续快速点选）：再次自注入触发键，让它在自己的
                // OnKeyDown 周期里执行，保持"一次按键一次提交"的语义。
                if (!_SendAsyncCommitTriggerKey())
                {
                    WIND_LOG_WARN(L"AsyncCommitTrigger: chained trigger key injection failed, dropping remaining commits\n");
                    _DropPendingAsyncCommits();
                }
            }
            return S_OK;
        }
    }

    // 同 OnTestKeyDown：不调 OnTestKeyDown 只调本函数的宿主（QQ 那类）也要记。重复记无妨。
    _NoteKeyDownForCtrlRaw(wParam);

    // Ctrl+Shift+F12: Dump TSF ring buffer logs to clipboard (debug aid for AppContainer)
    // 默认关，判据与 OnTestKeyDown 共用——见 _IsLogDumpHotkey。
    if (_IsLogDumpHotkey(wParam))
    {
        *pfEaten = TRUE;
        CFileLogger& lg = CFileLogger::Instance();

        // 顺带重读配置文件，否则改完 mode/level 要退出宿主才生效。
        lg.ReloadConfig();

        // 环形缓冲为空时不碰剪贴板（免得清掉用户内容），但提示照给，好分清是没生效还是没日志。
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

    // 密码框强制英文抑制：与 OnTestKeyDown 成对，Chrome/QQ 等宿主无视 pfEaten=FALSE 仍调本函数。
    if (_pTextService->IsPasswordSuppressActive())
        return S_OK;

    // 英文自动配对不在本地处理：OnTestKeyDown 已按 `PairEngine::ShouldEat` 吃下配对键，这里落到
    // `_SendKeyToService` 由 core 出字记栈（配对栈只在 core 一处）。IPC 断连兜底见 `ipc_failed_*` 分支。

    // For function hotkeys (like Ctrl+`), use normalized modifiers (no left/right distinction)
    uint32_t modifiers = CHotkeyManager::GetCurrentModifiers();
    uint32_t normalizedMods = CHotkeyManager::NormalizeModifiers(modifiers);
    uint32_t normalizedKeyHash = CHotkeyManager::CalcKeyHash(normalizedMods, (uint32_t)wParam);

    CHotkeyManager* pHotkeyMgr = _pTextService->GetHotkeyManager();

    // 候选显示时 Ctrl 如实上报（OnTestKeyDown 已起头的这里是 Repeat，什么都不发）。
    if (_TryCtrlRawDown(wParam, lParam, modifiers))
        return S_OK;

    // Check if this is a KeyUp triggered key (toggle mode keys like Shift, Ctrl, CapsLock)
    // Use hash-based lookup first, then fallback to VK-based detection
    //
    // TSF sends generic VK_SHIFT/VK_CONTROL as wParam, but the hotkey whitelist
    // registers specific VK_LSHIFT/VK_RSHIFT/VK_LCONTROL/VK_RCONTROL.
    // Resolve the generic VK to specific left/right variant for proper hash matching.
    BOOL isToggleModeKey = FALSE;
    uint32_t resolvedVK = (uint32_t)wParam;
    // 优先用 modifiers，降级 GetAsyncKeyState（同 _MarkPendingToggleKey）。
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

    // CapsLock 的会话态绑定不在 TSF 处理，见 OnTestKeyDown 里的同位置注释。
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
        // modifiers 为主、GetAsyncKeyState 补充：WebView2 等宿主下只靠后者会误判「无其它修饰」。
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

    // Ctrl+Space 兜底切换：只在 OnTestKeyDown 确实吃下过这个键时执行。系统热键正常时 msctf
    // 自己消费该键、不会调到这里；系统热键失效的机器上 compartment 永远不翻，只能由这里切换。
    // 不会双切换：OnKeyDown 被调用就说明 msctf 没拿它当热键，_ctrlSpaceEatenInTest 是独占凭据。
    // 这里不再看 _ctrlSpaceToggleEnabled：test 阶段已判过，再判只会「吃了不办」。
    // 日志指纹见 tsf-openclose-compartment-semantics.md。
    if (_ctrlSpaceEatenInTest
        && wind::ctrlspace::IsCtrlSpaceChord((uint32_t)wParam, modifiers))
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

    // Policy 早期闸门：Chrome / QQ 等宿主无视 test 的 pfEaten=FALSE 仍调 OnKeyDown。不满足 policy
    // 的 chineseOnly / session 热键在这里放行，否则下方会把键发给服务端，英文模式也触发 AddWord 等动作。
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
    // 与 OnTestKeyDown 同一判据（_HasInputSession），否则那边吃下的键这边落成 isInputKey=FALSE，
    // 「吃了再吐」时严格 TSF 宿主直接丢键。
    BOOL hasInputSession = _HasInputSession();

    // 与 OnTestKeyDown 对称：中文 + CapsLock ON + 非全角 + 无会话的字母同步透传，
    // 免得被下方 state_change_letter_consume 兜底误吃。
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
        (GetKeyState(VK_CAPITAL) & 0x0001) &&
        !_IsEnglishHalfEatenPunctKey(wParam, modifiers); // 与 OnTestKeyDown 同一例外

    // 与 OnTestKeyDown 的 `chinese_punct_passthrough` 对称（必须，不是保险）：那边已不吃，这边再算
    // 输入键就是 test(FALSE)+down(TRUE) 翻转，Chrome 类宿主会出两次字。
    BOOL punctPassthrough =
        isChineseMode && !hasInputSession && !_pTextService->IsFullWidth() &&
        _IsPassthroughPunctKey(wParam, modifiers);

    // 与 OnTestKeyDown 的 Number 分支对称（同一个 ShouldEatNumberKey）：Test 放行的数字类键这里也不转发，
    // 否则服务端某条路径一出字，test 不吃却照调 OnKeyDown 的 Chrome 类宿主就会出两次字。
    BOOL numberPassthrough =
        isChineseMode && CHotkeyManager::ClassifyInputKey(wParam, modifiers) == HotkeyType::Number &&
        !wind::numberkey::ShouldEatNumberKey(hasInputSession != FALSE, true,
                                             _pTextService->IsFullWidth() != FALSE,
                                             _SpaceBeforeDigitFor(wParam, modifiers));

    // Track whether this is a Ctrl/Alt combo that needs cleanup-then-passthrough
    BOOL isCtrlAltCleanup = FALSE;

    // 配对跳出键判据，必须与 OnTestKeyDown 的 pair_jumpout_forward 逐条一致，且放在
    // `hasInputSession || isChineseMode` 门之外：英文模式进不了那个门，那边吃了这边不发，键就凭空消失。
    BOOL isPairJumpOut = (_pairPendingDepth > 0 && !IsPairStateStale()
                          && _IsJumpOutKey((UINT)wParam)
                          && !(modifiers & (KEYMOD_CTRL | KEYMOD_ALT | KEYMOD_SHIFT)));
    if (isPairJumpOut)
        isInputKey = TRUE;

    // 软键盘总闸的转发侧，与 OnTestKeyDown 调同一个判据函数；同样放在门外（英文模式进不去）。
    BOOL isSoftKeyboardKey = _IsSoftKeyboardEatenKey(wParam, modifiers);
    if (isSoftKeyboardKey)
    {
        isInputKey = TRUE;
    }

    if (hasInputSession || isChineseMode)
    {
        // Ctrl/Alt combos during active input session: mark as input key so we can
        // send to the service; only an explicit pass-through response returns it to the host.
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

            // 配对跳出键与软键盘键必须留在本链首位，否则会被下面 `isInputKey = hasInputSession` 覆盖回 FALSE。
            if (isPairJumpOut || isSoftKeyboardKey)
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
                // 透传场景一律不视为输入键（保持 pfEaten=FALSE 同步透传，不发 core）：
                // CapsLock 下的字母/标点，以及中文模式下产物即原样 ASCII 的标点。
                isInputKey = (capsLockLetterPassthrough || capsLockPunctPassthrough || punctPassthrough ||
                              numberPassthrough)
                                 ? FALSE
                                 : (keyType != HotkeyType::None);
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
    // 英文模式 + 半角 + 该键配了英半列：与 OnTestKeyDown 的 english_custom_punct 成对，必须同条件转发。
    else if (!isChineseMode && _IsCustomEnglishPunctKey(wParam, modifiers))
    {
        isInputKey = TRUE;
    }
    // 英文模式 + 半角 + 该键是配对字符：与 OnTestKeyDown 的 english_autopair 成对，是它唯一的转发出口。
    else if (!isChineseMode && !_pTextService->IsFullWidth()
             && !(modifiers & (KEYMOD_CTRL | KEYMOD_ALT))
             && _englishPairEngine.ShouldEat(
                    _MapVkToEnglishPairChar(wParam, (modifiers & KEYMOD_SHIFT) != 0)))
    {
        isInputKey = TRUE;
    }

    if (!isKeyDownHotkey && !isInputKey)
    {
        // OnTestKeyDown 按当时状态吃下的字母，到这里状态可能已变（快打 "d<space>d" 时组合已提交），
        // 中文模式下仍须吃掉以保持一致。CapsLock 透传字母例外：那边主动不吃，这里吃了就彻底丢失。
        if (isChineseMode && wParam >= 'A' && wParam <= 'Z' && !(modifiers & (KEYMOD_CTRL | KEYMOD_ALT))
            && !capsLockLetterPassthrough)
        {
            // Letter key in Chinese mode slipped through due to state change - consume it
            *pfEaten = TRUE;
            _LogKeyDecision(L"down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::Letter,
                            isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"state_change_letter_consume");
        }
        else if (numberPassthrough)
        {
            // 宿主自己出的数字：输入序列由出口守卫记下（_NoteInputSequenceAtExit）。
            _LogKeyDecision(L"down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::Number,
                            isChineseMode, hasComposition, _hasCandidates, hasInputSession, FALSE, L"number_passthrough");
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

    // 一段输入的第一个键：宿主界面此刻必定已显示，按前台 band 复核 host 候选窗落位。
    // SearchHost 二次聚焦会跳过 focus_gained，这里是唯一可靠的时机。
    if (!hasComposition)
    {
        _pTextService->RecheckHostBand(L"key");
    }

    // Update caret position before sending key event
    // This ensures the candidate window appears at the correct position
    _pTextService->SendCaretPositionUpdate();

    // Send key to the service using binary protocol (SYNC mode)
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
            // 英文配对键的断连兜底：已吃下的键吐回去会在 EverEdit 这类宿主丢字，故本地插入单个字符，
            // 不配对不记栈。「中文 + CapsLock」同样按英文语义吃了配对键，一并覆盖。
            bool hasShift = (modifiers & KEYMOD_SHIFT) != 0;
            wchar_t pairChar = _MapVkToEnglishPairChar(wParam, hasShift);
            if ((!isChineseMode || (GetKeyState(VK_CAPITAL) & 0x0001)) && !_pTextService->IsFullWidth()
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

    // 智能符号 hold 预览态须在响应处理前采样：PassThrough 分支会把组合收口掉。
    BOOL holdActiveBeforeResponse = _pTextService->IsHoldCompositionActive();

    // SYNC: Wait for response and handle it directly
    // This is simpler and matches Weasel's architecture
    // 先清零重放标志：只有本次响应置的位才算数（见其声明处说明）。
    _pendingReplayToHost = FALSE;
    // 记下本次应答对应的按键：CommitThenDefer 据此只让这个键的 keyup 开延迟组合。
    _responseKeyVk = (UINT)wParam;
    ResponseType keyResponse = ResponseType::Error;
    *pfEaten = _HandleServiceResponse(&keyResponse);
    _responseKeyVk = 0;
    if (*pfEaten && wParam >= VK_F1 && wParam <= VK_F12 && !(modifiers & (KEYMOD_CTRL | KEYMOD_ALT)))
    {
        _eatenFnKeyVk = (UINT)wParam;
        _eatenFnKeyUpSent = FALSE;
    }

    // ── 联想态透传：组合已收口，把这一键还给宿主 ──
    // 置位来自 ResponseType::ClearCompositionThenPassThrough（那里解释为何必须重放）。排在 hold 重放
    // 之前：这是服务端显式声明的意图，hold 那条是本地推断，两者实际互斥。
    // 已知代价：联想态 + 待跳出配对时按 Tab/Enter 从这里 return，depth 不归零，下一次同键多一轮 IPC 才自愈；
    // 不丢键且有 TTL 兜底，故不特殊处理。
    // Ctrl/Alt 组合不重放、按原生透传还给宿主：重放只补主键，Ctrl 先松开时宿主会收到普通字母。
    if (_pendingReplayToHost && isCtrlAltCleanup)
    {
        _pendingReplayToHost = FALSE;
    }
    else if (_pendingReplayToHost)
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

    // ── hold 预览态 + 无法代劳的键：吃键 → 收口 → 重放 ──
    // 服务端回了 PassThrough，_HandleServiceResponse 已在这个合法编辑上下文里把符号收口。不能吐回
    // FALSE（吃了再吐会丢键，且组合态下回车是「确认输入」而非换行），也不能更早收口（宿主不保证先消化写入）。
    // 所以吃掉原键再 SendInput 重放；Ctrl/Alt 组合同理（重放时物理修饰键仍按着）。判据见 HoldReplayPolicy.h，
    // 来龙去脉见 smart-symbol-compat-notes.md「HoldComposition 方案」。
    if (wind::holdreplay::ShouldReplayAfterHold(holdActiveBeforeResponse != FALSE, *pfEaten != FALSE,
                                                _IsHoldReplayKey(wParam) != FALSE,
                                                isCtrlAltCleanup != FALSE))
    {
        _ReplayKeyToHost((WORD)wParam);
        *pfEaten = TRUE;
        _LogKeyDecision(L"down", _pTextService->GetFocusSessionId(), wParam, modifiers,
                        CHotkeyManager::ClassifyInputKey(wParam, modifiers),
                        isChineseMode, hasComposition, _hasCandidates, hasInputSession,
                        TRUE, L"hold_commit_then_replay");
        return S_OK;
    }

    // ── 配对跳出转发的 desync 兜底 ──
    // 按 isPairJumpOut 吃了键，core 却回 PassThrough：它的配对栈已空而本地 depth 还挂着。以 core 为准
    // 归零 depth，并重放键（直接吐回 FALSE 会在不补发的宿主丢掉 Tab/Enter）。
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

    // 数字键"吃了又吐"（有候选时被吃、coordinator 回 PassThrough）：OnKeyDown 返 FALSE，
    // 输入序列由出口守卫记下（_NoteInputSequenceAtExit），无须在此补设。

    // 服务端显式声明按键归属。加词编辑等动作也会关闭组合，不能据此改判为透传。
    // PassThrough / ClearCompositionThenPassThrough 在这里原生交还；其余消费响应均保留吃键决定。
    if (isCtrlAltCleanup && wind::ctrlalt::ShouldReturnToHost(keyResponse))
        *pfEaten = FALSE;

    _LogKeyDecision(L"down", _pTextService->GetFocusSessionId(), wParam, modifiers,
                    isKeyDownHotkey ? HotkeyType::Hotkey : CHotkeyManager::ClassifyInputKey(wParam, modifiers),
                    isChineseMode, hasComposition, _hasCandidates, hasInputSession, *pfEaten,
                    isCtrlAltCleanup && !*pfEaten ? L"ctrl_alt_cleanup_passthrough" : L"service_response");

    return S_OK;
}

STDAPI CKeyEventSink::OnTestKeyUp(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten)
{
    *pfEaten = FALSE;
    _observeGate.OnKeyUp((uint32_t)wParam); // 扩展旁路上报的去重记忆，见 ObservedKeyGate.h

    // 屏蔽键一律放行（见 OnTestKeyDown）。
    if (wParam == WindPassthrough::kVkMenuMask)
        return S_OK;

    // 合成提交触发键的 keyup 半程：干净吃掉，不进下面任何 keyup 逻辑。
    if (wParam == VK_ASYNC_COMMIT_TRIGGER)
    {
        *pfEaten = TRUE;
        return S_OK;
    }

    // 自生成键放行的 keyup 半程。传 TRUE = 只认「给 keyup 的」条目，否则连按同一个键时会误消费
    // 给下一个 keydown 的条目。见 KeyEventSink.h 里 skip 表的说明。
    if (_TryConsumeSkipKey(wParam, TRUE))
    {
        *pfEaten = FALSE;
        return S_OK;
    }

    // Ctrl 如实上报的松开：先于禁用 / 配对拦截等一切闸门（按住翻转靠它收起）。不吃、不返回。
    _DispatchCtrlRawUp(wParam);

    if (_DispatchEatenFnKeyUp(wParam, FALSE))
    {
        *pfEaten = TRUE;
        return S_OK;
    }

    // Keyboard disabled by system: pass through all keys
    if (_pTextService->IsKeyboardDisabled())
        return S_OK;

    // direct_commit 顶码：余码新组合在触发键的 keyup 才开，先到者开、另一处自然 no-op。不能是
    // 「下一个 keyup」：快打时前一个编码键的 keyup 紧跟着到，组合会在提交后立刻开（见
    // DeferredCompositionPolicy.h）。别的键的 keyup 交给下一次 keydown / 兜底定时器。
    if (_pTextService->HasDeferredComposition()
        && !wind::deferredcomp::ShouldOpenOnKeyUp(_pTextService->DeferredTriggerVk(), (uint32_t)wParam))
    {
        WIND_LOG_DEBUG_FMT(L"DeferredComposition: keyup vk=0x%02X 不是触发键 0x%02X，暂不开组合\n",
                           (UINT)wParam, _pTextService->DeferredTriggerVk());
    }
    else if (_pTextService->HasDeferredComposition())
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
        // 纯修饰键必须与 keydown 一致放行，否则宿主会停在「Shift 按下且从未松开」。
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
    _observeGate.OnKeyUp((uint32_t)wParam); // 同 OnTestKeyUp：宿主可能只调其一

    // 屏蔽键一律放行（见 OnTestKeyDown）。
    if (wParam == WindPassthrough::kVkMenuMask)
        return S_OK;

    // 合成提交键（VK_ASYNC_COMMIT_TRIGGER）的 keyup 半程：吃掉返回，不进下面的逻辑。
    // 与下面「顶码触发键」不是同一个概念。
    if (wParam == VK_ASYNC_COMMIT_TRIGGER)
    {
        *pfEaten = TRUE;
        return S_OK;
    }

    // 同 OnTestKeyUp（那边先到就已清掉记账，这里不再发）。
    _DispatchCtrlRawUp(wParam);

    if (_DispatchEatenFnKeyUp(wParam, TRUE))
    {
        *pfEaten = TRUE;
        return S_OK;
    }

    // direct_commit 顶码：同 OnTestKeyUp，只在触发键的 keyup 开余码组合。
    if (_pTextService->HasDeferredComposition()
        && !wind::deferredcomp::ShouldOpenOnKeyUp(_pTextService->DeferredTriggerVk(), (uint32_t)wParam))
    {
        WIND_LOG_DEBUG_FMT(L"DeferredComposition: keyup vk=0x%02X 不是触发键 0x%02X，暂不开组合\n",
                           (UINT)wParam, _pTextService->DeferredTriggerVk());
    }
    else if (_pTextService->HasDeferredComposition())
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
            _PushSkipKey(_pendingPairAction.vk, FALSE);
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
        // 必须排掉只有会话语义的登记（`keys.session_actions` 也落在 _keyUpHotkeys），否则会丢掉
        // 0x8000 状态通知标记、被服务端当成模式切换请求。
        BOOL isConfiguredAsToggle = (pHotkeyMgr != nullptr && pHotkeyMgr->IsKeyUpHotkey(keyHash)
                                     && !pHotkeyMgr->IsKeyUpSessionOnlyHotkey(keyHash));

        // Get current Caps Lock state
        BOOL capsLockOn = (GetKeyState(VK_CAPITAL) & 0x0001) != 0;

        // Always send CapsLock event to the service for:
        // 1. Mode toggle (if configured)
        // 2. CapsLock indicator display (A/a prompt)
        // 3. Toolbar state update
        // Use a special modifier to indicate whether this is for mode toggle
        uint32_t mods = KEYMOD_CAPSLOCK;
        if (!isConfiguredAsToggle)
        {
            // Add a marker to indicate this is just for CapsLock state notification, not mode toggle
            // The service checks this to decide whether to toggle mode
            mods |= 0x8000; // High bit as "state notification only" marker
        }

        // Update caret position before sending CapsLock event
        _pTextService->SendCaretPositionUpdate();

        // SYNC: Send key event and wait for response
        // The service pushes state update followed by CMD_CONSUMED response
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

    // 中文模式下统计由服务端负责；但 CapsLock 透传键（无会话、非全角）不经服务端，需在此统计为英文输入。
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
    // it will be sent to the service and recorded there. We should not count it here.
    // CapsLock 透传键不经服务端，跳过下方去重检查；但其中英文半角会吃的键已被吃下转发，由 core 记账。
    if (capsLockPassthrough && _IsEnglishHalfEatenPunctKey(wParam, modifiers))
        return S_OK;
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

// Dispatch the pending toggle key (Shift/Ctrl) to the service.
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

    // 按住期间有鼠标操作（Shift+点击 / 拖选）⇒ 不是单击，不切换。
    // 放在时长判定之后：长按本来就不切，日志也不必重复报。
    wind::toggletap::TapSignals signals = _pendingTapSignals;
    const MouseAsyncState mouse = ReadMouseAsyncState();
    signals.mouseDownAtRelease = mouse.down;
    signals.mousePressedSincePress = signals.mousePressedSincePress || mouse.pressedSinceLastCall;
    _pendingTapSignals = {};
    if (wind::toggletap::IsTapCancelledByMouse(signals))
    {
        WIND_LOG_DEBUG_FMT(L"Toggle key tap cancelled by mouse: vk=0x%02X press=%d sel=%d release=%d since=%d\n",
            pendingKey, signals.mouseDownAtPress, signals.selectionChanged,
            signals.mouseDownAtRelease, signals.mousePressedSincePress);
        return TRUE;
    }
    WIND_LOG_DEBUG_FMT(L"Toggle key tap accepted: vk=0x%02X held=%lums\n", pendingKey, pressDuration);

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

// 经 WM_HOTKEY 到达的热键送服务端（与 OnKeyDown 同一通路）。
BOOL CKeyEventSink::DispatchHotkey(uint32_t vk, uint32_t mods)
{
    // 用于候选热键（Pin/Delete）、AddWord（规避 Chromium 宿主双处理，caret 由 WndProc 先行补发）、
    // 快捷输入选词键 / Alt+数字上屏注释。
    // 这里不在按键处理期间（裸 WM_HOTKEY 回调），上屏须按 nonKeyContext 走异步会话：Word 会拒发
    // 此时的 TF_ES_SYNC（见 CTextService::CommitText 的 nonKeyContext 分支）。
    // 返回「这个键要不要交还宿主」（没送达或响应是交还类）；交还由调用方注入，见 CandidateHotkeyPolicy.h。
    // 待判单击的作废在 WM_HOTKEY 入口（NoteHotkeyMessage）：前台竞态丢弃的那次也要作废。
    if (!_SendKeyToService(vk, mods, KEY_EVENT_DOWN))
    {
        WIND_LOG_ERROR_FMT(L"DispatchHotkey: _SendKeyToService failed vk=0x%02X mods=0x%04X\n", vk, mods);
        return TRUE;
    }
    // 保存/恢复而非置 TRUE/FALSE：响应处理里若重入了别的热键派发，内层不能把外层复位。
    const BOOL prevInHotkeyDispatch = _inHotkeyDispatch;
    _inHotkeyDispatch = TRUE;
    // 接收失败时 responseType 保持 Error ⇒ 不交还：键已送达，服务端可能已经处理过它，
    // 再注入一次就是重复执行。
    ResponseType responseType = ResponseType::Error;
    _HandleServiceResponse(&responseType);
    // ClearCompositionThenPassThrough 分支置的重放位是给 OnKeyDown 的，这里按响应类型判，
    // 位清掉免得残留到下一次按键。
    _pendingReplayToHost = FALSE;
    _inHotkeyDispatch = prevInHotkeyDispatch;
    return wind::candhotkey::GivesBackToHost(responseType) ? TRUE : FALSE;
}

// 这一键不经按键回调，够不着那里「别的键取消待判单击」：在这里作废，否则 Ctrl+数字
// 500ms 内松开 Ctrl 会被当成 Ctrl 单击（lctrl 作中英切换键时顺带切英文）。
void CKeyEventSink::NoteHotkeyMessage()
{
    if (_pendingKeyUpKey != 0)
        WIND_LOG_DEBUG_FMT(L"Cancel pending toggle (wm_hotkey): pending=0x%02X\n", _pendingKeyUpKey);
    wind::ctrlraw::OnHotkeyDispatched(_ctrlRaw, _pendingKeyUpKey);
    _pendingKeyUpModifiers = 0;
}

void CKeyEventSink::ResetCtrlRaw()
{
    wind::ctrlraw::OnFocusLost(_ctrlRaw);
    _ctrlRawTapSignals = {};
}

// 失焦时吃掉过按下的功能键的松开多半送不到：补报一次松开（按住档的注释切换键靠它收起详细态），
// 再丢掉记账——不丢的话，之后宿主正常处理的同一个键，松开会被误吃。
void CKeyEventSink::ResetEatenFnKey()
{
    if (_eatenFnKeyVk != 0 && !_eatenFnKeyUpSent)
    {
        _eatenFnKeyUpSent = TRUE;
        if (_SendKeyToService((uint32_t)_eatenFnKeyVk, 0, KEY_EVENT_UP))
            _HandleServiceResponse();
    }
    _eatenFnKeyVk = 0;
    _eatenFnKeyUpSent = FALSE;
}

BOOL CKeyEventSink::_SendKeyToService(uint32_t keyCode, uint32_t modifiers, uint8_t eventType, uint8_t extraToggles)
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

        if (_pTextService->HasCompositionOrRecover())
        {
            // 组合进行中不发 CMD_IME_ACTIVATED：服务端处理它会清空输入缓冲、毁掉进行中的组合。
            // 组合结束后由 WM_SERVICE_READY 补同步。
            WIND_LOG_INFO(L"NeedsStateSync: composition active, clearing flag without sync\n");
            pIPCClient->ClearNeedsSyncFlag();
        }
        else
        {
            if (_pTextService->_DoFullStateSync())
                _pTextService->CancelServiceReadyRetry();

            // 重连同步后补发光标位置，让服务端处理重启后的首键前就有有效锚点。
            _pTextService->SendCaretPositionUpdate();
        }
    }

    _pTextService->TryRecoverFocusState();

    // Get scan code from virtual key (optional, set to 0 if not needed)
    uint32_t scanCode = MapVirtualKeyW(keyCode, MAPVK_VK_TO_VSC);

    // Get toggles and event sequence
    uint8_t toggles = _GetTogglesSnapshot() | extraToggles;
    uint16_t eventSeq = _GetNextEventSeq();

    // IMPORTANT: Always use the passed-in modifiers from CHotkeyManager::GetCurrentModifiers()
    // which calls GetAsyncKeyState(). The _modsState state machine can get out of sync
    // when we pass keys through to the system (e.g., Ctrl+S for save).
    // Using stale _modsState causes all subsequent keys to appear as having Ctrl held.

    // prevChar 取法见 SmartPunctPrevChar.h：数字后智能标点只认输入序列（_lastPassthroughDigit）。
    // 现读值仍要 Consume，防 OnEndEdit 晚到的宿主（微信）拿到上一键的旧值。
    // 任何标点键都如实带上序列值，别在这里挑「哪些标点算数」：那是服务端 `smart_list` 的策略，
    // 抄一份到 DLL 必然漂移（见 smart-symbol-compat-notes.md 的 4b）。DLL 只报事实。
    // 用 ClassifyInputKey 而非 IsPunctuationKey：前者还覆盖 Shift+主键盘数字（`!` `@`…）。
    // 只有 keydown 消费/清零序列：keyup（单击 Shift、CapsLock 抬起）清零会吃掉「3 → Shift → .」里的数字。
    const bool isKeyDown = (eventType == KEY_EVENT_DOWN);
    const BOOL isPunctKey =
        (CHotkeyManager::ClassifyInputKey(keyCode, modifiers) == HotkeyType::Punctuation);
    const uint16_t prevChar = WindSmartPunct::ResolvePrevChar(
        (uint16_t)_pTextService->ConsumeCachedPrevChar(), (uint16_t)_lastPassthroughDigit,
        isKeyDown && isPunctKey != FALSE);
    if (isKeyDown && _lastPassthroughDigit != 0)
    {
        if (isPunctKey)
        {
            // 真机验证锚点；刻意不打字符内容（那是用户输入）。
            WIND_LOG_DEBUG_FMT(L"smart_punct_after_digit: prevChar from input sequence, vk=0x%02X\n",
                               keyCode);
        }
        // 标点键已消费；非标点键（组字、选候选等）也要清，否则「58的。」里的 `。` 会继承到 `8`。
        _lastPassthroughDigit = 0;
    }

    // 透传上报位：只在 keydown 这一发带出并清零（见 PassthroughNote.h 的 TakeOnKeyDownSend）。
    if (eventType == KEY_EVENT_DOWN && _passthrough.TakeOnKeyDownSend())
    {
        toggles |= TOGGLE_PASSTHROUGH_KEY;
        WIND_LOG_DEBUG(L"passthrough_before_key: 上一次按键送达后有键进了宿主，随本事件上报\n");
    }

    BOOL result = pIPCClient->SendKeyEvent(keyCode, scanCode, modifiers, eventType, toggles, eventSeq, prevChar,
                                           _seqChar);
    if (result && isKeyDown)
        _observeGate.OnSentToCore(keyCode); // 核心会自己给扩展报这一键，旁路不再报
    // 发送失败必须把位放回：漏报是唯一不可接受的方向（服务端会把下一按判成 press2，删掉用户刚打的字）。
    if (!result && (toggles & TOGGLE_PASSTHROUGH_KEY) != 0)
    {
        _passthrough.RestoreOnSendFailure();
    }

    WIND_LOG_DEBUG_FMT(L"_SendKeyToService: vk=0x%02X, mods=0x%04X, elapsed=%dms\n",
                 keyCode, modifiers, GetTickCount() - startTime);

    return result;
}

BOOL CKeyEventSink::_HandleServiceResponse(ResponseType* outType)
{
    LARGE_INTEGER startTime, midTime, freq;
    QueryPerformanceCounter(&startTime);
    QueryPerformanceFrequency(&freq);

    CIPCClient* pIPCClient = _pTextService->GetIPCClient();
    if (pIPCClient == nullptr)
        return TRUE; // Default to eating the key if no IPC

    ServiceResponse response;

    // Bridge pipe 上的响应只读一次：state push 走独立 push pipe，不会夹在前面。StatusUpdate 是
    // lshift / SystemModeSwitch 等同步操作的正式响应，必须交给下方 switch，别吃掉再读下一条（会超时断连）。
    if (!pIPCClient->ReceiveResponse(response))
    {
        // 丢响应后两侧状态失同步：本地复位组合并置 resync，让下一次按键走「有会话」路径送服务端，
        // 由其权威响应重建状态。
        WIND_LOG_ERROR(L"Failed to receive response from service, performing local composition reset");
        if (_pTextService->HasActiveComposition())
        {
            _pTextService->EndComposition(nullptr, _inHotkeyDispatch);
        }
        _isComposing = FALSE;
        _hasCandidates = FALSE;
        _pTextService->NotifyCandidatesVisibilityChanged(FALSE);

        // 连续失败到上限就放弃自愈、走 passthrough，免得服务长时间挂掉时 ENTER/ESC 等被永久吃。
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
    if (outType != nullptr) *outType = response.type;
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
        // 服务端在无占位联想期间不会回 PassThrough（已被改判成收口），收到它就说明服务端已不在联想态。
        _EndAssocBareSession();
        _pTextService->FlushHoldCompositionIfActive(_inHotkeyDispatch);
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

                _EndAssocBareSession();
                _pTextService->InsertTextAndStartComposition(response.text, response.newComposition);
                // seqChar 照记（数字记账不记，理由见下方分支）：新组合不论最终上屏还是取消，
                // 已写进文档的末字都是 response.text 的末位。联想态就走这条。
                NoteCommittedSeqChar(response.text);
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
                // pair_via_comp 开着的宿主（AutoCAD 表格）丢弃 TSF 直写的 ASCII：空闲态纯 ASCII 改以按键注入。
                bool asciiInject = _pairViaComposition.load(std::memory_order_relaxed) && !_isComposing &&
                                   !response.replacingHeld && !response.text.empty();
                for (wchar_t c : response.text)
                    if (c < 0x20 || c > 0x7E) { asciiInject = false; break; }
                if (!(asciiInject && _pTextService->InjectUnicodeText(response.text)))
                    _pTextService->CommitText(response.text, _inHotkeyDispatch,
                                              response.replacingHeld ? TRUE : FALSE);
                QueryPerformanceCounter(&ctMid1);

                // 上屏文本末位即新的「光标前一字符」，据此维护输入序列记账（见 header 注释）。
                // 只在这条「提交后不留组合」的分支记：restartComposition 分支提交后立刻又起了
                // 组合，光标前是组合内容而非 response.text 末位，记了就是错的。
                _TrackCommittedTextForSmartPunct(response.text);

                _isComposing = FALSE;
                _hasCandidates = FALSE;
                _EndAssocBareSession();
                if (response.assocSession)
                {
                    // 无占位的联想会话：宿主里没有组合，转发资格只能在这里**同步**记下
                    // （服务端的 STATUS_HOTKEY_SESSION 推送是异步的，赢不了下一键）。
                    // 候选窗仍开着，故不走下面的收起。
                    _assocBareSession = TRUE;
                    _hotkeyModeSession = TRUE;
                    // 丢弃此前残留的「鼠标按下过」位（选单元格的那一下点击），之后只认会话期间的点击。
                    (void)ReadMouseAsyncState();
                    _pTextService->NotifyCandidatesVisibilityChanged(TRUE);
                }
                else
                {
                    _pTextService->NotifyCandidatesVisibilityChanged(FALSE);
                }

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
            _EndAssocBareSession(); // 组合已存在，会话判据改由 HasActiveComposition() 承担
            // 若 HoldComposition 计时器活跃（中文符号待提交），把符号定格并入 prefix，
            // 与新组合内容在同一次 UpdateComposition 内显示（符号无下划线、新内容有，
            // 复用顶码聚合分段）。曾用 Flush（CommitText+立即开新组合）——WPS/微信下
            // 该模式被误读成替换，符号被新输入顶掉（与顶码双写同根）。
            _pTextService->AbsorbHeldIntoPrefix();
            _isComposing = TRUE;
            _hasCandidates = TRUE;
            // 先开/更新组合，再注册候选 UI 元素，顺序不能反：IMM32 桥只在组合已存在时才把
            // BeginUIElement 映射成 IMN_OPENCANDIDATE，反了靠它打开候选盒的宿主什么都不显示。
            _pTextService->UpdateComposition(response.composition, response.caretPos);
            _pTextService->NotifyCandidatesVisibilityChanged(TRUE);

            // Re-send caret position after composition update so the service can
            // reposition the candidate window with the up-to-date coordinates.
            _pTextService->SendCaretPositionUpdate();

            QueryPerformanceCounter(&ucEnd);
            int ucMs = (int)((ucEnd.QuadPart - ucStart.QuadPart) * 1000 / freq.QuadPart);
            WIND_LOG_DEBUG_FMT(L"UpdateComposition total took %dms\n", ucMs);
        }
        return TRUE;

    case ResponseType::ClearComposition:
        WIND_LOG_DEBUG(L"Received ClearComposition from service\n");
        _EndAssocBareSession();
        _isComposing = FALSE;
        _hasCandidates = FALSE;
        _pTextService->NotifyCandidatesVisibilityChanged(FALSE);
        _pTextService->EndComposition(nullptr, _inHotkeyDispatch);
        return TRUE;

    case ResponseType::ClearCompositionThenPassThrough:
        // 与上一分支同样收组合，只是这一键要还给宿主（与键码无关）。仍返回 TRUE：吐 FALSE 是
        // 「吃了再吐」，EverEdit 等会丢键。交还由按键入口在收口后 SendInput 重放：OnKeyDown 读
        // 下面这一位；DispatchHotkey 按响应类型判，由 WndProc 注入。
        WIND_LOG_DEBUG(L"Received ClearCompositionThenPassThrough from service\n");
        _EndAssocBareSession();
        _isComposing = FALSE;
        _hasCandidates = FALSE;
        _pTextService->NotifyCandidatesVisibilityChanged(FALSE);
        _pTextService->EndComposition(nullptr, _inHotkeyDispatch);
        _pendingReplayToHost = TRUE;
        return TRUE;

    case ResponseType::StatusUpdate:
        // StatusUpdate 是 lshift/SystemModeSwitch/FocusGained/IMEActivated 等同步操作
        // 的标准响应类型 (自包含 mode + iconLabel + hotkeys), 走 UpdateFullStatus 一并
        // 同步 _bChineseMode mirror + TSF compartments + LangBar UI。
        WIND_LOG_DEBUG(L"Received StatusUpdate as final response\n");
        // 热键模式会话位：`UpdateFullStatus` 收不到 statusFlags，在这里单独镜像。它是高频的纠正路径
        // （切中英 / 焦点切换 / 激活），漏一次靠下一次纠正。
        SetCandidateSessionActive(response.IsHotkeySession());
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
        // Key was consumed by a hotkey，或候选导航（翻页 / 移高亮）：组合串没变但候选变了，
        // 宿主自绘（UI-less）时必须通知它重读，否则停在旧页；不自绘时只是一次无害的 UpdateUIElement。
        WIND_LOG_DEBUG(L"Key consumed by hotkey\n");
        if (_hasCandidates || _assocBareSession)
        {
            _pTextService->NotifyCandidatesVisibilityChanged(TRUE);
        }
        return TRUE;

    case ResponseType::InsertTextWithCursor:
        _EndAssocBareSession(); // 这类应答会写宿主/开组合，联想会话随之结束
        {
            WIND_LOG_DEBUG(L"Processing InsertTextWithCursor response\n");
            // 提交方式二选一（服务端 pair_via_comp 下发，默认直写）：按键注入以 VK_PACKET 产生真实 WM_CHAR，
            // 给丢弃 TSF 直写 ASCII 却谎报 S_OK 的宿主（AutoCAD 表格，见 pair-commit-host-compat-notes.md），
            // 失败回退直写。之后都靠 _SimulatePairKey(VK_LEFT) 回移光标。
            bool injectEnabled = _pairViaComposition.load(std::memory_order_relaxed);
            if (!(injectEnabled && _pTextService->InjectUnicodeText(response.text)))
                _pTextService->CommitText(response.text, _inHotkeyDispatch);
            // 输入序列记账：cursorOffset>0 时光标前其实是左符号而末位是右符号，但都不是数字、结论同为清零。
            _TrackCommittedTextForSmartPunct(response.text);
            _isComposing = FALSE;
            _hasCandidates = FALSE;
            // 一次传入完整格数：修饰键按住时待执行动作会覆盖，逐格调用会只留下最后一格。
            _SimulatePairKey(VK_LEFT, response.cursorOffset);
            // 配对插入：记一层待跳出深度（core 侧 pair_tracker 的镜像计数），是跳出键的吃键闸门。
            // 四条配对建立路径都经此响应。
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
            // 光标跨过了右符号：光标前必是符号。
            _seqChar = '?';
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
        _EndAssocBareSession(); // 这类应答会写宿主/开组合，联想会话随之结束
        {
            // 智能符号：删除光标前 N 个字符并插入替换文本。默认走 TSF 同步范围替换，不依赖修饰键松开；
            // TSF 失败才回退 SendInput。取舍见 TextService::ReplacePrecedingChars 的 kTryTsfRangeReplace。
            // 热键派发期间不另传非按键上下文：同步会话被拒时落到 SendInput，不要编辑锁也不留组合。
            WIND_LOG_DEBUG(L"Processing ReplaceBackward response (smart symbol)\n");
            _pTextService->ReplacePrecedingChars(response.replaceCount, response.text);
            // 纯删除（`ime.undo_commit`）：光标前换成了什么看不见，清零；有替换文本则记它的末位。
            if (response.text.empty())
                _seqChar = WindInputSeq::kUnknown;
            else
                NoteCommittedSeqChar(response.text);
            _isComposing = FALSE;
            _hasCandidates = FALSE;
        }
        return TRUE;

    case ResponseType::HoldComposition:
        _EndAssocBareSession(); // 这类应答会写宿主/开组合，联想会话随之结束
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
        _EndAssocBareSession(); // 这类应答会写宿主/开组合，联想会话随之结束
        {
            // 标点顶屏 + 智能符号 HoldComposition：聚合式，候选文本并入 _pendingCommitPrefix（留在组合内），
            // 中文符号作 held 放入同一组合，最终一次 CommitText 收口。别改成「真提交 + 立即重开组合」：
            // diff 式宿主（微信、Tabby）会误读成替换而丢字。中文符号不能进 prefix，press2 要替换它。
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
            // direct_commit 顶码：立即真提交顶出文本，余码新组合延迟到本键 keyup（或兜底定时器）才开，
            // 隔一拍消息泵躲开 diff 式宿主整锁合并。见 docs/design/top-commit-mode-design.md。
            WIND_LOG_DEBUG_FMT(L"Processing CommitThenDefer: commit=%ls defer=%ls timeoutMs=%u\n",
                               response.text.c_str(), response.newComposition.c_str(),
                               response.holdTimeoutMs);
            _EndAssocBareSession();
            _pTextService->CommitText(response.text, _inHotkeyDispatch);
            // 数字记账不记：余码组合随后就开，光标前是组合内容而非 response.text 末位
            // （同 restartComposition 分支的理由）。seqChar 照记，理由同那个分支。
            NoteCommittedSeqChar(response.text);
            _isComposing = FALSE;
            _hasCandidates = FALSE;
            _pTextService->NotifyCandidatesVisibilityChanged(FALSE);
            _pTextService->StashDeferredComposition(response.newComposition, response.holdTimeoutMs,
                                                    _responseKeyVk);
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

    // Notify the service to clear input buffer and hide candidate window
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
    // Sync mode from service response
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

// 英文半角下会被吃下转发 core 的标点键：英半自定义键，或英文配对键（含右符号）。
// 判据与 OnTestKeyDown 的 english_autopair / english_custom_punct 两条同源，
// 也与 core 的 `handle_english_custom_punct` 接手判据逐条对应。
BOOL CKeyEventSink::_IsEnglishHalfEatenPunctKey(WPARAM vk, uint32_t modifiers) const
{
    if (_IsCustomEnglishPunctKey(vk, modifiers))
        return TRUE;
    if (modifiers & (KEYMOD_CTRL | KEYMOD_ALT))
        return FALSE;
    wchar_t pairChar = _MapVkToEnglishPairChar(vk, (modifiers & KEYMOD_SHIFT) != 0);
    return pairChar != 0 && _englishPairEngine.ShouldEat(pairChar);
}

// 该键在中文模式下是否该透传（不吃），由 core 推送的字符集合决定，空集合恒 FALSE。
// 只答「配置上该不该」：调用方须自行叠上 `!hasInputSession`（组码中按标点是顶码）与
// `!IsFullWidth()`（全角下 `#` 要出 `＃`）两道动态闸门。
BOOL CKeyEventSink::_IsPassthroughPunctKey(WPARAM vk, uint32_t modifiers) const
{
    // 按当下标点态二选一（运行时状态，只能问 CTextService 的镜像）。英文标点态那份是中文那份的超集。
    const std::set<wchar_t>& chars = _pTextService->IsChinesePunct()
                                         ? _cnPassthroughPunctChars
                                         : _enPassthroughPunctChars;
    if (chars.empty())
        return FALSE;
    // Ctrl/Alt 组合是功能热键，不参与出字（同 _IsCustomEnglishPunctKey）。
    if (modifiers & (KEYMOD_CTRL | KEYMOD_ALT))
        return FALSE;
    const BOOL shift = (modifiers & KEYMOD_SHIFT) != 0;
    wchar_t ch = 0;
    if (vk >= L'0' && vk <= L'9')
    {
        // 主键盘数字排：`VirtualKeyToPunctuation` 只认 OEM 键，故自带一张表（下标即 VK_0..VK_9，
        // 与 core `key_convert::punct_char` 的 shifted 列同源）。不按 Shift 时是数字，不参与透传。
        if (!shift)
            return FALSE;
        static const wchar_t kShiftedDigits[10] = {
            L')', L'!', L'@', L'#', L'$', L'%', L'^', L'&', L'*', L'('
        };
        ch = kShiftedDigits[vk - L'0'];
    }
    else
    {
        // OEM 标点键（`-_` `=+` `[{` `]}` `\|` `;:` `\'"` `,<` `.>` `/?` `` `~ ``）两态。
        ch = CHotkeyManager::VirtualKeyToPunctuation(vk, shift);
    }
    return ch != 0 && chars.count(ch) > 0;
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
    else if (key == CONFIG_KEY_CN_PASSTHROUGH_PUNCT)
    {
        // 格式同上（count(u8) + [ch:u16(LE)]...，core 侧复用 encode_custom_en_punct_value）。
        _cnPassthroughPunctChars.clear();
        if (value.empty()) return;
        uint8_t count = value[0];
        for (size_t i = 0; i < count && (1 + i * 2 + 2) <= value.size(); i++)
        {
            uint16_t ch = *reinterpret_cast<const uint16_t*>(value.data() + 1 + i * 2);
            _cnPassthroughPunctChars.insert((wchar_t)ch);
        }
        WIND_LOG_INFO_FMT(L"CN passthrough punct chars updated: count=%d\n", (int)_cnPassthroughPunctChars.size());
    }
    else if (key == CONFIG_KEY_EN_PASSTHROUGH_PUNCT)
    {
        _enPassthroughPunctChars.clear();
        if (value.empty()) return;
        uint8_t count = value[0];
        for (size_t i = 0; i < count && (1 + i * 2 + 2) <= value.size(); i++)
        {
            uint16_t ch = *reinterpret_cast<const uint16_t*>(value.data() + 1 + i * 2);
            _enPassthroughPunctChars.insert((wchar_t)ch);
        }
        WIND_LOG_INFO_FMT(L"EN passthrough punct chars updated: count=%d\n", (int)_enPassthroughPunctChars.size());
    }
    else if (key == CONFIG_KEY_SPACE_CUSTOM_COLS)
    {
        // 格式：mask(u8)（对齐 Rust encode_space_custom_cols_value）。
        _spaceCustomCols = value.empty() ? 0 : value[0];
        WIND_LOG_INFO_FMT(L"Space custom cols updated: mask=0x%X\n", (unsigned)_spaceCustomCols);
    }
    else if (key == CONFIG_KEY_PAIR_STATE_TTL)
    {
        // 格式：secs(u16 LE)（对齐 Rust encode_pair_state_ttl_value）
        if (value.size() < 2) return;
        uint16_t secs = *reinterpret_cast<const uint16_t*>(value.data());
        _pairStateTtlMs = (ULONGLONG)secs * 1000ULL;
        WIND_LOG_INFO_FMT(L"Pair state TTL updated: %d s\n", (int)secs);
    }
    else if (key == CONFIG_KEY_PAIR_VIA_COMP)
    {
        // 格式：enabled(u8)（对齐 Rust encode_pair_via_comp_value）。
        // 开启后 InsertTextWithCursor 走按键注入提交，替代 TSF 直写。
        const bool enabled = wind::paircommit::DecodePairViaComp(
            value.data(), value.size(), _pairViaComposition.load());
        _pairViaComposition.store(enabled);
        WIND_LOG_INFO_FMT(L"Pair via composition updated: enabled=%d\n", enabled ? 1 : 0);
    }
    else if (key == CONFIG_KEY_CTRL_SPACE_TOGGLE)
    {
        // 格式：enabled(u8)（对齐 Rust encode_ctrl_space_toggle_value）。
        const bool enabled = wind::ctrlspace::DecodeToggleValue(value.data(), value.size(), _ctrlSpaceToggleEnabled.load());
        _ctrlSpaceToggleEnabled.store(enabled);
        WIND_LOG_INFO_FMT(L"Ctrl+Space toggle updated: enabled=%d\n", enabled ? 1 : 0);
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
    else if (key == CONFIG_KEY_SPACE_BEFORE_DIGIT)
    {
        // 格式：enabled(u8)（对齐 Rust push_space_before_digit_config）。
        if (value.empty()) return;
        _spaceBeforeDigit = value[0] != 0;
        WIND_LOG_INFO_FMT(L"Space before digit updated: %d\n", (int)_spaceBeforeDigit);
    }
    else if (key == CONFIG_KEY_COMPOSITION_PLACEHOLDER)
    {
        // 格式：kind(u8)（对齐 Rust encode_composition_placeholder_value）。0 = 空格、1 = ZWSP、
        // 2 = 盲文空白（U+2800）。原样存下，由 PlaceholderForKind 取字符：认不出的值回落空格。
        // 空文本兜底占位按本进程 compat 规则取字符，决策在 core。
        if (value.empty()) return;
        _pTextService->SetCompositionPlaceholderKind(value[0]);
        WIND_LOG_INFO_FMT(L"Composition placeholder updated: kind=%d\n", (int)value[0]);
    }
    else if (key == CONFIG_KEY_MULTILINE_COMMIT)
    {
        // 格式：mode(u8)（对齐 Rust encode_multiline_commit_value）。0 = commit、1 = paste。
        if (value.empty()) return;
        _pTextService->SetMultilineCommitMode(value[0]);
        WIND_LOG_INFO_FMT(L"Multiline commit mode updated: mode=%d\n", (int)value[0]);
    }
    else if (key == CONFIG_KEY_COMPAT_TITLE_MATCH)
    {
        // 格式：enabled(u8)（对齐 Rust encode_compat_title_match_value）。core 按「存在 compat
        // 标题规则」推；开着 OnSetFocus 才取窗口标题随 focus_gained 上报。空值保持现状。
        const bool enabled = wind::window_title::ParseTitleMatchValue(
            value.data(), value.size(), _pTextService->IsTitleMatchEnabled() != FALSE);
        _pTextService->SetTitleMatchEnabled(enabled ? TRUE : FALSE);
        WIND_LOG_INFO_FMT(L"Compat title match updated: enabled=%d\n", enabled ? 1 : 0);
    }
    else if (key == CONFIG_KEY_EXTENSION_KEY_OBSERVE)
    {
        // 格式：enabled(u8)（对齐 Rust encode_extension_key_observe_value）。空值保持现状。
        if (value.empty()) return;
        _observeKeys.store(value[0] != 0, std::memory_order_relaxed);
        WIND_LOG_INFO_FMT(L"Extension key observe updated: enabled=%d\n", value[0] != 0 ? 1 : 0);
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

    // Build CommitRequestPayload（尺寸跟 sizeof 走，有 BinaryProtocol.h 的 static_assert 兜底）
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
        // 输入序列记账（见 header）。只在不留组合时记，理由同 _HandleServiceResponse 那处。
        _TrackCommittedTextForSmartPunct(text);
        _isComposing = FALSE;
        _hasCandidates = FALSE;
    }

    // Handle mode change
    if (modeChanged)
    {
        _pTextService->SetInputMode(chineseMode);
    }
}

// 读 resync 旗 + 过期检查，到期立即清旗。_resyncFailStreak 在此不清零：只由响应成功清零，
// 否则「连续失败 → 降级」会被时间抹掉。
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

// 「有活跃输入会话」的唯一判据（为何收口见声明处）。分量：
//   HasCompositionOrRecover : TSF 组合活跃，或宿主收走了组合而缓冲仍在。
//   _hasCandidates          : 候选窗有内容而组合为空（非 app_inline 时 core 发空组合串）。
//   _hotkeyModeSession      : 热键激活的模式（见下）。
//   _IsResyncActive         : 上次 IPC 失败后的自愈窗口，视作有会话以便重握手。
//   HasDeferredComposition  : direct_commit 顶码已提交、余码组合尚未重开的真空期；漏掉会让
//     空格插入字面空格、退格删掉刚上屏的字。
BOOL CKeyEventSink::_HasInputSession()
{
    // 恢复中（宿主收走了组合、缓冲仍在）同样算会话。
    return _pTextService->HasCompositionOrRecover()
        || _hasCandidates
        // 热键激活的模式（加词 / 临拼 / 特殊 / 生僻字）在 `input.caret.*_via_composition = false`
        // 时不建 composition。本位由服务端经 STATUS_HOTKEY_SESSION 驱动，DLL 只镜像不推断。
        || _hotkeyModeSession
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
// 修饰键按住时（如打 "(" 的 Shift）光标键延到修饰键松开再发。别改成用 SendInput 临时松开再恢复
// Shift：系统会多生成 Shift keydown，重新武装 _pendingKeyUpKey，松开物理 Shift 时误切中英。
// ============================================================================

// 直通 ime.pair 的推送落点（WM_PAIR_COMMIT 窗口消息回调，不在 OnKeyDown 调用栈里）。
// 提交与左移都必须挪进按键上下文（合成提交键），不能在这里直接做：宿主插入点不会跟着更新，
// 左移会把光标打到文档开头。见 docs/architecture/pair-commit-host-compat-notes.md 第八节。
void CKeyEventSink::HandlePairCommitPush(const std::wstring& text, uint32_t moveLeft)
{
    WIND_LOG_DEBUG_FMT(L"HandlePairCommitPush: textLen=%zu, moveLeft=%u\n",
                       text.length(), moveLeft);

    if (text.empty())
    {
        // 正常不会推空文本；真收到时左移与记账都没有对应正文，照做只会让深度与 core 失配。
        WIND_LOG_WARN(L"HandlePairCommitPush: 空文本，丢弃（不左移、不记深度）\n");
        return;
    }

    if (QueueAsyncCommitViaSyntheticKey(text, /*replacingHeld=*/FALSE, /*pairCommit=*/TRUE, moveLeft))
        return;

    // 合成键注入失败（极罕见）：降级为非按键上下文直接提交，保证不丢字。走异步会话，左移可能
    // 抢在文本落定之前，是这条降级路的已知残留竞态。
    WIND_LOG_WARN(L"HandlePairCommitPush: 合成提交键注入失败，退回非按键上下文直接提交\n");
    _pTextService->CommitText(text, /*nonKeyContext=*/TRUE);
    _TrackCommittedTextForSmartPunct(text);
    _isComposing = FALSE;
    _hasCandidates = FALSE;
    _ApplyPairCommitTail(/*pairCommit=*/TRUE, moveLeft);
}

// 配对上屏的收尾：左移回两段之间 + 记一层待跳出深度。记账判据是 `pairCommit` 而非
// `moveLeft > 0`：`jump="0"` 时 core 照样压一层配对栈，深度必须与 core 的 pair_tracker 严格同步。
void CKeyEventSink::_ApplyPairCommitTail(BOOL pairCommit, uint32_t moveLeft)
{
    if (!pairCommit)
        return;
    // jump=0 时不注入方向键（光标本就该落在末尾），但栈照记——两件事判据不同。
    if (moveLeft > 0)
        _SimulatePairKey(VK_LEFT, (int)moveLeft);
    _pairPendingDepth++;
    TouchPairState();
}

// 丢弃全部待提交，必须走这一个出口：丢掉的配对条目在 core 侧已压栈，DLL < core 这个失配方向
// 没有自愈路径（pair_jumpout_desync_replay 只治反方向），只能留 WARN。
void CKeyEventSink::_DropPendingAsyncCommits()
{
    size_t pairLayers = 0;
    for (const PendingAsyncCommit& c : _pendingAsyncCommits)
    {
        if (c.pairCommit) pairLayers++;
    }
    if (pairLayers > 0)
    {
        WIND_LOG_WARN_FMT(L"AsyncCommit: 丢弃 %zu 条待提交, 其中 %zu 条是配对上屏 —— "
                          L"core 侧那 %zu 层配对栈将成孤儿\n",
                          _pendingAsyncCommits.size(), pairLayers, pairLayers);
    }
    _pendingAsyncCommits.clear();
}

void CKeyEventSink::_SimulatePairKey(WORD vk, int count)
{
    if (count <= 0)
        return;

    if (_AreModifiersHeld())
    {
        // Defer: save action, execute when modifiers released.
        // 后来者整条覆盖（含 count）：只有最后一次配对的光标定位有意义，count 是这一次要走的格数，
        // 保留旧 count 会让多字符右段（`-->`）的光标停错位置。
        _pendingPairAction.vk = vk;
        _pendingPairAction.count = count;
        _pendingPairAction.active = true;
        WIND_LOG_DEBUG_FMT(L"Auto-pair: deferred vk=0x%02X x%d (modifiers held)\n",
            (WORD)vk, _pendingPairAction.count);
        return;
    }

    // No modifiers: execute immediately via skip list.
    // count 个 down+up 一次 SendInput 提交，免得被并发的真实输入从中间穿插。
    std::vector<INPUT> inputs;
    inputs.reserve((size_t)count * 2);
    for (int i = 0; i < count; i++)
    {
        _PushSkipKey(vk, FALSE);

        INPUT down = {};
        down.type = INPUT_KEYBOARD;
        down.ki.wVk = vk;
        inputs.push_back(down);

        INPUT up = {};
        up.type = INPUT_KEYBOARD;
        up.ki.wVk = vk;
        up.ki.dwFlags = KEYEVENTF_KEYUP;
        inputs.push_back(up);
    }
    SendInput((UINT)inputs.size(), inputs.data(), sizeof(INPUT));
}

// 把一个已被我们吃掉的键原样重放给宿主。不做修饰键 defer（与 _SimulatePairKey 不同）：重放的是
// 用户真按的键，物理修饰键叠加恰好还原本意（Shift+Enter 就该是 Shift+Enter）。
void CKeyEventSink::_ReplayKeyToHost(WORD vk)
{
    // skip 条目只压一个（给 down）：keyup 走正常路径，OnTestKeyUp 的 direct_commit 顶码分支指望它经过；
    // 多压的 keyup 条目在不调 OnTestKeyUp 的宿主上会残留。
    _PushSkipKey(vk, FALSE);

    INPUT inputs[2] = {};
    inputs[0].type = INPUT_KEYBOARD;
    inputs[0].ki.wVk = vk;
    inputs[1].type = INPUT_KEYBOARD;
    inputs[1].ki.wVk = vk;
    inputs[1].ki.dwFlags = KEYEVENTF_KEYUP;
    UINT sent = SendInput(2, inputs, sizeof(INPUT));
    if (sent != 2)
    {
        // 注入失败 = 这个键彻底消失（原键已被吃掉），必须留痕。
        WIND_LOG_WARN_FMT(L"Replay: SendInput sent %u of 2 for vk=0x%02X, key lost\n",
                          sent, (uint32_t)vk);
        return;
    }
    WIND_LOG_DEBUG_FMT(L"Replay: replayed vk=0x%02X to host\n", (uint32_t)vk);
}

bool CKeyEventSink::_AreModifiersHeld()
{
    return (GetAsyncKeyState(VK_SHIFT) & 0x8000) != 0 ||
           (GetAsyncKeyState(VK_CONTROL) & 0x8000) != 0 ||
           (GetAsyncKeyState(VK_MENU) & 0x8000) != 0;
}

// ── SkipKeyTable 的三个薄包装 ────────────────────────────────────────────────
// 判定逻辑全在 SkipKeyTable（有单元测试），这里只负责传时钟、打日志、BOOL/bool 转换。

void CKeyEventSink::_LogExpiredSkipKeys()
{
    SkipKeyTable::Entry expired[SkipKeyTable::kExpiredLogCap];
    const int n = _skipKeys.DrainExpired(expired, SkipKeyTable::kExpiredLogCap);
    for (int i = 0; i < n; i++)
    {
        // 注入的键没能回到本 sink：前台窗口已换、宿主此刻不走 TSF、SendInput 被 UIPI 拦。
        WIND_LOG_WARN_FMT(L"SkipKey: expired unconsumed vk=0x%02X forKeyUp=%d (total=%d)\n",
                          (uint32_t)expired[i].vk, (int)expired[i].forKeyUp,
                          _skipKeys.ExpiredTotal());
    }
}

void CKeyEventSink::_PushSkipKey(WORD vk, BOOL forKeyUp)
{
    const int droppedBefore = _skipKeys.DroppedTotal();
    _skipKeys.Push(vk, forKeyUp != FALSE, GetTickCount64());
    if (_skipKeys.DroppedTotal() != droppedBefore)
    {
        // 满表**不能静默丢弃**：这个注入键会被当成用户真按的键走完整 IME 流程。
        WIND_LOG_WARN_FMT(L"SkipKey: table full (%d), dropped vk=0x%02X (total=%d)\n",
                          SkipKeyTable::kMaxKeys, (uint32_t)vk, _skipKeys.DroppedTotal());
    }
    _LogExpiredSkipKeys();
}

BOOL CKeyEventSink::_TryConsumeSkipKey(WPARAM wParam, BOOL forKeyUp)
{
    const bool hit = _skipKeys.TryConsume((uint16_t)wParam, forKeyUp != FALSE, GetTickCount64());
    _LogExpiredSkipKeys();
    if (hit)
    {
        WIND_LOG_DEBUG_FMT(L"SkipKey: vk=0x%02X forKeyUp=%d bypassed IME, remaining=%d\n",
                           (uint32_t)wParam, (int)forKeyUp, _skipKeys.Count());
    }
    return hit ? TRUE : FALSE;
}

BOOL CKeyEventSink::_SendAsyncCommitTriggerKey()
{
    // down+up 一次性提交，免得被真实输入穿插。VK_ASYNC_COMMIT_TRIGGER 是未分配值，不走 _PushSkipKey：
    // 它要被 OnTestKeyDown/OnKeyDown 的专门分支吃掉，而不是直接放行。
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

BOOL CKeyEventSink::QueueAsyncCommitViaSyntheticKey(const std::wstring& text, BOOL replacingHeld,
                                                    BOOL pairCommit, uint32_t moveLeft)
{
    if (text.empty())
    {
        // 无事可做。但配对那条不能静默返回 TRUE：调用方会以为已排队而不补记账，深度永远不 +1。
        // 调用方已有空文本闸门，这里让不可达变成可检测。
        if (pairCommit)
        {
            WIND_LOG_WARN(L"QueueAsyncCommitViaSyntheticKey: 配对上屏拿到空文本，拒绝排队\n");
            return FALSE;
        }
        return TRUE;
    }

    if (_pendingAsyncCommits.size() >= MAX_PENDING_ASYNC_COMMITS)
    {
        // 队列积压 = 上一批触发键没送达（多半目标窗口已失焦），整体清空并留痕，好过无界增长或乱序上屏。
        WIND_LOG_WARN(L"QueueAsyncCommitViaSyntheticKey: pending queue full, dropping all pending commits\n");
        _DropPendingAsyncCommits();
    }
    _pendingAsyncCommits.push_back(PendingAsyncCommit{text, replacingHeld, pairCommit, moveLeft});

    if (!_SendAsyncCommitTriggerKey())
    {
        WIND_LOG_WARN_FMT(L"QueueAsyncCommitViaSyntheticKey: trigger key injection failed, textLen=%zu\n",
                          text.length());
        _pendingAsyncCommits.pop_back();
        return FALSE;
    }
    WIND_LOG_DEBUG_FMT(L"QueueAsyncCommitViaSyntheticKey: queued textLen=%zu, pair=%d, moveLeft=%u, pending=%zu\n",
                       text.length(), (int)pairCommit, moveLeft, _pendingAsyncCommits.size());
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
    // openclose 是回读的真实 compartment 值。门控不可省：日志宏实参在调用点求值，不门控就是每个英文字符两次 COM 调用。
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

