#include "TextService.h"
#include "EditableContextPolicy.h"
#include "CandidateHotkeyPolicy.h"
#include "KeyEventSink.h"
#include "IPCClient.h"
#include "LangBarItemButton.h"
#include "CaretEditSession.h"
#include "ImmOpenCandidateNudge.h"
#include "DisplayAttributeInfo.h"
#include "HotkeyManager.h"
#include "HostWindow.h"
#include "MultilineCommit.h"        // 多行文本粘贴上屏：CRLF 规范化 + 下发值判读
#include "CompositionPlaceholder.h" // 组合占位识别 + 兜底取值（空格 / ZWSP / 盲文空白，由 core 按应用决定，GH#175）
#include "WindowTitlePolicy.h" // 窗口标题采集判据（开关 / 截断；compat 的 title 条件）
#include "ServiceReadyRetryPolicy.h" // focus_gained 补发判决
#include "CompositionRecoverPolicy.h" // 宿主过早终止组合后接上（受控输入框回写 value）
#include <vector>
#include <algorithm>
#include <shellscalingapi.h>
#include <inputscope.h> // ITfInputScope / InputScope 枚举

// EditableContextPolicy.h 为了能用 g++ 在非 Windows 机器上单测而不含 Win32 头，两个 SDK 常量在那边按数值
// 重述。这两行把「与 SDK 对一遍」变成编译期门禁：抄错时判据静默失效，而那边的单测用同一个常量，照绿。
static_assert(wind::editable::kEmptyContextHr == static_cast<std::uint32_t>(TF_E_EMPTYCONTEXT),
              "kEmptyContextHr 与 SDK 的 TF_E_EMPTYCONTEXT 不一致");
static_assert(wind::editable::kReadOnlyDynFlag == static_cast<std::uint32_t>(TF_SD_READONLY),
              "kReadOnlyDynFlag 与 SDK 的 TF_SD_READONLY 不一致");
// CandidateHotkeyPolicy.h 同理按数值重述了 KEYMOD_* 通用位。
static_assert(wind::candhotkey::kModShift == KEYMOD_SHIFT, "candhotkey kModShift != KEYMOD_SHIFT");
static_assert(wind::candhotkey::kModCtrl == KEYMOD_CTRL, "candhotkey kModCtrl != KEYMOD_CTRL");
static_assert(wind::candhotkey::kModAlt == KEYMOD_ALT, "candhotkey kModAlt != KEYMOD_ALT");
static_assert(wind::candhotkey::kModWin == KEYMOD_WIN, "candhotkey kModWin != KEYMOD_WIN");

// GUID_PROP_INPUTSCOPE 在 SDK 头中仅为 EXTERN_C 声明，其字节定义需某个 TU 启用 INITGUID
// 才会生成；直接引用会产生 LNK2019。这里本地定义该 GUID 值（与 inputscope.h 一致），
// 避免引入 <initguid.h> 把本文件所有 GUID 实体化而与已链接定义冲突。
static const GUID kGuidPropInputScope =
    { 0x1713dd5a, 0x68e7, 0x4a5b, { 0x9a, 0xf6, 0x59, 0x2a, 0x59, 0x5c, 0x77, 0x8d } };

// InputScope bit 常量（与服务端、inputscope.h 一致；权威定义见 Windows SDK `um/InputScope.h`）。
// 邻近的 IS_PRIVATE=61 / IS_SEARCH=50 不是密码框信号（无痕窗口的普通输入框即报 IS_PRIVATE），勿并入。
static const UINT64 kScopeBitPassword = 1ULL << 31; // IS_PASSWORD
static const UINT64 kScopeBitNumericPassword = 1ULL << 63; // IS_NUMERIC_PASSWORD
// 密码框判据的唯一定义处：core 的 `is_password_scope` 与之逐位对齐，勿各自展开。
static const UINT64 kPasswordScopeBits = kScopeBitPassword | kScopeBitNumericPassword;

// 诊断 HUD 的上报 reason，语义与 Rust coordinator 侧 reason_from 一致：0 None / 1 CompartmentDisabled /
// 2 InputScopePassword / 3 NumericPassword / 4 ContextDisabled。disabled（线程级 compartment）优先级最高。
//
// `rawMask` 必须是折位之前宿主原始报的 InputScope：context 级 KEYBOARD_DISABLED 会被折进 IS_PASSWORD 位，
// 折位后分不出「宿主报了密码」与「宿主只是禁用了输入」（有的浏览器无输入焦点时也置 context 级禁用）。
// 4 这一档把这个区分带给服务端，服务端见 reason=4 且 mask 含密码位才采信。
static inline uint8_t ComputeInputReason(bool disabled, UINT64 rawMask, bool ctxDisabled)
{
    if (disabled) return 1;
    if (rawMask & (1ULL << 63)) return 3; // IS_NUMERIC_PASSWORD
    if (rawMask & (1ULL << 31)) return 2; // IS_PASSWORD
    if (ctxDisabled) return 4;            // context 级 KEYBOARD_DISABLED，宿主未报密码 scope
    return 0;
}

// TSF 标准 compartment GUID（本地定义，避免链接 TSF GUID 静态库产生 LNK2019）。
// 宿主（含 Chromium 系浏览器密码框）会在 context 上置 KEYBOARD_DISABLED 表示"禁用输入法"；
// 这是比 InputScope 更可靠的密码框信号（小狼毫/Weasel 即用此判定），且无痕普通框不会置位。
static const GUID kGuidCompartmentKeyboardDisabled =
    { 0x71a5b253, 0x1951, 0x466b, { 0x9f, 0xbc, 0x9c, 0x88, 0x08, 0xfa, 0x84, 0xf2 } };

// 判断屏幕坐标点是否不落在任何显示器上（物理上不存在的野坐标）。个别宿主（某些 Qt / OpenGL 应用）的
// GetTextExt 会返回这类坐标，调用方应丢弃、回退到窗口相对的方法。
//
// 不要改回「是否落在前台窗口矩形外」：焦点窗口 ≠ 前台窗口（桌面输入、点过任务栏后）与拖动窗口时
// （GetTextExt 与 GetWindowRect 来自不同时刻）都会误判合法坐标；真野坐标都远离所有显示器。
// 用 MonitorFromPoint 而非 SM_*VIRTUALSCREEN：虚拟屏幕是外接矩形，多屏错位排布时会放行空隙里的坐标。
// 本判据在 DPI 转换前执行，显示器范围与坐标可能不同语境；可接受，因为它只做「离谱与否」的粗判断，
// 野坐标差的是几千像素，远超 DPI 缩放造成的偏差。
static bool IsScreenPointOutsideAllMonitors(LONG x, LONG y)
{
    POINT pt = { x, y };
    return MonitorFromPoint(pt, MONITOR_DEFAULTTONULL) == nullptr;
}

// 读焦点 context 的 TSF InputScope 集合，用于识别密码框等语义控件：枚举值按位编码为 bitmask
// （bit N 表示枚举值 N 存在，如 IS_PASSWORD=31），交由服务端决策。
// 需要读锁（ITfReadOnlyProperty::GetValue 要 read cookie）；OnSetFocus 期间同步读锁通常可得，失败时返回 0。
class CQueryInputScopeEditSession : public ITfEditSession
{
public:
    CQueryInputScopeEditSession(ITfContext* pContext, UINT64* pMaskOut)
        : _refCount(1), _pContext(pContext), _pMaskOut(pMaskOut)
    {
        if (_pContext)
            _pContext->AddRef();
        if (_pMaskOut)
            *_pMaskOut = 0;
    }

    STDMETHODIMP QueryInterface(REFIID riid, void** ppvObj) override
    {
        if (ppvObj == nullptr)
            return E_INVALIDARG;
        *ppvObj = nullptr;
        if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITfEditSession))
            *ppvObj = static_cast<ITfEditSession*>(this);
        if (*ppvObj)
        {
            AddRef();
            return S_OK;
        }
        return E_NOINTERFACE;
    }

    STDMETHODIMP_(ULONG) AddRef() override { return InterlockedIncrement(&_refCount); }
    STDMETHODIMP_(ULONG) Release() override
    {
        LONG cr = InterlockedDecrement(&_refCount);
        if (cr == 0)
            delete this;
        return cr;
    }

    STDMETHODIMP DoEditSession(TfEditCookie ec) override
    {
        if (_pContext == nullptr || _pMaskOut == nullptr)
            return E_FAIL;

        ITfReadOnlyProperty* pProp = nullptr;
        if (FAILED(_pContext->GetAppProperty(kGuidPropInputScope, &pProp)) || pProp == nullptr)
            return S_OK;

        // 在两处 range 读取并合并 InputScope：
        //  - selection（光标/选区所在内容 range）：宿主常把 IS_PASSWORD 等设在内容 range 上
        //  - 文档起点空 range：兜底，反映文档级提示（如 IS_PRIVATE）
        // 某些宿主（Chromium 系浏览器）两者不一致，故都读，避免漏掉 IS_PASSWORD。
        ITfRange* ranges[2] = { nullptr, nullptr };
        TF_SELECTION sel = {};
        ULONG fetched = 0;
        if (SUCCEEDED(_pContext->GetSelection(ec, TF_DEFAULT_SELECTION, 1, &sel, &fetched)) && fetched > 0)
            ranges[0] = sel.range; // 持有引用
        ITfRange* pStart = nullptr;
        if (SUCCEEDED(_pContext->GetStart(ec, &pStart)))
            ranges[1] = pStart;

        for (int r = 0; r < 2; ++r)
        {
            if (ranges[r] == nullptr)
                continue;
            VARIANT var;
            VariantInit(&var);
            if (SUCCEEDED(pProp->GetValue(ec, ranges[r], &var)) && var.vt == VT_UNKNOWN && var.punkVal != nullptr)
            {
                ITfInputScope* pInputScope = nullptr;
                if (SUCCEEDED(var.punkVal->QueryInterface(IID_ITfInputScope, reinterpret_cast<void**>(&pInputScope))) && pInputScope != nullptr)
                {
                    InputScope* pScopes = nullptr;
                    UINT count = 0;
                    if (SUCCEEDED(pInputScope->GetInputScopes(&pScopes, &count)) && pScopes != nullptr)
                    {
                        for (UINT i = 0; i < count; ++i)
                        {
                            int v = static_cast<int>(pScopes[i]);
                            if (v >= 0 && v < 64)
                                *_pMaskOut |= (1ULL << v);
                        }
                        CoTaskMemFree(pScopes);
                    }
                    pInputScope->Release();
                }
            }
            VariantClear(&var);
        }

        for (int r = 0; r < 2; ++r)
            if (ranges[r] != nullptr)
                ranges[r]->Release();
        pProp->Release();
        return S_OK;
    }

private:
    ~CQueryInputScopeEditSession() { SafeRelease(_pContext); }

    LONG _refCount;
    ITfContext* _pContext;
    UINT64* _pMaskOut;
};

// 结束组合的编辑会话。接管传入组合指针的所有权：DoEditSession 里结束并释放，请求失败则在析构里释放。
class CEndCompositionEditSession : public ITfEditSession
{
public:
    // pComposition ownership is transferred to this object
    CEndCompositionEditSession(CTextService* pTextService, ITfComposition* pComposition)
        : _refCount(1), _pTextService(pTextService), _pComposition(pComposition)
    {
        _pTextService->AddRef();
        // Note: pComposition ownership is transferred, no AddRef needed
    }

    ~CEndCompositionEditSession()
    {
        _pTextService->Release();
        // If DoEditSession was never called (request failed), release the composition
        if (_pComposition != nullptr)
        {
            WIND_LOG_DEBUG(L"~CEndCompositionEditSession: Releasing orphaned composition\n");
            _pComposition->Release();
            _pComposition = nullptr;
        }
    }

    // IUnknown
    STDMETHODIMP QueryInterface(REFIID riid, void** ppvObj)
    {
        if (ppvObj == nullptr) return E_INVALIDARG;
        *ppvObj = nullptr;
        if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITfEditSession))
        {
            *ppvObj = (ITfEditSession*)this;
            AddRef();
            return S_OK;
        }
        return E_NOINTERFACE;
    }

    STDMETHODIMP_(ULONG) AddRef()
    {
        return InterlockedIncrement(&_refCount);
    }

    STDMETHODIMP_(ULONG) Release()
    {
        LONG cr = InterlockedDecrement(&_refCount);
        if (cr == 0) delete this;
        return cr;
    }

    // ITfEditSession
    STDMETHODIMP DoEditSession(TfEditCookie ec)
    {
        if (_pComposition != nullptr)
        {
            // Get the composition range and clear the text before ending
            // This prevents the composition text from being committed
            ITfRange* pRange = nullptr;
            if (SUCCEEDED(_pComposition->GetRange(&pRange)))
            {
                // Clear the composition text (set to empty string)
                pRange->SetText(ec, 0, L"", 0);
                pRange->Release();
            }

            _pComposition->EndComposition(ec);

            // Release the composition
            _pComposition->Release();
            _pComposition = nullptr;
            WIND_LOG_DEBUG(L"DoEditSession: Composition ended and released\n");
        }
        return S_OK;
    }

private:
    LONG _refCount;
    CTextService* _pTextService;
    ITfComposition* _pComposition;  // Owned composition pointer
};

// 只删残留的编辑会话：恢复中服务端清空了缓冲（Esc、退格删光），残留要跟着撤掉。
class CDropLeftoverEditSession : public ITfEditSession
{
public:
    CDropLeftoverEditSession(CTextService* pTextService, ITfContext* pContext)
        : _refCount(1), _pTextService(pTextService), _pContext(pContext)
    {
        _pTextService->AddRef();
        _pContext->AddRef();
    }
    ~CDropLeftoverEditSession()
    {
        _pTextService->Release();
        _pContext->Release();
    }
    STDMETHODIMP QueryInterface(REFIID riid, void** ppvObj)
    {
        if (ppvObj == nullptr) return E_INVALIDARG;
        *ppvObj = nullptr;
        if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITfEditSession))
        {
            *ppvObj = (ITfEditSession*)this;
            AddRef();
            return S_OK;
        }
        return E_NOINTERFACE;
    }
    STDMETHODIMP_(ULONG) AddRef() { return InterlockedIncrement(&_refCount); }
    STDMETHODIMP_(ULONG) Release()
    {
        LONG cr = InterlockedDecrement(&_refCount);
        if (cr == 0) delete this;
        return cr;
    }
    STDMETHODIMP DoEditSession(TfEditCookie ec)
    {
        _pTextService->_DropRecoverLeftover(ec, _pContext);
        return S_OK;
    }

private:
    LONG _refCount;
    CTextService* _pTextService;
    ITfContext* _pContext;
};

// 恢复解除时清掉残留。接管调用方给的 range 引用（记录在请求前就已清掉，见 _DisarmRecover）。
class CPurgeLeftoverEditSession : public ITfEditSession
{
public:
    CPurgeLeftoverEditSession(CTextService* pTextService, ITfRange* pRange, const std::wstring& text)
        : _refCount(1), _pTextService(pTextService), _pRange(pRange), _text(text)
    {
        _pTextService->AddRef();
    }
    ~CPurgeLeftoverEditSession()
    {
        _pTextService->Release();
        _pRange->Release();
    }
    STDMETHODIMP QueryInterface(REFIID riid, void** ppvObj)
    {
        if (ppvObj == nullptr) return E_INVALIDARG;
        *ppvObj = nullptr;
        if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITfEditSession))
        {
            *ppvObj = (ITfEditSession*)this;
            AddRef();
            return S_OK;
        }
        return E_NOINTERFACE;
    }
    STDMETHODIMP_(ULONG) AddRef() { return InterlockedIncrement(&_refCount); }
    STDMETHODIMP_(ULONG) Release()
    {
        LONG cr = InterlockedDecrement(&_refCount);
        if (cr == 0) delete this;
        return cr;
    }
    STDMETHODIMP DoEditSession(TfEditCookie ec)
    {
        _pTextService->_PurgeLeftover(ec, _pRange, _text);
        return S_OK;
    }

private:
    LONG _refCount;
    CTextService* _pTextService;
    ITfRange* _pRange;
    std::wstring _text;
};

// EditSession for committing text atomically (end composition + insert text in one session)
// This prevents race conditions where async EndComposition clears text inserted by a subsequent InsertText.
class CCommitTextEditSession : public ITfEditSession
{
public:
    // pComposition ownership is transferred to this object (may be nullptr if no active composition)
    // paste=TRUE：组合清空并结束后，文本改经剪贴板 + Ctrl+V 交给宿主（见 MultilineCommit.h）。
    CCommitTextEditSession(CTextService* pTextService, ITfContext* pContext,
                           ITfComposition* pComposition, const std::wstring& text, BOOL paste = FALSE)
        : _refCount(1), _pTextService(pTextService), _pContext(pContext),
          _pComposition(pComposition), _text(text), _paste(paste), _success(FALSE), _executed(FALSE)
    {
        _pTextService->AddRef();
        _pContext->AddRef();
    }

    ~CCommitTextEditSession()
    {
        _pTextService->Release();
        _pContext->Release();
        if (_pComposition != nullptr)
        {
            WIND_LOG_WARN(L"~CCommitTextEditSession: Releasing orphaned composition\n");
            _pComposition->Release();
            _pComposition = nullptr;
        }
    }

    // IUnknown
    STDMETHODIMP QueryInterface(REFIID riid, void** ppvObj)
    {
        if (ppvObj == nullptr) return E_INVALIDARG;
        *ppvObj = nullptr;
        if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITfEditSession))
        {
            *ppvObj = (ITfEditSession*)this;
            AddRef();
            return S_OK;
        }
        return E_NOINTERFACE;
    }

    STDMETHODIMP_(ULONG) AddRef() { return InterlockedIncrement(&_refCount); }
    STDMETHODIMP_(ULONG) Release()
    {
        LONG cr = InterlockedDecrement(&_refCount);
        if (cr == 0) delete this;
        return cr;
    }

    // ITfEditSession
    // 在当前选区直接插入文本 (不经 composition)。用于无 active composition 的兜底路径
    // (鼠标上屏 / GetRange 失败)。
    HRESULT _InsertAtSelection(TfEditCookie ec, const std::wstring& text, DWORD flags = 0)
    {
        if (text.empty())
            return S_OK;

        ITfInsertAtSelection* pInsertAtSel = nullptr;
        HRESULT hr = _pContext->QueryInterface(IID_ITfInsertAtSelection, (void**)&pInsertAtSel);
        if (FAILED(hr) || pInsertAtSel == nullptr)
        {
            WIND_LOG_DEBUG(L"CCommitTextEditSession: Failed to get ITfInsertAtSelection\n");
            return E_FAIL;
        }

        ITfRange* pRange = nullptr;
        hr = pInsertAtSel->InsertTextAtSelection(ec, flags, text.c_str(), (LONG)text.length(), &pRange);
        pInsertAtSel->Release();

        if (FAILED(hr))
        {
            WIND_LOG_DEBUG_FMT(L"CCommitTextEditSession: InsertTextAtSelection failed hr=0x%08X\n", hr);
            return hr;
        }

        if (pRange != nullptr)
        {
            pRange->Collapse(ec, TF_ANCHOR_END);
            TF_SELECTION sel = {};
            sel.range = pRange;
            sel.style.ase = TF_AE_NONE;
            sel.style.fInterimChar = FALSE;
            _pContext->SetSelection(ec, 1, &sel);
            pRange->Release();
        }
        return S_OK;
    }

    STDMETHODIMP DoEditSession(TfEditCookie ec)
    {
        // 「本会话确实跑过」，必须是函数第一句，且与成败无关。CommitText 靠它区分「宿主没给锁、会话一次都没执行」
        // 与「执行了但没写成」，两者的善后相反。见 WasExecuted()。
        _executed = TRUE;

        // 恢复中（宿主过早终止过组合）：同一会话里删残留，再不带组合地插入上屏文字。Chromium 只要见到
        // 本次编辑里开过组合（含 InsertTextAtSelection 默认建的组合），就先把文字当组合送给页面再提交；
        // 受控输入框在两步之间回写 value 会打断组合，上屏文字重复一份。不开组合时它只发一次删除加一次
        // insertText，与微软拼音相同。
        BOOL recoverInsert = _recoverDropped;
        if (_pComposition == nullptr && _pTextService->HasRecoverRecord())
        {
            _pTextService->_DropRecoverLeftover(ec, _pContext);
            recoverInsert = TRUE;
        }
        if (_pComposition == nullptr && recoverInsert && !_paste)
        {
            // 只对 Chromium 不建默认组合；别的宿主照旧（默认组合让 IMM32 桥接的程序仍按 IME 上屏看待）。
            const DWORD flags = CCaretEditSession::IsChromiumContext(_pContext) ? TF_IAS_NO_DEFAULT_COMPOSITION : 0;
            HRESULT hr = _InsertAtSelection(ec, _text, flags);
            WIND_LOG_DEBUG_FMT(L"Recover: commit without composition hr=0x%08X\n", hr);
            if (FAILED(hr))
                return hr;
            _success = TRUE;
            return S_OK;
        }

        // 标准 IME 提交语义：把上屏文字 SetText 到组合 range 后再 EndComposition，宿主在 OnEndComposition
        // 看到的就是上屏文字（同微软拼音/搜狗/Rime）。「清空组合 + 旁路 InsertTextAtSelection」会被跟打/统计类
        // 应用判为非 IME 输入。原子性由单一编辑会话保证。
        //
        // 已知宿主缺陷，本层刻意不规避：上屏文本以换行结尾时，WPS 会把之后新的组合区渲染成带下划线的组合态
        // 且重绘不消失（残留的 display attribute，文档格式本身干净）。我们的流程（SetText + 清 GUID_PROP_ATTRIBUTE
        // + EndComposition）与 Weasel / SampleIME 一致。已证伪的规避：SetText 前后两次 Clear（均 S_OK、区间精确，
        // WPS 不采纳）、摘掉末尾换行后 InsertTextAtSelection 补回。
        if (_pComposition != nullptr)
        {
            BOOL committedViaComposition = FALSE;
            ITfRange* pRange = nullptr;
            if (SUCCEEDED(_pComposition->GetRange(&pRange)))
            {
                // EndComposition 之前清掉组合态的显示属性（下划线挂在 GUID_PROP_ATTRIBUTE 上，由 _SetDisplayAttribute
                // 写入）：TSF 不保证宿主在 EndComposition 时自行清除，SampleIME / Weasel 同样显式 Clear。
                // SetText 前后各清一次：前后 range 覆盖的是两段不同文本。都不可挪到 Collapse 之后（那时长度为 0）。
                ITfProperty* pDisplayAttrProp = nullptr;
                HRESULT hrProp = _pContext->GetProperty(GUID_PROP_ATTRIBUTE, &pDisplayAttrProp);
                if (FAILED(hrProp) || pDisplayAttrProp == nullptr)
                {
                    WIND_LOG_WARN_FMT(L"CCommitTextEditSession: GetProperty(GUID_PROP_ATTRIBUTE) failed hr=0x%08X, prop=%p\n",
                                      hrProp, (void*)pDisplayAttrProp);
                }
                HRESULT hrClearBefore = E_FAIL;
                if (pDisplayAttrProp != nullptr)
                    hrClearBefore = pDisplayAttrProp->Clear(ec, pRange);

                // 把组合 range 内容替换为最终文字；_text 为空则等价于清空。粘贴模式只清空，正文随后经 Ctrl+V 送达。
                //
                // flags 必须是 0，不能用 TF_ST_CORRECTION：后者要求宿主保留原文本的格式，而原文本正是带组合下划线的
                // preedit，宿主会把下划线作为真实字符格式写进文档，之后新输入的字全都继承，TSF 侧 Clear 也清不掉。
                // 上屏是定稿不是更正（Weasel 同样用 0）。
                pRange->SetText(ec, 0, _paste ? L"" : _text.c_str(), _paste ? 0 : (LONG)_text.length());

                // SetText 之后重新取一次组合 range 再 Clear：上面那个 pRange 是替换之前取的，替换后是否仍覆盖
                // 新文本属于宿主实现细节。
                HRESULT hrClearAfter = E_FAIL;
                if (pDisplayAttrProp != nullptr)
                {
                    ITfRange* pRangeFresh = nullptr;
                    if (SUCCEEDED(_pComposition->GetRange(&pRangeFresh)) && pRangeFresh != nullptr)
                    {
                        hrClearAfter = pDisplayAttrProp->Clear(ec, pRangeFresh);
                        pRangeFresh->Release();
                    }
                    else
                    {
                        hrClearAfter = pDisplayAttrProp->Clear(ec, pRange);
                    }
                    pDisplayAttrProp->Release();
                    WIND_LOG_DEBUG_FMT(L"CCommitTextEditSession: display attr cleared before=0x%08X after=0x%08X\n",
                                       hrClearBefore, hrClearAfter);
                }
                // 光标定位到插入文本之后, 作为后续输入起点。
                pRange->Collapse(ec, TF_ANCHOR_END);
                TF_SELECTION sel = {};
                sel.range = pRange;
                sel.style.ase = TF_AE_NONE;
                sel.style.fInterimChar = FALSE;
                _pContext->SetSelection(ec, 1, &sel);
                pRange->Release();
                committedViaComposition = TRUE;
            }
            _pComposition->EndComposition(ec);
            _pComposition->Release();
            _pComposition = nullptr;
            if (committedViaComposition && !_paste)
            {
                _success = TRUE;
                WIND_LOG_DEBUG(L"CCommitTextEditSession: SetText + EndComposition committed\n");
                wind::caret::PostImmCandidateRefreshIfHost(L"上屏"); // 让宿主按上屏后的光标刷新位置
                return S_OK;
            }
            // GetRange 失败 (极少): composition 已结束但文字未写入, fallthrough
            // 到 InsertTextAtSelection 兜底, 避免静默丢字。
            if (!committedViaComposition)
                WIND_LOG_DEBUG(L"CCommitTextEditSession: GetRange failed, falling back to InsertTextAtSelection\n");
        }

        // 粘贴模式：组合已在上面清空并结束，这里才注入 Ctrl+V。注入的按键排在宿主输入队列里，
        // 要等本会话（同步：当前按键处理；异步：TSF 授锁时）返回后才被处理，届时组合早已结束。
        if (_paste)
        {
            if (_pTextService->_PasteTextViaClipboard(_text))
            {
                _success = TRUE;
                return S_OK;
            }
            WIND_LOG_WARN_FMT(L"CCommitTextEditSession: paste failed, inserting at selection instead, textLen=%zu\n",
                              _text.length());
        }

        // 无 active composition 的回退路径 (鼠标上屏 / 上述 GetRange 失败兜底): 走
        // InsertTextAtSelection。此路本就没有 composition, 换行不必摘出, 整串照发。
        {
            HRESULT hr = _InsertAtSelection(ec, _text);
            if (FAILED(hr))
                return hr;
        }

        _success = TRUE;
        WIND_LOG_DEBUG(L"CCommitTextEditSession: Text committed via InsertTextAtSelection fallback\n");
        return S_OK;
    }

    BOOL GetSuccess() const { return _success; }
    void SetRecoverDropped() { _recoverDropped = TRUE; }


    // 组合是否仍在本对象手里。DoEditSession 跑到组合分支后无论成败都会 EndComposition + Release + 置空。
    // 不要拿它当「会话是否执行过」的判据：无组合的提交里它从一开始就是 FALSE，用 WasExecuted()。
    BOOL HasPendingComposition() const { return _pComposition != nullptr; }

    // DoEditSession 是否真的被执行过，与成败无关。宿主拒发同步锁时据此退异步重试，而不是落 SendInput：
    // SendInput 受 UIPI（被拦时静默失败）与物理修饰键污染，非 TSF-aware 宿主还会把注入字符当虚拟键码解释。
    BOOL WasExecuted() const { return _executed; }

private:
    LONG _refCount;
    CTextService* _pTextService;
    ITfContext* _pContext;
    ITfComposition* _pComposition;  // Owned composition pointer
    std::wstring _text;
    BOOL _paste;
    BOOL _success;
    BOOL _executed;  // DoEditSession 跑过没有（与成败无关）；见 WasExecuted()
    BOOL _recoverDropped = FALSE; // 残留已在上屏前单独一把锁里删掉（见 CommitText），照恢复中上屏处理
};

// EditSession：把光标前 count 个字符替换为 text（智能符号纠错替换）。
// 在单一同步 EditSession 内完成：取选区光标 → 起点回退 count 字符 → SetText 覆盖。
// 同步、原子、不受输入队列时序与修饰键状态影响——优于"合成退格 + 提交"组合。
class CReplaceBackwardEditSession : public ITfEditSession
{
public:
    CReplaceBackwardEditSession(CTextService* pTextService, ITfContext* pContext,
                                int count, const std::wstring& text)
        : _refCount(1), _pTextService(pTextService), _pContext(pContext),
          _count(count), _text(text), _success(FALSE)
    {
        _pTextService->AddRef();
        _pContext->AddRef();
    }

    ~CReplaceBackwardEditSession()
    {
        _pTextService->Release();
        _pContext->Release();
    }

    // IUnknown
    STDMETHODIMP QueryInterface(REFIID riid, void** ppvObj)
    {
        if (ppvObj == nullptr) return E_INVALIDARG;
        *ppvObj = nullptr;
        if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITfEditSession))
        {
            *ppvObj = (ITfEditSession*)this;
            AddRef();
            return S_OK;
        }
        return E_NOINTERFACE;
    }

    STDMETHODIMP_(ULONG) AddRef() { return InterlockedIncrement(&_refCount); }
    STDMETHODIMP_(ULONG) Release()
    {
        LONG cr = InterlockedDecrement(&_refCount);
        if (cr == 0) delete this;
        return cr;
    }

    // ITfEditSession
    STDMETHODIMP DoEditSession(TfEditCookie ec)
    {
        TF_SELECTION sel = {};
        ULONG fetched = 0;
        if (FAILED(_pContext->GetSelection(ec, TF_DEFAULT_SELECTION, 1, &sel, &fetched)) ||
            fetched == 0 || sel.range == nullptr)
        {
            WIND_LOG_DEBUG(L"CReplaceBackwardEditSession: GetSelection failed\n");
            return E_FAIL;
        }

        ITfRange* pRange = sel.range; // 取得所有权，末尾 Release

        // 先折叠到光标（选区末端），确保 range 锚定在光标处，再向前覆盖 count 个字符，
        // 即便此刻存在非空选区也只替换"光标前 count 字符"。
        pRange->Collapse(ec, TF_ANCHOR_END);

        LONG shifted = 0;
        HRESULT hr = pRange->ShiftStart(ec, -_count, &shifted, nullptr);
        if (FAILED(hr) || shifted != -_count)
        {
            // 无法回退足够字符（行首 / 不可编辑等）：放弃，交由调用方走 SendInput 兜底。
            WIND_LOG_DEBUG_FMT(L"CReplaceBackwardEditSession: ShiftStart failed hr=0x%08X shifted=%ld\n", hr, shifted);
            pRange->Release();
            return E_FAIL;
        }

        hr = pRange->SetText(ec, TF_ST_CORRECTION, _text.c_str(), (LONG)_text.length());
        if (FAILED(hr))
        {
            WIND_LOG_DEBUG_FMT(L"CReplaceBackwardEditSession: SetText failed hr=0x%08X\n", hr);
            pRange->Release();
            return hr;
        }

        // 诊断用：SetText 报成功后回读同一 range 核对内容。Chromium/Qt 内嵌 TSFTextStore 可能报 S_OK 而渲染与
        // TSF 模型不一致，此时回读仍「看起来正确」，用来区分是我们 range 算错还是宿主渲染层的问题。
        {
            WCHAR readback[64] = {};
            ULONG readbackLen = 0;
            HRESULT hrRead = pRange->GetText(ec, 0, readback, 63, &readbackLen);
            std::wstring readbackStr(readback, readbackLen);
            WIND_LOG_DEBUG_FMT(L"CReplaceBackwardEditSession: readback hr=0x%08X text='%s' expected='%s' match=%d\n",
                               hrRead, readbackStr.c_str(), _text.c_str(),
                               (SUCCEEDED(hrRead) && readbackStr == _text) ? 1 : 0);
        }

        // 光标定位到替换文本之后。
        pRange->Collapse(ec, TF_ANCHOR_END);
        TF_SELECTION newSel = {};
        newSel.range = pRange;
        newSel.style.ase = TF_AE_NONE;
        newSel.style.fInterimChar = FALSE;
        _pContext->SetSelection(ec, 1, &newSel);

        pRange->Release();
        _success = TRUE;
        WIND_LOG_DEBUG(L"CReplaceBackwardEditSession: range replace committed\n");
        return S_OK;
    }

    BOOL GetSuccess() const { return _success; }

private:
    LONG _refCount;
    CTextService* _pTextService;
    ITfContext* _pContext;
    int _count;
    std::wstring _text;
    BOOL _success;
};

// EditSession for updating composition
class CUpdateCompositionEditSession : public ITfEditSession
{
public:
    // placeholder：_text 为空时兜底写入的占位（core 按应用下发：空格 / ZWSP / 盲文空白，见 CompositionPlaceholder.h）。
    CUpdateCompositionEditSession(CTextService* pTextService, ITfContext* pContext, const std::wstring& text, int caretPos = -1, BOOL noUnderline = FALSE,
                                  const wchar_t* placeholder = wind::placeholder::kSpacePlaceholder)
        : _refCount(1), _pTextService(pTextService), _pContext(pContext), _text(text), _caretPos(caretPos), _noUnderline(noUnderline),
          _placeholder(placeholder)
    {
        _pTextService->AddRef();
        _pContext->AddRef();
    }

    ~CUpdateCompositionEditSession()
    {
        _pTextService->Release();
        _pContext->Release();
    }

    // IUnknown
    STDMETHODIMP QueryInterface(REFIID riid, void** ppvObj)
    {
        if (ppvObj == nullptr) return E_INVALIDARG;
        *ppvObj = nullptr;
        if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITfEditSession))
        {
            *ppvObj = (ITfEditSession*)this;
            AddRef();
            return S_OK;
        }
        return E_NOINTERFACE;
    }

    STDMETHODIMP_(ULONG) AddRef()
    {
        return InterlockedIncrement(&_refCount);
    }

    STDMETHODIMP_(ULONG) Release()
    {
        LONG cr = InterlockedDecrement(&_refCount);
        if (cr == 0) delete this;
        return cr;
    }

    // ITfEditSession
    STDMETHODIMP DoEditSession(TfEditCookie ec)
    {
        HRESULT hr = S_OK;

        // 1. If no composition exists, start one
        if (_pTextService->_pComposition == nullptr)
        {
            // 恢复中：先删掉上一次被宿主终止时留下的编码，再用完整缓冲在原处重开组合。
            _pTextService->_DropRecoverLeftover(ec, _pContext);

            // Get current selection (cursor position) to start composition there
            TF_SELECTION tfSelection;
            ULONG cFetched;
            if (FAILED(_pContext->GetSelection(ec, TF_DEFAULT_SELECTION, 1, &tfSelection, &cFetched)) || cFetched != 1)
            {
                return E_FAIL;
            }

            // 不对 selection 做任何加工：StartComposition 拿到的就是当前 selection，用户选中着文字时它非退化。
            // 「空文本时折叠成插入点再建组合」不可行：对组合做任何操作都可能清掉选中内容，且因宿主而异；
            // 故开关关掉的模式根本不发 UpdateComposition（见 `input.caret.*`）。
            // 但 selection 并不安全：开关为 true（出厂）时协调器发来占位空格，下面的 SetText 会把覆盖着选中文字的
            // range 整体替换掉（TSF 标准语义，与宿主无关），出厂配置下选中文字依然会被吃掉。

            ITfContextComposition* pContextComp = nullptr;
            if (FAILED(_pContext->QueryInterface(IID_ITfContextComposition, (void**)&pContextComp)))
            {
                tfSelection.range->Release();
                return E_FAIL;
            }

            // Start composition
            hr = pContextComp->StartComposition(
                ec,
                tfSelection.range,
                (ITfCompositionSink*)_pTextService,
                &_pTextService->_pComposition);

            pContextComp->Release();
            tfSelection.range->Release();

            if (FAILED(hr) || _pTextService->_pComposition == nullptr)
            {
                WIND_LOG_ERROR(L"StartComposition failed\n");
                return E_FAIL;
            }
            WIND_LOG_DEBUG(L"StartComposition succeeded\n");
            wind::caret::PostImmOpenCandidateIfHost(L"组合开始");
            // Weasel 模式：标记 composition 刚刚创建。下一次 SendCaretPositionUpdate
            // 不会立即发 IPC，而是等 OnLayoutChange 提供 reflow 后的权威坐标，
            // 50ms timer 兜底（应对不发 OnLayoutChange 的应用，如某些 CUAS 路径）。
            _pTextService->_compositionJustStarted = TRUE;
            _pTextService->_firstShowProbeSeq = 0; // 新组合开始：试探采样计数归零
            // 「宿主没有插入点可报」是上一轮组合的判决，新组合重新判：同一进程里画布退化而面板输入框正常，
            // 判决跨组合残留会把面板里正确的坐标也按默认位置处理。这是该闩唯一的复位点：读闩处的
            // LatchApplies 只挡「组合已经没了」，拦不住新组合建好时上一轮的残留判决。
            _pTextService->_caretDefaultPosLatched = FALSE;
        }

        // 2. Get range from composition
        ITfRange* pRange = nullptr;
        if (FAILED(_pTextService->_pComposition->GetRange(&pRange)))
        {
            return E_FAIL;
        }

        // 3. Set text
        // 空文本 ⇒ 补一个占位字符把 range 撑开，覆盖所有路径的全局兜底，别删：WPS 等对零长度 range 回退化矩形，
        // 候选窗失去坐标来源；临拼 / 特殊 / 生僻字经直达热键进入、`commit_then_new_composition(text, "")` 等路径
        // 都发空组合区。`[input.caret]` 的逐模式开关不靠删它生效：开关为 false 时协调器根本不发 UpdateComposition。
        // 插入点放在占位之前（step 5），视觉上无偏移；EndComposition/CommitText 时清掉。
        // 占位字符由调用方传入（_placeholder）：core 按 compat 规则下发，默认空格，浏览器出厂 ZWSP
        // （受控 `<input>` 会 trim 组合中的 value，见 CompositionPlaceholder.h）。
        BOOL isPlaceholder = _text.empty();
        const wchar_t* textPtr = isPlaceholder ? _placeholder : _text.c_str();
        LONG textLen = isPlaceholder ? 1 : (LONG)_text.length();

        hr = pRange->SetText(ec, TF_ST_CORRECTION, textPtr, textLen);

        if (SUCCEEDED(hr))
        {
            _pTextService->_lastCompositionWriteTick = GetTickCount64();

            // 4. Apply display attribute to show underline
            // 占位不加下划线；服务端自己发来的占位（非嵌入模式的编码替身、联想态等）同样不加：有宽度的盲文空白
            // （U+2800）加上下划线后在钉钉表格里会显示成一小段「_」。
            if (!isPlaceholder && !wind::placeholder::IsPlaceholderText(_text))
                _SetDisplayAttribute(ec, pRange);

            // 5. Position cursor within composition
            ITfRange* pRangeForSel = nullptr;
            if (SUCCEEDED(_pTextService->_pComposition->GetRange(&pRangeForSel)))
            {
                if (isPlaceholder && textLen > 0)
                {
                    // Placeholder mode: position cursor BEFORE the placeholder character.
                    // This way GetTextExt returns valid coordinates at the original cursor
                    // position, while the placeholder space appears after it (like Bingling IME).
                    pRangeForSel->Collapse(ec, TF_ANCHOR_START);
                }
                else if (_caretPos >= 0 && _caretPos < (int)_text.length())
                {
                    // Move the range start to the caret position, then collapse to start
                    // This positions the cursor at the specified offset within the composition
                    LONG shifted = 0;
                    pRangeForSel->Collapse(ec, TF_ANCHOR_START);
                    pRangeForSel->ShiftEnd(ec, (LONG)_caretPos, &shifted, nullptr);
                    pRangeForSel->ShiftStart(ec, (LONG)_caretPos, &shifted, nullptr);
                }
                else
                {
                    // Default: cursor at end of composition
                    pRangeForSel->Collapse(ec, TF_ANCHOR_END);
                }

                TF_SELECTION sel = {};
                sel.range = pRangeForSel;
                sel.style.ase = TF_AE_NONE;
                sel.style.fInterimChar = FALSE;
                _pContext->SetSelection(ec, 1, &sel);

                pRangeForSel->Release();
            }
        }

        pRange->Release();

        // 在这个有效编辑会话里缓存 caret 位置：WebView 类宿主会拒绝 TF_INVALID_COOKIE 的独立 CCaretEditSession。
        // 组合刚创建时跳过：宿主尚未 reflow，缓存旧坐标会让 timer 兜底取到陈旧值，等 timer/OnLayoutChange 现查。
        if (SUCCEEDED(hr) && !_pTextService->_compositionJustStarted)
        {
            _CacheCaretPosition(ec);
        }

        return hr;
    }

private:
    int _caretPos;         // Cursor position within composition (-1 = at end)
    BOOL _noUnderline;     // 整段不设下划线属性（智能符号 HoldComposition 观感对齐已上屏）
    const wchar_t* _placeholder; // 空文本兜底占位（静态字符串，长度恒为 1）

    void _CacheCaretPosition(TfEditCookie ec)
    {
        ITfContextView* pContextView = nullptr;
        if (FAILED(_pContext->GetActiveView(&pContextView)) || pContextView == nullptr)
            return;

        // Get current caret position (selection)
        TF_SELECTION sel[1];
        ULONG fetched = 0;
        if (SUCCEEDED(_pContext->GetSelection(ec, TF_DEFAULT_SELECTION, 1, sel, &fetched)) && fetched > 0 && sel[0].range != nullptr)
        {
            RECT caretRect = {};
            BOOL clipped = FALSE;
            if (SUCCEEDED(pContextView->GetTextExt(ec, sel[0].range, &caretRect, &clipped)))
            {
                // 跳过退化矩形（h=0，WPS 首次组合 reflow 前常见）。缓存仅供 timer 兜底；OnLayoutChange 路径会清缓存现查。
                LONG h = caretRect.bottom - caretRect.top;
                // 同 GetCaretPositionFromTSF：跳过退化矩形(h<=0)与越界坐标，避免把不可信坐标
                // 缓存后被后续路径取用。越界时不缓存，留待 fallback 链重新求解。
                // 越界判据同样已从「前台窗口」放宽为「所有显示器」，理由见那里的注释。
                if (h > 0 && !IsScreenPointOutsideAllMonitors(caretRect.left, caretRect.top)
                    && !CCaretEditSession::IsOutsideViewExt(pContextView, caretRect))
                {
                    _pTextService->_cachedCaretRect = caretRect;
                    _pTextService->_hasCachedCaretPos = TRUE;
                }
            }
            sel[0].range->Release();
        }

        // Get composition start position
        if (_pTextService->_pComposition != nullptr)
        {
            ITfRange* pCompRange = nullptr;
            if (SUCCEEDED(_pTextService->_pComposition->GetRange(&pCompRange)) && pCompRange != nullptr)
            {
                ITfRange* pStartRange = nullptr;
                if (SUCCEEDED(pCompRange->Clone(&pStartRange)) && pStartRange != nullptr)
                {
                    pStartRange->Collapse(ec, TF_ANCHOR_START);
                    // 组合头部若有待提交前缀（顶码聚合），上报的组合起点须偏移到
                    // 余码段起点——候选窗锚点跟随余码而非已顶出的文字。
                    LONG prefixLen = (LONG)_pTextService->GetPendingCommitPrefixLength();
                    if (prefixLen > 0)
                    {
                        LONG moved = 0;
                        pStartRange->ShiftEnd(ec, prefixLen, &moved, nullptr);
                        pStartRange->ShiftStart(ec, prefixLen, &moved, nullptr);
                    }
                    RECT compStartRect = {};
                    BOOL clipped = FALSE;
                    if (SUCCEEDED(pContextView->GetTextExt(ec, pStartRange, &compStartRect, &clipped)))
                    {
                        LONG compH = compStartRect.bottom - compStartRect.top;
                        if (compH > 0)
                        {
                            _pTextService->_cachedCompStartRect = compStartRect;
                            _pTextService->_hasCachedCompStartPos = TRUE;
                        }
                    }
                    pStartRange->Release();
                }

                // 整个组合 range 的包围矩形，与上面折叠版取自同一次会话、同一 cookie。这条缓存路径（src=TSF_CACHED）
                // 是上报的主力，只接异步路径会让矩形几乎永远发不出去。
                RECT compRect = {};
                BOOL clippedRect = FALSE;
                if (SUCCEEDED(pContextView->GetTextExt(ec, pCompRange, &compRect, &clippedRect)) &&
                    compRect.bottom > compRect.top)
                {
                    _pTextService->_cachedCompRect = compRect;
                    _pTextService->_hasCachedCompRect = TRUE;
                }
                pCompRange->Release();
            }
        }

        pContextView->Release();
    }

    void _SetDisplayAttribute(TfEditCookie ec, ITfRange* pRange)
    {
        // Get the display attribute atom from TextService
        TfGuidAtom gaDisplayAttr = _pTextService->GetDisplayAttributeInputAtom();
        if (gaDisplayAttr == TF_INVALID_GUIDATOM)
        {
            WIND_LOG_DEBUG(L"Display attribute not initialized\n");
            return;
        }

        // Get ITfProperty for display attribute
        ITfProperty* pDisplayAttrProp = nullptr;
        if (FAILED(_pContext->GetProperty(GUID_PROP_ATTRIBUTE, &pDisplayAttrProp)))
        {
            WIND_LOG_DEBUG(L"Failed to get GUID_PROP_ATTRIBUTE property\n");
            return;
        }

        VARIANT var;
        var.vt = VT_I4;
        var.lVal = gaDisplayAttr;

        // 分段显示属性（对齐微软 IME）：组合头部的待提交前缀（顶码已顶出的字）
        // 不带下划线——先 Clear 整段，再只对余码段 SetValue。前缀为 0 时即整段。
        pDisplayAttrProp->Clear(ec, pRange);

        // 整段无下划线模式（智能符号 HoldComposition）：中文符号留在组合态等待
        // press2 替换/超时提交，但观感上应与已上屏文本一致——Clear 后不再设值。
        if (_noUnderline)
        {
            WIND_LOG_DEBUG(L"Display attribute cleared (noUnderline mode)\n");
            pDisplayAttrProp->Release();
            return;
        }

        LONG prefixLen = (LONG)_pTextService->GetPendingCommitPrefixLength();
        ITfRange* pAttrRange = nullptr;
        if (prefixLen > 0)
        {
            if (SUCCEEDED(pRange->Clone(&pAttrRange)) && pAttrRange != nullptr)
            {
                LONG moved = 0;
                pAttrRange->ShiftStart(ec, prefixLen, &moved, nullptr);
            }
        }
        else
        {
            pAttrRange = pRange;
            pAttrRange->AddRef();
        }

        HRESULT hr = E_FAIL;
        if (pAttrRange != nullptr)
        {
            hr = pDisplayAttrProp->SetValue(ec, pAttrRange, &var);
            pAttrRange->Release();
        }
        if (FAILED(hr))
        {
            WIND_LOG_DEBUG(L"Failed to set display attribute\n");
        }
        else
        {
            WIND_LOG_DEBUG_FMT(L"Display attribute set (prefixLen=%ld)\n", prefixLen);
        }

        pDisplayAttrProp->Release();
    }

private:
    LONG _refCount;
    CTextService* _pTextService;
    ITfContext* _pContext;
    std::wstring _text;
};

static const LONG DEFAULT_CARET_HEIGHT = WIND_DEFAULT_CARET_HEIGHT;

CTextService::CTextService()
    : _refCount(1)
    , _pThreadMgr(nullptr)
    , _tfClientId(TF_CLIENTID_NULL)
    , _dwThreadMgrEventSinkCookie(TF_INVALID_COOKIE)
    , _dwThreadFocusSinkCookie(TF_INVALID_COOKIE)
    , _uiElementId((DWORD)-1)
    , _uiElementShown(FALSE)
    , _pUIElementMgr(nullptr)
    , _uiHostDraws(FALSE)
    , _uiLessThread(FALSE)
    , _uiElementStateSent(-1)
    , _uiUpdatedFlags(0)
    , _uiSnapshotDirty(FALSE)
    , _uiHostReadsCandidates(FALSE)
    , _pSourceSingle(nullptr)
    , _funcProviderRegistered(FALSE)
    , _hHotkeyWnd(nullptr)
    , _hotkeyWndClass(0)
    , _hotkeysActive(FALSE)
    , _candidatesVisible(FALSE)
    , _addWordHotkeysActive(FALSE)
    , _focusIsPassword(false)
    , _focusInputScopeMask(0)
    , _passwordSuppressEnabled(TRUE)  // 默认开，与 core 的 password_suppress_enabled 初值一致
    , _compositionPlaceholderKind(0)  // 默认空格（历史行为），core 握手时按本进程 compat 规则推
    , _multilineCommitMode(0)         // 默认 commit，core 握手时按本进程 compat 规则推
    , _diagSnapshotEnabled(FALSE)     // 默认关，与 core 的 input_diag_hud_visible 初值一致
    , _titleMatchEnabled(0)           // 默认关：core 握手时按「本进程可能命中标题规则」推
    , _focusSentTitleBlind(FALSE)
    , _hasThreadFocus(FALSE)
    , _isProcessForeground(FALSE)
    , _activateFlags(0)
    , _pKeyEventSink(nullptr)
    , _pIPCClient(nullptr)
    , _pLangBarItemButton(nullptr)
    , _pHotkeyManager(nullptr)
    , _pHostWindow{}
    , _bChineseMode(TRUE)
    , _bFullWidth(FALSE)
    , _bChinesePunct(TRUE)  // 出厂中文标点态；首个 status 推送会校正
    , _bSoftKeyboard(FALSE)
    , _bSoftKeyboardKeys(FALSE)
    , _lastCapsKeyTick(0)
    , _lastActivateTick(0)
    , _focusSessionId(0)
    , _hasFocus(FALSE)
    , _hasTextInputContext(FALSE)
    , _pLastActiveDocMgr(nullptr)
    , _pLastFocusedDocMgr(nullptr)
    , _focusLostSent(FALSE)
    , _editCtxReported(FALSE)
    , _pComposition(nullptr)
    , _hasCachedCaretPos(FALSE)
    , _hasCachedCompStartPos(FALSE)
    , _hasCachedCompRect(FALSE)
    , _compositionJustStarted(FALSE)
    , _needsFocusRecovery(FALSE)
    , _lastFocusCaretX(0)
    , _lastFocusCaretY(0)
    , _lastFocusCaretHeight(DEFAULT_CARET_HEIGHT)
    , _hasLastKnownCaretPos(FALSE)
    , _hostGaveTsfCaret(FALSE)
    , _lastKnownCaretX(0)
    , _lastKnownCaretY(0)
    , _lastKnownCaretHeight(DEFAULT_CARET_HEIGHT)
    , _gaDisplayAttributeInput(TF_INVALID_GUIDATOM)
    , _dwLayoutSinkCookie(TF_INVALID_COOKIE)
    , _pLayoutSinkContext(nullptr)
    , _dwTextEditSinkCookie(TF_INVALID_COOKIE)
    , _pTextEditSinkContext(nullptr)
    , _cachedPrevChar(0)
    , _dwOpenCloseSinkCookie(TF_INVALID_COOKIE)
    , _bInCompartmentChange(FALSE)
    , _bKeyboardDisabled(FALSE)
    , _dwKeyboardDisabledSinkCookie(TF_INVALID_COOKIE)
    , _dwConversionSinkCookie(TF_INVALID_COOKIE)
    , _bInConversionChange(FALSE)
{
    ZeroMemory(&_cachedCaretRect, sizeof(_cachedCaretRect));
    ZeroMemory(&_cachedCompStartRect, sizeof(_cachedCompStartRect));
    ZeroMemory(&_cachedCompRect, sizeof(_cachedCompRect));
    DllAddRef();
}

CTextService::~CTextService()
{
    _ClearRecoverRecord(); // 兜底：正常已在 Deactivate 里解除
    if (_pLastActiveDocMgr != nullptr)
    {
        _pLastActiveDocMgr->Release();
        _pLastActiveDocMgr = nullptr;
    }
    if (_pLastFocusedDocMgr != nullptr)
    {
        _pLastFocusedDocMgr->Release();
        _pLastFocusedDocMgr = nullptr;
    }
    DllRelease();
}

STDAPI CTextService::QueryInterface(REFIID riid, void** ppvObj)
{
    if (ppvObj == nullptr)
        return E_INVALIDARG;

    *ppvObj = nullptr;

    if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITfTextInputProcessor))
    {
        *ppvObj = (ITfTextInputProcessor*)this;
    }
    else if (IsEqualIID(riid, IID_ITfTextInputProcessorEx))
    {
        *ppvObj = (ITfTextInputProcessorEx*)this;
    }
    else if (IsEqualIID(riid, IID_ITfThreadMgrEventSink))
    {
        *ppvObj = (ITfThreadMgrEventSink*)this;
    }
    else if (IsEqualIID(riid, IID_ITfCompositionSink))
    {
        *ppvObj = (ITfCompositionSink*)this;
    }
    else if (IsEqualIID(riid, IID_ITfDisplayAttributeProvider))
    {
        *ppvObj = (ITfDisplayAttributeProvider*)this;
    }
    else if (IsEqualIID(riid, IID_ITfTextLayoutSink))
    {
        *ppvObj = (ITfTextLayoutSink*)this;
    }
    else if (IsEqualIID(riid, IID_ITfTextEditSink))
    {
        *ppvObj = (ITfTextEditSink*)this;
    }
    else if (IsEqualIID(riid, IID_ITfCompartmentEventSink))
    {
        *ppvObj = (ITfCompartmentEventSink*)this;
    }
    else if (IsEqualIID(riid, IID_ITfThreadFocusSink))
    {
        *ppvObj = (ITfThreadFocusSink*)this;
    }
    else if (IsEqualIID(riid, IID_ITfUIElement))
    {
        *ppvObj = (ITfUIElement*)(ITfCandidateListUIElementBehavior*)this;
    }
    else if (IsEqualIID(riid, IID_ITfCandidateListUIElement))
    {
        *ppvObj = (ITfCandidateListUIElement*)(ITfCandidateListUIElementBehavior*)this;
    }
    else if (IsEqualIID(riid, IID_ITfCandidateListUIElementBehavior))
    {
        *ppvObj = (ITfCandidateListUIElementBehavior*)this;
    }
    else if (IsEqualIID(riid, __uuidof(ITfIntegratableCandidateListUIElement)))
    {
        // 独立基类（不经 ITfUIElement），直接转型。见下方该接口段的说明。
        *ppvObj = (ITfIntegratableCandidateListUIElement*)this;
    }
    else if (IsEqualIID(riid, IID_ITfFunctionProvider))
    {
        *ppvObj = (ITfFunctionProvider*)this;
    }

    if (*ppvObj)
    {
        AddRef();
        return S_OK;
    }

    // 诊断：被查询但不支持的 riid——msctf/宿主探测接口失败的序列据此可见。
    // 只在未匹配分支打，命中路径不刷屏。
    {
        WCHAR szIid[64] = {};
        StringFromGUID2(riid, szIid, ARRAYSIZE(szIid));
        WIND_LOG_DEBUG_FMT(L"TextService::QueryInterface E_NOINTERFACE riid=%ls", szIid);
    }

    return E_NOINTERFACE;
}

STDAPI_(ULONG) CTextService::AddRef()
{
    return InterlockedIncrement(&_refCount);
}

STDAPI_(ULONG) CTextService::Release()
{
    LONG cr = InterlockedDecrement(&_refCount);

    if (cr == 0)
    {
        delete this;
    }

    return cr;
}

STDAPI CTextService::Activate(ITfThreadMgr* pThreadMgr, TfClientId tfClientId)
{
    return ActivateEx(pThreadMgr, tfClientId, 0);
}

STDAPI CTextService::ActivateEx(ITfThreadMgr* pThreadMgr, TfClientId tfClientId, DWORD dwFlags)
{
    WIND_LOG_INFO_FMT(L"TextService::ActivateEx called tfClientId=0x%08X dwFlags=0x%08X", tfClientId, dwFlags);

    // 诊断：读线程管理器的激活标志（TF_TMF_*：ACTIVATED/SECUREMODE/IMMERSIVE/CONSOLE 等），
    // 定位游戏宿主里激活被拒发生在哪一层。纯只读探测，失败只记日志，不影响主流程。
    {
        ITfThreadMgrEx* pThreadMgrEx = nullptr;
        HRESULT hrQI = pThreadMgr
            ? pThreadMgr->QueryInterface(IID_ITfThreadMgrEx, (void**)&pThreadMgrEx)
            : E_POINTER;
        if (SUCCEEDED(hrQI) && pThreadMgrEx)
        {
            DWORD dwActiveFlags = 0;
            HRESULT hrFlags = pThreadMgrEx->GetActiveFlags(&dwActiveFlags);
            if (SUCCEEDED(hrFlags))
                WIND_LOG_DEBUG_FMT(L"ActivateEx activeFlags=0x%08X", dwActiveFlags);
            else
                WIND_LOG_DEBUG_FMT(L"ActivateEx GetActiveFlags failed hr=0x%08X", hrFlags);
            pThreadMgrEx->Release();
        }
        else
        {
            WIND_LOG_DEBUG_FMT(L"ActivateEx QueryInterface(ITfThreadMgrEx) failed hr=0x%08X", hrQI);
        }
    }

    // 起表：激活后的 compartment 变化是系统初始化同步，不是用户操作。
    // 见 _lastActivateTick 注释与下方两处 OnChange 守卫。
    _lastActivateTick = GetTickCount64();

    _activateFlags = dwFlags;
    // UI-less 线程：宿主（游戏 / 全屏应用）声明只接受能交出 UI 控制权的 TIP。规范原话是
    // TIP 此时「已经知道」该线程不要它的 UI——故一激活就报给服务端，让候选窗从第一个
    // 组合起就不弹（否则要等首次 BeginUIElement 回 FALSE 才收，先弹再收闪一帧）。
    _uiLessThread = (dwFlags & TF_TMAE_UIELEMENTENABLEDONLY) ? TRUE : FALSE;
    _uiElementStateSent = -1;
    if (_uiLessThread)
    {
        WIND_LOG_INFO(L"ActivateEx: TF_TMAE_UIELEMENTENABLEDONLY set — host is a UI-less thread, candidate UI will be host-drawn\n");
    }

    WindHostProcessInfo currentHost;
    if (WindQueryCurrentProcessInfo(&currentHost))
        WindLogHostProcessInfo(4, L"compat.activate.current_host", currentHost);
    else
        WIND_LOG_WARN(L"compat.activate.current_host query failed");

    _pThreadMgr = pThreadMgr;
    _pThreadMgr->AddRef();

    _tfClientId = tfClientId;

    // Initialize thread manager event sink
    if (!_InitThreadMgrEventSink())
    {
        WIND_LOG_ERROR(L"_InitThreadMgrEventSink failed\n");
        Deactivate();
        return E_FAIL;
    }
    WIND_LOG_INFO(L"ThreadMgrEventSink initialized\n");

    // Initialize IPC client
    if (!_InitIPCClient())
    {
        WIND_LOG_ERROR(L"_InitIPCClient failed\n");
        Deactivate();
        return E_FAIL;
    }
    WIND_LOG_INFO(L"IPCClient initialized\n");

    // Initialize hotkey manager with default config
    _pHotkeyManager = new CHotkeyManager();
    WIND_LOG_INFO(L"HotkeyManager initialized\n");

    // Initialize key event sink
    if (!_InitKeyEventSink())
    {
        WIND_LOG_ERROR(L"_InitKeyEventSink failed\n");
        Deactivate();
        return E_FAIL;
    }
    WIND_LOG_INFO(L"KeyEventSink initialized\n");

    // 初始化 RegisterHotKey 用的隐藏消息窗口（候选可见时动态注册系统级热键）
    if (!_InitHotkeyWindow())
    {
        WIND_LOG_WARN(L"_InitHotkeyWindow failed (non-fatal, Ctrl+digit may double-process in Chromium hosts)\n");
    }

    // Initialize display attribute
    if (!_InitDisplayAttribute())
    {
        WIND_LOG_WARN(L"_InitDisplayAttribute failed (non-fatal)\n");
        // Not fatal, continue without display attribute
    }
    else
    {
        WIND_LOG_INFO(L"DisplayAttribute initialized\n");
    }

    // Initialize language bar button
    if (!_InitLangBarButton())
    {
        WIND_LOG_WARN(L"_InitLangBarButton failed (non-fatal)\n");
        // Not fatal, continue without language bar button
    }
    else
    {
        WIND_LOG_INFO(L"LangBarButton initialized\n");
    }

    // Initialize compartment event sink for GUID_COMPARTMENT_KEYBOARD_OPENCLOSE
    // This allows us to respond when the system toggles the IME open/close state (e.g., Ctrl+Space)
    if (!_InitOpenCloseCompartment())
    {
        WIND_LOG_WARN(L"_InitOpenCloseCompartment failed (non-fatal)\n");
    }
    else
    {
        WIND_LOG_INFO(L"OpenCloseCompartment initialized\n");
    }

    // Initialize compartment event sink for GUID_COMPARTMENT_KEYBOARD_DISABLED
    // This allows us to stop intercepting keys when system disables keyboard input
    if (!_InitKeyboardDisabledCompartment())
    {
        WIND_LOG_WARN(L"_InitKeyboardDisabledCompartment failed (non-fatal)\n");
    }
    else
    {
        WIND_LOG_INFO(L"KeyboardDisabledCompartment initialized\n");
    }

    // Initialize INPUTMODE_CONVERSION compartment — exposes real Chinese/English mode
    // to external observers (KBLSwitch, Win11 taskbar). OPENCLOSE stays TRUE for our
    // internal OnTestKeyDown needs; this compartment carries the actual mode signal.
    if (!_InitConversionCompartment())
    {
        WIND_LOG_WARN(L"_InitConversionCompartment failed (non-fatal)\n");
    }
    else
    {
        WIND_LOG_INFO(L"ConversionCompartment initialized\n");
    }

    // Update caret position before notifying activation
    // This ensures status indicators appear at the correct position immediately
    SendCaretPositionUpdate();

    // 通知服务端 IME 已激活并全量同步状态；_DoFullStateSync 内含 lazy connect（首次安装后服务可能还在启动）。
    _DoFullStateSync(); // 内含 UIElement 状态上报（UI-less 线程从激活起就不弹窗）

    // 切换输入法不会重发 OnSetFocus：不在这里补读，服务端就沿用上一个实例留下的密码态，切走再切回后
    // 图标一直显「英」。必须排在 IMEActivated 之后：同一 bridge 线程按序处理，服务端先清旧态再收新读数。
    _ReportFocusInputStateOnActivate();

    // 焦点已在文本框里时，按「刚拿到焦点」走一遍完整焦点处理（同 Weasel）。只补密码读数不够：focus_gained
    // 才带顶层窗口类名 / 标题，新实例的 client_token 在服务端没有窗口上下文，按类名的应用兼容规则全不命中。
    // 顺带补上新实例从未做过的 layout / edit sink 挂接与 _hasFocus。服务端还没连上时由 _needsFocusRecovery 补发。
    // 只在本线程是前台线程时做：后台线程补发会冒充前台焦点，把活动客户端 / 初始模式归属推到后台进程。
    if (_pThreadMgr != nullptr && _hasThreadFocus)
    {
        ITfDocumentMgr* pDocMgrFocus = nullptr;
        if (SUCCEEDED(_pThreadMgr->GetFocus(&pDocMgrFocus)) && pDocMgrFocus != nullptr)
        {
            WIND_LOG_DEBUG(L"ActivateEx: focus already present, replaying OnSetFocus\n");
            OnSetFocus(pDocMgrFocus, nullptr);
            pDocMgrFocus->Release();
        }
    }

    // NOTE: Using synchronous IPC mode (no reader thread)
    // Reference: Weasel uses sync IPC with librime and it works well
    // The reader thread is not started - responses are received synchronously in OnKeyDown

    WIND_LOG_INFO(L"TextService::Activate completed successfully (sync IPC mode)\n");
    return S_OK;
}

STDAPI CTextService::Deactivate()
{
    WIND_LOG_INFO(L"TextService::Deactivate called\n");

    // 最先注销 compartment sinks：系统在输入法切换过程中会写 OPENCLOSE/CONVERSION
    // compartment，若 sink 仍挂着会被 OnChange 当作用户切换请求上报服务，
    // 把服务端权威中英状态污染成英文（表现为"切换输入法后默认变英文"）。
    _UninitOpenCloseCompartment();
    _UninitKeyboardDisabledCompartment();
    _UninitConversionCompartment();

    if (_pKeyEventSink != nullptr)
    {
        _pKeyEventSink->FlushEnglishStats();
    }

    // End any active composition before deactivating
    _DisarmRecover(L"deactivate");
    EndComposition();

    // 清理候选 UI 元素（必须在 ThreadMgr 释放之前）
    NotifyCandidatesVisibilityChanged(FALSE);
    // UI-less 记账归零：下次 ActivateEx 重新问宿主、重新报服务端。
    // 读取闩一并清掉——pid 会被系统复用，留着它会让下一个宿主一上来就被判成自绘。
    _uiHostDraws = FALSE;
    _uiHostReadsCandidates = FALSE;
    _uiSnapshot = UiElementSnapshot();
    _uiSnapshotDirty = FALSE;
    _uiElementStateSent = -1;

    // Unregister layout sink and edit sink
    _UnadviseTextLayoutSink();
    _UnadviseTextEditSink();

    // Release language bar button
    _UninitLangBarButton();

    // Release display attribute
    _UninitDisplayAttribute();

    // Release compartment event sinks
    _UninitOpenCloseCompartment();
    _UninitKeyboardDisabledCompartment();
    _UninitConversionCompartment();

    // 卸载 RegisterHotKey 隐藏窗口（必须在 KeyEventSink 释放之前，因为 WM_HOTKEY
    // 路径会回调 KeyEventSink）
    _UninitHotkeyWindow();

    // Release key event sink
    _UninitKeyEventSink();

    // 断开前通知服务端 IME 即将停用，让它立即隐藏工具栏。
    if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
    {
        WIND_LOG_DEBUG(L"Sending ime_deactivated to service\n");
        // SendIMEDeactivated is async (fire-and-forget), no response expected
        _pIPCClient->SendIMEDeactivated();
    }

    // Release host window (before IPC client, so shared memory is still valid during shutdown)
    _DestroyHostWindow();

    // Release IPC client
    _UninitIPCClient();

    // Release hotkey manager
    if (_pHotkeyManager != nullptr)
    {
        delete _pHotkeyManager;
        _pHotkeyManager = nullptr;
    }

    // Release thread manager event sink
    _UninitThreadMgrEventSink();

    // Release thread manager
    SafeRelease(_pThreadMgr);

    _tfClientId = TF_CLIENTID_NULL;

    WIND_LOG_INFO(L"TextService::Deactivate completed\n");
    return S_OK;
}

BOOL CTextService::_InitThreadMgrEventSink()
{
    ITfSource* pSource = nullptr;
    HRESULT hr = _pThreadMgr->QueryInterface(IID_ITfSource, (void**)&pSource);

    if (SUCCEEDED(hr))
    {
        hr = pSource->AdviseSink(IID_ITfThreadMgrEventSink,
                                 (ITfThreadMgrEventSink*)this,
                                 &_dwThreadMgrEventSinkCookie);

        // 并行 advise ITfThreadFocusSink — 线程级（进程 foreground）焦点通知。
        // 实现此接口让我们在 TSF 注册上看起来像"现代 IME"，让 Chromium / QQNT
        // 等宿主走完整 IME-first 调度路径而非 fallback，规避 Ctrl+数字 被双处理。
        HRESULT hrTf = pSource->AdviseSink(IID_ITfThreadFocusSink,
                                          (ITfThreadFocusSink*)this,
                                          &_dwThreadFocusSinkCookie);
        if (FAILED(hrTf))
        {
            WIND_LOG_WARN_FMT(L"AdviseSink(ITfThreadFocusSink) failed hr=0x%08X\n", (uint32_t)hrTf);
            _dwThreadFocusSinkCookie = TF_INVALID_COOKIE;
        }
        else
        {
            WIND_LOG_INFO(L"ITfThreadFocusSink advised\n");
        }

        pSource->Release();
    }

    // 缓存 ITfUIElementMgr，避免每次候选变化都 QueryInterface（NotifyCandidatesVisibilityChanged 使用）。
    if (_pUIElementMgr == nullptr)
    {
        HRESULT hrUI = _pThreadMgr->QueryInterface(IID_ITfUIElementMgr, (void**)&_pUIElementMgr);
        if (FAILED(hrUI))
        {
            _pUIElementMgr = nullptr;
        }
    }

    // 通过 ITfSourceSingle::AdviseSingleSink 把自己注册为该 IME 实例的 Function Provider。
    // 这是其它成熟 TSF IME 的标准做法，让 Chromium / QQNT 把我们识别为"现代 IME"，
    // 规避 Ctrl+数字 等热键被宿主同时处理。
    if (_pSourceSingle == nullptr)
    {
        HRESULT hrSS = _pThreadMgr->QueryInterface(IID_ITfSourceSingle, (void**)&_pSourceSingle);
        if (SUCCEEDED(hrSS) && _pSourceSingle != nullptr)
        {
            ITfFunctionProvider* pFP = static_cast<ITfFunctionProvider*>(this);
            HRESULT hrAdv = _pSourceSingle->AdviseSingleSink(_tfClientId, IID_ITfFunctionProvider, pFP);
            if (SUCCEEDED(hrAdv))
            {
                _funcProviderRegistered = TRUE;
                WIND_LOG_INFO(L"ITfFunctionProvider advised via ITfSourceSingle\n");
            }
            else
            {
                WIND_LOG_WARN_FMT(L"AdviseSingleSink(ITfFunctionProvider) failed hr=0x%08X\n", (uint32_t)hrAdv);
            }
        }
        else
        {
            WIND_LOG_WARN_FMT(L"QueryInterface(ITfSourceSingle) failed hr=0x%08X\n", (uint32_t)hrSS);
            _pSourceSingle = nullptr;
        }
    }

    return SUCCEEDED(hr);
}

void CTextService::_UninitThreadMgrEventSink()
{
    if (_dwThreadMgrEventSinkCookie != TF_INVALID_COOKIE || _dwThreadFocusSinkCookie != TF_INVALID_COOKIE)
    {
        ITfSource* pSource = nullptr;
        if (SUCCEEDED(_pThreadMgr->QueryInterface(IID_ITfSource, (void**)&pSource)))
        {
            if (_dwThreadMgrEventSinkCookie != TF_INVALID_COOKIE)
            {
                pSource->UnadviseSink(_dwThreadMgrEventSinkCookie);
            }
            if (_dwThreadFocusSinkCookie != TF_INVALID_COOKIE)
            {
                pSource->UnadviseSink(_dwThreadFocusSinkCookie);
            }
            pSource->Release();
        }
        _dwThreadMgrEventSinkCookie = TF_INVALID_COOKIE;
        _dwThreadFocusSinkCookie = TF_INVALID_COOKIE;
    }

    if (_pUIElementMgr != nullptr)
    {
        _pUIElementMgr->Release();
        _pUIElementMgr = nullptr;
    }

    if (_pSourceSingle != nullptr)
    {
        if (_funcProviderRegistered)
        {
            _pSourceSingle->UnadviseSingleSink(_tfClientId, IID_ITfFunctionProvider);
            _funcProviderRegistered = FALSE;
        }
        _pSourceSingle->Release();
        _pSourceSingle = nullptr;
    }
}

// 跨进程让出热键：某进程经 race check 让出热键时，PostMessage 给当前前台进程的 IME 隐藏窗口，
// 让对方立即重试注册。消息 ID 用 RegisterWindowMessage 全局注册，各进程取到相同 ID。
static UINT GetRetryHotkeyMessageId()
{
    static UINT s_msg = RegisterWindowMessageW(L"WindInputHotkeyRetry_v1");
    return s_msg;
}

// 前台自检定时器：兜底热键泄漏，不是兜底焦点信号缺失（OnKillThreadFocus 在 Chrome/VSCode/Edge 上可靠触发，
// 只比 DocMgr 级失焦晚约 100ms）。热键注册状态可能因 WM_HOTKEY 竞态等与焦点不同步，而 RegisterHotKey 冲突
// （1409）会让前台应用彻底拿不到热键，值得一道独立防线。本进程持有任何热键就定期校验。
static constexpr UINT_PTR kFocusCheckTimerId = 0x57494E44; // 'WIND'
static constexpr UINT     kFocusCheckIntervalMs = 500;

// 语言栏输入可用性同步的迟滞。须显著大于宿主 DocMgr churn 的翻转间隔（QQ 密码框约 180ms 一轮
// 「可编辑 ↔ READONLY」），又要短到点进文本框后察觉不出图标滞后。

// 慢焦点探针阈值：正常焦点切换（开 DEBUG 日志）约 5~13ms，20ms 不会被常规切换触发，又能抓住宿主 churn 焦点的堆积。
static constexpr double kSlowFocusWarnMs = 20.0;

// 焦点空窗超过多久才抬到 INFO。0~2ms 的 DocMgr 抖动很常见且用户无感，全抬会淹没日志；用户从切过去到敲下
// 第一个键最快也要几十毫秒，50ms 以上才是「首字符直通」可能发生的区间。
static constexpr ULONGLONG kFocusGapWarnMs = 50;

// locked/transient DocMgr 判据：XamlIsland 之类的容器文档，RequestEditSession 返回 TF_E_NOLOCK，OnSetFocus
// 对它跳过 focus_gained（防组合 replay 到不稳定文档）。两位必须同时置才算命中：
//   · 只判 dynFlags 0x20（TS_SD_UIINTEGRATIONENABLE，能力位）：Win11 任务管理器（WinUI 3）搜索框的主
//     DocMgr 天生带它（dynFlags=0x30 statFlags=0x40），会被整个吞掉，焦点归属停在上一个应用；
//   · 只判 statFlags 0x4（TS_SS_TRANSITORY，身份位）：Chrome / JetBrains 在真能打字的 context 上也置它，
//     explorer 任务栏（dynFlags=0x80000000 statFlags=0x4）也会被改掉。
// 真正的 XamlIsland 容器（explorer 地址栏）两位都有。
// OnSetFocus 里三处要判它（跳过 focus_gained 的守卫、doc_changed 收口的预判、_pLastActiveDocMgr 入缓存条件），
// 必须同源，漏改一处就会让 lost/gained 失配。
static constexpr DWORD kUiIntegrationDynFlag = 0x20; // TS_SD_UIINTEGRATIONENABLE（能力位）
static constexpr DWORD kTransitoryStatFlag   = 0x04; // TS_SS_TRANSITORY（身份位）

static inline BOOL IsLockedTransientDocMgr(DWORD dynFlags, DWORD statFlags)
{
    return ((dynFlags & kUiIntegrationDynFlag) != 0 && (statFlags & kTransitoryStatFlag) != 0)
               ? TRUE
               : FALSE;
}

// 激活静默期：ActivateEx 之后这段时间内的 compartment 变化视为系统初始化同步而非用户操作
// （激活后约 96ms 出现一次 CONVERSION 变化，取 250ms 留余量）。不取更大值：它与 _hasThreadFocus 的滞后
// （500ms 自检定时器兜底）叠加，就是切换应用后外部改模式被忽略的最长窗口。
static constexpr ULONGLONG kActivateSettleMs = 250;

// Win32 RegisterHotKey 热键 ID（前置声明给 OnKillThreadFocus 使用）
// 窗口类名按 Debug / Release 区分（class name 是 per-process 不会跨进程冲突，但
// 命名约定与 pipe / CLSID 等其他跨进程资源保持一致，便于 Spy++ 等工具区分两版本）。
#ifdef WIND_DEV_VARIANT
static const wchar_t* kHotkeyWndClassName = L"WindInputHotkeyWndDebug";
static const wchar_t* kHotkeyWndTitle     = L"WindInputHotkeyDebug";
#else
static const wchar_t* kHotkeyWndClassName = L"WindInputHotkeyWnd";
static const wchar_t* kHotkeyWndTitle     = L"WindInputHotkey";
#endif
// 候选热键（置顶/删除）的 id 段。id 与组合键之间没有约定：注册时按服务端 SESSION 热键表的迭代序顺次取 id，
// 并把 (id, rawHash) 记进 _candidateHotkeyIds，WM_HOTKEY 分发处反解，与加词热键同构。
static constexpr int  kHotkeyIdCandidateBase = 0x4000; // 候选热键，32 个槽位（0x4000..0x401F）
static constexpr int  kHotkeyIdCandidateMax  = 32;
static constexpr int  kHotkeyIdAddWordBase = 0x4020; // 加词热键（add_word / open_add_word_dialog），最多 16 个

// 加词热键重新评估自触发消息：_ReevaluateAddWordHotkey 从任意线程 PostMessage 此消息到
// _hHotkeyWnd，主线程 WndProc 收到后执行真正的 RegisterHotKey/UnregisterHotKey（该 API
// 须在拥有窗口的线程调用）。WM_APP 段为窗口私有，隐藏窗口专用不冲突。
static constexpr UINT WM_WIND_REEVAL_ADDWORD = WM_APP + 0x51;

STDAPI CTextService::OnSetThreadFocus()
{
    // tid/inst 与 compat.openclose.onchange 对齐，用于确认两者是否同一实例。
    WIND_LOG_DEBUG_FMT(L"OnSetThreadFocus called tid=%lu inst=0x%p\n", GetCurrentThreadId(), this);
    _hasThreadFocus = TRUE;

    // 重新取一次「当前焦点文档有没有可编辑上下文」：此刻的 _hasTextInputContext 很可能是 OnSetFocus 失焦分支
    // 留下的陈旧 FALSE（给加词热键门卫用的「DocMgr 走了」），不是「进了不可输入的地方」。直接拿它驱动语言栏，
    // Alt+Tab / 点任务栏时图标会闪「英」。GetFocus 拿不到文档时什么都不做，等 gaining 分支给权威判据。
    BOOL langBarJudgeable = FALSE;
    if (_pThreadMgr != nullptr)
    {
        ITfDocumentMgr* pFocusDoc = nullptr;
        if (SUCCEEDED(_pThreadMgr->GetFocus(&pFocusDoc)) && pFocusDoc != nullptr)
        {
            _hasTextInputContext = _DocMgrHasEditableContext(pFocusDoc);
            pFocusDoc->Release();
            langBarJudgeable = TRUE;
        }
    }
    WIND_LOG_DEBUG_FMT(L"OnSetThreadFocus: judgeable=%d hasTextCtx=%d",
                       langBarJudgeable ? 1 : 0, _hasTextInputContext ? 1 : 0);

    // 候选可见性热键由 NotifyCandidatesVisibilityChanged 驱动，这里不补（切焦点时候选窗通常已消失）。
    // 加词热键不依赖候选，重新评估；放在上面的重查之后，门卫读的就是 _hasTextInputContext。
    _ReevaluateAddWordHotkey();

    return S_OK;
}

// ITfThreadFocusSink — 线程退出 foreground。
STDAPI CTextService::OnKillThreadFocus()
{
    WIND_LOG_DEBUG_FMT(L"OnKillThreadFocus called tid=%lu inst=0x%p\n", GetCurrentThreadId(), this);
    _hasThreadFocus = FALSE;
    // Ctrl 如实上报的松开多半送不到了，丢掉记账（不丢的话下一次同侧 Ctrl 会被当成连发吞掉）。
    if (_pKeyEventSink != nullptr)
    {
        _pKeyEventSink->ResetCtrlRaw();
        _pKeyEventSink->ResetEatenFnKey();
    }
    // 空窗计时丢弃而非结算：应用失去前台后用户是去别处干活了，不丢的话切走十分钟再回来会记出一条假空窗。
    _focusGapStartTick = 0;
    _focusGapReason = nullptr;
    // 立即让出所有热键，让前台应用的 IME 实例能注册成功。
    if (_hotkeysActive)
    {
        _UnregisterCandidateHotkeys();
    }
    if (_addWordHotkeysActive)
    {
        _UnregisterAddWordHotkeys();
    }

    // 整个应用失去前台 = 真正离开了当前文档，在此收口输入态（DocMgr 级失焦不承担这件事），否则切走再切回
    // 会残留上次没打完的组合。传缓存的 DocMgr 作 hint：组合建在它上面，此刻 GetFocus() 未必可用，不传可能落到
    // forced cleanup，把残留文本提交进文档。
    //
    // 刻意不做 doc_changed 那样的 willSkipFocusGained 预判，别再「补齐」它：
    //   1. locked/transient 是 DocMgr 级判据，线程失焦是进程级事件，跨层复用；
    //   2. 真 transient 场景里该进程此前必有过正常 focus_gained，这条 lost 是对的，挡掉会让工具栏不隐藏；
    //   3. 服务端 `is_stale_focus_event` 只放行 token == active 的失焦，从未获焦的 token 发来的 lost 必被丢弃。
    CleanupInputStateForDocChange(_pLastActiveDocMgr, FOCUS_LOST_REASON_THREAD);
    // 失焦时不销毁 HostWindow：SearchHost/任务管理器等的 locked/transient DocMgr 会被跳过 focus_gained，
    // 而 HostWindow 重建依赖 focus_gained，销毁就再也不会重建。失焦时服务端经本进程 event（WriteHide）隐藏即可。
    return S_OK;
}

// ============================================================================
// Win32 RegisterHotKey 支持
// 候选可见时把置顶/删词热键注册为系统级热键，OS 在 WM_KEYDOWN 派发之前直接消费，规避 QQNT 等 Chromium
// 类宿主的加速键双处理；无候选时立即 UnregisterHotKey 还给宿主。
// 组合键来自服务端 SESSION 热键表（`keys.pin_candidate` / `keys.delete_candidate`），本层不写死。
// ============================================================================

BOOL CTextService::_InitHotkeyWindow()
{
    if (_hHotkeyWnd != nullptr) return TRUE;

    HINSTANCE hInst = g_hInstance; // DLL 实例句柄（dllmain 设置）

    WNDCLASSEXW wc = {};
    wc.cbSize        = sizeof(wc);
    wc.lpfnWndProc   = _HotkeyWndProc;
    wc.hInstance     = hInst;
    wc.lpszClassName = kHotkeyWndClassName;
    _hotkeyWndClass = RegisterClassExW(&wc);
    if (_hotkeyWndClass == 0)
    {
        DWORD err = GetLastError();
        // ERROR_CLASS_ALREADY_EXISTS (1410) 是正常情况（同进程多次激活）
        if (err != 1410)
        {
            WIND_LOG_WARN_FMT(L"RegisterClassExW(hotkey) failed err=%u\n", err);
            return FALSE;
        }
    }

    // 消息专用窗口（HWND_MESSAGE 父窗口），不可见、不占桌面位置。
    _hHotkeyWnd = CreateWindowExW(0, kHotkeyWndClassName, kHotkeyWndTitle,
                                   0, 0, 0, 0, 0,
                                   HWND_MESSAGE, nullptr, hInst, nullptr);
    if (_hHotkeyWnd == nullptr)
    {
        WIND_LOG_WARN_FMT(L"CreateWindowEx(hotkey) failed err=%u\n", (uint32_t)GetLastError());
        return FALSE;
    }
    // 把 this 存到窗口数据，WndProc 用来取回 CTextService 实例
    SetWindowLongPtrW(_hHotkeyWnd, GWLP_USERDATA, (LONG_PTR)this);
    WIND_LOG_INFO_FMT(L"Hotkey window created hwnd=0x%p\n", _hHotkeyWnd);

    // 初始化 thread focus 状态：TSF 的 OnSetThreadFocus 仅在焦点 transition 时触发，
    // 若 IME 激活时本进程恰好已经是前台（典型场景：用户刚点中输入框），不能等
    // 它的通知。用 GetForegroundWindow 的进程 ID 做自检种子。
    HWND hFg = GetForegroundWindow();
    if (hFg != nullptr)
    {
        DWORD fgPid = 0;
        GetWindowThreadProcessId(hFg, &fgPid);
        // 两者此刻只能用同一个猜测值：TSF 回调尚未到达，没有更好的信号。
        // 之后各走各的权威来源——_hasThreadFocus 归 ITfThreadFocusSink，
        // _isProcessForeground 归自检定时器。
        _hasThreadFocus = (fgPid == GetCurrentProcessId());
        _isProcessForeground = _hasThreadFocus;
        WIND_LOG_DEBUG_FMT(L"_InitHotkeyWindow: initial thread focus seed=%d (fgPid=%u ownPid=%u)\n",
                           (int)_hasThreadFocus, fgPid, GetCurrentProcessId());
    }
    // 启动前台自检 timer：兜底热键泄漏，见 kFocusCheckTimerId 注释。
    SetTimer(_hHotkeyWnd, kFocusCheckTimerId, kFocusCheckIntervalMs, nullptr);
    return TRUE;
}

void CTextService::_UninitHotkeyWindow()
{
    if (_hotkeysActive)
    {
        _UnregisterCandidateHotkeys();
    }
    if (_addWordHotkeysActive)
    {
        _UnregisterAddWordHotkeys();
    }
    if (_hHotkeyWnd != nullptr)
    {
        DestroyWindow(_hHotkeyWnd);
        _hHotkeyWnd = nullptr;
    }
    // DestroyWindow 会连带销毁该窗口的所有 timer，但 pending 标志必须手动复位：本函数与 _InitHotkeyWindow
    // 成对、可重入（Deactivate → Activate），标志残留为 TRUE 会让重建后的窗口不再起计时，语言栏图标卡在旧状态。
    if (_hotkeyWndClass != 0)
    {
        UnregisterClassW(kHotkeyWndClassName, g_hInstance);
        _hotkeyWndClass = 0;
    }
}

namespace
{
    // 内部 KEYMOD（SHIFT=1/CTRL=2/ALT=4/WIN=8）→ Win32 RegisterHotKey fsModifiers
    //（ALT=1/CTRL=2/SHIFT=4/WIN=8）。SHIFT 与 ALT 位互换，必须逐位映射，绝不能直传。
    UINT _ToWin32HotkeyMods(uint32_t keymod)
    {
        UINT f = 0;
        if (keymod & KEYMOD_CTRL)  f |= MOD_CONTROL;
        if (keymod & KEYMOD_SHIFT) f |= MOD_SHIFT;
        if (keymod & KEYMOD_ALT)   f |= MOD_ALT;
        if (keymod & KEYMOD_WIN)   f |= MOD_WIN;
        return f;
    }
}

// 候选可见时把置顶/删除热键注册成系统级热键。
//
// 组合键只能来自服务端推来的 SESSION 热键表，绝不能在本层写死：RegisterHotKey 先于一切拿到键，这里写死的话
// 用户改了 `keys.pin_candidate`，新组合会直接落进宿主。这条通路不出现在 grep `pin_candidate` 的结果里，
// 单元测试也抓不到（Rust 侧测的是表内容）；改热键值域时要记得它。
void CTextService::_RegisterCandidateHotkeys()
{
    if (_hHotkeyWnd == nullptr || _hotkeysActive) return;
    // 没拿到 thread focus 时绝不注册：多进程 IME 实例竞争同一组热键会 1409，让前台实例反而注册不上。
    // 要本应用在前台（TSF 信号）且本进程就是前台窗口所属进程（多进程宿主下后者为假）。
    // 英文模式也不注册：表里的键服务端只在中文模式认领。不置 _hotkeysActive，切回中文后下次候选出现时自然重试。
    if (!wind::candhotkey::ShouldRegister(_hasThreadFocus, _isProcessForeground, _bChineseMode)
        || _pHotkeyManager == nullptr) return;

    const auto& session = _pHotkeyManager->SessionHotkeys();
    // 表还没同步过来就不置 _hotkeysActive：置了等于宣称「已注册」，候选显隐不会再触发注册，这一段输入的
    // 候选热键就静默失效。留着不置位，下一次候选出现时自然重试。
    if (session.empty())
    {
        WIND_LOG_DEBUG(L"RegisterCandidateHotkeys skipped: session hotkey table empty\n");
        return;
    }
    int id = kHotkeyIdCandidateBase;
    int registered = 0;
    for (uint32_t rawHash : session)
    {
        if (id >= kHotkeyIdCandidateBase + kHotkeyIdCandidateMax) break; // 安全上限
        uint32_t vk     = rawHash & 0xFFFF;
        uint32_t keymod = rawHash >> 16;
        UINT fsMods = _ToWin32HotkeyMods(keymod) | MOD_NOREPEAT;
        if (RegisterHotKey(_hHotkeyWnd, id, fsMods, vk))
        {
            _candidateHotkeyIds.emplace_back(id, rawHash);
            registered++;
        }
        id++;
    }
    _hotkeysActive = TRUE;
    WIND_LOG_DEBUG_FMT(L"RegisterCandidateHotkeys: registered=%d/%d\n",
                       registered, (int)session.size());
}

void CTextService::_UnregisterCandidateHotkeys()
{
    if (_hHotkeyWnd == nullptr || !_hotkeysActive) return;

    // 按注册记录逐个卸载：id 与组合键的对应关系由 _candidateHotkeyIds 保存，
    // 不能再按「Pin 段 + N / Delete 段 + N」推算——那个约定已经不存在了。
    for (const auto& kv : _candidateHotkeyIds)
    {
        UnregisterHotKey(_hHotkeyWnd, kv.first);
    }
    _candidateHotkeyIds.clear();
    _hotkeysActive = FALSE;
    WIND_LOG_DEBUG(L"UnregisterCandidateHotkeys\n");
}

// 中英模式集中 setter：赋值后触发加词热键重评（模式是门卫条件）。reeval 内部 post，
// 故本函数可从任意线程（含 async reader 线程）安全调用。
void CTextService::_SetChineseMode(BOOL v)
{
    _bChineseMode = v;
    _ReevaluateAddWordHotkey();
}

// 线程安全入口：RegisterHotKey/UnregisterHotKey 必须在拥有 _hHotkeyWnd 的线程调用，
// 而模式变化可能来自 async reader 线程（StatePushCallback）。统一 post 到窗口线程执行。
void CTextService::_ReevaluateAddWordHotkey()
{
    if (_hHotkeyWnd != nullptr)
    {
        PostMessageW(_hHotkeyWnd, WM_WIND_REEVAL_ADDWORD, 0, 0);
    }
}

// 主线程：按门卫条件（中文 + 文本框 + 非密码框 + thread focus）注册或注销加词热键。幂等。
void CTextService::_DoReevaluateAddWordHotkey()
{
    // _isProcessForeground 与 _hasThreadFocus 并列：理由见 _RegisterCandidateHotkeys。
    BOOL want = _hasThreadFocus && _isProcessForeground
                && _bChineseMode && _hasTextInputContext && !_focusIsPassword;
    // 候选热键同样以中文模式为门卫：候选可见期间切到英文，立即还给宿主；
    // 候选可见期间切回中文，候选显隐不会再变，只能在这里补注册。
    switch (wind::candhotkey::OnModeReevaluate(_bChineseMode != FALSE, _candidatesVisible != FALSE,
                                               _hotkeysActive != FALSE))
    {
    case wind::candhotkey::Action::Register:
        _RegisterCandidateHotkeys();
        break;
    case wind::candhotkey::Action::Unregister:
        _UnregisterCandidateHotkeys();
        break;
    case wind::candhotkey::Action::None:
        break;
    }
    if (want && !_addWordHotkeysActive)
    {
        _RegisterAddWordHotkeys();
    }
    else if (!want && _addWordHotkeysActive)
    {
        _UnregisterAddWordHotkeys();
    }
}

void CTextService::_RegisterAddWordHotkeys()
{
    if (_hHotkeyWnd == nullptr || _addWordHotkeysActive) return;
    // 与候选热键同规：无 thread focus 绝不注册，避免多进程 IME 实例争抢同一组合键
    // 引发 ERROR_HOTKEY_ALREADY_REGISTERED (1409)。
    if (!_hasThreadFocus || !_isProcessForeground || _pHotkeyManager == nullptr) return;

    const auto& globals = _pHotkeyManager->GlobalHotkeys();
    int id = kHotkeyIdAddWordBase;
    int registered = 0;
    for (uint32_t rawHash : globals)
    {
        if (id >= kHotkeyIdAddWordBase + 16) break; // 安全上限
        uint32_t vk = rawHash & 0xFFFF;
        uint32_t keymod = rawHash >> 16;
        UINT fsMods = _ToWin32HotkeyMods(keymod) | MOD_NOREPEAT;
        if (RegisterHotKey(_hHotkeyWnd, id, fsMods, vk))
        {
            _addWordHotkeyIds.emplace_back(id, rawHash);
            registered++;
        }
        id++;
    }
    _addWordHotkeysActive = !_addWordHotkeyIds.empty();
    WIND_LOG_DEBUG_FMT(L"RegisterAddWordHotkeys: registered=%d/%d\n", registered, (int)globals.size());
}

void CTextService::_UnregisterAddWordHotkeys()
{
    if (_hHotkeyWnd == nullptr) return;
    for (const auto& kv : _addWordHotkeyIds)
    {
        UnregisterHotKey(_hHotkeyWnd, kv.first);
    }
    _addWordHotkeyIds.clear();
    _addWordHotkeysActive = FALSE;
    WIND_LOG_DEBUG(L"UnregisterAddWordHotkeys\n");
}

LRESULT CALLBACK CTextService::_HotkeyWndProc(HWND hWnd, UINT msg, WPARAM wParam, LPARAM lParam)
{
    // 加词热键重新评估（由 _ReevaluateAddWordHotkey 从任意线程 post）：在窗口线程执行
    // 真正的 RegisterHotKey/UnregisterHotKey。
    if (msg == WM_WIND_REEVAL_ADDWORD)
    {
        CTextService* self = reinterpret_cast<CTextService*>(GetWindowLongPtrW(hWnd, GWLP_USERDATA));
        if (self != nullptr)
        {
            self->_DoReevaluateAddWordHotkey();
        }
        return 0;
    }
    // 跨进程"热键已释放，请重试"通知：其他进程的 race check 让出热键时投递。
    // 我们立即重新评估并尝试注册。注意 msg ID 是动态分配的，必须用 if 而非 case。
    static UINT s_retryMsg = GetRetryHotkeyMessageId();
    if (msg == s_retryMsg)
    {
        CTextService* self = reinterpret_cast<CTextService*>(GetWindowLongPtrW(hWnd, GWLP_USERDATA));
        if (self != nullptr)
        {
            // 复核自己确实是前台再注册（这条消息可能来得稍晚，焦点已经又变了）。
            HWND hFg = GetForegroundWindow();
            DWORD fgPid = 0;
            if (hFg != nullptr) GetWindowThreadProcessId(hFg, &fgPid);
            if (fgPid == GetCurrentProcessId())
            {
                // 恢复的是热键资格，不是焦点信号（后者归 ITfThreadFocusSink）。
                self->_isProcessForeground = TRUE;
                // 候选可见性热键由 NotifyCandidatesVisibilityChanged 驱动；
                // 候选下一次出现时自然会重新注册。
                // 加词热键不依赖候选，须在此主动重评（已在窗口线程）。
                self->_DoReevaluateAddWordHotkey();
                WIND_LOG_DEBUG(L"Received hotkey retry signal, marked process foreground\n");
            }
        }
        return 0;
    }
    // 定时自检：兜底热键泄漏。每 500ms 跑一次，如果发现本进程不再前台但仍持有热键，
    // 主动释放并通知前台进程重试。注意本分支被 holdsAnyHotkey 门控，因此它纠正的是
    // 「热键状态」而非「_hasThreadFocus 的正确性」——不要把它当作焦点信号的兜底。
    if (msg == WM_TIMER && wParam == kFocusCheckTimerId)
    {
        CTextService* self = reinterpret_cast<CTextService*>(GetWindowLongPtrW(hWnd, GWLP_USERDATA));
        if (self != nullptr)
        {
            HWND hFg = GetForegroundWindow();
            DWORD fgPid = 0;
            if (hFg != nullptr) GetWindowThreadProcessId(hFg, &fgPid);
            BOOL nowForeground = (fgPid == GetCurrentProcessId());
            BOOL holdsAnyHotkey = self->_hotkeysActive || self->_addWordHotkeysActive;
            if (!nowForeground && holdsAnyHotkey)
            {
                WIND_LOG_DEBUG_FMT(L"FocusCheck timer: not foreground (fgPid=%u ownPid=%u), releasing\n",
                                   fgPid, GetCurrentProcessId());
                // 只动热键归属，不碰 _hasThreadFocus：多进程宿主（前台窗口在别的 pid）下写它会冲掉 TSF 给的
                // TRUE 且永不恢复，OnChange 的 !_hasThreadFocus 早退恒成立，中英切换整个失效。
                self->_isProcessForeground = FALSE;
                if (self->_hotkeysActive) self->_UnregisterCandidateHotkeys();
                if (self->_addWordHotkeysActive) self->_UnregisterAddWordHotkeys();
                // 通知前台 IME 立即重试
                const wchar_t* classNames[] = { L"WindInputHotkeyWnd", L"WindInputHotkeyWndDebug" };
                UINT retryMsg = GetRetryHotkeyMessageId();
                for (auto cls : classNames)
                {
                    HWND target = nullptr;
                    while ((target = FindWindowExW(HWND_MESSAGE, target, cls, nullptr)) != nullptr)
                    {
                        DWORD targetPid = 0;
                        GetWindowThreadProcessId(target, &targetPid);
                        if (targetPid == fgPid)
                        {
                            PostMessageW(target, retryMsg, 0, 0);
                            break;
                        }
                    }
                }
            }
            else if (nowForeground && !self->_isProcessForeground)
            {
                // 反向：本进程成为前台窗口所属进程，恢复热键资格。
                // 候选可见性热键由 NotifyCandidatesVisibilityChanged 驱动。
                self->_isProcessForeground = TRUE;
                // 加词热键不依赖候选，须主动重评（已在窗口线程）。
                self->_DoReevaluateAddWordHotkey();
            }
        }
        return 0;
    }
    if (msg == WM_HOTKEY)
    {
        CTextService* self = reinterpret_cast<CTextService*>(GetWindowLongPtrW(hWnd, GWLP_USERDATA));
        if (self != nullptr && self->_pKeyEventSink != nullptr)
        {
            // 不论下面分不分发（竞态丢弃、vk 解不出），这都是一次组合键：先作废待判单击。
            self->_pKeyEventSink->NoteHotkeyMessage();
            // 焦点切换竞态：按下热键的瞬间焦点可能正从 A 切到 B，A 尚未 unregister，WM_HOTKEY 落到 A 的隐藏窗口，
            // 直接处理会把动作下发到已失焦的实例。实时复核前台进程，不是自己就让出热键，丢弃这一次按键。
            HWND hFg = GetForegroundWindow();
            DWORD fgPid = 0;
            if (hFg != nullptr)
            {
                GetWindowThreadProcessId(hFg, &fgPid);
            }
            if (fgPid != GetCurrentProcessId())
            {
                WIND_LOG_DEBUG_FMT(L"WM_HOTKEY race: not foreground (fgPid=%u ownPid=%u), releasing hotkeys\n",
                                   fgPid, GetCurrentProcessId());
                // 同上：只让出热键，焦点信号归 ITfThreadFocusSink 独占。
                self->_isProcessForeground = FALSE;
                if (self->_hotkeysActive) self->_UnregisterCandidateHotkeys();
                if (self->_addWordHotkeysActive) self->_UnregisterAddWordHotkeys();
                // 通知前台进程的 IME hidden window 立即重试注册（避免它要等下次
                // 候选变化才发现热键空了）。两个变体的 class name 都搜。
                const wchar_t* classNames[] = { L"WindInputHotkeyWnd", L"WindInputHotkeyWndDebug" };
                UINT retryMsg = GetRetryHotkeyMessageId();
                for (auto cls : classNames)
                {
                    HWND target = nullptr;
                    while ((target = FindWindowExW(HWND_MESSAGE, target, cls, nullptr)) != nullptr)
                    {
                        DWORD targetPid = 0;
                        GetWindowThreadProcessId(target, &targetPid);
                        if (targetPid == fgPid)
                        {
                            PostMessageW(target, retryMsg, 0, 0);
                            WIND_LOG_DEBUG_FMT(L"Posted retry to foreground IME hwnd=0x%p pid=%u\n", target, fgPid);
                            break;
                        }
                    }
                }
                return 0;
            }
            int id = (int)wParam;
            uint32_t vk = 0;
            uint32_t mods = 0;
            const bool candidateSegment =
                id >= kHotkeyIdCandidateBase && id < kHotkeyIdCandidateBase + kHotkeyIdCandidateMax;
            if (candidateSegment)
            {
                // 候选热键：从注册记录反解 (vk, KEYMOD)，与加词热键同法。
                // 绝不能按 id 推算修饰键：组合键来自服务端配置，「哪一组是置顶、哪一组是删除」在协调器侧按 hash 判。
                for (const auto& kv : self->_candidateHotkeyIds)
                {
                    if (kv.first == id)
                    {
                        vk = kv.second & 0xFFFF;
                        mods = kv.second >> 16;
                        break;
                    }
                }
            }
            else if (id >= kHotkeyIdAddWordBase && id < kHotkeyIdAddWordBase + 16)
            {
                // 加词热键：从注册记录反解 (vk, KEYMOD)，下发给 coordinator 按 hash 匹配 action。
                for (const auto& kv : self->_addWordHotkeyIds)
                {
                    if (kv.first == id)
                    {
                        vk = kv.second & 0xFFFF;
                        mods = kv.second >> 16;
                        break;
                    }
                }
                // WM_HOTKEY 通路不经过 OnKeyDown 的 caret 更新；加词要弹预览候选窗，
                // 先补一次 caret 更新确保定位准确（对齐原 OnKeyDown 通路）。
                if (vk != 0)
                {
                    self->SendCaretPositionUpdate();
                    // 「热键模式会话」的乐观置位，只为盖住 IPC 空窗，不是权威。权威是服务端的
                    // STATUS_HOTKEY_SESSION（见 _SyncStateFromResponse），但 DispatchHotkey 是同步 IPC，状态推送
                    // 还在它之后，空窗里按 Esc 会被判「无会话」透传。随后第一条状态推送以权威值覆盖。
                    //
                    // 正因为过宽才必须是乐观的：这个 id 段是 GlobalHotkeys() 全集（加词、软键盘、key_actions 里进
                    // overlay 的动词……），DLL 只有 (vk, keymod)，分不出动作；当权威用的话，软键盘开关这种不产候选、
                    // 不发 ClearComposition 的动作会让位永远不清，此后 Enter/Esc/Backspace 吃了再吐。
                    // id 段只有 16 个，超出的静默退回普通 TSF 转发链路，在 Chromium 类宿主里会被同名加速键抢走，
                    // 故服务端给 GLOBAL 位（hotkey.rs 的 `hotkey_policy_for`）要克制。
                    if (self->_pKeyEventSink != nullptr)
                    {
                        self->_pKeyEventSink->ReleaseAssocBareOwnership();
                        self->_pKeyEventSink->SetCandidateSessionActive(TRUE);
                    }
                }
            }
            if (vk != 0)
            {
                WIND_LOG_DEBUG_FMT(L"WM_HOTKEY id=0x%04X vk=0x%02X mods=0x%04X\n", id, vk, mods);
                const BOOL giveBack = self->_pKeyEventSink->DispatchHotkey(vk, mods);
                // 系统热键吃下的键必须有出口：服务端不要它（如联想态的 Ctrl+数字）就注入一次
                // 原键交还宿主。只注入主键，修饰键靠物理按着，宿主看到的仍是 Ctrl+数字；注入前
                // 候选热键必须已注销，判据见 OnHotkeyDispatched。
                const auto giveBackAction = wind::candhotkey::OnHotkeyDispatched(
                    candidateSegment, giveBack != FALSE, self->_hotkeysActive != FALSE);
                if (giveBackAction != wind::candhotkey::GiveBack::None)
                {
                    const uint32_t held = GetCurrentModifiers();
                    if (!wind::candhotkey::RequiredModifiersHeld(mods, held))
                    {
                        // 同步 IPC 期间修饰键已松开：注入出去就是另一个键，宁可丢掉。
                        WIND_LOG_WARN_FMT(L"WM_HOTKEY give back skipped, modifier released: vk=0x%02X "
                                          L"mods=0x%04X held=0x%04X\n", vk, mods, held);
                    }
                    else
                    {
                        if (giveBackAction == wind::candhotkey::GiveBack::UnregisterThenReplay)
                        {
                            self->_UnregisterCandidateHotkeys();
                        }
                        WIND_LOG_DEBUG_FMT(L"WM_HOTKEY give back to host: vk=0x%02X unregistered=%d\n", vk,
                                           (int)(giveBackAction == wind::candhotkey::GiveBack::UnregisterThenReplay));
                        self->_pKeyEventSink->ReplayKeyToHost((WORD)vk);
                    }
                }
            }
        }
        return 0;
    }
    return DefWindowProcW(hWnd, msg, wParam, lParam);
}

// ============================================================================
// ITfFunctionProvider
// 经 ITfSourceSingle::AdviseSingleSink 注册为该 IME 的 Function Provider，让 Chromium / QQNT 识别为完整 IME。
// stub 实现：GetFunction 一律返回 E_NOINTERFACE，注册存在本身就够了。
// ITfFunctionProvider::GetDescription 与 ITfUIElement::GetDescription 同签名，多继承合并为单一实现。
// ============================================================================

STDAPI CTextService::GetType(GUID* pguid)
{
    if (pguid == nullptr) return E_INVALIDARG;
    // 用 IME 本身的 CLSID 作为 function provider 类型标识
    *pguid = c_clsidTextService;
    return S_OK;
}

STDAPI CTextService::GetFunction(REFGUID rguid, REFIID riid, IUnknown** ppunk)
{
    if (ppunk == nullptr) return E_INVALIDARG;
    *ppunk = nullptr;
    // 不提供任何具体 function。如果未来需要支持 ITfFnSearchCandidateProvider /
    // ITfFnReverseConversion 等，在此处分发。
    return E_NOINTERFACE;
}

// ============================================================================
// ITfUIElement / ITfCandidateListUIElement / ITfCandidateListUIElementBehavior
//
// 两种宿主、两套数据：
//  - 宿主不接管（BeginUIElement 回 pbShow=TRUE，绝大多数桌面程序）：候选由服务进程自己的窗口绘制。这里只维持
//    「注册存在」，Chromium / QQNT 据此走完整 IME-first 调度、不再和我们抢 Ctrl+数字；getter 返回占位数据
//    （count=1 / "…"），键路径不多一次 IPC。
//  - 宿主接管（pbShow=FALSE，或线程以 TF_TMAE_UIELEMENTENABLEDONLY 激活；全屏游戏、搜索框、SDL / Unreal 等）：
//    候选由宿主画。每次候选变化经 CMD_UIELEMENT_QUERY 从服务端拉快照（_uiSnapshot），再 UpdateUIElement 通知
//    宿主来读；宿主的 SetSelection / Finalize / Abort 经 CMD_UIELEMENT_ACTION 回流。服务端按 pid 记账
//    （CMD_UIELEMENT_STATE），该进程聚焦期间不弹自己的候选窗。
//
// 规范要点（learn.microsoft.com/windows/win32/tsf/uiless-mode-overview）：
//  - BeginUIElement 回 FALSE 后 TIP 必须调 UpdateUIElement，宿主到那时才读内容（首次 GetUpdatedFlags 应全位
//    置位）；回 TRUE 时可不调，但 EndUIElement 必须调。
//  - 候选列表须按「页」而非「滚动」推进，页索引在列表存续期间不该变。
//  - GetSelection 无选中时回 S_FALSE。
// 设计与状态机见 docs/design/game-compat-tsf-uielement.md。
//
// Dota 2（起源2引擎）画不出候选不是这段代码的问题，别在这里试：它走 IMM32 且有按 TSF Profile Description
// 比对的身份白名单，见设计文档 §1.1。
// ============================================================================

// 候选列表 UI 元素的 GUID：给宿主 QI/去重用的标识，CUAS 桥不据它决定是否开候选盒。
// 故保留自有稳定 GUID，不impersonate 其它输入法。
static const GUID kWindCandidateUIElementGuid =
    { 0xb3e54a91, 0x7c20, 0x4b6a, { 0xa1, 0x5e, 0x82, 0x09, 0x77, 0x55, 0x44, 0x33 } };

// 含 TF_CLUIE_DOCUMENTMGR：GetDocumentMgr 返回真实焦点文档（同 Weasel），首次 Update 通知宿主取一次 docmgr，
// 据此把候选关联到文本。保留本位是因为它符合规范，与游戏能否绘制无关（设计文档 §1.1）。
static constexpr DWORD kUiElementAllFlags =
    TF_CLUIE_DOCUMENTMGR | TF_CLUIE_COUNT | TF_CLUIE_SELECTION
    | TF_CLUIE_STRING | TF_CLUIE_PAGEINDEX | TF_CLUIE_CURRENTPAGE;

STDAPI CTextService::GetDescription(BSTR* pbstrDescription)
{
    if (pbstrDescription == nullptr) return E_INVALIDARG;
    *pbstrDescription = SysAllocString(L"WindInput Candidate List");
    return *pbstrDescription ? S_OK : E_OUTOFMEMORY;
}

STDAPI CTextService::GetGUID(GUID* pguid)
{
    if (pguid == nullptr) return E_INVALIDARG;
    *pguid = kWindCandidateUIElementGuid;
    return S_OK;
}

STDAPI CTextService::Show(BOOL bShow)
{
    WIND_LOG_DEBUG_FMT(L"ITfUIElement::Show(%d)\n", (int)bShow);
    BOOL prevHostDraws = _uiHostDraws;
    _uiElementShown = bShow;
    _uiHostDraws = !bShow;
    // 宿主中途改主意（先允许我们画、后来 Show(FALSE) 自己画，或反过来）：立刻报服务端
    // 收/弹候选窗；转为宿主画时还得把数据备好并通知它来读。
    _ReportUiElementState();
    if (!prevHostDraws && _uiHostDraws && _uiElementId != (DWORD)-1)
    {
        _UpdateUiElementForHost();
    }
    return S_OK;
}

STDAPI CTextService::IsShown(BOOL* pbShow)
{
    if (pbShow == nullptr) return E_INVALIDARG;
    *pbShow = _uiElementShown;
    return S_OK;
}

STDAPI CTextService::GetUpdatedFlags(DWORD* pdwFlags)
{
    if (pdwFlags == nullptr) return E_INVALIDARG;
    _EnsureUiSnapshotFresh();
    *pdwFlags = _UiElementUseSnapshot() ? _uiUpdatedFlags : kUiElementAllFlags;
    WIND_LOG_DEBUG_FMT(L"UIElement host read: GetUpdatedFlags=0x%X snapshot=%d count=%u\n",
                       *pdwFlags, (int)_UiElementUseSnapshot(),
                       (UINT)_uiSnapshot.items.size());
    return S_OK;
}

STDAPI CTextService::GetDocumentMgr(ITfDocumentMgr** ppdim)
{
    // 返回当前焦点文档：宿主靠它把候选列表关联到文本（同 Weasel，取 GetFocus 的真实 docmgr），这是规范该有的答法。
    // 它不是游戏能否绘制候选的判据，别为某个游戏来回改这里的返回值（设计文档 §1.1）。
    if (ppdim == nullptr) return E_INVALIDARG;
    *ppdim = nullptr;
    if (_pThreadMgr == nullptr) return E_FAIL;
    if (FAILED(_pThreadMgr->GetFocus(ppdim)) || *ppdim == nullptr) return E_FAIL;
    return S_OK; // GetFocus 已 AddRef，调用方负责 Release
}

STDAPI CTextService::GetCount(UINT* puCount)
{
    if (puCount == nullptr) return E_INVALIDARG;
    _EnsureUiSnapshotFresh();
    // 无快照且宿主不接管时给占位 1：至少 1 个候选才能让 TSF 认为候选 UI "有意义"（Chromium 调度所需）。
    *puCount = _UiElementUseSnapshot() ? (UINT)_uiSnapshot.items.size() : 1;
    return S_OK;
}

STDAPI CTextService::GetSelection(UINT* puIndex)
{
    if (puIndex == nullptr) return E_INVALIDARG;
    _EnsureUiSnapshotFresh();
    if (!_UiElementUseSnapshot())
    {
        *puIndex = 0;
        return S_OK;
    }
    if (_uiSnapshot.items.empty())
    {
        *puIndex = 0;
        return S_FALSE; // 规范：无选中回 S_FALSE，首参无效
    }
    *puIndex = _uiSnapshot.selected;
    return S_OK;
}

STDAPI CTextService::GetString(UINT uIndex, BSTR* pstr)
{
    if (pstr == nullptr) return E_INVALIDARG;
    _EnsureUiSnapshotFresh(TRUE); // ★ 取内容：闩未合上时也要补拉，否则首次读到的是上一帧
    if (!_UiElementUseSnapshot())
    {
        *pstr = SysAllocString(L"…"); // 占位
        return *pstr ? S_OK : E_OUTOFMEMORY;
    }
    if (uIndex >= _uiSnapshot.items.size())
    {
        *pstr = nullptr;
        return E_INVALIDARG;
    }
    const std::wstring& text = _uiSnapshot.items[uIndex];
    *pstr = SysAllocStringLen(text.c_str(), (UINT)text.size());
    WIND_LOG_DEBUG_FMT(L"UIElement host read: GetString(%u)=\"%s\"\n", uIndex, text.c_str());
    // 判定点：把真实候选串交出去了。GetCount / GetUpdatedFlags 这类元信息 msctf 自己也会问，不足以说明有人要画，
    // 取走文本才是。占位分支（上面的早退）不打闩。
    _NoteHostReadCandidates();
    return *pstr ? S_OK : E_OUTOFMEMORY;
}

STDAPI CTextService::GetPageIndex(UINT* pIndex, UINT uSize, UINT* puPageCnt)
{
    if (puPageCnt == nullptr) return E_INVALIDARG;
    _EnsureUiSnapshotFresh();
    if (!_UiElementUseSnapshot())
    {
        *puPageCnt = 1;
        if (pIndex && uSize >= 1) pIndex[0] = 0;
        return S_OK;
    }
    // 页起点表：每页 pageSize 条，count 为 0 时 0 页（SampleIME 同款）。宿主惯常先传
    // pIndex=NULL 取页数再分配数组二次调用；uSize 不足时只填得下的部分，不报错。
    const UINT count = (UINT)_uiSnapshot.items.size();
    const UINT pageSize = _uiSnapshot.pageSize ? _uiSnapshot.pageSize : 1;
    const UINT pages = count ? (count + pageSize - 1) / pageSize : 0;
    *puPageCnt = pages;
    WIND_LOG_DEBUG_FMT(L"UIElement host read: GetPageIndex pages=%u pageSize=%u count=%u wantArray=%d\n",
                       pages, pageSize, count, (int)(pIndex != nullptr));
    if (pIndex)
    {
        for (UINT i = 0; i < pages && i < uSize; ++i)
        {
            pIndex[i] = i * pageSize;
        }
    }
    return S_OK;
}

STDAPI CTextService::SetPageIndex(UINT* pIndex, UINT uPageCnt)
{
    // 宿主想改页的切法（每页几条）。分页由服务端配置（ui.candidate.per_page）决定，
    // 且规范建议列表存续期间不改页索引——这里接受调用但不改切法（Weasel 同款）。
    (void)pIndex; (void)uPageCnt;
    return S_OK;
}

STDAPI CTextService::GetCurrentPage(UINT* puPage)
{
    if (puPage == nullptr) return E_INVALIDARG;
    _EnsureUiSnapshotFresh();
    *puPage = _UiElementUseSnapshot() ? _uiSnapshot.currentPage : 0;
    return S_OK;
}

STDAPI CTextService::SetSelection(UINT nIndex)
{
    WIND_LOG_DEBUG_FMT(L"ITfCandidateListUIElementBehavior::SetSelection(%u)\n", nIndex);
    if (!_UiElementHostDraws()) return S_OK; // 宿主不画时它也不该操作；忽略
    if (nIndex >= _uiSnapshot.items.size()) return E_INVALIDARG;
    _SendUiElementAction(UIELEMENT_ACTION_SET_SELECTION, nIndex);
    // 同一条管道按序处理：上面的 action 先于下面的 query 到达，拉回来的就是新高亮。
    _UpdateUiElementForHost();
    return S_OK;
}

STDAPI CTextService::Finalize(void)
{
    WIND_LOG_DEBUG(L"ITfCandidateListUIElementBehavior::Finalize\n");
    if (!_UiElementHostDraws()) return S_OK;
    // 上屏结果经 push 管道回来（CommitText → 收组合 → NotifyCandidatesVisibilityChanged(FALSE)
    // → EndUIElement），与鼠标点选候选同一条路，这里不等应答。
    _SendUiElementAction(UIELEMENT_ACTION_FINALIZE, 0);
    return S_OK;
}

STDAPI CTextService::Abort(void)
{
    WIND_LOG_DEBUG(L"ITfCandidateListUIElementBehavior::Abort\n");
    if (!_UiElementHostDraws()) return S_OK;
    _SendUiElementAction(UIELEMENT_ACTION_ABORT, 0); // 服务端推 ClearComposition 回来收口
    return S_OK;
}

// ==== ITfIntegratableCandidateListUIElement ====
// Win8+ 搜索框一类宿主的候选集成（键路由、候选编号显示），对照 Weasel CandidateList.cpp。
// 与游戏能否绘制候选无关（设计文档 §1.1）。
STDAPI CTextService::SetIntegrationStyle(GUID guidIntegrationStyle)
{
    (void)guidIntegrationStyle;
    return S_OK;
}

STDAPI CTextService::GetSelectionStyle(TfIntegratableCandidateListSelectionStyle* ptfSelectionStyle)
{
    if (ptfSelectionStyle == nullptr) return E_INVALIDARG;
    *ptfSelectionStyle = STYLE_ACTIVE_SELECTION; // 高亮项即待上屏项，与桌面候选窗一致
    return S_OK;
}

STDAPI CTextService::OnKeyDown(WPARAM wParam, LPARAM lParam, BOOL* pfEaten)
{
    (void)wParam; (void)lParam;
    if (pfEaten == nullptr) return E_INVALIDARG;
    // 不在这里吃键：按键的权威处理在 ITfKeyEventSink，谎报吃键会把本该走那条路的键吞掉。返回 FALSE 让键落回
    // 正常处理链。
    *pfEaten = FALSE;
    return S_OK;
}

STDAPI CTextService::ShowCandidateNumbers(BOOL* pfShow)
{
    if (pfShow == nullptr) return E_INVALIDARG;
    *pfShow = TRUE; // 让宿主画候选序号（1/2/3…）
    return S_OK;
}

STDAPI CTextService::FinalizeExactCompositionString(void)
{
    return E_NOTIMPL;
}

void CTextService::_ReportUiElementState()
{
    uint32_t flags = 0;
    // HOST_DRAWS 只报声明，不带上读取闩：声明是事实、读取是推断，core 要能分开记账（推断那条可被 compat 规则关掉）。
    if (_uiHostDraws) flags |= UIELEMENT_FLAG_HOST_DRAWS;
    if (_uiLessThread) flags |= UIELEMENT_FLAG_UI_LESS_THREAD;
    if (_uiHostReadsCandidates) flags |= UIELEMENT_FLAG_HOST_READS;
    if ((int32_t)flags == _uiElementStateSent) return;
    if (_pIPCClient == nullptr || !_pIPCClient->IsConnected()) return; // 未连上：留着 -1，下次再报
    UiElementStatePayload payload = {};
    payload.pid = GetCurrentProcessId();
    payload.flags = flags;
    if (_pIPCClient->SendAsync(CMD_UIELEMENT_STATE, &payload, sizeof(payload)))
    {
        _uiElementStateSent = (int32_t)flags;
        WIND_LOG_INFO_FMT(L"uielement state reported: flags=0x%X (host_draws=%d ui_less=%d host_reads=%d)\n",
                          flags, (int)_uiHostDraws, (int)_uiLessThread,
                          (int)_uiHostReadsCandidates);
    }
}

BOOL CTextService::_RefreshUiElementSnapshot()
{
    // 无论拉成功还是失败，快照都已按「当前能拿到的最新」重算过一次，不再是过期的。
    // 失败时留着 dirty 只会让后续每个 getter 都重试一次同步 IPC，把宿主 UI 线程拖死。
    _uiSnapshotDirty = FALSE;
    if (_pIPCClient == nullptr) return FALSE;
    ServiceResponse resp;
    if (!_pIPCClient->SendSync(CMD_UIELEMENT_QUERY, nullptr, 0, resp)
        || resp.type != ResponseType::UiElementPage)
    {
        WIND_LOG_WARN(L"uielement: snapshot query failed, clearing snapshot\n");
        BOOL hadItems = !_uiSnapshot.items.empty();
        _uiSnapshot = UiElementSnapshot();
        _uiUpdatedFlags = hadItems ? kUiElementAllFlags : 0;
        return FALSE;
    }
    UiElementSnapshot next;
    next.items = std::move(resp.uiCandidates);
    next.selected = resp.uiSelected;
    next.pageSize = resp.uiPageSize ? resp.uiPageSize : 1;
    next.currentPage = resp.uiCurrentPage;

    // 变化位：宿主据 GetUpdatedFlags 决定重读哪些部分（SDL 只在 STRING/COUNT 变时重排版）。
    DWORD flags = 0;
    if (next.items.size() != _uiSnapshot.items.size()) flags |= TF_CLUIE_COUNT | TF_CLUIE_STRING | TF_CLUIE_PAGEINDEX;
    else if (next.items != _uiSnapshot.items) flags |= TF_CLUIE_STRING;
    if (next.pageSize != _uiSnapshot.pageSize) flags |= TF_CLUIE_PAGEINDEX;
    if (next.selected != _uiSnapshot.selected) flags |= TF_CLUIE_SELECTION;
    if (next.currentPage != _uiSnapshot.currentPage) flags |= TF_CLUIE_CURRENTPAGE;
    _uiUpdatedFlags = flags;
    _uiSnapshot = std::move(next);
    return TRUE;
}

void CTextService::_UpdateUiElementForHost()
{
    if (_pUIElementMgr == nullptr || _uiElementId == (DWORD)-1) return;
    _RefreshUiElementSnapshot();
    _pUIElementMgr->UpdateUIElement(_uiElementId);
}

void CTextService::_EnsureUiSnapshotFresh(BOOL contentRead)
{
    // 这是本类唯一一处在 msctf sink 派发栈内的同步 IPC（UpdateUIElement → 宿主 GetString → 这里），最坏把宿主
    // UI 线程按住读超时（1500ms），期间进程内其它 sink / TIP 一并挂起。
    // 正常路径每次激活至多一次（GetString 成功出口合闩）；但 GetString 的两个早退不合闩，快照查询持续失败或
    // 回空时会每键一次，由 IPC 熔断（_ShouldAttemptOperation / _RecordFailure）兜住。
    // 不要为此调小超时：读超时与断连/熔断是同一套，调小会在负载高时误断 IPC，熔断才是这条路的闸门。
    // 不变量看守：脏位与「已认定在画」不该共存（UiElementPolicy.h 的 InvariantBroken）。破了下面照常补拉、结果
    // 仍正确，但说明有人在别处置了脏。记 WARN 以便不开文件日志也能导出。
    if (wind::uielement::InvariantBroken(_uiSnapshotDirty != FALSE,
                                         _uiHostDraws != FALSE,
                                         _uiHostReadsCandidates != FALSE))
    {
        WIND_LOG_WARN(L"uielement: 脏位与「已认定在画」同时成立——调用方不变量已被破坏，"
                      L"取元信息 getter 的补拉路径此前从未执行过，请复核设计文档 1.4\n");
    }
    if (!wind::uielement::ShouldRefreshOnGet(_uiSnapshotDirty != FALSE,
                                             contentRead != FALSE,
                                             _uiHostReadsCandidates != FALSE))
    {
        return;
    }
    _RefreshUiElementSnapshot();
}

void CTextService::_NoteHostReadCandidates()
{
    if (_uiHostReadsCandidates) return;
    _uiHostReadsCandidates = TRUE;
    WIND_LOG_INFO(L"uielement: 宿主实际读取了候选串（pbShow=TRUE 却来读）——判定为宿主自绘，"
                  L"服务端将收起本进程的候选窗\n");
    _ReportUiElementState();
}

void CTextService::_SendUiElementAction(uint32_t action, uint32_t arg)
{
    if (_pIPCClient == nullptr) return;
    UiElementActionPayload payload = {};
    payload.action = action;
    payload.arg = arg;
    _pIPCClient->SendAsync(CMD_UIELEMENT_ACTION, &payload, sizeof(payload));
}

void CTextService::NotifyCandidatesVisibilityChanged(BOOL hasCandidates)
{
    // 候选可见 → 注册系统级热键拦截置顶/删词组合键（keys.pin_candidate / keys.delete_candidate，
    // 见 _RegisterCandidateHotkeys）；候选消失 → 卸载，还给宿主。规避 Chromium 类宿主的加速键双处理。
    _candidatesVisible = hasCandidates;
    if (hasCandidates && !_hotkeysActive)
    {
        _RegisterCandidateHotkeys();
    }
    else if (!hasCandidates && _hotkeysActive)
    {
        _UnregisterCandidateHotkeys();
    }

    if (_pUIElementMgr == nullptr) return;

    if (hasCandidates && _uiElementId == (DWORD)-1)
    {
        BOOL bShow = TRUE;
        // 先取快照再 BeginUIElement：宿主（以及 IMM32 桥）在 Begin 回调里就会读 GetCount/GetString，
        // 那时只有占位「…」宿主拿到的就是占位。代价是每个组合起手多一次同步拉取（亚毫秒）。
        _RefreshUiElementSnapshot();
        _uiUpdatedFlags = kUiElementAllFlags;
        // 通过 Behavior 路径解决菱形继承
        HRESULT hr = _pUIElementMgr->BeginUIElement(
            static_cast<ITfUIElement*>(static_cast<ITfCandidateListUIElementBehavior*>(this)),
            &bShow, &_uiElementId);
        if (SUCCEEDED(hr))
        {
            _uiElementShown = bShow;
            _uiHostDraws = !bShow;
            WIND_LOG_DEBUG_FMT(L"BeginUIElement ok id=%u show=%d\n", _uiElementId, (int)bShow);
            // 这里不置「热键模式会话」位：它由服务端经 STATUS_HOTKEY_SESSION 权威驱动（见
            // _SyncStateFromResponse），DLL 只镜像，多一个本地写入方就多一个真相源。
            //
            // 有位要主张就强制重报一次，绕过 _uiElementStateSent 的去重：每个 TSF 线程一个实例、各有一份
            // 去重状态，而服务端按裸 pid 记账，同进程另一线程首次 Begin 报 flags=0 会抹掉本线程主张过的位，
            // 本实例因去重不再重报就一直不收窗。每次组合起手重报只是把「永久」降级成「到下一次组合起手为止」，
            // 同进程两线程持不同位时还会互相翻牌；根治要载荷加 tid、服务端按 (pid, tid) 记账，
            // 见 docs/design/game-compat-tsf-uielement.md §4.4「已知缺口」。
            if (_uiHostDraws || _uiLessThread || _uiHostReadsCandidates)
            {
                _uiElementStateSent = -1;
            }
            _ReportUiElementState();
            // 规范：回 FALSE 后必须 UpdateUIElement；回 TRUE 时可不调，这里仍调，为的是把会读的宿主逼出来：
            // IMM32 桥只在收到 UpdateUIElement 之后才来读，不在首键给它这次机会，读取闩要到第二键才合上，
            // 用户会看到「先两个框、再收掉一个」。不读的宿主不会因此拉快照，键路径零额外 IPC。
            _uiUpdatedFlags = kUiElementAllFlags;
            _pUIElementMgr->UpdateUIElement(_uiElementId);
        }
        else
        {
            WIND_LOG_WARN_FMT(L"BeginUIElement failed hr=0x%08X\n", (uint32_t)hr);
            _uiElementId = (DWORD)-1;
        }
    }
    else if (!hasCandidates && _uiElementId != (DWORD)-1)
    {
        HRESULT hr = _pUIElementMgr->EndUIElement(_uiElementId);
        WIND_LOG_DEBUG_FMT(L"EndUIElement id=%u hr=0x%08X\n", _uiElementId, (uint32_t)hr);
        _uiElementId = (DWORD)-1;
        _uiElementShown = FALSE;
        // 同 BeginUIElement：不在这里清「热键模式会话」位。加词开着时候选窗若因故收一次会把活着的会话清掉，
        // 而服务端没有状态变化、不会再推来纠正。
        // 只清快照，不动 _uiHostDraws：它记录宿主的意愿，下一次 BeginUIElement 会重新问；服务端记账照旧，
        // 避免每次组合结束都收/弹一次。
        _uiSnapshot = UiElementSnapshot();
        _uiUpdatedFlags = 0;
        _uiSnapshotDirty = FALSE;
    }
    else if (hasCandidates && _uiElementId != (DWORD)-1)
    {
        if (wind::uielement::ShouldRefreshEagerly(_uiHostDraws != FALSE,
                                                  _uiHostReadsCandidates != FALSE))
        {
            _UpdateUiElementForHost(); // 拉快照 + 通知宿主重读
        }
        else
        {
            // 还不知道有没有人在读：只记账不拉取，通知照发。宿主真来读 getter 时由 _EnsureUiSnapshotFresh 补拉。
            // 别改成无条件 _UpdateUiElementForHost()：那会给每个宿主的每次按键加一次 UI 线程上的同步 IPC。
            _uiSnapshotDirty = TRUE;
            _pUIElementMgr->UpdateUIElement(_uiElementId);
        }
    }
}

STDAPI CTextService::OnInitDocumentMgr(ITfDocumentMgr* pDocMgr)
{
    return S_OK;
}

STDAPI CTextService::OnUninitDocumentMgr(ITfDocumentMgr* pDocMgr)
{
    return S_OK;
}

STDAPI CTextService::OnSetFocus(ITfDocumentMgr* pDocMgrFocus, ITfDocumentMgr* pDocMgrPrevFocus)
{
    WIND_LOG_DEBUG_FMT(L"OnSetFocus called focus=0x%p prev=0x%p", pDocMgrFocus, pDocMgrPrevFocus);

    // 慢焦点探针：本函数在宿主 UI 线程上同步做 COM 查询外加一次阻塞 IPC 往返（SendFocusGained），
    // 宿主高频 churn 焦点时会表现为鼠标光标卡顿。记 WARN：WARN 恒进环形缓冲，用户不开文件日志也能用
    // Ctrl+Shift+F12 导出证据，开 DEBUG 本身会加重卡顿。
    const LONGLONG focusProbeT0 = WindLog::PerfNow();
    double focusIpcMs = 0.0;

    _hasFocus = (pDocMgrFocus != nullptr);
    // 「上一份 focus_gained 没带标题」只属于那一份：焦点一动就作废，只有下面真发出 focus_gained 时
    // 才重新置位。否则焦点转到 transient / 不可编辑的 DocMgr 后，标题开关一到仍会补发 focus_gained，
    // 服务端把不可编辑的焦点当成可编辑（工具栏误显示）。
    _focusSentTitleBlind = FALSE;

    // If gaining focus (pDocMgrFocus is not null)
    if (pDocMgrFocus != nullptr)
    {
        _focusSessionId++;

        // 无占位联想中文档焦点反弹：鼠标刚按过 ⇒ 用户点了别处（WPS 点单元格就是这样表现的，
        // 且不发选区变化），让服务端收掉联想窗，而不是等自动隐藏计时。
        if (_pKeyEventSink != nullptr && _pKeyEventSink->BareAssocMouseClicked() && _pIPCClient != nullptr &&
            _pIPCClient->IsConnected())
        {
            WIND_LOG_DEBUG(L"bare assoc: 焦点反弹且鼠标刚按过，按用户点击处理\n");
            _pIPCClient->SendSelectionChanged(0);
        }

        // 焦点抖动免疫：判据取「文档变没变」而非「失过焦没有」。DocMgr 级是噪声层，清理推迟到另一个文档
        // 拿到焦点时执行，抖动自然被判为同一文档而跳过（见 docs/architecture/tsf-docmgr-focus-semantics.md）。
        // 判据取 _pLastFocusedDocMgr（含 transient）而非 _pLastActiveDocMgr：后者排除 locked/transient，
        // 拿它判抖动会让 transient 次次判成换文档（explorer 地址栏丢首字母）。
        const BOOL isSameDocMgr = (_pLastFocusedDocMgr != nullptr && pDocMgrFocus == _pLastFocusedDocMgr);
        WIND_LOG_DEBUG_FMT(L"Focus gained focusSession=%llu sameDoc=%d doc=0x%p",
                           _focusSessionId, isSameDocMgr ? 1 : 0, pDocMgrFocus);

        if (!isSameDocMgr && _pLastActiveDocMgr != nullptr)
        {
            // 真的换了文档：在旧 doc 上收口。必须传 hint：此刻 GetFocus() 已指向新 doc。
            //
            // 收口会发 focus_lost，它必须与随后的 focus_gained 配对。下面的 XamlIsland locked 守卫会对
            // transient 新 DocMgr 跳过 focus_gained，用户可能就停在它上面打字、不再有 gained，服务端的
            // ime_active 被清掉后无人恢复。故预判守卫是否命中，命中就不发 lost，本地清理照做。
            // _DocMgrHasEditableContext 是纯查询，只在真正换文档时多查一次。
            DWORD incomingDynFlags = 0;
            DWORD incomingStatFlags = 0;
            _DocMgrHasEditableContext(pDocMgrFocus, &incomingDynFlags, &incomingStatFlags);
            const BOOL willSkipFocusGained =
                IsLockedTransientDocMgr(incomingDynFlags, incomingStatFlags);
            CleanupInputStateForDocChange(_pLastActiveDocMgr, FOCUS_LOST_REASON_DOC_CHANGED,
                                          !willSkipFocusGained);
        }

        // Register ITfTextLayoutSink on the new context to receive
        // layout change notifications (for accurate candidate window positioning)
        ITfContext* pContext = nullptr;
        if (SUCCEEDED(pDocMgrFocus->GetTop(&pContext)) && pContext != nullptr)
        {
            _AdviseTextLayoutSink(pContext);
            _AdviseTextEditSink(pContext);
            pContext->Release();
        }

        // 这两行都在焦点热路径上：采集一次进程信息要 OpenProcess + 令牌查询 +
        // 映像路径 + 窗口标题长度。必须走带级别闸门的封装，不能裸调采集函数。
        // 标题只记长度（title_len），不记原文——标题是用户数据。
        WindLogCurrentProcessInfo(4, L"compat.focus.current_host");
        WindLogForegroundProcessInfo(4, L"compat.focus.foreground_host");

        // 语言栏的 ForceRefresh 不在这里：它会让系统回调 GetIcon 重绘，而此刻 _hasTextInputContext 还是上一段焦点
        // 的值（下面才算）。

        // 换文档时复位组合状态。同一文档抖回来不复位：_isComposing/_hasCandidates 清零后 hasInputSession
        // 为假，紧接着的 Backspace / 空格 / 数字选字会被透传给宿主。
        // 配对状态同样在此复位（理由见 CleanupInputStateForDocChange）。
        if (!isSameDocMgr && _pKeyEventSink != nullptr)
        {
            _pKeyEventSink->ResetComposingState();
        }

        // 判焦点 DocMgr 有没有可编辑 context：用 TSF 状态位（TF_SD_READONLY 等）而非 GetTextExt，
        // 后者是布局 API，很多框架（JetBrains/Java Swing）不实现；Chrome 把「无文本框」context 标为只读。
        DWORD docMgrDynFlags = 0;
        DWORD docMgrStatFlags = 0;
        _hasTextInputContext =
            _DocMgrHasEditableContext(pDocMgrFocus, &docMgrDynFlags, &docMgrStatFlags);
        WIND_LOG_DEBUG_FMT(L"OnSetFocus: hasTextCtx=%d focusSession=%llu", _hasTextInputContext, _focusSessionId);

        // 读取焦点控件的 InputScope（密码框/邮箱/URL 等语义），随 focus_gained 上报给服务端决策。
        // 仅对真正有可编辑上下文的文档查询，避免对无文本控件的 DocMgr 多跑一次同步读锁。
        UINT64 inputScopeMask = _hasTextInputContext ? _QueryInputScopeMask(pDocMgrFocus) : 0;

        // 密码框信号（同 Weasel）：宿主在 context 上置 GUID_COMPARTMENT_KEYBOARD_DISABLED 表示此控件禁用输入法。
        // Chromium 密码框会置位，无痕模式普通可编辑框不会。
        // 它是 context 级，与线程级 `_bKeyboardDisabled`（OnTestKeyDown 全放行的依据）是两回事：命中时键仍会被
        // DLL 吃下，必须靠补 IS_PASSWORD 位驱动抑制门控（IsPasswordSuppressActive / core apply_input_diag），
        // mask 是这条信号唯一的出口，勿删。密码框判据也复用到加词热键门卫：密码框不注册加词热键。
        const UINT64 rawScopeMask = inputScopeMask; // 补位前的原始 InputScope，供诊断区分来源
        _focusIsPassword = (_hasTextInputContext && _IsFocusKeyboardDisabled(pDocMgrFocus)) != FALSE;
        if (_focusIsPassword)
            inputScopeMask |= kScopeBitPassword;
        // 自留一份：IsPasswordSuppressActive 的吃键门控须在 OnTestKeyDown 本地算出（早于 IPC）。
        _focusInputScopeMask = inputScopeMask;

        // 诊断汇总：「这个焦点为什么（不）该输入中文」的全部信号打在同一行，含单独记录的 context 级
        // KEYBOARD_DISABLED（否则只体现为 mask 补位，与宿主报的 IS_PASSWORD 分不开）。
        // 只传已算好的标量：日志宏不做级别短路，参数在级别判定之前求值，焦点热路径上拼字符串即是开销。
        WIND_LOG_DEBUG_FMT(
            L"compat.focus.signals focusSession=%llu hasTextCtx=%d dynFlags=0x%X statFlags=0x%X "
            L"rawScope=0x%llX scopePassword=%d ctxKbdDisabled=%d threadKbdDisabled=%d",
            _focusSessionId, _hasTextInputContext ? 1 : 0, docMgrDynFlags, docMgrStatFlags,
            rawScopeMask,
            (rawScopeMask & kPasswordScopeBits) != 0 ? 1 : 0,
            _focusIsPassword ? 1 : 0, _bKeyboardDisabled ? 1 : 0);

        // 「不可输入」的呈现由协调器单点判定（Rust 侧 InputBlock），DLL 只上报信号，不自己判、不带迟滞。
        // 吃键闸门 IsPasswordSuppressActive() 保留：那要在 IPC 之前给出答案。
        if (_pLangBarItemButton != nullptr)
        {
            _pLangBarItemButton->ForceRefresh();
        }

        // 焦点 caret：先发异步编辑会话请求，再走同步回退链。内联执行的宿主（记事本）会在下面这行里跑完回调，
        // _lastFocusCaretX/Y/Source 当场就是 TSF 值；排队执行的宿主（Word）由回调补发 caret_update。
        // 不能只靠同步链：OnSetFocus 不是按键上下文，同步会话必被拒，回退链交出跨窗口的 Win32 光标却返回 TRUE。
        _lastFocusCaretSource = CARET_SRC_UNKNOWN;
        RequestFocusCaretAsync(pDocMgrFocus);

        LONG caretX = 0, caretY = 0, caretHeight = 0;
        int  caretSource = CARET_SRC_UNKNOWN;
        if (_lastFocusCaretSource != CARET_SRC_UNKNOWN)
        {
            // 异步回调已内联跑完并填好了权威坐标，同步链没有必要再跑一遍。
            caretX       = _lastFocusCaretX;
            caretY       = _lastFocusCaretY;
            caretHeight  = _lastFocusCaretHeight;
            caretSource  = _lastFocusCaretSource;
            WIND_LOG_DEBUG_FMT(L"OnSetFocus: 异步焦点坐标已内联就位 x=%ld y=%ld h=%ld src=%d",
                               caretX, caretY, caretHeight, caretSource);
        }
        else if (!GetCaretPosition(&caretX, &caretY, &caretHeight, &caretSource) && _hasLastKnownCaretPos)
        {
            caretX = _lastKnownCaretX;
            caretY = _lastKnownCaretY;
            caretHeight = _lastKnownCaretHeight;
            caretSource = CARET_SRC_LAST_KNOWN;
            WIND_LOG_INFO_FMT(L"OnSetFocus: using last known caret position x=%ld y=%ld h=%ld", caretX, caretY, caretHeight);
        }
        WIND_LOG_DEBUG_FMT(
            L"compat.focus.caret focusSession=%llu x=%ld y=%ld height=%ld src=%d",
            _focusSessionId, caretX, caretY, caretHeight, caretSource
        );
        _lastFocusCaretX = caretX;
        _lastFocusCaretY = caretY;
        _lastFocusCaretHeight = caretHeight > 0 ? caretHeight : DEFAULT_CARET_HEIGHT;
        _lastFocusCaretSource = caretSource;

        // XamlIsland/transient locked DocMgr 守卫：这类容器文档上 RequestEditSession 返回 TF_E_NOLOCK，
        // 发 focus_gained 会让服务端把组合 replay 进去，用户点走时组合文字在屏幕 (0,0) 处上屏，故跳过。
        // 命中条件见 IsLockedTransientDocMgr：能力位 dynFlags 0x20 必须与身份位 statFlags 0x4 合取，只判前者
        // 会把 WinUI 3 宿主（任务管理器）整个吞掉，下面这行日志用来看守卫有没有介入。
        // 不会有「后续稳定 DocMgr 补一个 gained」：上面 doc_changed 收口据此预判是否发 focus_lost，
        // 改本守卫的命中条件时必须同步那一处。
        if ((docMgrDynFlags & kUiIntegrationDynFlag) && !(docMgrStatFlags & kTransitoryStatFlag))
        {
            WIND_LOG_INFO_FMT(
                L"OnSetFocus: dynFlags 含 UI 集成能力位但非 transient（statFlags=0x%X），照常上报 focus_gained focusSession=%llu",
                docMgrStatFlags, _focusSessionId);
        }

        if (IsLockedTransientDocMgr(docMgrDynFlags, docMgrStatFlags))
        {
            WIND_LOG_INFO_FMT(
                L"OnSetFocus: skipping focus_gained for locked/transient DocMgr dynFlags=0x%X statFlags=0x%X focusSession=%llu",
                docMgrDynFlags, docMgrStatFlags, _focusSessionId);
            // Fall through — sinks and LangBar are already set up above.
            // Do not send focus_gained IPC.
            _BeginFocusGap(L"locked_transient");
        }
        // 无可编辑 context（如 QQ Ctrl+1 切会话）：发 focus_gained 会让服务端把上次组合状态 replay 回来，
        // 宿主没地方接，候选框与服务端缓冲残留。改为显式发 focus_lost 让服务端清空并隐藏 UI。
        else if (!_hasTextInputContext)
        {
            WIND_LOG_INFO_FMT(
                L"OnSetFocus: new DocMgr has no editable context, sending focus_lost focusSession=%llu",
                _focusSessionId);
            if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
            {
                // 同样计入 focusIpcMs：无可编辑上下文的宿主（CAD 绘图区、浏览器非输入区）
                // 走的是本分支而非 focus_gained，不计就会在最该测的场景里恒报 0。
                // SendFocusLost 是 async 写（无响应等待），但写超时仍有 300ms。
                const LONGLONG lostT0 = WindLog::PerfNow();
                // NO_EDIT_CTX：新文档确实没有可输入的地方，残留 buffer 无处可去必须清，
                // 工具栏也该隐藏；但宿主还在前台、输入法仍激活，故不动 ime_active。
                _pIPCClient->SendFocusLost(FOCUS_LOST_REASON_NO_EDIT_CTX);
                _focusLostSent = TRUE;
                _editCtxReported = FALSE; // 已告知服务端"没有可编辑上下文"，勿再补 CTX_LOST
                focusIpcMs += WindLog::PerfMsSince(lostT0);
            }
            _needsFocusRecovery = FALSE;
            _BeginFocusGap(L"no_edit_ctx");
        }
        // SendFocusGained 是同步的（send + ReceiveResponse）：服务端在响应里回传权威模式，保证首个
        // OnTestKeyDown 之前模式就绪，避免切过来首键上屏英文（见 IPCClient::SendFocusGained）。代价是每次
        // 焦点切换在宿主 UI 线程上一次 IPC 往返；重型处理由服务端写响应后异步执行，再经
        // CMD_ACTIVATION_STATUS_PUSH 推回。服务晚于 TSF 启动也能覆盖（内部 lazy connect）。
        else if (_pIPCClient != nullptr)
        {
            // 上报的 disabled 字段统一为线程级 KEYBOARD_DISABLED（与 OnChange 里的 SendInputStateReport 同源），
            // 语义＝「系统禁用了输入法，DLL 已全放行」。密码框（context 级）已折进 mask 的 IS_PASSWORD 位，
            // 传 _focusIsPassword 会让 core 误读成「键已放行」，抑制被自我否决。
            uint8_t inputReason = ComputeInputReason(_bKeyboardDisabled != FALSE, rawScopeMask, _focusIsPassword);
            // 焦点顶层窗口类：服务端据此区分 explorer.exe 的过渡型窗口（任务栏 / Alt+Tab 切换器）与停留型窗口
            // （桌面 / 文件管理器），二者进程名相同。成本是几次进程内调用，与随后的同步 IPC 不在一个量级。
            // 标题只在 core 推开采集时取，否则恒空；取法不发 WM_GETTEXT。
            std::wstring focusRootClass;
            std::wstring focusRootTitle;
            _QueryFocusRootWindowIdentity(pDocMgrFocus, focusRootClass, focusRootTitle);
            // 与 compat.focus.foreground_host（前台窗口类）配对，可在日志里比对焦点顶层窗口与前台窗口是否同一个。
            // 标题是用户数据：任何级别都只记长度。
            WIND_LOG_DEBUG_FMT(L"compat.focus.rootclass focusSession=%llu class=%ls title_len=%u",
                               _focusSessionId, focusRootClass.c_str(), (unsigned)focusRootTitle.size());
            // 单独计时这一段：它是本函数里唯一会阻塞在别的进程上的调用，
            // 需要能和 COM/日志开销分开归因。
            const LONGLONG focusIpcT0 = WindLog::PerfNow();
            const BOOL focusSent = _pIPCClient->SendFocusGained(
                caretX, caretY, caretHeight, inputScopeMask, _bKeyboardDisabled != FALSE, inputReason,
                caretSource, focusRootClass.c_str(), focusRootTitle.c_str());
            focusIpcMs += WindLog::PerfMsSince(focusIpcT0);
            // 排队档的异步回调据此判定是否补发 caret_update。必须在这里置位而非发送前：内联档的回调早已在上面跑完，
            // 那时读到旧值，正确地选择不补发。
            _focusGainedSentForSession = _focusSessionId;
            _focusSentTitleBlind = (focusSent && !IsTitleMatchEnabled()) ? TRUE : FALSE;
            if (focusSent)
            {
                WIND_LOG_DEBUG_FMT(L"FocusGained sent (sync) focusSession=%llu ipc=%.1fms",
                                   _focusSessionId, focusIpcMs);
                _needsFocusRecovery = FALSE;
                _focusLostSent = FALSE; // 新会话开始，下次离开文档时须再发一次 focus_lost
                CancelServiceReadyRetry();
                // 服务端此刻已知「焦点在可编辑控件里」。置位后，焦点离开时
                // _ReportEditContextLost 才会在翻转沿补一条 CTX_LOST。
                _editCtxReported = TRUE;
                _pIPCClient->ClearNeedsSyncFlag();
                _EndFocusGap();
            }
            else
            {
                WIND_LOG_WARN_FMT(L"FocusGained IPC send failed focusSession=%llu", _focusSessionId);
                _needsFocusRecovery = TRUE;
                if (_pLangBarItemButton != nullptr)
                    _pLangBarItemButton->ScheduleFocusRecoveryRetry();
            }
        }

        // 诊断快照（异步、默认关）。放在 focus_gained 之后：那条同步 IPC 挂着首字延迟，采集不能进去。
        // docMgrChanged 必须在下面更新 _pLastActiveDocMgr 之前算，缓存一刷新这个信息就丢了（只有 DLL 知道）。
        // 也刻意放在 transient 跳过分支之外：那正是「首字母上屏」类问题的现场。
        SendDiagSnapshotIfEnabled(pDocMgrFocus, _pLastActiveDocMgr != pDocMgrFocus);

        // 记住本次活跃文档供下次比对。必须 AddRef 保活：旧 DocMgr 释放后新对象可能落在同一地址，
        // 换文档会被误判成抖动而漏清理。locked/transient 不入缓存：否则紧随其后的真实文档会被判成换文档，
        // 清掉刚输入的内容。
        if (!IsLockedTransientDocMgr(docMgrDynFlags, docMgrStatFlags) &&
            _pLastActiveDocMgr != pDocMgrFocus)
        {
            if (_pLastActiveDocMgr != nullptr)
                _pLastActiveDocMgr->Release();
            _pLastActiveDocMgr = pDocMgrFocus;
            _pLastActiveDocMgr->AddRef();
        }

        // 抖动判据用的缓存：无条件记录本次获焦的 DocMgr，transient 也要记。上面那个缓存排除 transient
        // 是为了收口 hint 指向真实文档，这里回答「同一个 doc 又获焦时该不该当成抖动」，两者对 transient
        // 的答案恰好相反，必须分开存。
        if (_pLastFocusedDocMgr != pDocMgrFocus)
        {
            if (_pLastFocusedDocMgr != nullptr)
                _pLastFocusedDocMgr->Release();
            _pLastFocusedDocMgr = pDocMgrFocus;
            _pLastFocusedDocMgr->AddRef();
        }
    }

    // If losing focus (pDocMgrFocus is null)
    if (pDocMgrFocus == nullptr)
    {
        WIND_LOG_DEBUG_FMT(L"DocMgr focus lost focusSession=%llu (ctx_lost only; 输入态清理仍延后到 OnKillThreadFocus)",
                           _focusSessionId);

        if (_pKeyEventSink != nullptr)
        {
            _pKeyEventSink->FlushEnglishStats();
        }

        // 这里刻意不结束组合 / 清输入态 / 复位会话态：DocMgr 级失焦是噪声（Excel 同一 DocMgr 置空再设回），
        // 在此销毁会让首字符直接上屏。输入态清理由两条能分辨真伪的路径承担：另一个文档拿到焦点（OnSetFocus
        // 的 doc_changed 分支）与整个应用失去前台（OnKillThreadFocus）。见 docs/architecture/tsf-docmgr-focus-semantics.md。
        //
        // 工具栏可见性例外：翻错了只是闪一下（UI 层隐藏防抖吸收），漏报则点到非文本框后工具栏永不隐藏。
        // 故补一条 CTX_LOST，它只翻可见性标志、不碰输入态。
        _ReportEditContextLost();

        // Unregister layout sink when losing focus
        _UnadviseTextLayoutSink();
        _UnadviseTextEditSink();

        // 失焦时不销毁 HostWindow（见 OnKillThreadFocus）：locked/transient DocMgr 会跳过 focus_gained，销毁后
        // 无法重建，靠服务端的 WriteHide 隐藏即可。

        _needsFocusRecovery = FALSE;

        _BeginFocusGap(L"null_docmgr");

        // 刚进入恢复就遇到焦点空档：宿主是在把输入转给别的窗口并会重投首键（AutoCAD 绘图区→命令行）。
        // 保留缓冲会让重投的键叠在首键上（gg），按旧语义清掉残留与服务端缓冲。
        if (wind::recover::ShouldAbortOnFocusGap(!_recoverLeftover.empty(), GetTickCount64() - _recoverArmTick))
        {
            _DisarmRecover(L"focus gap");
            if (_pKeyEventSink != nullptr)
                _pKeyEventSink->OnCompositionUnexpectedlyTerminated();
        }

        // 离开文本框：清门卫状态，下方 reeval 会注销加词热键，把 Ctrl+= 还给宿主。
        _hasTextInputContext = FALSE;
        _focusIsPassword = false;
        // 掩码随焦点走：不清会把上个控件的密码位带到新焦点，令抑制门控误放行。
        _focusInputScopeMask = 0;

    }

    // 焦点/文本框上下文变化后重新评估加词热键（gaining/losing 两分支汇合于此）。
    _ReevaluateAddWordHotkey();

    // 慢焦点探针收口（gaining/losing 两分支都经过这里）。见函数开头的说明。
    const double focusTotalMs = WindLog::PerfMsSince(focusProbeT0);
    if (focusTotalMs >= kSlowFocusWarnMs)
    {
        // hasTextCtx 决定走 focus_gained(同步往返) 还是 focus_lost(异步写)，
        // 是归因的关键分支位——CAD 绘图区一类宿主预期为 0。
        WIND_LOG_WARN_FMT(
            L"perf.focus.slow total=%.1fms ipc=%.1fms focusSession=%llu gaining=%d hasTextCtx=%d",
            focusTotalMs, focusIpcMs, _focusSessionId,
            pDocMgrFocus != nullptr ? 1 : 0, _hasTextInputContext ? 1 : 0);
    }

    return S_OK;
}

STDAPI CTextService::OnPushContext(ITfContext* pContext)
{
    return S_OK;
}

STDAPI CTextService::OnPopContext(ITfContext* pContext)
{
    return S_OK;
}

BOOL CTextService::_InitKeyEventSink()
{
    _pKeyEventSink = new CKeyEventSink(this);
    if (_pKeyEventSink == nullptr)
        return FALSE;

    return _pKeyEventSink->Initialize();
}

void CTextService::_UninitKeyEventSink()
{
    if (_pKeyEventSink != nullptr)
    {
        _pKeyEventSink->Uninitialize();
        _pKeyEventSink->Release();
        _pKeyEventSink = nullptr;
    }
}

// ============================================================================
// State sync helpers
// ============================================================================

void CTextService::_SyncStateFromResponse(const ServiceResponse& response)
{
    if (response.type != ResponseType::StatusUpdate)
        return;

    _SetChineseMode(response.IsChineseMode());
    _bFullWidth = response.IsFullWidth();
    _bChinesePunct = response.IsChinesePunct();
    _bSoftKeyboard = response.IsSoftKeyboard();
    _bSoftKeyboardKeys = response.IsSoftKeyboardKeys();
    // 热键模式会话：无条件镜像服务端的值（level-triggered，见 STATUS_HOTKEY_SESSION），服务端是唯一权威，
    // DLL 不做本地推断。
    if (_pKeyEventSink != nullptr)
    {
        _pKeyEventSink->SetCandidateSessionActive(response.IsHotkeySession());
    }

    // compartment 如实反映中英模式（值语义），见 _SetOpenCloseCompartment 定义处的说明。
    _SetOpenCloseCompartment(_bChineseMode);
    // Sync真实中英文模式到 INPUTMODE_CONVERSION compartment（供 KBLSwitch / 任务栏读取）
    _SetConversionMode(_bChineseMode);

    // Sync full status to LangBarItemButton
    if (_pLangBarItemButton != nullptr)
    {
        BOOL bCapsLock = (GetKeyState(VK_CAPITAL) & 0x0001) != 0;
        _pLangBarItemButton->UpdateFullStatus(
            response.IsChineseMode(),
            response.IsFullWidth(),
            response.IsChinesePunct(),
            response.IsToolbarVisible(),
            bCapsLock,
            response.iconLabel.empty() ? nullptr : response.iconLabel.c_str()
        );
    }

    // Update hotkey whitelist if present
    if (response.HasHotkeys() && _pHotkeyManager != nullptr)
    {
        WIND_LOG_DEBUG(L"Updating hotkey whitelist from state sync\n");
        _pHotkeyManager->UpdateHotkeys(
            response.keyDownHotkeys,
            response.keyUpHotkeys
        );
    }

    WIND_LOG_INFO_FMT(L"State synced: mode=%d, width=%d, punct=%d, toolbar=%d, hostRender=%d\n",
        response.IsChineseMode(), response.IsFullWidth(),
        response.IsChinesePunct(), response.IsToolbarVisible(), response.IsHostRenderAvailable());
}

void CTextService::_DestroyHostWindow()
{
    // Destroy non-candidate (owned) windows first, then the candidate (owner) last, so
    // owned tooltip/status windows are torn down before their z-order owner disappears.
    for (int k = HOST_WINDOW_KIND_COUNT - 1; k >= 0; --k)
    {
        if (_pHostWindow[k] != nullptr)
        {
            _pHostWindow[k]->Uninitialize();
            delete _pHostWindow[k];
            _pHostWindow[k] = nullptr;
        }
    }
}

void CTextService::_EnsureHostRenderSetup(const ServiceResponse& response, BOOL forceRefresh)
{
    if (_pIPCClient == nullptr || !_pIPCClient->IsConnected())
        return;

    CHostWindow*& candidate = _pHostWindow[HOST_WINDOW_CANDIDATE];
    BOOL hadHostWindow = (candidate != nullptr);
    BOOL hostRenderAvailable = response.IsHostRenderAvailable();
    BOOL shouldRetryExistingHost = forceRefresh && hadHostWindow && !hostRenderAvailable;

    if (!hostRenderAvailable && !shouldRetryExistingHost)
    {
        if (forceRefresh && hadHostWindow)
        {
            WIND_LOG_INFO(L"Host render unavailable after refresh, disabling existing host windows\n");
            _DestroyHostWindow();
        }
        return;
    }

    if (shouldRetryExistingHost)
    {
        WIND_LOG_WARN(L"Host render flag missing after reconnect, retrying setup because host window was previously active\n");
    }

    // 唯一调用点（ApplyActivationStatusResponse）传 forceRefresh=TRUE：activation 即重建。band 复核由
    // RecheckHostBand 在输入首键时做。
    if (candidate != nullptr)
    {
        WIND_LOG_INFO(L"Refreshing host render windows after service reconnection\n");
        _DestroyHostWindow();
    }

    _CreateHostWindows(nullptr);
}

BOOL CTextService::_HostBandNeedsRebuild()
{
    CHostWindow* candidate = _pHostWindow[HOST_WINDOW_CANDIDATE];
    if (candidate == nullptr)
        return FALSE;
    // 没拿到 owner 也要重建：候选窗不被搜索面板 own，同 band 内仍可能压在面板下面。
    // 句柄失效也要：owner 被宿主销毁时被 own 的候选窗会被连带销毁。
    return wind::hostband::NeedsRebuild(candidate->GetForegroundHostBand(),
                                        candidate->GetRequestedBand(),
                                        candidate->HasOwner() != FALSE,
                                        IsWindow(candidate->GetHwnd()) != FALSE)
               ? TRUE
               : FALSE;
}

// 建窗时的 band 是当时探的：SearchHost 开机预启动、界面未显示时就收到 activation，建出的候选窗被开始菜单
// 盖住，且 activation 推送不会重来。故在一段输入的第一个键（宿主界面必定已显示）按前台 band 复核一次；
// 不放在焦点进入：SearchHost 二次聚焦会跳过 focus_gained，键才是必经之路。
//
// 跑在键路径上：不需重建时只是两个系统调用，且只在本线程有 host 候选窗时发生。整组重建而不是 UpdateBand：
// tooltip/状态窗被候选窗 own，只重建候选会让它们挂在已销毁的 owner 上。
void CTextService::RecheckHostBand(const wchar_t* why)
{
    if (!_HostBandNeedsRebuild())
        return;
    if (_pIPCClient == nullptr || !_pIPCClient->IsConnected())
        return;

    CHostWindow* candidate = _pHostWindow[HOST_WINDOW_CANDIDATE];
    // 旧窗的依据随上报带走：新窗在 recheck 后必然 requested=前台、owner=true，不带旧值
    // 中央日志里就分不清当初是 band 判错还是只缺 owner（本地这行日志在 SearchHost 里落不了盘）。
    wind::hostband::Placement prev = {};
    prev.probedBand = candidate->GetProbedHostBand();
    prev.requestedBand = candidate->GetRequestedBand();
    prev.actualBand = candidate->GetCurrentBand();
    prev.hasOwner = candidate->HasOwner() != FALSE;
    WIND_LOG_INFO_FMT(L"Host band recheck (%ls): foreground band=%u, window requested=%u actual=%u owner=%d, recreating\n",
        why, candidate->GetForegroundHostBand(), prev.requestedBand, prev.actualBand, prev.hasOwner ? 1 : 0);
    _DestroyHostWindow();
    _CreateHostWindows(&prev);
}

void CTextService::_CreateHostWindows(const wind::hostband::Placement* prev)
{
    WIND_LOG_INFO(L"Host render available, requesting setup\n");

    ServiceResponse hrResponse;
    if (_pIPCClient->SendHostRenderRequest(hrResponse) &&
        hrResponse.type == ResponseType::HostRenderSetup &&
        !hrResponse.hostRenderSetups.empty())
    {
        // Create the candidate window first (pass 0) so its hwnd can serve as the z-order
        // owner for the tooltip/status windows (pass 1) — owned windows always sit above
        // their owner, so the tooltip never gets occluded by the candidate band window.
        HWND candidateOwner = NULL;
        for (int pass = 0; pass < 2; ++pass)
        {
            for (size_t i = 0; i < hrResponse.hostRenderSetups.size(); ++i)
            {
                const HostRenderSetupInfo& info = hrResponse.hostRenderSetups[i];
                bool isCandidate = (info.windowKind == HOST_WINDOW_CANDIDATE);
                if ((pass == 0) != isCandidate)
                    continue; // pass 0: candidate only; pass 1: the rest
                if (info.windowKind >= HOST_WINDOW_KIND_COUNT)
                    continue;
                if (info.shmName.empty() || info.eventName.empty())
                    continue;
                if (_pHostWindow[info.windowKind] != nullptr)
                    continue; // already created (defensive against duplicate entries)

                CHostWindow* win = new CHostWindow();
                HWND owner = isCandidate ? NULL : candidateOwner; // others owned by candidate
                if (!win->Initialize(
                        info.shmName.c_str(),
                        info.eventName.c_str(),
                        info.maxBufferSize,
                        hrResponse.hostRenderInstanceId, // stamp frames' target against this
                        _pIPCClient, // weak ref: candidate routes mouse events back to the service
                        (HostWindowKind)info.windowKind,
                        owner))
                {
                    WIND_LOG_WARN_FMT(L"Host window kind=%u init failed, skipping\n", info.windowKind);
                    delete win;
                    continue;
                }
                _pHostWindow[info.windowKind] = win;
                if (isCandidate)
                    candidateOwner = win->GetHwnd();
            }
        }

        if (_pHostWindow[HOST_WINDOW_CANDIDATE] != nullptr)
        {
            WIND_LOG_INFO(L"Host windows initialized successfully\n");
            _ReportHostRenderPlaced(prev);
        }
        else
        {
            WIND_LOG_WARN(L"Candidate host window missing after setup, falling back to Go window\n");
            // 通知服务端集中记日志并提示用户候选已回退到本地窗口（如受限 UWP 宿主里连 band=0 的
            // CreateWindowInBand 都失败）。
            uint32_t reason = HOST_RENDER_FAIL_WINDOW_CREATE;
            _pIPCClient->SendAsync(CMD_HOST_RENDER_FAILED, &reason, sizeof(reason));
        }
    }
    else
    {
        WIND_LOG_WARN(L"Host render setup request failed, falling back to Go window\n");
        uint32_t reason = HOST_RENDER_FAIL_WINDOW_CREATE;
        _pIPCClient->SendAsync(CMD_HOST_RENDER_FAILED, &reason, sizeof(reason));
    }
}

// 中央日志里留一笔「据什么、往哪建、落在哪」。AppContainer 宿主（SearchHost）写不了
// tsf_log，不靠这条就看不到它的 band 判定。低频诊断，按协议约定走 CMD_EXT 信封。
void CTextService::_ReportHostRenderPlaced(const wind::hostband::Placement* prev)
{
    CHostWindow* candidate = _pHostWindow[HOST_WINDOW_CANDIDATE];
    if (candidate == nullptr || _pIPCClient == nullptr)
        return;
    wind::hostband::Placement now = {};
    now.probedBand = candidate->GetProbedHostBand();
    now.requestedBand = candidate->GetRequestedBand();
    now.actualBand = candidate->GetCurrentBand();
    now.hasOwner = candidate->HasOwner() != FALSE;
    now.hasPassWindow = candidate->HasPassWindow() != FALSE;
    std::vector<uint8_t> payload = wind::hostband::EncodeExt(
        wind::hostband::kPlacedKind, wind::hostband::PlacedBody(GetCurrentProcessId(), now, prev));
    _pIPCClient->SendAsync(CMD_EXT, payload.data(), (uint32_t)payload.size());
}

// ============================================================================
// Compartment event sink for GUID_COMPARTMENT_KEYBOARD_OPENCLOSE
// ============================================================================

BOOL CTextService::_InitOpenCloseCompartment()
{
    if (_pThreadMgr == nullptr)
        return FALSE;

    ITfCompartmentMgr* pCompMgr = nullptr;
    HRESULT hr = _pThreadMgr->QueryInterface(IID_ITfCompartmentMgr, (void**)&pCompMgr);
    if (FAILED(hr) || pCompMgr == nullptr)
    {
        WIND_LOG_ERROR(L"Failed to get ITfCompartmentMgr from ThreadMgr\n");
        return FALSE;
    }

    ITfCompartment* pCompartment = nullptr;
    hr = pCompMgr->GetCompartment(GUID_COMPARTMENT_KEYBOARD_OPENCLOSE, &pCompartment);
    pCompMgr->Release();

    if (FAILED(hr) || pCompartment == nullptr)
    {
        WIND_LOG_ERROR(L"Failed to get GUID_COMPARTMENT_KEYBOARD_OPENCLOSE compartment\n");
        return FALSE;
    }

    // Set initial state to open (Chinese mode)
    VARIANT var;
    var.vt = VT_I4;
    var.lVal = TRUE;
    pCompartment->SetValue(_tfClientId, &var);

    // Advise for changes
    ITfSource* pSource = nullptr;
    hr = pCompartment->QueryInterface(IID_ITfSource, (void**)&pSource);
    pCompartment->Release();

    if (FAILED(hr) || pSource == nullptr)
    {
        WIND_LOG_ERROR(L"Failed to get ITfSource from compartment\n");
        return FALSE;
    }

    hr = pSource->AdviseSink(IID_ITfCompartmentEventSink, (ITfCompartmentEventSink*)this, &_dwOpenCloseSinkCookie);
    pSource->Release();

    if (FAILED(hr))
    {
        WIND_LOG_ERROR(L"Failed to advise compartment event sink\n");
        _dwOpenCloseSinkCookie = TF_INVALID_COOKIE;
        return FALSE;
    }

    WIND_LOG_DEBUG(L"Compartment OPENCLOSE sink advised successfully\n");
    return TRUE;
}

void CTextService::_UninitOpenCloseCompartment()
{
    if (_dwOpenCloseSinkCookie == TF_INVALID_COOKIE || _pThreadMgr == nullptr)
        return;

    ITfCompartmentMgr* pCompMgr = nullptr;
    if (SUCCEEDED(_pThreadMgr->QueryInterface(IID_ITfCompartmentMgr, (void**)&pCompMgr)) && pCompMgr != nullptr)
    {
        ITfCompartment* pCompartment = nullptr;
        if (SUCCEEDED(pCompMgr->GetCompartment(GUID_COMPARTMENT_KEYBOARD_OPENCLOSE, &pCompartment)) && pCompartment != nullptr)
        {
            ITfSource* pSource = nullptr;
            if (SUCCEEDED(pCompartment->QueryInterface(IID_ITfSource, (void**)&pSource)) && pSource != nullptr)
            {
                pSource->UnadviseSink(_dwOpenCloseSinkCookie);
                pSource->Release();
            }
            pCompartment->Release();
        }
        pCompMgr->Release();
    }

    _dwOpenCloseSinkCookie = TF_INVALID_COOKIE;
    WIND_LOG_DEBUG(L"Compartment OPENCLOSE sink unadvised\n");
}

LONG CTextService::GetOpenCloseCompartmentValue()
{
    if (_pThreadMgr == nullptr)
        return -1;

    ITfCompartmentMgr* pCompMgr = nullptr;
    if (FAILED(_pThreadMgr->QueryInterface(IID_ITfCompartmentMgr, (void**)&pCompMgr)) || pCompMgr == nullptr)
        return -1;

    ITfCompartment* pCompartment = nullptr;
    HRESULT hr = pCompMgr->GetCompartment(GUID_COMPARTMENT_KEYBOARD_OPENCLOSE, &pCompartment);
    pCompMgr->Release();
    if (FAILED(hr) || pCompartment == nullptr)
        return -1;

    VARIANT var;
    VariantInit(&var);
    hr = pCompartment->GetValue(&var);
    pCompartment->Release();

    if (FAILED(hr) || var.vt != VT_I4)
        return -1;
    return var.lVal;
}

// 写 OPENCLOSE compartment，使其如实等于当前中英模式（0=英文/IME 关，1=中文/IME 开）。
//
// 不变量：任何改变 _bChineseMode 的路径都必须调用本函数，脱节时宿主会保存并恢复错误状态（如 gvim）。
// 不要改回钉死为 1：TSF 在 compartment=0 时照样回调 OnTestKeyDown，见
// docs/architecture/tsf-openclose-compartment-semantics.md。
BOOL CTextService::_SetOpenCloseCompartment(BOOL bOpen)
{
    if (_pThreadMgr == nullptr)
        return FALSE;

    ITfCompartmentMgr* pCompMgr = nullptr;
    HRESULT hr = _pThreadMgr->QueryInterface(IID_ITfCompartmentMgr, (void**)&pCompMgr);
    if (FAILED(hr) || pCompMgr == nullptr)
        return FALSE;

    ITfCompartment* pCompartment = nullptr;
    hr = pCompMgr->GetCompartment(GUID_COMPARTMENT_KEYBOARD_OPENCLOSE, &pCompartment);
    pCompMgr->Release();

    if (FAILED(hr) || pCompartment == nullptr)
        return FALSE;

    // 值没变就别写：ITfCompartment::SetValue 不做值比较，同值写入也会向宿主和所有 sink 广播 OnChange。
    // _SyncStateFromResponse 每收到一次状态推送就调用本函数，而推送广播给所有加载本 DLL 的进程，
    // 不加守卫则任何一个应用里的状态变化都会让每个宿主收到一次「IME 开关变了」。_SetConversionMode 有同样的守卫。
    const LONG desired = bOpen ? TRUE : FALSE;
    VARIANT cur;
    VariantInit(&cur);
    if (SUCCEEDED(pCompartment->GetValue(&cur)) && cur.vt == VT_I4 && cur.lVal == desired)
    {
        pCompartment->Release();
        return TRUE;
    }

    // Set guard to prevent re-entrant OnChange
    _bInCompartmentChange = TRUE;

    VARIANT var;
    var.vt = VT_I4;
    var.lVal = desired;
    hr = pCompartment->SetValue(_tfClientId, &var);
    pCompartment->Release();

    _bInCompartmentChange = FALSE;

    // OPENCLOSE 是 Windows 语言指示器的数据源，这条日志回答「系统指示器被翻了几次」（我们的图标全程正确
    // 而用户仍见闪烁时的唯一解释）。上面的幂等守卫吃掉同值写入，能走到这里的每一条都是真的翻转。
    WIND_LOG_DEBUG_FMT(L"compat.openclose.write value=%d hr=0x%08X tid=%lu inst=0x%p",
                       (int)desired, (uint32_t)hr, GetCurrentThreadId(), this);

    return SUCCEEDED(hr);
}

// Ctrl 当前是否按住。两条 compartment 路径都要在写 IPC 的那一刻取：它是服务端「忽略软件切换中英」放行
// 系统热键的唯一判据（Ctrl+Space 与宿主切中英走同一条通路、载荷相同）。
// 两个 API 都问：`GetAsyncKeyState` 读硬件当下状态，`GetKeyState` 读本线程消息队列的同步状态，
// 而 compartment 回调不保证排在按键消息之后。
static bool _CtrlHeldNow()
{
    return (GetAsyncKeyState(VK_CONTROL) & 0x8000) != 0 || (GetKeyState(VK_CONTROL) & 0x8000) != 0;
}

STDAPI CTextService::OnChange(REFGUID rguid)
{
    if (_pThreadMgr == nullptr)
        return S_OK;

    // ================================================================
    // GUID_COMPARTMENT_KEYBOARD_DISABLED
    // ================================================================
    if (IsEqualGUID(rguid, GUID_COMPARTMENT_KEYBOARD_DISABLED))
    {
        ITfCompartmentMgr* pCompMgr = nullptr;
        HRESULT hr = _pThreadMgr->QueryInterface(IID_ITfCompartmentMgr, (void**)&pCompMgr);
        if (FAILED(hr) || pCompMgr == nullptr)
            return S_OK;

        ITfCompartment* pCompartment = nullptr;
        hr = pCompMgr->GetCompartment(GUID_COMPARTMENT_KEYBOARD_DISABLED, &pCompartment);
        pCompMgr->Release();

        if (FAILED(hr) || pCompartment == nullptr)
            return S_OK;

        VARIANT var;
        VariantInit(&var);
        hr = pCompartment->GetValue(&var);
        pCompartment->Release();

        if (FAILED(hr) || var.vt != VT_I4)
            return S_OK;

        BOOL bDisabled = (var.lVal != 0);
        if (_bKeyboardDisabled == bDisabled)
            return S_OK;

        _bKeyboardDisabled = bDisabled;

        WIND_LOG_INFO_FMT(L"Compartment KEYBOARD_DISABLED changed: %d\n", bDisabled);

        // End composition when keyboard becomes disabled
        if (bDisabled)
            EndComposition();

        // Update language bar to show disabled state
        if (_pLangBarItemButton != nullptr)
            _pLangBarItemButton->UpdateKeyboardDisabled(bDisabled);

        // 诊断 HUD：compartment 变更是「焦点未变但禁用态翻转」（如 SPA 内原地跳到密码框），不触发 OnSetFocus，
        // 故单独上报一次 input_state_report。mask 用当前焦点 DocMgr 重新查询，只算一次。
        if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
        {
            ITfDocumentMgr* pDocMgrCur = nullptr;
            UINT64 curMask = 0;
            if (SUCCEEDED(_pThreadMgr->GetFocus(&pDocMgrCur)) && pDocMgrCur != nullptr)
            {
                curMask = _QueryInputScopeMask(pDocMgrCur);
                pDocMgrCur->Release();
            }
            // 与 core 同步更新自留掩码：本路径是「焦点未变但禁用态翻转」（SPA 原地跳到
            // 密码框），不走 OnSetFocus，不更新则抑制门控会一直用旧焦点的掩码。
            _focusInputScopeMask = curMask;
            uint8_t curReason = ComputeInputReason(bDisabled != FALSE, curMask, false);
            _pIPCClient->SendInputStateReport(GetCurrentProcessId(), bDisabled != FALSE, curReason, curMask);
            // 窗口/上下文快照也补一份，否则 HUD 的输入态那半更新、窗口那半停在上一次焦点，自相矛盾。
            // docMgrChanged=FALSE：本路径按定义没换文档。
            ITfDocumentMgr* pDocMgrDiag = nullptr;
            if (SUCCEEDED(_pThreadMgr->GetFocus(&pDocMgrDiag)) && pDocMgrDiag != nullptr)
            {
                SendDiagSnapshotIfEnabled(pDocMgrDiag, FALSE);
                pDocMgrDiag->Release();
            }
        }

        return S_OK;
    }

    // ================================================================
    // GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION
    //
    // 外部工具（如 KBLSwitch 按应用锁定中英文）会写入此 compartment。
    // 我们读取 IME_CMODE_NATIVE 位并按需切换内部模式，使外部锁定生效。
    // ================================================================
    if (IsEqualGUID(rguid, GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION))
    {
        if (_bInConversionChange)
            return S_OK;  // 自身写入引起的通知，跳过

        // 无前台焦点时的 compartment 变化只能是系统切换输入法等噪声（Ctrl+Space 与 KBLSwitch 都针对前台应用），
        // 忽略，防止污染服务端权威模式。判据必须是 _hasThreadFocus（本应用在前台），不是 _hasFocus（DocMgr 级）：
        // 最小化的记事本仍残留 DocMgr 焦点，会把用户操作别的输入法引起的变化上报成全局模式切换；反过来
        // 前台但 DocMgr 焦点未建立时会丢掉真实切换。
        WIND_LOG_INFO_FMT(L"compat.conversion.onchange hasFocus=%d hasThreadFocus=%d curMode=%d\n",
                          _hasFocus ? 1 : 0, _hasThreadFocus ? 1 : 0, _bChineseMode ? 1 : 0);
        if (!_hasThreadFocus)
        {
            WIND_LOG_INFO(L"Compartment CONVERSION changed while not foreground, ignored\n");
            return S_OK;
        }
        // 激活静默期：刚 ActivateEx 完时系统会写 compartment 做初始化同步，不是用户操作；激活时本应用正是前台，
        // _hasThreadFocus 挡不住它，故显式加窗口。
        if (GetTickCount64() - _lastActivateTick < kActivateSettleMs)
        {
            WIND_LOG_INFO(L"Compartment CONVERSION changed within activate settle window, ignored\n");
            return S_OK;
        }

        ITfCompartmentMgr* pCompMgr = nullptr;
        HRESULT hr = _pThreadMgr->QueryInterface(IID_ITfCompartmentMgr, (void**)&pCompMgr);
        if (FAILED(hr) || pCompMgr == nullptr)
            return S_OK;

        ITfCompartment* pCompartment = nullptr;
        hr = pCompMgr->GetCompartment(GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION, &pCompartment);
        pCompMgr->Release();

        if (FAILED(hr) || pCompartment == nullptr)
            return S_OK;

        VARIANT var;
        VariantInit(&var);
        hr = pCompartment->GetValue(&var);
        pCompartment->Release();

        if (FAILED(hr) || var.vt != VT_I4)
            return S_OK;

        BOOL bWantChinese = ((DWORD)var.lVal & IME_CMODE_NATIVE) ? TRUE : FALSE;
        if (bWantChinese == _bChineseMode)
            return S_OK;  // 与当前一致，无需切换

        WIND_LOG_INFO_FMT(L"Compartment CONVERSION changed externally: %s -> %s\n",
            _bChineseMode ? L"Chinese" : L"English",
            bWantChinese ? L"Chinese" : L"English");

        // 与 OPENCLOSE 路径一致：清状态、通知服务端、刷新 LangBar。
        if (_pKeyEventSink != nullptr)
            _pKeyEventSink->FlushEnglishStats();

        EndComposition();
        ResetComposingState(TRUE);  // 中英切换保留配对状态：光标与已插入的右符号都没变
        NotifyCandidatesVisibilityChanged(FALSE);  // 同 _ApplyModeSwitch：收口候选 UIElement

        BOOL newChineseMode = bWantChinese;
        if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
        {
            ServiceResponse response;
            if (_pIPCClient->SendSystemModeSwitch(newChineseMode != FALSE,
                                                 ModeSwitchSource::CompartmentConversion,
                                                 _CtrlHeldNow(), response))
            {
                if (response.type == ResponseType::CommitText && !response.text.empty())
                    CommitText(response.text);
                if (response.type == ResponseType::StatusUpdate || response.type == ResponseType::CommitText)
                    newChineseMode = response.IsChineseMode() ? TRUE : FALSE;
            }
        }

        _SetChineseMode(newChineseMode);

        if (_pLangBarItemButton != nullptr)
            _pLangBarItemButton->UpdateLangBarButton(_bChineseMode);

        // 若服务端把模式仲裁成了与外部请求不同的值，回写 compartment 保持一致。
        if (newChineseMode != bWantChinese)
            _SetConversionMode(newChineseMode);

        return S_OK;
    }

    // ================================================================
    // GUID_COMPARTMENT_KEYBOARD_OPENCLOSE
    // ================================================================
    if (!IsEqualGUID(rguid, GUID_COMPARTMENT_KEYBOARD_OPENCLOSE))
        return S_OK;

    // Avoid re-entrant handling when we set the compartment ourselves
    if (_bInCompartmentChange)
        return S_OK;

    // 无焦点时的 OPENCLOSE 变化是系统切换输入法等噪声，照单全收会改写服务端权威模式，故忽略，下次聚焦/激活
    // 从服务端同步权威值。判据须为 _hasThreadFocus（前台），而非 _hasFocus（DocMgr 级）。
    // tid/inst：同一进程可能有多个 TSF 线程各持一个 CTextService，不打实例身份分不清
    // 「回调落在另一实例」与「有人把它清零了」。
    WIND_LOG_INFO_FMT(L"compat.openclose.onchange hasFocus=%d hasThreadFocus=%d fg=%d curMode=%d tid=%lu inst=0x%p\n",
                      _hasFocus ? 1 : 0, _hasThreadFocus ? 1 : 0, _isProcessForeground ? 1 : 0,
                      _bChineseMode ? 1 : 0, GetCurrentThreadId(), this);
    if (!_hasThreadFocus)
    {
        WIND_LOG_INFO(L"Compartment OPENCLOSE changed while not foreground, ignored\n");
        return S_OK;
    }
    // 激活静默期，理由同 CONVERSION 路径。
    if (GetTickCount64() - _lastActivateTick < kActivateSettleMs)
    {
        WIND_LOG_INFO(L"Compartment OPENCLOSE changed within activate settle window, ignored\n");
        return S_OK;
    }

    // Read current compartment value
    ITfCompartmentMgr* pCompMgr = nullptr;
    HRESULT hr = _pThreadMgr->QueryInterface(IID_ITfCompartmentMgr, (void**)&pCompMgr);
    if (FAILED(hr) || pCompMgr == nullptr)
        return S_OK;

    ITfCompartment* pCompartment = nullptr;
    hr = pCompMgr->GetCompartment(GUID_COMPARTMENT_KEYBOARD_OPENCLOSE, &pCompartment);
    pCompMgr->Release();

    if (FAILED(hr) || pCompartment == nullptr)
        return S_OK;

    VARIANT var;
    VariantInit(&var);
    hr = pCompartment->GetValue(&var);
    pCompartment->Release();

    if (FAILED(hr) || var.vt != VT_I4)
        return S_OK;

    BOOL bOpen = (var.lVal != 0);
    WIND_LOG_INFO_FMT(L"Compartment OPENCLOSE changed: %d (current mode: %s)\n",
        bOpen, _bChineseMode ? L"Chinese" : L"English");

    // CapsLock 联动噪声抑制：CapsLock 状态变化（物理按键或服务端 cancel_on_mode_switch 的注入）后，
    // Windows 会异步联动写 OPENCLOSE（延迟约 0.5~1s）。照常 toggle 并上报会与服务端的 CapsLock 注入
    // 形成振荡回路，故短窗口内把 compartment 拉回真实模式，不切换不上报。
    //
    // 例外（勿删）：开大写后立刻按 Ctrl+Space 的真实切换落在同一时间窗。系统热键触发时 Ctrl 仍按住，
    // 联动写入则无伴随按键，故 Ctrl 按住 → 放行。
    if (GetTickCount64() - _lastCapsKeyTick < 1500)
    {
        BOOL ctrlHeld = (GetAsyncKeyState(VK_CONTROL) & 0x8000) || (GetKeyState(VK_CONTROL) & 0x8000);
        if (!ctrlHeld)
        {
            WIND_LOG_INFO(L"Compartment OPENCLOSE changed right after CapsLock activity (no Ctrl held), suppressed (restore mode)\n");
            _SetOpenCloseCompartment(_bChineseMode);
            return S_OK;
        }
        WIND_LOG_INFO(L"Compartment OPENCLOSE changed within CapsLock window but Ctrl held -> treat as user toggle\n");
    }

    // 严格值语义：compartment 的值就是中英状态的真相（0=英文/IME 关，1=中文/IME 开），与其它输入法
    // 及 TSF/IMM 对宿主的契约一致。系统热键取反 compartment ⇒ 直接按值切换，不需要看见那个按键。
    // 放弃「钉死为 1」的原因见 docs/architecture/tsf-openclose-compartment-semantics.md。
    //
    // 不变量：任何改变 _bChineseMode 的路径都必须同步写 compartment（_SetOpenCloseCompartment），
    // 漏一处就会让它与实际模式脱节，而它是对宿主的唯一真话。
    // 这里 compartment 已由系统/宿主写好，故 compartmentAlreadySet=TRUE。
    return _ApplyModeSwitch(bOpen, TRUE, ModeSwitchSource::CompartmentOpenClose);
}

// 应用一次中英模式切换：刷统计、结束组合、通知服务端、落 _bChineseMode 与两个 compartment。
// 两条路径共用，差别只在 compartment 由谁写：
//   compartmentAlreadySet=TRUE  —— OPENCLOSE 变化是系统/宿主写的，值已经对，仅在服务端
//                                  仲裁出不同模式时才回写；
//   compartmentAlreadySet=FALSE —— 我们是发起方（按键侧兜底），compartment 还停在旧值，
//                                  必须无条件写，否则违反「改 _bChineseMode 必同步写
//                                  compartment」的不变量，对宿主说的就是假话。
// source 只进日志，用于把两条路径在排查时分开。
// 日志用的来源名。排查锚点，勿改字面：`Mode switch via compartment|ctrl_space_key` 写在
// docs/architecture/tsf-openclose-compartment-semantics.md 的排查步骤里。
static const WCHAR* _ModeSwitchSourceName(ModeSwitchSource source)
{
    switch (source)
    {
    case ModeSwitchSource::CompartmentOpenClose:  return L"compartment";
    case ModeSwitchSource::CompartmentConversion: return L"conversion";
    case ModeSwitchSource::CtrlSpaceKey:          return L"ctrl_space_key";
    default:                                      return L"unknown";
    }
}

HRESULT CTextService::_ApplyModeSwitch(BOOL requestedMode, BOOL compartmentAlreadySet, ModeSwitchSource source)
{
    BOOL newChineseMode = requestedMode;
    const WCHAR* sourceName = _ModeSwitchSourceName(source);
    // 服务端的「忽略软件切换中英」靠这一位放行系统热键，必须在这一刻取：系统热键（Ctrl+Space）翻 compartment
    // 时 Ctrl 尚未释放，宿主自己写则没有伴随按键。
    const bool ctrlHeld = _CtrlHeldNow();

    // 值与当前模式一致：宿主重复下发同一状态（gvim 每次 ESC 都写 0）或系统联动噪声。早退不做任何副作用，
    // 否则每次都白跑 EndComposition + 同步 IPC + 刷 LangBar/工具栏，并把英文统计切碎。值语义下无需回写。
    if (newChineseMode == _bChineseMode)
    {
        WIND_LOG_INFO_FMT(L"Mode request via %s (%d) matches current mode (%s), no-op\n",
            sourceName, requestedMode, _bChineseMode ? L"Chinese" : L"English");
        return S_OK;
    }

    WIND_LOG_INFO_FMT(L"Mode switch via %s: %s -> %s\n",
        sourceName,
        _bChineseMode ? L"Chinese" : L"English",
        newChineseMode ? L"Chinese" : L"English");

    // 已知限制：下面几步在同步 IPC 之前执行，而服务端是否采纳这次切换要 IPC 之后才知道，于是被 per-app
    // `ignore_host_ime_switch` 拒绝的那次，组合照样已被终止、英文统计段也已切开。暂不修：宿主关 IME 几乎
    // 总发生在焦点离开输入框时，组合本就会被终止。要修的形状：先 `SendSystemModeSwitch` 拿仲裁结果，
    // 等于当前模式则只修 compartment 后早退，否则再 Flush/End/Reset 并按回包 CommitText；届时须重测
    // `take_input_on_mode_switch` 的回包路径。

    // Flush English stats before any mode switch
    if (_pKeyEventSink != nullptr)
        _pKeyEventSink->FlushEnglishStats();

    // End any active composition since we're switching modes
    EndComposition();
    ResetComposingState(TRUE);  // 中英切换保留配对状态：光标与已插入的右符号都没变
    // 候选 UIElement 也要收口：_SetChineseMode 切到英文只会经 OnModeReevaluate 卸候选热键，不会 EndUIElement，
    // 宿主（Chrome 地址栏）会一直当输入法候选窗还开着而压住自己的联想，直到下次上屏才恢复。
    NotifyCandidatesVisibilityChanged(FALSE);

    // 通知服务端模式切换（同步：可能回带待定输入的 CommitText）
    if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
    {
        ServiceResponse response;
        if (_pIPCClient->SendSystemModeSwitch(newChineseMode != FALSE, source, ctrlHeld, response))
        {
            if (response.type == ResponseType::CommitText && !response.text.empty())
            {
                CommitText(response.text);
                WIND_LOG_INFO_FMT(L"SystemModeSwitch: committed pending text (len=%zu)\n", response.text.size());
            }
            if (response.type == ResponseType::StatusUpdate || response.type == ResponseType::CommitText)
            {
                newChineseMode = response.IsChineseMode() ? TRUE : FALSE;
            }
        }
        else
        {
            WIND_LOG_WARN(L"SystemModeSwitch IPC failed, proceeding with local toggle\n");
        }
    }

    _SetChineseMode(newChineseMode);

    if (_pLangBarItemButton != nullptr)
        _pLangBarItemButton->UpdateLangBarButton(_bChineseMode);

    // compartment 通常已等于新模式，无需回写。例外：服务端仲裁出不同模式（如密码框强制英文）时，
    // 必须把它拉回真实模式。在 OnChange 上下文里写未必生效（内部「值相同就不写」的守卫会读到未落定的
    // 旧值），真正兜底的是状态推送回来后 _SyncStateFromResponse 里的那次写入，判定逻辑不依赖此处成功。
    if (!compartmentAlreadySet || newChineseMode != requestedMode)
    {
        // 只有真被仲裁时才这么说：按键路径 compartmentAlreadySet=FALSE 恒进入本分支，
        // 无条件打这条会谎称「服务端仲裁了」，把下次排查引向不存在的仲裁。
        if (newChineseMode != requestedMode)
            WIND_LOG_INFO_FMT(L"Service arbitrated mode to %s (requested %s), syncing compartment\n",
                newChineseMode ? L"Chinese" : L"English", requestedMode ? L"Chinese" : L"English");
        _SetOpenCloseCompartment(_bChineseMode);
    }

    // 同步真实中英文模式到 INPUTMODE_CONVERSION（KBLSwitch / 任务栏读取此 compartment）
    _SetConversionMode(_bChineseMode);

    WIND_LOG_INFO_FMT(L"Mode set via %s -> %s\n",
        sourceName, _bChineseMode ? L"Chinese" : L"English");

    return S_OK;
}

// 按键侧兜底的中英切换（Ctrl+Space）。
//
// 只有系统没把 Ctrl+Space 当 IME 热键时才会走到这里：OnKeyDown 只在 OnTestKeyDown 吃键后调用，
// 而吃下该键就意味着 msctf 不会再拿它当热键、compartment 不会被翻，两条路径天然互斥。
// 系统热键失效的机器上全靠这条，见 docs/architecture/tsf-openclose-compartment-semantics.md 第 4 节。
BOOL CTextService::ToggleModeFromKey()
{
    // 不加 _hasThreadFocus 守卫：那是 OnChange 用来过滤 compartment 广播噪声的，
    // 按键只会送到有焦点的实例，不存在噪声；而多进程宿主下该标志本身就不可靠。
    return SUCCEEDED(_ApplyModeSwitch(!_bChineseMode, FALSE, ModeSwitchSource::CtrlSpaceKey));
}

BOOL CTextService::_InitKeyboardDisabledCompartment()
{
    if (_pThreadMgr == nullptr)
        return FALSE;

    ITfCompartmentMgr* pCompMgr = nullptr;
    HRESULT hr = _pThreadMgr->QueryInterface(IID_ITfCompartmentMgr, (void**)&pCompMgr);
    if (FAILED(hr) || pCompMgr == nullptr)
        return FALSE;

    ITfCompartment* pCompartment = nullptr;
    hr = pCompMgr->GetCompartment(GUID_COMPARTMENT_KEYBOARD_DISABLED, &pCompartment);
    pCompMgr->Release();

    if (FAILED(hr) || pCompartment == nullptr)
        return FALSE;

    // Read current value
    VARIANT var;
    VariantInit(&var);
    if (SUCCEEDED(pCompartment->GetValue(&var)) && var.vt == VT_I4)
        _bKeyboardDisabled = (var.lVal != 0);

    // Advise for changes
    ITfSource* pSource = nullptr;
    hr = pCompartment->QueryInterface(IID_ITfSource, (void**)&pSource);
    pCompartment->Release();

    if (FAILED(hr) || pSource == nullptr)
        return FALSE;

    hr = pSource->AdviseSink(IID_ITfCompartmentEventSink, (ITfCompartmentEventSink*)this, &_dwKeyboardDisabledSinkCookie);
    pSource->Release();

    if (FAILED(hr))
    {
        _dwKeyboardDisabledSinkCookie = TF_INVALID_COOKIE;
        return FALSE;
    }

    WIND_LOG_DEBUG_FMT(L"Compartment KEYBOARD_DISABLED sink advised, current=%d\n", _bKeyboardDisabled);
    return TRUE;
}

void CTextService::_UninitKeyboardDisabledCompartment()
{
    if (_dwKeyboardDisabledSinkCookie == TF_INVALID_COOKIE || _pThreadMgr == nullptr)
        return;

    ITfCompartmentMgr* pCompMgr = nullptr;
    if (SUCCEEDED(_pThreadMgr->QueryInterface(IID_ITfCompartmentMgr, (void**)&pCompMgr)) && pCompMgr != nullptr)
    {
        ITfCompartment* pCompartment = nullptr;
        if (SUCCEEDED(pCompMgr->GetCompartment(GUID_COMPARTMENT_KEYBOARD_DISABLED, &pCompartment)) && pCompartment != nullptr)
        {
            ITfSource* pSource = nullptr;
            if (SUCCEEDED(pCompartment->QueryInterface(IID_ITfSource, (void**)&pSource)) && pSource != nullptr)
            {
                pSource->UnadviseSink(_dwKeyboardDisabledSinkCookie);
                pSource->Release();
            }
            pCompartment->Release();
        }
        pCompMgr->Release();
    }

    _dwKeyboardDisabledSinkCookie = TF_INVALID_COOKIE;
    WIND_LOG_DEBUG(L"Compartment KEYBOARD_DISABLED sink unadvised\n");
}

// ============================================================================
// Compartment event sink for GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION
//
// 此 compartment 是 Windows 标准的「中/英文模式」对外通道：IME_CMODE_NATIVE 位 1=中文、0=英文。
// 第三方工具（KBLSwitch 等按应用锁中英文）与 Win11 任务栏语言指示器都读写它。
// ============================================================================

// IME_CMODE_NATIVE from imm.h. 不引入 imm.h，避免拉入整个 IMM32 头文件。
#ifndef IME_CMODE_NATIVE
#define IME_CMODE_NATIVE 0x0001
#endif

BOOL CTextService::_InitConversionCompartment()
{
    if (_pThreadMgr == nullptr)
        return FALSE;

    ITfCompartmentMgr* pCompMgr = nullptr;
    HRESULT hr = _pThreadMgr->QueryInterface(IID_ITfCompartmentMgr, (void**)&pCompMgr);
    if (FAILED(hr) || pCompMgr == nullptr)
        return FALSE;

    ITfCompartment* pCompartment = nullptr;
    hr = pCompMgr->GetCompartment(GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION, &pCompartment);
    pCompMgr->Release();

    if (FAILED(hr) || pCompartment == nullptr)
        return FALSE;

    // Sync initial value to current internal mode.
    VARIANT var;
    var.vt = VT_I4;
    var.lVal = _bChineseMode ? IME_CMODE_NATIVE : 0;
    _bInConversionChange = TRUE;
    pCompartment->SetValue(_tfClientId, &var);
    _bInConversionChange = FALSE;

    ITfSource* pSource = nullptr;
    hr = pCompartment->QueryInterface(IID_ITfSource, (void**)&pSource);
    pCompartment->Release();

    if (FAILED(hr) || pSource == nullptr)
        return FALSE;

    hr = pSource->AdviseSink(IID_ITfCompartmentEventSink, (ITfCompartmentEventSink*)this, &_dwConversionSinkCookie);
    pSource->Release();

    if (FAILED(hr))
    {
        _dwConversionSinkCookie = TF_INVALID_COOKIE;
        return FALSE;
    }

    WIND_LOG_DEBUG_FMT(L"Compartment INPUTMODE_CONVERSION sink advised, initial=%d\n", _bChineseMode);
    return TRUE;
}

void CTextService::_UninitConversionCompartment()
{
    if (_dwConversionSinkCookie == TF_INVALID_COOKIE || _pThreadMgr == nullptr)
        return;

    ITfCompartmentMgr* pCompMgr = nullptr;
    if (SUCCEEDED(_pThreadMgr->QueryInterface(IID_ITfCompartmentMgr, (void**)&pCompMgr)) && pCompMgr != nullptr)
    {
        ITfCompartment* pCompartment = nullptr;
        if (SUCCEEDED(pCompMgr->GetCompartment(GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION, &pCompartment)) && pCompartment != nullptr)
        {
            ITfSource* pSource = nullptr;
            if (SUCCEEDED(pCompartment->QueryInterface(IID_ITfSource, (void**)&pSource)) && pSource != nullptr)
            {
                pSource->UnadviseSink(_dwConversionSinkCookie);
                pSource->Release();
            }
            pCompartment->Release();
        }
        pCompMgr->Release();
    }

    _dwConversionSinkCookie = TF_INVALID_COOKIE;
    WIND_LOG_DEBUG(L"Compartment INPUTMODE_CONVERSION sink unadvised\n");
}

BOOL CTextService::_SetConversionMode(BOOL bChinese)
{
    if (_pThreadMgr == nullptr)
        return FALSE;

    ITfCompartmentMgr* pCompMgr = nullptr;
    HRESULT hr = _pThreadMgr->QueryInterface(IID_ITfCompartmentMgr, (void**)&pCompMgr);
    if (FAILED(hr) || pCompMgr == nullptr)
        return FALSE;

    ITfCompartment* pCompartment = nullptr;
    hr = pCompMgr->GetCompartment(GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION, &pCompartment);
    pCompMgr->Release();

    if (FAILED(hr) || pCompartment == nullptr)
        return FALSE;

    // 仅维护 IME_CMODE_NATIVE 位，保留外界可能写入的其他位（FULLSHAPE/SYMBOL 等）。
    VARIANT cur;
    VariantInit(&cur);
    DWORD prev = 0;
    if (SUCCEEDED(pCompartment->GetValue(&cur)) && cur.vt == VT_I4)
        prev = (DWORD)cur.lVal;

    DWORD next = bChinese ? (prev | IME_CMODE_NATIVE) : (prev & ~IME_CMODE_NATIVE);
    if (next == prev)
    {
        pCompartment->Release();
        return TRUE;  // 无需写入，避免触发多余 OnChange
    }

    _bInConversionChange = TRUE;

    VARIANT var;
    var.vt = VT_I4;
    var.lVal = (LONG)next;
    hr = pCompartment->SetValue(_tfClientId, &var);
    pCompartment->Release();

    _bInConversionChange = FALSE;
    return SUCCEEDED(hr);
}

BOOL CTextService::_DoFullStateSync(BOOL fromServiceReady)
{
    if (_pIPCClient == nullptr)
        return FALSE;

    const int failLevel = fromServiceReady ? 3 /* info */ : 2 /* warn */;
    _pIPCClient->SetPipeBusyExempt(fromServiceReady);

    // Lazy connect: push pipe may reconnect before main pipe is established (service restart).
    if (!_pIPCClient->IsConnected() && !_pIPCClient->Connect())
    {
        _pIPCClient->SetPipeBusyExempt(FALSE);
        WindLog::Output(failLevel, L"_DoFullStateSync: main pipe not connected, skipping\n");
        return FALSE;
    }

    // 异步：只发 fire-and-forget 的 CMD_IME_ACTIVATED，服务端先回 Ack 解除本同步调用，处理完再经
    // push 管道推 CMD_ACTIVATION_STATUS_PUSH，AsyncReader 回调 PostMessage 到 TSF 线程执行
    // ApplyActivationStatusResponse（_SyncStateFromResponse + _EnsureHostRenderSetup）。
    // 同步等回包会阻塞宿主 UI 线程，explorer.exe 等 shell 宿主会与服务端的跨进程调用形成环形等待。
    WIND_LOG_INFO(L"Sending IMEActivated (async); state will arrive via CMD_ACTIVATION_STATUS_PUSH\n");

    BOOL sent = _pIPCClient->SendIMEActivated();
    if (!sent)
    {
        // Stale pipe: write failed and Disconnect() was called; retry after fresh connect.
        sent = _pIPCClient->Connect() && _pIPCClient->SendIMEActivated();
        if (!sent)
            WindLog::Output(failLevel, L"_DoFullStateSync: SendIMEActivated failed\n");
    }
    _pIPCClient->SetPipeBusyExempt(FALSE);

    // 清除 _needsStateSync 提前到此处，后续 KeyEventSink 路径不再重复 state sync；push 暂未到时
    // 下一次焦点切换会重新拉起 activation 流程。
    //
    // _needsFocusRecovery 不在这里清，交给末尾的 TryRecoverFocusState 消费：IME_ACTIVATED 不带窗口
    // 类名 / 标题，按类名的应用兼容规则全部不命中；冷启动时 focus_gained 发不出去，在这里一清补发就不会发生。
    _pIPCClient->ClearNeedsSyncFlag();

    // UIElement「谁画候选」的记账住在服务端，服务重启就没了；本函数正是每次（重）连后
    // 的全量同步点，故无条件重报一次：UI-less 线程从激活起就不弹窗，普通线程报 0 把
    // 本 pid 上次留下的记账（pid 复用 / 上次会话宿主接管过）清掉。
    _uiElementStateSent = -1;
    _ReportUiElementState();

    // 先前发不出去的 focus_gained（服务没连上）在此补发，带上窗口类名 / 标题。
    TryRecoverFocusState();
    return sent;
}

void CTextService::CancelServiceReadyRetry()
{
    if (_pLangBarItemButton != nullptr)
        _pLangBarItemButton->CancelServiceReadyRetry();
}

// ApplyActivationStatusResponse 在 TSF 线程上把 push pipe 接收到的 activation status 落地。
// 等价于原同步路径 ReceiveResponse → _SyncStateFromResponse + _EnsureHostRenderSetup。
// 调用点：CLangBarItemButton::_MsgWndProc 处理 WM_ACTIVATION_STATUS 时。
void CTextService::ApplyActivationStatusResponse(const ServiceResponse& response)
{
    _SyncStateFromResponse(response);
    // 与原 _DoFullStateSync 一致使用 forceRefresh=TRUE：activation 是新建/重建状态机的时刻,
    // 需要主动 (re)setup HostRender, 而不是惰性等下次窗口变化触发。
    _EnsureHostRenderSetup(response, TRUE);
}

// 焦点空窗记账。见 TextService.h 的 _focusGapStartTick：空窗内按键根本不经过输入法，
// 两侧日志都不留痕，只能在进出空窗的边界上主动记一笔，否则事后无从归因。
void CTextService::_BeginFocusGap(const WCHAR* reason)
{
    // 已在空窗中就不重新计时：宿主一次焦点重建会连抖数次，每次重置会把一段连续的「不能输入」切成若干短段。
    if (_focusGapStartTick != 0)
        return;
    _focusGapStartTick = GetTickCount64();
    _focusGapReason = reason;
    WIND_LOG_DEBUG_FMT(L"compat.focus.gap begin reason=%s focusSession=%llu", reason, _focusSessionId);
}

void CTextService::_EndFocusGap()
{
    if (_focusGapStartTick == 0)
        return;
    const ULONGLONG ms = GetTickCount64() - _focusGapStartTick;
    const WCHAR* reason = (_focusGapReason != nullptr) ? _focusGapReason : L"unknown";
    if (ms >= kFocusGapWarnMs)
    {
        WIND_LOG_INFO_FMT(
            L"compat.focus.gap end reason=%s duration=%llums focusSession=%llu"
            L" —— 该窗口内按键不经输入法，首字符可能已直落宿主",
            reason, ms, _focusSessionId);
    }
    else
    {
        WIND_LOG_DEBUG_FMT(L"compat.focus.gap end reason=%s duration=%llums focusSession=%llu",
                           reason, ms, _focusSessionId);
    }
    _focusGapStartTick = 0;
    _focusGapReason = nullptr;
}

void CTextService::TryRecoverFocusState()
{
    if (!_needsFocusRecovery || _pIPCClient == nullptr || !_pIPCClient->IsConnected())
        return;
    if (_ResendFocusGained(L"recovery"))
    {
        _needsFocusRecovery = FALSE;
        _pIPCClient->ClearNeedsSyncFlag();
        _EndFocusGap();
        SendCaretPositionUpdate();
        WIND_LOG_INFO(L"Deferred focus recovery sent (async), state will arrive via push\n");
    }
    else
    {
        WIND_LOG_WARN_FMT(L"Deferred focus recovery send failed focusSession=%llu", _focusSessionId);
        _needsFocusRecovery = FALSE;
    }
}

BOOL CTextService::RetryFocusRecovery()
{
    using wind::svcready::FocusAction;
    if (_pIPCClient == nullptr)
        return TRUE;
    const FocusAction action = wind::svcready::DecideFocusRecovery(
        _needsFocusRecovery != FALSE, HasFocus() != FALSE, _hasThreadFocus != FALSE,
        _pIPCClient->IsConnected() != FALSE, _pIPCClient->StateEstablished() != FALSE);
    switch (action)
    {
    case FocusAction::StopRecovered:
        return TRUE;
    case FocusAction::StopNoFocus:
    case FocusAction::StopBackground:
        WIND_LOG_INFO_FMT(L"Focus resend retry stopped (%ls)\n",
                          action == FocusAction::StopNoFocus ? L"no focus" : L"background");
        _needsFocusRecovery = FALSE;
        return TRUE;
    case FocusAction::StopAlreadySynced:
        // 按键路径会在下次按键时 TryRecoverFocusState 补上，这里不抢。
        WIND_LOG_INFO(L"Focus resend retry stopped (connection already has traffic)\n");
        return TRUE;
    case FocusAction::Resend:
        break;
    }

    // 只补 focus_gained，不走 _DoFullStateSync：IME_ACTIVATED 会把用户在窗口期里切过的中英重置。
    if (!_pIPCClient->IsConnected() && !_pIPCClient->Connect())
        return FALSE;
    if (!_ResendFocusGained(L"retry"))
        return FALSE;
    _needsFocusRecovery = FALSE;
    _pIPCClient->ClearNeedsSyncFlag();
    _EndFocusGap();
    CancelServiceReadyRetry();
    SendCaretPositionUpdate();
    WIND_LOG_INFO(L"Focus resend retry succeeded\n");
    return TRUE;
}

void CTextService::SetTitleMatchEnabled(BOOL bEnabled)
{
    const LONG prev = InterlockedExchange(&_titleMatchEnabled, bEnabled ? 1 : 0);
    // 关→开：首个 focus_gained 可能早于开关到达（新进程首焦 vs 握手推送是竞态），补发交给
    // TSF 线程判（本函数跑在 async reader 线程，不能发同步 IPC）。
    if (prev == 0 && bEnabled && _pLangBarItemButton != nullptr)
        _pLangBarItemButton->PostTitleMatchResync();
}

void CTextService::ResyncFocusForTitleMatch()
{
    // 判据见 WindowTitlePolicy.h ShouldResyncFocusOnSwitch。
    // 「有焦点」须连带本线程在前台（_hasThreadFocus）：_hasFocus 只记 DocMgr 焦点，失去线程焦点时
    // 不清零。开关是逐客户端推的（加第一条标题规则时所有进程同时 0→1），不带这一条，每个挂着
    // DocMgr 焦点的后台进程都会补发 focus_gained、冒充前台，活动客户端落到最后发到的那个。
    if (!wind::window_title::ShouldResyncFocusOnSwitch(_focusSentTitleBlind != FALSE, IsTitleMatchEnabled() != FALSE,
                                                       HasFocus() != FALSE && _hasThreadFocus != FALSE,
                                                       HasCompositionOrRecover() != FALSE))
        return;
    if (_pIPCClient == nullptr || !_pIPCClient->IsConnected())
        return;
    _focusSentTitleBlind = FALSE;
    if (_ResendFocusGained(L"title_match_on"))
        WIND_LOG_DEBUG_FMT(L"compat.focus.title_resync sent focusSession=%llu", _focusSessionId);
}

BOOL CTextService::_ResendFocusGained(const wchar_t* why)
{
    LONG caretX = _lastFocusCaretX;
    LONG caretY = _lastFocusCaretY;
    LONG caretHeight = _lastFocusCaretHeight > 0 ? _lastFocusCaretHeight : DEFAULT_CARET_HEIGHT;
    // 缓存值的来源随缓存一起继承——坐标与来源必须成对读写，分开就等于又把「拿到了坐标」
    // 和「拿到了那个坐标」压回同一个真值。
    int  caretSource = _lastFocusCaretSource;

    if (GetCaretPosition(&caretX, &caretY, &caretHeight, &caretSource))
    {
        _lastFocusCaretX = caretX;
        _lastFocusCaretY = caretY;
        _lastFocusCaretHeight = caretHeight > 0 ? caretHeight : DEFAULT_CARET_HEIGHT;
        _lastFocusCaretSource = caretSource;
    }
    else if (_hasLastKnownCaretPos)
    {
        caretX = _lastKnownCaretX;
        caretY = _lastKnownCaretY;
        caretHeight = _lastKnownCaretHeight;
        caretSource = CARET_SRC_LAST_KNOWN;
        WIND_LOG_INFO_FMT(L"Resending focus (%ls) with last known caret x=%ld y=%ld h=%ld", why, caretX, caretY, caretHeight);
    }

    WIND_LOG_INFO_FMT(L"Resending focus_gained (%ls) focusSession=%llu x=%ld y=%ld h=%ld src=%d",
        why, _focusSessionId, caretX, caretY, caretHeight, caretSource);

    // 与 OnSetFocus / compartment OnChange 一致，重新查询焦点 DocMgr 的 InputScope mask，避免诊断 HUD
    // 误显示「原因: 无」；拿不到焦点 DocMgr 时 mask 退化为 0。窗口类名 / 标题同样现取：不带的话
    // 服务端按「不知道是哪个窗口」解析，窗口规则全不命中，且会被当成换了窗口去重推 DLL 配置。
    UINT64 mask = 0;
    bool ctxDisabled = false;
    std::wstring rootClass;
    std::wstring rootTitle;
    ITfDocumentMgr* pDocMgr = nullptr;
    if (_pThreadMgr != nullptr && SUCCEEDED(_pThreadMgr->GetFocus(&pDocMgr)) && pDocMgr != nullptr)
    {
        mask = _QueryInputScopeMask(pDocMgr);
        ctxDisabled = _IsFocusKeyboardDisabled(pDocMgr) != FALSE;
        _QueryFocusRootWindowIdentity(pDocMgr, rootClass, rootTitle);
        pDocMgr->Release();
    }
    // context 级 KEYBOARD_DISABLED（Chromium 网页密码框）折成 IS_PASSWORD 位，与 OnSetFocus 同：
    // 不折的话补发的 focus_gained 会把服务端的密码抑制当场解除。
    const UINT64 rawMask = mask;
    if (ctxDisabled)
        mask |= kScopeBitPassword;
    // 标题只记长度（用户数据）。
    WIND_LOG_DEBUG_FMT(L"compat.focus.rootclass (%ls) focusSession=%llu class=%ls title_len=%u",
                       why, _focusSessionId, rootClass.c_str(), (unsigned)rootTitle.size());
    uint8_t reason = ComputeInputReason(_bKeyboardDisabled != FALSE, rawMask, ctxDisabled);
    return _pIPCClient->SendFocusGained((int)caretX, (int)caretY, (int)caretHeight, mask,
                                        _bKeyboardDisabled != FALSE, reason, caretSource,
                                        rootClass.c_str(), rootTitle.c_str());
}

BOOL CTextService::_InitIPCClient()
{
    _pIPCClient = new CIPCClient();
    if (_pIPCClient == nullptr)
        return FALSE;

    // 连接服务端（失败无妨，之后会重试）
    if (!_pIPCClient->Connect())
    {
        WIND_LOG_WARN(L"Failed to connect to Service, will retry later\n");
    }

    // 激活状态推送回调（CMD_ACTIVATION_STATUS_PUSH）：服务端 HandleIMEActivated / HandleFocusGained 异步完成后推来。
    // 必须 Post 到 TSF 线程：_SyncStateFromResponse / _EnsureHostRenderSetup 会碰 STA-bound 的 TSF COM 对象。
    CTextService* pThis = this;
    _pIPCClient->SetActivationPushCallback([pThis](const ServiceResponse& response) {
        if (pThis->_pLangBarItemButton != nullptr)
        {
            pThis->_pLangBarItemButton->PostActivationStatus(response);
        }
    });

    // 仅模式推送回调（CMD_MODE_PUSH）：服务端在回 Ack 前入队此轻量包，让 _bChineseMode/_bFullWidth
    // 比激活推送更早就绪，消除首次按键竞态窗口。
    // 此回调在 AsyncReader 线程执行，不得访问 TSF COM 对象；InterlockedExchange 的全屏障保证
    // TSF 主线程 OnTestKeyDown 立即见到新值。
    _pIPCClient->SetModePushCallback([pThis](bool chineseMode, bool fullWidth, bool chinesePunct) {
        ::InterlockedExchange(reinterpret_cast<LONG*>(&pThis->_bChineseMode),
                              chineseMode ? TRUE : FALSE);
        ::InterlockedExchange(reinterpret_cast<LONG*>(&pThis->_bFullWidth),
                              fullWidth ? TRUE : FALSE);
        ::InterlockedExchange(reinterpret_cast<LONG*>(&pThis->_bChinesePunct),
                              chinesePunct ? TRUE : FALSE);
    });

    // 图标刷新推送（CMD_REFRESH_ICON）：服务端换了共享内存里的位图但状态没变
    // （调试菜单改角标形状、演示动画每帧）。此处只把请求转到 TSF 线程，
    // 不动任何状态字段——需要变的东西全在 SHM 里，DLL 侧无副本。
    _pIPCClient->SetRefreshIconCallback([pThis]() {
        if (pThis->_pLangBarItemButton != nullptr)
        {
            pThis->_pLangBarItemButton->PostRefreshIcon();
        }
    });

    // 外部程序启动回调（CMD_SHELL_EXEC）：在前台宿主进程里调 ShellExecuteW，借它的前台权限让窗口正确置顶。
    // 回调在 AsyncReader 线程执行，ShellExecuteW 线程安全，无需切回 TSF 线程。
    // lpDirectory 用服务端算好的工作目录：传 nullptr 会沿用宿主的当前目录（通用文件对话框会改掉它），
    // 靠相对路径找数据文件的程序会找不到自己的数据。
    _pIPCClient->SetShellExecCallback([](const std::wstring& target, const std::wstring& params, const std::wstring& dir, const std::wstring& verb, const std::wstring& show) {
        const wchar_t* pParams = params.empty() ? nullptr : params.c_str();
        const wchar_t* pDir = dir.empty() ? nullptr : dir.c_str();
        // 空 verb 传 nullptr 而非 L"open"：nullptr 用该文件类型的默认动词（未必是 open）。
        const wchar_t* pVerb = verb.empty() ? nullptr : verb.c_str();
        // 语义名 → SW_ 常量。取值已由服务端白名单校验；未知值落 SW_SHOWNORMAL，
        // 这只会在「新服务加了新取值 + 旧 DLL」的版本错配下出现。
        int nShow = SW_SHOWNORMAL;
        if      (show == L"min")    nShow = SW_SHOWMINNOACTIVE;  // 最小化且不抢焦点
        else if (show == L"max")    nShow = SW_SHOWMAXIMIZED;
        else if (show == L"hidden") nShow = SW_HIDE;

        // 启动期间把线程降为 DPI-unaware：manifest 未声明 DPI 感知的子进程继承创建线程的上下文，
        // 而本回调跑在用户所在的宿主里，同一程序在不同宿主里启动会得到不同感知级别（宿主是 PMv2 时，
        // 老程序按 96 DPI 作画而系统不再缩放）。降为 unaware 后与用户自己双击启动一致：有 manifest 的
        // 照走自己的声明，没有的由系统做位图缩放。
        //
        // 动态取符号（Win10 1607+），取不到就退回原样调用。必须无条件还原且只包这一次 ShellExecuteW：
        // 这是宿主的 AsyncReader 线程，留下被改写的上下文会让宿主后续的窗口/坐标操作换一套语义。
        //
        // 只需改这一处：cmdbar 的 `open` 与 `proc.run` 都经 push_shell_exec 到这里；`proc.shell` 走
        // `cmd /C`，继承 cmd.exe 自己的感知级别；服务端 open_path / open_app 的目标都带 manifest。
        using SetThreadDpiAwarenessContextFn =
            DPI_AWARENESS_CONTEXT(WINAPI*)(DPI_AWARENESS_CONTEXT);
        static auto pSetThreadDpiAwarenessContext =
            reinterpret_cast<SetThreadDpiAwarenessContextFn>(
                GetProcAddress(GetModuleHandleW(L"user32.dll"), "SetThreadDpiAwarenessContext"));

        DPI_AWARENESS_CONTEXT prevCtx = nullptr;
        if (pSetThreadDpiAwarenessContext != nullptr)
            prevCtx = pSetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_UNAWARE);

        HINSTANCE ret = ::ShellExecuteW(nullptr, pVerb, target.c_str(), pParams, pDir, nShow);

        if (prevCtx != nullptr)
            pSetThreadDpiAwarenessContext(prevCtx);
        // ──────────────────────────────────────────────────────────────────

        if (reinterpret_cast<INT_PTR>(ret) <= 32)
            WIND_LOG_ERROR_FMT(L"ShellExecuteW failed: target=%s dir=%s verb=%s code=%d", target.c_str(), dir.c_str(), verb.c_str(), static_cast<int>(reinterpret_cast<INT_PTR>(ret)));
    });

    // Set up state push callback
    _pIPCClient->SetStatePushCallback([pThis](const ServiceResponse& response) {
        // This callback is called from the async reader thread
        // We need to update our state and notify the language bar
        WIND_LOG_INFO_FMT(L"State push received: mode=%d, fullWidth=%d, punct=%d, caps=%d\n",
                     response.IsChineseMode(), response.IsFullWidth(),
                     response.IsChinesePunct(), response.IsCapsLock());

        // Update internal state (atomic operation, thread-safe)
        pThis->_SetChineseMode(response.IsChineseMode());
        pThis->_bFullWidth = response.IsFullWidth();
        pThis->_bChinesePunct = response.IsChinesePunct();
        pThis->_bSoftKeyboard = response.IsSoftKeyboard();
        pThis->_bSoftKeyboardKeys = response.IsSoftKeyboardKeys();
        // 同 _SyncStateFromResponse：本位由服务端权威驱动，这里无条件镜像。
        // 两条路都要写——只写一条的话，另一条通路来的状态推送会让镜像停在旧值。
        if (pThis->_pKeyEventSink != nullptr)
        {
            pThis->_pKeyEventSink->SetCandidateSessionActive(response.IsHotkeySession());
        }

        // Update language bar button using thread-safe PostUpdateFullStatus
        // This posts a message to the UI thread instead of calling COM directly
        if (pThis->_pLangBarItemButton != nullptr)
        {
            pThis->_pLangBarItemButton->PostUpdateFullStatus(
                response.IsChineseMode(),
                response.IsFullWidth(),
                response.IsChinesePunct(),
                response.IsToolbarVisible(),
                response.IsCapsLock(),
                response.iconLabel.empty() ? nullptr : response.iconLabel.c_str()
            );
        }
    });

    // Set up commit text callback for mouse click on candidate
    _pIPCClient->SetCommitTextCallback([pThis](const std::wstring& text) {
        // This callback is called from the async reader thread
        WIND_LOG_DEBUG_FMT(L"Commit text received from Go, textLen=%zu\n", text.length());

        // Use PostCommitText to ensure EndComposition is called before InsertText on UI thread
        // This fixes the issue where text was inserted into composition range
        if (pThis->_pLangBarItemButton != nullptr)
        {
            pThis->_pLangBarItemButton->PostCommitText(text);
        }
        else
        {
            // Fallback: direct InsertText (composition won't be ended properly)
            pThis->InsertText(text);
        }
    });

    // Set up replace-backward callback for undo commit push from service
    _pIPCClient->SetReplaceBackwardCallback([pThis](int count, const std::wstring& text) {
        // This callback is called from the async reader thread; hop to the TSF
        // thread via the message window (ReplacePrecedingChars needs it).
        WIND_LOG_DEBUG_FMT(L"Replace backward received from service, count=%d\n", count);
        if (pThis->_pLangBarItemButton != nullptr)
        {
            pThis->_pLangBarItemButton->PostReplaceBackward(count, text);
        }
        else
        {
            WIND_LOG_WARN(L"Replace backward push dropped: no LangBarItemButton\n");
        }
    });

    // Set up pair-commit callback for 直通 ime.pair push from service
    _pIPCClient->SetPairCommitCallback([pThis](const std::wstring& text, uint32_t moveLeft) {
        // Async reader thread → TSF thread via the message window（CommitText 要开
        // EditSession、合成 VK_LEFT 依赖本线程输入状态）。
        WIND_LOG_DEBUG_FMT(L"Pair commit received from service, moveLeft=%u\n", moveLeft);
        if (pThis->_pLangBarItemButton != nullptr)
        {
            pThis->_pLangBarItemButton->PostPairCommit(text, moveLeft);
        }
        else
        {
            WIND_LOG_WARN(L"Pair commit push dropped: no LangBarItemButton\n");
        }
    });

    // Set up clear composition callback for mode toggle via menu
    _pIPCClient->SetClearCompositionCallback([pThis]() {
        // This callback is called from the async reader thread
        WIND_LOG_DEBUG(L"Clear composition received from service\n");

        if (pThis->_pLangBarItemButton != nullptr)
        {
            pThis->_pLangBarItemButton->PostClearComposition();
        }
        else
        {
            // Fallback: direct EndComposition
            pThis->EndComposition();
        }
    });

    // Set up update composition callback for mouse click partial confirm
    _pIPCClient->SetUpdateCompositionCallback([pThis](const std::wstring& text, int caretPos) {
        // This callback is called from the async reader thread
        WIND_LOG_DEBUG_FMT(L"Update composition received from service, textLen=%zu, caret=%d\n",
                           text.length(), caretPos);

        if (pThis->_pLangBarItemButton != nullptr)
        {
            pThis->_pLangBarItemButton->PostUpdateComposition(text, caretPos);
        }
        else
        {
            // Fallback: direct UpdateComposition
            pThis->UpdateComposition(text, caretPos);
        }
    });

    // Set up config sync callback for English auto-pair
    _pIPCClient->SetSyncConfigCallback([pThis](const std::string& key, const std::vector<uint8_t>& value) {
        if (pThis->_pKeyEventSink != nullptr)
        {
            pThis->_pKeyEventSink->OnSyncConfig(key, value);
        }
    });

    // 启动异步读线程，接收服务端的状态推送
    if (!_pIPCClient->StartAsyncReader())
    {
        WIND_LOG_WARN(L"Failed to start async reader thread (non-fatal)\n");
        // Non-fatal - we can still use sync IPC
    }
    else
    {
        WIND_LOG_INFO(L"Async reader thread started for state push\n");
    }

    // 服务就绪回调：推送管道连上时服务端发 CMD_SERVICE_READY。经 LangBarItemButton 的消息窗口转到 TSF 线程
    // （与 PostUpdateFullStatus 等共用的跨线程通道）。
    _pIPCClient->SetServiceReadyCallback([pThis]() {
        // 推送通道（重）连上了 = 新的服务会话：会话级开关回到默认。标题采集开关必须清零——
        // 服务端收到 token 后、待自身就绪才发 CMD_SERVICE_READY，且必排在握手配置之前（同一条
        // 管道按序到达，本回调在 reader 线程上同步执行；服务端的保证见 PushServer::mark_ready），
        // 故清零必然早于握手推来的新值，不会把它冲掉。
        pThis->SetTitleMatchEnabled(FALSE);
        if (pThis->_pLangBarItemButton != nullptr)
            pThis->_pLangBarItemButton->PostServiceReady();
    });

    return TRUE;
}

void CTextService::_UninitIPCClient()
{
    if (_pIPCClient != nullptr)
    {
        // Stop async reader thread first
        _pIPCClient->StopAsyncReader();
        _pIPCClient->Disconnect();
        delete _pIPCClient;
        _pIPCClient = nullptr;
    }
}

// EditSession for inserting text at current selection
class CInsertTextEditSession : public ITfEditSession
{
public:
    CInsertTextEditSession(CTextService* pTextService, ITfContext* pContext, const std::wstring& text)
        : _refCount(1), _pTextService(pTextService), _pContext(pContext), _text(text), _success(FALSE)
    {
        _pTextService->AddRef();
        _pContext->AddRef();
    }

    ~CInsertTextEditSession()
    {
        _pTextService->Release();
        _pContext->Release();
    }

    // IUnknown
    STDMETHODIMP QueryInterface(REFIID riid, void** ppvObj)
    {
        if (ppvObj == nullptr) return E_INVALIDARG;
        *ppvObj = nullptr;
        if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITfEditSession))
        {
            *ppvObj = (ITfEditSession*)this;
            AddRef();
            return S_OK;
        }
        return E_NOINTERFACE;
    }

    STDMETHODIMP_(ULONG) AddRef() { return InterlockedIncrement(&_refCount); }
    STDMETHODIMP_(ULONG) Release()
    {
        LONG cr = InterlockedDecrement(&_refCount);
        if (cr == 0) delete this;
        return cr;
    }

    // ITfEditSession
    STDMETHODIMP DoEditSession(TfEditCookie ec)
    {
        _pTextService->_DropRecoverLeftover(ec, _pContext);

        // Get ITfInsertAtSelection interface
        ITfInsertAtSelection* pInsertAtSel = nullptr;
        HRESULT hr = _pContext->QueryInterface(IID_ITfInsertAtSelection, (void**)&pInsertAtSel);
        if (FAILED(hr) || pInsertAtSel == nullptr)
        {
            WIND_LOG_DEBUG(L"InsertTextEditSession: Failed to get ITfInsertAtSelection\n");
            return E_FAIL;
        }

        // Insert text at current selection
        ITfRange* pRange = nullptr;
        hr = pInsertAtSel->InsertTextAtSelection(
            ec,
            0,  // No special flags
            _text.c_str(),
            (LONG)_text.length(),
            &pRange);

        pInsertAtSel->Release();

        if (FAILED(hr))
        {
            WIND_LOG_DEBUG_FMT(L"InsertTextEditSession: InsertTextAtSelection failed hr=0x%08X\n", hr);
            return hr;
        }

        if (pRange != nullptr)
        {
            // Move selection to end of inserted text
            pRange->Collapse(ec, TF_ANCHOR_END);

            TF_SELECTION sel = {};
            sel.range = pRange;
            sel.style.ase = TF_AE_NONE;
            sel.style.fInterimChar = FALSE;
            _pContext->SetSelection(ec, 1, &sel);

            pRange->Release();
        }

        _success = TRUE;
        WIND_LOG_DEBUG_FMT(L"InsertTextEditSession: Successfully inserted '%s'\n", _text.c_str());
        return S_OK;
    }

    BOOL GetSuccess() const { return _success; }

private:
    LONG _refCount;
    CTextService* _pTextService;
    ITfContext* _pContext;
    std::wstring _text;
    BOOL _success;
};

BOOL CTextService::InsertText(const std::wstring& text)
{
    if (text.empty())
    {
        return TRUE;
    }

    // Try TSF method first (works on main thread with proper context)
    if (_pThreadMgr != nullptr)
    {
        // Get current document manager
        ITfDocumentMgr* pDocMgr = nullptr;
        HRESULT hr = _pThreadMgr->GetFocus(&pDocMgr);
        if (SUCCEEDED(hr) && pDocMgr != nullptr)
        {
            // Get top context
            ITfContext* pContext = nullptr;
            hr = pDocMgr->GetTop(&pContext);
            pDocMgr->Release();

            if (SUCCEEDED(hr) && pContext != nullptr)
            {
                // Try to insert using TSF EditSession
                CInsertTextEditSession* pEditSession = new CInsertTextEditSession(this, pContext, text);

                HRESULT hrSession;
                // Use TF_ES_SYNC to ensure synchronous execution
                hr = pContext->RequestEditSession(_tfClientId, pEditSession, TF_ES_SYNC | TF_ES_READWRITE, &hrSession);

                BOOL success = pEditSession->GetSuccess();
                pEditSession->Release();
                pContext->Release();

                if (SUCCEEDED(hr) && SUCCEEDED(hrSession) && success)
                {
                    WIND_LOG_DEBUG(L"InsertText: Successfully used TSF method\n");
                    return TRUE;
                }

                WIND_LOG_DEBUG_FMT(L"InsertText: TSF method failed (hr=0x%08X, hrSession=0x%08X), falling back to SendInput\n", hr, hrSession);
                WIND_LOG_DEBUG_FMT(
                    L"compat.insert_text_fallback focusSession=%llu textLen=%zu hr=0x%08X hrSession=0x%08X",
                    _focusSessionId, text.length(), hr, hrSession
                );
                WindLogForegroundProcessInfo(4, L"compat.insert_text_fallback.foreground_host");
            }
        }
    }

    // Fallback: Use SendInput for batch input (all characters at once)
    // This works from any thread and is used when TSF method fails
    WIND_LOG_DEBUG_FMT(L"InsertText: Using SendInput batch method for '%s'\n", text.c_str());

    // Allocate INPUT structures for all characters (2 per char: down + up)
    std::vector<INPUT> inputs;
    inputs.reserve(text.length() * 2);

    for (wchar_t ch : text)
    {
        // 标记为自生成，避免被自己的 OnTestKeyDown 钩子当成真实按键二次处理
        // （同 ReplacePrecedingChars 兜底路径，见其注释）。
        if (_pKeyEventSink != nullptr) _pKeyEventSink->MarkSyntheticKey(VK_PACKET);

        INPUT inputDown = {};
        inputDown.type = INPUT_KEYBOARD;
        inputDown.ki.wVk = 0;
        inputDown.ki.wScan = ch;
        inputDown.ki.dwFlags = KEYEVENTF_UNICODE;
        inputs.push_back(inputDown);

        INPUT inputUp = {};
        inputUp.type = INPUT_KEYBOARD;
        inputUp.ki.wVk = 0;
        inputUp.ki.wScan = ch;
        inputUp.ki.dwFlags = KEYEVENTF_UNICODE | KEYEVENTF_KEYUP;
        inputs.push_back(inputUp);
    }

    // Send all inputs at once - this makes text appear instantly
    UINT sent = SendInput((UINT)inputs.size(), inputs.data(), sizeof(INPUT));

    if (sent != inputs.size())
    {
        WIND_LOG_WARN_FMT(L"InsertText: SendInput sent %u of %u inputs\n", sent, (UINT)inputs.size());
    }
    else
    {
        WIND_LOG_DEBUG_FMT(
            L"compat.sendinput_commit focusSession=%llu textLen=%zu inputs=%u",
            _focusSessionId, text.length(), (UINT)inputs.size()
        );
    }

    return TRUE;
}

// Static variables to track last known good caret position
static LONG s_lastCaretX = 0;
static LONG s_lastCaretY = 0;
static LONG s_lastCaretHeight = 20;
static BOOL s_hasLastCaretPos = FALSE;

// HoldComposition 计时器回调使用的 thread_local 实例指针。
// SetTimer(NULL,...) 的回调在 TSF UI 线程上触发，thread_local 保证线程安全。
static thread_local CTextService* g_holdTimerInstance = nullptr;

// Get caret position using TSF APIs (for browsers and modern apps)
BOOL CTextService::GetCaretPositionFromTSF(LONG* px, LONG* py, LONG* pHeight, BOOL* pUsedCompStart,
                                           RECT* pCompRect, BOOL* pHasCompRect, BOOL* pIsDefaultPos,
                                           BOOL* pUsedImmCandidateForm, BOOL* pRejectedOutsideView)
{
    if (pRejectedOutsideView)
    {
        *pRejectedOutsideView = FALSE;
    }
    if (pUsedCompStart)
    {
        *pUsedCompStart = FALSE;
    }
    if (pIsDefaultPos)
    {
        *pIsDefaultPos = FALSE;
    }
    if (pUsedImmCandidateForm)
    {
        *pUsedImmCandidateForm = FALSE;
    }

    if (_pThreadMgr == nullptr)
    {
        return FALSE;
    }

    // Get current document manager
    ITfDocumentMgr* pDocMgr = nullptr;
    HRESULT hr = _pThreadMgr->GetFocus(&pDocMgr);
    if (FAILED(hr) || pDocMgr == nullptr)
    {
        return FALSE;
    }

    // Get top context
    ITfContext* pContext = nullptr;
    hr = pDocMgr->GetTop(&pContext);
    pDocMgr->Release();

    if (FAILED(hr) || pContext == nullptr)
    {
        return FALSE;
    }

    // 用编辑会话取坐标。带上 _pComposition：组合中若 selection 的 GetTextExt 退化，会话内部
    // 降级用组合起点当 caret（见 CCaretEditSession::DoEditSession 的「锚点降级」）。
    RECT rc = {}, rcCompStart = {};
    BOOL hasCompStart = FALSE;
    BOOL usedCompStart = FALSE;
    BOOL usedImm = FALSE;
    BOOL result = CCaretEditSession::GetCaretAndCompositionStartRect(
        pContext, _tfClientId, _pComposition, &rc, &rcCompStart, &hasCompStart,
        (LONG)GetPendingCommitPrefixLength(), &usedCompStart, pCompRect, pHasCompRect, &usedImm,
        pRejectedOutsideView);
    pContext->Release();

    if (pUsedCompStart)
    {
        *pUsedCompStart = usedCompStart;
    }
    if (pUsedImmCandidateForm)
    {
        *pUsedImmCandidateForm = usedImm;
    }

    if (result)
    {
        // rc contains screen coordinates
        *px = rc.left;
        *py = rc.bottom;  // Position below the caret
        *pHeight = rc.bottom - rc.top;

        // 零高矩形 = GetTextExt 退化（WPS 等在无组合时常见），返回 FALSE 让调用方回退到
        // GetGUIThreadInfo。
        if (*pHeight <= 0)
        {
            // 唯一例外：本次组合已判定「宿主对本 context 没有插入点可报」（CARET_RETRY 那次异步取坐标在
            // 重试窗口过完后仍是退化矩形）。此后每个按键都拿到同一个退化矩形，return FALSE 会跌到无关控件的
            // GUI_CARET，服务端大偏移逃生阀把锚点重锁过去，第二个键又跳回左上角；故采信宿主给的角落坐标。
            // 没闩住判决时退化只意味着宿主还没排完版，照旧丢弃等下一帧。
            // 判决作用域是同一次组合，组合有四个结束出口，作用域在这里判而不在各出口复位，见 LatchApplies。
            if (!wind::caret::LatchApplies(_caretDefaultPosLatched, _pComposition != nullptr))
            {
                WIND_LOG_DEBUG(L"GetCaretPositionFromTSF: Degenerate rect (height=0), falling back\n");
                return FALSE;
            }
            // 高度必须合成：0 会被服务端 caret_is_valid（要求 height > 0）整条丢掉。
            *pHeight = DEFAULT_CARET_HEIGHT;
            if (pIsDefaultPos)
            {
                *pIsDefaultPos = TRUE;
            }
            WIND_LOG_DEBUG_FMT(L"GetCaretPositionFromTSF: 退化矩形 (%ld,%ld,%ld,%ld)，"
                               L"本次组合已判定宿主无插入点，按默认位置采信 h=%ld\n",
                               rc.left, rc.top, rc.right, rc.bottom, *pHeight);
        }
        else if (_caretDefaultPosLatched)
        {
            // 宿主排完版了 ⇒ 上一轮「没有插入点」的判决作废。这行日志不可省：清闩点必须自己留痕，
            // 下游 `src=gui_caret` 有好几个同形成因，靠周围日志反推不出闩是谁清的。
            WIND_LOG_DEBUG_FMT(L"退化判决作废: by=sync_valid_rect h=%ld\n", *pHeight);
            _caretDefaultPosLatched = FALSE;
        }

        // 坐标越界保护：不落在任何显示器上时判不可信，返回 FALSE 回退到 GUIThreadInfo / GetCaretPos
        // （见 IsScreenPointOutsideAllMonitors）。不要收紧成前台窗口判据：桌面输入时 GUIThreadInfo 给的是
        // 别的 shell 窗口残留的 caret，比被丢弃的 TSF 坐标差得多。
        if (IsScreenPointOutsideAllMonitors(rc.left, rc.top))
        {
            WIND_LOG_DEBUG_FMT(L"GetCaretPositionFromTSF: caret(%ld,%ld) outside all monitors, falling back\n", rc.left, rc.top);
            return FALSE;
        }

        // Save as last known good position
        s_lastCaretX = *px;
        s_lastCaretY = *py;
        s_lastCaretHeight = *pHeight;
        s_hasLastCaretPos = TRUE;

        WIND_LOG_DEBUG(L"GetCaretPositionFromTSF: Success\n");
        return TRUE;
    }

    return FALSE;
}

BOOL CTextService::RefreshTextInputContext()
{
    if (!_hasTextInputContext && _pThreadMgr != nullptr)
    {
        ITfDocumentMgr* pDocMgr = nullptr;
        HRESULT hr = _pThreadMgr->GetFocus(&pDocMgr);
        if (SUCCEEDED(hr) && pDocMgr != nullptr)
        {
            _hasTextInputContext = _DocMgrHasEditableContext(pDocMgr);
            pDocMgr->Release();
            if (_hasTextInputContext)
                WIND_LOG_DEBUG_FMT(L"RefreshTextInputContext: late editable context focusSession=%llu", _focusSessionId);
            else
                WIND_LOG_DEBUG_FMT(L"RefreshTextInputContext: docmgr present but not editable focusSession=%llu", _focusSessionId);
        }
        else
        {
            WIND_LOG_DEBUG_FMT(L"RefreshTextInputContext: no focused docmgr hr=0x%08X focusSession=%llu",
                (uint32_t)hr, _focusSessionId);
        }
    }
    return _hasTextInputContext;
}

// 读取焦点文档的 TSF InputScope 集合，编码为 bitmask（bit N = 枚举值 N 存在）。
// 失败/无 InputScope 时返回 0（视为 IS_DEFAULT/未知）。详见 CQueryInputScopeEditSession。
UINT64 CTextService::_QueryInputScopeMask(ITfDocumentMgr* pDocMgr)
{
    if (pDocMgr == nullptr)
        return 0;

    ITfContext* pCtx = nullptr;
    if (FAILED(pDocMgr->GetTop(&pCtx)) || pCtx == nullptr)
        return 0;

    UINT64 mask = 0;
    CQueryInputScopeEditSession* pES = new CQueryInputScopeEditSession(pCtx, &mask);
    if (pES != nullptr)
    {
        HRESULT hrSession = S_OK;
        HRESULT hr = pCtx->RequestEditSession(_tfClientId, pES, TF_ES_SYNC | TF_ES_READ, &hrSession);
        if (FAILED(hr) || FAILED(hrSession))
        {
            WIND_LOG_DEBUG_FMT(L"_QueryInputScopeMask: RequestEditSession hr=0x%08X hrSession=0x%08X", hr, hrSession);
        }
        pES->Release();
    }

    pCtx->Release();
    WIND_LOG_DEBUG_FMT(L"_QueryInputScopeMask: mask=0x%016llX", (unsigned long long)mask);
    return mask;
}

// 读取焦点 context 上某个 bool 型 compartment（VT_I4），并打诊断日志。未设置时返回 false。
static bool ReadContextCompartmentBool(ITfContext* pContext, REFGUID guid, const wchar_t* name)
{
    bool value = false;
    ITfCompartmentMgr* pCompMgr = nullptr;
    if (SUCCEEDED(pContext->QueryInterface(IID_ITfCompartmentMgr, reinterpret_cast<void**>(&pCompMgr))) && pCompMgr != nullptr)
    {
        ITfCompartment* pComp = nullptr;
        if (SUCCEEDED(pCompMgr->GetCompartment(guid, &pComp)) && pComp != nullptr)
        {
            VARIANT var;
            VariantInit(&var);
            if (SUCCEEDED(pComp->GetValue(&var)) && var.vt == VT_I4)
                value = (var.lVal != 0);
            VariantClear(&var);
            pComp->Release();
        }
        pCompMgr->Release();
    }
    WIND_LOG_DEBUG_FMT(L"compartment %s = %d", name, value ? 1 : 0);
    return value;
}

// 激活时补读当前焦点的密码信号并上报，语义与 OnSetFocus 的读取段一致（rawScope + context 级
// KEYBOARD_DISABLED 折成 IS_PASSWORD 位）。拿不到焦点文档 / 不可编辑时不上报：服务端在
// handle_ime_activated 里已把诊断态清零，「没有信号」就是「非密码」。
void CTextService::_ReportFocusInputStateOnActivate()
{
    if (_pThreadMgr == nullptr || _pIPCClient == nullptr || !_pIPCClient->IsConnected())
        return;

    ITfDocumentMgr* pDocMgr = nullptr;
    if (FAILED(_pThreadMgr->GetFocus(&pDocMgr)) || pDocMgr == nullptr)
        return;

    UINT64 mask = 0;
    bool ctxDisabled = false;
    if (_DocMgrHasEditableContext(pDocMgr))
    {
        mask = _QueryInputScopeMask(pDocMgr);
        ctxDisabled = _IsFocusKeyboardDisabled(pDocMgr);
    }
    pDocMgr->Release();

    const UINT64 rawMask = mask; // 折位前：reason 要靠它区分「宿主报密码」与「context 级禁用」
    _focusIsPassword = ctxDisabled;
    if (ctxDisabled)
        mask |= kScopeBitPassword;
    _focusInputScopeMask = mask;

    WIND_LOG_DEBUG_FMT(L"compat.activate.signals rawScopeWithPwd=0x%llX ctxKbdDisabled=%d threadKbdDisabled=%d",
                       mask, ctxDisabled ? 1 : 0, _bKeyboardDisabled ? 1 : 0);
    _pIPCClient->SendInputStateReport(GetCurrentProcessId(), _bKeyboardDisabled != FALSE,
                                      ComputeInputReason(_bKeyboardDisabled != FALSE, rawMask, ctxDisabled), mask);
}

// 判断焦点 context 是否被宿主标记为"禁用输入法"（GUID_COMPARTMENT_KEYBOARD_DISABLED）。
// 这是 Weasel/小狼毫采用的密码框判定信号：Chromium 系浏览器密码框会置位它，而无痕模式
// 的普通可编辑框不会，因此能精确区分密码框与隐私字段，无需 UIA。
bool CTextService::_IsFocusKeyboardDisabled(ITfDocumentMgr* pDocMgr)
{
    if (pDocMgr == nullptr)
        return false;
    ITfContext* pContext = nullptr;
    if (FAILED(pDocMgr->GetTop(&pContext)) || pContext == nullptr)
        return false;
    bool disabled = ReadContextCompartmentBool(pContext, kGuidCompartmentKeyboardDisabled, L"KEYBOARD_DISABLED");
    pContext->Release();
    return disabled;
}

// 焦点是否为密码框：InputScope 的密码位，含 OnSetFocus 折进来的 context 级 KEYBOARD_DISABLED。
BOOL CTextService::IsPasswordScope() const
{
    return (_focusInputScopeMask & kPasswordScopeBits) != 0;
}

// 密码框强制英文抑制当前是否生效。必须镜像 core `apply_input_diag` 的判据：
//   suppress = is_password_scope(mask) && password_suppress_enabled
// 两侧漂移就会「吃了再吐」：DLL 吃了键、core 回 PassThrough，严格 TSF 宿主（Chrome/Electron）
// 不回退合成 WM_CHAR，密码框里完全打不出字。
//
// KEYBOARD_DISABLED 有两个层级，不能混为一谈：
//   · 线程级 `_bKeyboardDisabled`：OnTestKeyDown 开头全放行看的就是它，引擎收不到键，故在此早退；
//   · context 级 `_focusIsPassword`：Chromium 网页密码框置的是这一层，线程级不动，DLL 照常吃键。
// context 级信号已在 OnSetFocus 折进 mask 的 IS_PASSWORD 位，只判 mask 就覆盖两种来源；
// 不要把 `_focusIsPassword` 也列为早退条件，那会让网页密码框同时逃过放行与抑制。
BOOL CTextService::IsPasswordSuppressActive() const
{
    if (!_passwordSuppressEnabled)
        return FALSE;
    if (_bKeyboardDisabled)
        return FALSE;
    // IS_PASSWORD=31 / IS_NUMERIC_PASSWORD=63（对齐 core is_password_scope 的两位），常量在文件顶部单点定义。
    return (_focusInputScopeMask & kPasswordScopeBits) != 0;
}

// GetWindowBand：user32 的未文档化导出（与 CHostWindow::_ResolveAPIs 同源）。独立解析一份：
// 诊断要在未建 host 窗口时也能报 band，那正是「该走 host 却没走」的排查现场。
// 解析一次后缓存（含失败态，解析不到恒返回 0）。
static DWORD _QueryWindowBand(HWND hwnd)
{
    typedef BOOL (WINAPI* GetWindowBand_t)(HWND, DWORD*);
    static GetWindowBand_t s_pfnGetWindowBand = nullptr;
    static BOOL s_resolved = FALSE;
    if (!s_resolved)
    {
        s_resolved = TRUE;
        HMODULE hUser32 = GetModuleHandleW(L"user32.dll");
        if (hUser32 != nullptr)
            s_pfnGetWindowBand = (GetWindowBand_t)GetProcAddress(hUser32, "GetWindowBand");
    }
    DWORD band = 0;
    if (s_pfnGetWindowBand != nullptr && hwnd != nullptr)
        s_pfnGetWindowBand(hwnd, &band);
    return band;
}

// 窗口类名。取不到一律空串（消费端渲染成 "?"）——诊断数据缺一格也要能发出去。
static std::wstring _QueryWindowClass(HWND hwnd)
{
    if (hwnd == nullptr)
        return std::wstring();
    wchar_t buf[256] = {};
    int n = GetClassNameW(hwnd, buf, (int)(sizeof(buf) / sizeof(buf[0])));
    return n > 0 ? std::wstring(buf, (size_t)n) : std::wstring();
}

// 焦点窗口：两条通路按可信度依次尝试，并回报实际用了哪条（不同宿主下「焦点窗口」语义不同）。
//
// 刻意不含 GetForegroundWindow 兜底：WebView 类多进程宿主下它返回别的进程的窗口，当窗口身份判据是错的。
// 诊断快照另有自己的 fg 兜底，那里只求尽量给个值。
//
// pCtxIdOut 可为 nullptr；非空时顺带回填 top context 的指针值（诊断快照用）。
HWND CTextService::_ResolveFocusWindow(ITfDocumentMgr* pDocMgr, uint8_t* pSrcOut, uint64_t* pCtxIdOut)
{
    if (pSrcOut != nullptr)
        *pSrcOut = WND_SRC_NONE;

    HWND hwndFocus = nullptr;
    ITfContext* pContext = nullptr;
    if (pDocMgr != nullptr && SUCCEEDED(pDocMgr->GetTop(&pContext)) && pContext != nullptr)
    {
        if (pCtxIdOut != nullptr)
            *pCtxIdOut = (uint64_t)(uintptr_t)pContext;
        ITfContextView* pView = nullptr;
        if (SUCCEEDED(pContext->GetActiveView(&pView)) && pView != nullptr)
        {
            HWND h = nullptr;
            // 受限宿主（SearchHost 等）这里常返回 S_OK + null，故必须判 h 本身。
            if (SUCCEEDED(pView->GetWnd(&h)) && h != nullptr)
            {
                hwndFocus = h;
                if (pSrcOut != nullptr)
                    *pSrcOut = WND_SRC_TSF_VIEW;
            }
            pView->Release();
        }
        pContext->Release();
    }

    if (hwndFocus == nullptr)
    {
        GUITHREADINFO gti = {};
        gti.cbSize = sizeof(GUITHREADINFO);
        if (GetGUIThreadInfo(GetCurrentThreadId(), &gti) && gti.hwndFocus != nullptr)
        {
            hwndFocus = gti.hwndFocus;
            if (pSrcOut != nullptr)
                *pSrcOut = WND_SRC_GUI_THREAD;
        }
    }
    return hwndFocus;
}

// 窗口标题，至多 kMaxTitleChars 个 UTF-16 码元（截断不留半个代理对）。取不到为空串。
//
// 用 InternalGetWindowText 而非 GetWindowTextW：后者对本进程窗口会发 WM_GETTEXT，宿主 UI 线程卡住时
// 就卡在这里，而本函数跑在 OnSetFocus 的同步路径上。
static std::wstring _QueryWindowTitle(HWND hwnd)
{
    if (hwnd == nullptr)
        return std::wstring();
    // +1 给结尾 NUL，+1 多读一个码元：截断点落在代理对中间时 TruncateTitle 才看得见。
    wchar_t buf[wind::window_title::kMaxTitleChars + 2] = {};
    int n = InternalGetWindowText(hwnd, buf, (int)(sizeof(buf) / sizeof(buf[0])));
    if (n <= 0)
        return std::wstring();
    return std::wstring(wind::window_title::TruncateTitle(std::wstring_view(buf, (size_t)n)));
}

// 焦点所在顶层窗口的类名与标题，随 focus_gained 上报。空串 = 拿不到（服务端回落既有行为）。
//
// 取顶层（GA_ROOT）：任务栏 / 切换器的身份写在顶层窗口类名上（Shell_TrayWnd 等），子控件类名五花八门；
// 标题也在顶层（标签页名、文件名）。仅当 core 推开了 CONFIG_KEY_COMPAT_TITLE_MATCH 才取标题（用户数据）。
void CTextService::_QueryFocusRootWindowIdentity(ITfDocumentMgr* pDocMgr, std::wstring& windowClass,
                                                 std::wstring& windowTitle)
{
    windowClass.clear();
    windowTitle.clear();
    HWND hwndFocus = _ResolveFocusWindow(pDocMgr, nullptr, nullptr);
    if (hwndFocus == nullptr)
        return;
    HWND hwndRoot = GetAncestor(hwndFocus, GA_ROOT);
    if (hwndRoot == nullptr)
        hwndRoot = hwndFocus;
    windowClass = _QueryWindowClass(hwndRoot);
    if (wind::window_title::ShouldCollectTitle(IsTitleMatchEnabled() != FALSE))
        windowTitle = _QueryWindowTitle(hwndRoot);
}

// 采集并上报一次诊断快照。见 TextService.h 的声明注释。
void CTextService::SendDiagSnapshotIfEnabled(ITfDocumentMgr* pDocMgr, BOOL docMgrChanged)
{
    // 关闭时一次 Win32 调用都不做。这个早退是本功能"默认零开销"的全部依据。
    if (!_diagSnapshotEnabled)
        return;
    if (_pIPCClient == nullptr || !_pIPCClient->IsConnected())
        return;

    DiagSnapshotHeader head = {};
    head.pid = GetCurrentProcessId();
    head.focusSessionId = (uint32_t)(_focusSessionId & 0xFFFFFFFFULL);
    head.flags = docMgrChanged ? DIAG_FLAG_DOCMGR_CHANGED : (uint8_t)0;
    head.docMgrId = (uint64_t)(uintptr_t)pDocMgr;

    // 焦点窗口解析与 focus_gained 的窗口类上报共用同一个函数，见 _ResolveFocusWindow。
    uint8_t src = WND_SRC_NONE;
    HWND hwndFocus = _ResolveFocusWindow(pDocMgr, &src, &head.contextId);

    HWND hwndFg = GetForegroundWindow();
    if (hwndFocus == nullptr && hwndFg != nullptr)
    {
        hwndFocus = hwndFg;
        src = WND_SRC_FOREGROUND;
    }

    HWND hwndRoot = (hwndFocus != nullptr) ? GetAncestor(hwndFocus, GA_ROOT) : nullptr;

    head.focusHwnd = (uint64_t)(uintptr_t)hwndFocus;
    head.focusHwndSource = src;
    head.rootHwnd = (uint64_t)(uintptr_t)hwndRoot;
    head.rootBand = _QueryWindowBand(hwndRoot);
    head.fgHwnd = (uint64_t)(uintptr_t)hwndFg;
    if (hwndFg != nullptr)
    {
        DWORD fgPid = 0;
        GetWindowThreadProcessId(hwndFg, &fgPid);
        head.fgPid = fgPid;
    }
    // 报实际建成的 band，不是当初想建的那个（见 CHostWindow::GetCurrentBand）。
    CHostWindow* pCandHost = _pHostWindow[HOST_WINDOW_CANDIDATE];
    head.hostBand = (pCandHost != nullptr) ? pCandHost->GetCurrentBand() : 0;

    _pIPCClient->SendDiagSnapshot(head,
                                  _QueryWindowClass(hwndFocus),
                                  _QueryWindowClass(hwndRoot),
                                  _QueryWindowClass(hwndFg));
}

BOOL CTextService::_DocMgrHasEditableContext(ITfDocumentMgr* pDocMgr, DWORD* pDynFlagsOut,
                                             DWORD* pStatFlagsOut)
{
    if (pDynFlagsOut)
        *pDynFlagsOut = 0;
    if (pStatFlagsOut)
        *pStatFlagsOut = 0;

    if (pDocMgr == nullptr)
        return FALSE;

    ITfContext* pCtx = nullptr;
    HRESULT hr = pDocMgr->GetTop(&pCtx);
    if (FAILED(hr) || pCtx == nullptr)
    {
        WIND_LOG_DEBUG_FMT(L"_DocMgrHasEditableCtx: GetTop hr=0x%08X ctx=%p -> FALSE", hr, pCtx);
        if (pCtx) pCtx->Release();
        return FALSE;
    }

    TF_STATUS status = {};
    HRESULT hrStatus = pCtx->GetStatus(&status);
    // 判据本体在 EditableContextPolicy.h（纯逻辑、可单测），依据写在那里：成功时只认 TF_SD_READONLY
    // （TS_SS_TRANSITORY 单判不可靠，Chrome 与 JetBrains 都挂在能打字的 context 上）；失败时只有
    // TF_E_EMPTYCONTEXT 判不可编辑，其余未知失败码走宽松兜底。
    BOOL result = wind::editable::ContextIsEditable(static_cast<std::uint32_t>(hrStatus),
                                                    status.dwDynamicFlags)
                      ? TRUE
                      : FALSE;
    if (SUCCEEDED(hrStatus))
    {
        WIND_LOG_DEBUG_FMT(L"_DocMgrHasEditableCtx: dynFlags=0x%X statFlags=0x%X", status.dwDynamicFlags, status.dwStaticFlags);
        if (pDynFlagsOut)
            *pDynFlagsOut = status.dwDynamicFlags;
        if (pStatFlagsOut)
            *pStatFlagsOut = status.dwStaticFlags;
    }
    else
    {
        // `hr=0x80040509`（TF_E_EMPTYCONTEXT）是 Chromium 给「焦点不在可编辑元素上」准备的 DocMgr：
        // 照常建 context 但不挂 text store。把它兜底成 TRUE，点网页空白处也会发 focus_gained，工具栏永不隐藏。
        WIND_LOG_DEBUG_FMT(L"_DocMgrHasEditableCtx: GetStatus hr=0x%08X -> %s",
                           hrStatus, result ? L"TRUE(未知失败码, 宽松兜底)" : L"FALSE(空上下文)");
    }

    WIND_LOG_DEBUG_FMT(L"_DocMgrHasEditableCtx: -> %d", result);
    pCtx->Release();
    return result;
}

// Helper function to check if a window is a console/terminal window
static BOOL IsConsoleWindow(HWND hwnd)
{
    if (hwnd == nullptr)
        return FALSE;

    WCHAR className[256] = {0};
    if (GetClassNameW(hwnd, className, 256) == 0)
        return FALSE;

    // Check for known console window classes
    // ConsoleWindowClass - Traditional conhost.exe console
    // CASCADIA_HOSTING_WINDOW_CLASS - Windows Terminal
    // PseudoConsoleWindow - ConPTY pseudo console
    if (wcscmp(className, L"ConsoleWindowClass") == 0 ||
        wcscmp(className, L"CASCADIA_HOSTING_WINDOW_CLASS") == 0 ||
        wcsstr(className, L"Console") != nullptr ||
        wcsstr(className, L"Terminal") != nullptr)
    {
        return TRUE;
    }

    return FALSE;
}

// Try to get caret position for console/terminal windows
static BOOL GetConsoleCaretPosition(HWND hwndConsole, LONG* px, LONG* py, LONG* pHeight)
{
    if (hwndConsole == nullptr)
        return FALSE;

    // For Windows Terminal and modern consoles, we can try to get the console buffer info
    // This requires the console to be attached to our process or accessible

    // First, try to get the console window handle and screen buffer info
    // Note: GetConsoleWindow() returns the console for the CURRENT process,
    // which may not be the foreground console. We need a different approach.

    // Get window rect for calculations
    RECT rcWindow;
    if (!GetWindowRect(hwndConsole, &rcWindow))
        return FALSE;

    // Get client rect
    RECT rcClient;
    if (!GetClientRect(hwndConsole, &rcClient))
        return FALSE;

    // Calculate client area origin in screen coordinates
    POINT clientOrigin = {0, 0};
    ClientToScreen(hwndConsole, &clientOrigin);

    // Try to use GUITHREADINFO - sometimes works for console windows
    DWORD threadId = GetWindowThreadProcessId(hwndConsole, nullptr);
    GUITHREADINFO guiInfo = { sizeof(GUITHREADINFO) };

    if (GetGUIThreadInfo(threadId, &guiInfo) && guiInfo.hwndCaret != nullptr)
    {
        POINT caretPos;
        caretPos.x = guiInfo.rcCaret.left;
        caretPos.y = guiInfo.rcCaret.bottom;

        // Convert from client coordinates to screen coordinates
        ClientToScreen(guiInfo.hwndCaret, &caretPos);

        // Validate that it's within the console window area
        if (caretPos.x >= rcWindow.left && caretPos.x <= rcWindow.right &&
            caretPos.y >= rcWindow.top && caretPos.y <= rcWindow.bottom)
        {
            *px = caretPos.x;
            *py = caretPos.y;
            // 不依赖 max 宏（MSVC 由 <windows.h> 提供、MinGW 不一定有），用等价三元表达式
            LONG caretH = guiInfo.rcCaret.bottom - guiInfo.rcCaret.top;
            *pHeight = (caretH > 16) ? caretH : 16;

            WIND_LOG_DEBUG(L"GetConsoleCaretPosition: Got caret from GUITHREADINFO\n");
            return TRUE;
        }
    }

    // Fallback: Position the candidate window at a reasonable location
    // For consoles, we position it near the bottom of the visible area
    // This is better than the center, as typing usually happens at the bottom

    // Estimate: console typically shows text near the current cursor line
    // Position the IME window near the bottom-left of the console
    int clientWidth = rcClient.right - rcClient.left;
    int clientHeight = rcClient.bottom - rcClient.top;

    // Position at roughly 10% from left, 80% from top (near bottom where typing usually occurs)
    *px = clientOrigin.x + (clientWidth * 10 / 100);
    *py = clientOrigin.y + (clientHeight * 80 / 100);
    *pHeight = 16;  // Standard console line height approximation

    WIND_LOG_DEBUG_FMT(L"GetConsoleCaretPosition: Using console fallback position (%ld, %ld)\n", *px, *py);

    return TRUE;
}

BOOL CTextService::GetCaretPosition(LONG* px, LONG* py, LONG* pHeight, int* pSource,
                                    RECT* pCompRect, BOOL* pHasCompRect, BOOL* pRejectedOutsideView)
{
    if (pSource)
    {
        *pSource = CARET_SRC_UNKNOWN;
    }

    // First, check if the foreground window is a console/terminal
    HWND hwndForeground = GetForegroundWindow();
    BOOL isConsole = IsConsoleWindow(hwndForeground);

    if (isConsole)
    {
        WIND_LOG_DEBUG(L"GetCaretPosition: Detected console window\n");
    }

    // Method 1: Try TSF APIs first - this is the most reliable for browsers and modern apps
    // ITfContextView::GetTextExt provides accurate caret position in Chrome, Edge, etc.
    BOOL usedCompStart = FALSE;
    BOOL isDefaultPos = FALSE;
    BOOL usedImm = FALSE;
    // 调用方没要越界原因也得自己留一份：下面「别拿 Win32 光标顶替」的守卫对所有调用方生效
    // （OnSetFocus / _ResendFocusGained 都靠它沿用上次坐标），不能只对传了出参的调用方生效。
    BOOL rejectedLocal = FALSE;
    BOOL* pRejected = pRejectedOutsideView ? pRejectedOutsideView : &rejectedLocal;
    if (GetCaretPositionFromTSF(px, py, pHeight, &usedCompStart, pCompRect, pHasCompRect, &isDefaultPos,
                                &usedImm, pRejected))
    {
        if (pSource)
        {
            // 默认位置优先：它答的是「这里没有插入点」，与另外几级的「插入点在这儿」
            // 语义相反，标成 TSF 权威源会让服务端拿它做跟行/漂移校正。
            *pSource = isDefaultPos    ? CARET_SRC_TSF_DEFAULT_POS
                       : usedImm       ? CARET_SRC_IMM_CANDIDATE_FORM
                       : usedCompStart ? CARET_SRC_TSF_COMPOSITION
                                       : CARET_SRC_TSF_SELECTION;
        }
        return TRUE;
    }

    // 宿主给的是落在本输入框外的旧坐标：别拿 Win32 光标顶替（Chromium 的系统光标同样是旧值），
    // 交调用方沿用上次坐标。
    if (*pRejected)
    {
        WIND_LOG_DEBUG(L"GetCaretPosition: TSF 坐标越出显示区，交由 last known\n");
        return FALSE;
    }

    // For console windows, use specialized handling
    if (isConsole)
    {
        if (GetConsoleCaretPosition(hwndForeground, px, py, pHeight))
        {
            // Save as last known good position
            s_lastCaretX = *px;
            s_lastCaretY = *py;
            s_lastCaretHeight = *pHeight;
            s_hasLastCaretPos = TRUE;
            if (pSource)
            {
                *pSource = CARET_SRC_CONSOLE;
            }
            return TRUE;
        }
    }

    // 出现过宿主默认位置指纹的线程（见 ImmOpenCandidateNudge.h 的 IsGuiCaretUnreliable）：GUI caret 不可用，
    // 直接判失败，调用方退到 last known。Java 的系统光标恒在窗口客户区原点，报上去候选窗会先闪到屏幕左边；
    // 而 Windows 输入编辑框位置不随单元格变，last known 恰是它。
    if (wind::caret::IsGuiCaretUnreliable())
    {
        WIND_LOG_DEBUG(L"GetCaretPosition: IMM32 宿主不取 GUI caret（恒为窗口原点），交由 last known\n");
        return FALSE;
    }

    // Method 3: Try to get caret position from the GUI thread info
    // This works well for traditional Win32 applications
    GUITHREADINFO guiInfo = { sizeof(GUITHREADINFO) };

    if (GetGUIThreadInfo(0, &guiInfo))
    {
        // Check if there's an active caret
        if (guiInfo.hwndCaret != nullptr)
        {
            POINT caretPos;
            caretPos.x = guiInfo.rcCaret.left;
            caretPos.y = guiInfo.rcCaret.bottom;

            // Convert from client coordinates to screen coordinates
            ClientToScreen(guiInfo.hwndCaret, &caretPos);

            // Validate position (not at origin, which usually means failure)
            if (caretPos.x > 0 || caretPos.y > 0)
            {
                *px = caretPos.x;
                *py = caretPos.y;
                *pHeight = guiInfo.rcCaret.bottom - guiInfo.rcCaret.top;

                if (*pHeight <= 0)
                    *pHeight = 20;  // Default caret height

                // Save as last known good position
                s_lastCaretX = *px;
                s_lastCaretY = *py;
                s_lastCaretHeight = *pHeight;
                s_hasLastCaretPos = TRUE;

                // 这是跨窗口的 Win32 光标，不属于当前 TSF context，宿主只在部分场景维护它（Word 仅正文行），
                // 故标为 GUI 源，消费端不得当权威坐标。
                if (pSource)
                {
                    *pSource = CARET_SRC_GUI_CARET;
                }
                return TRUE;
            }
        }
    }

    // Fallback to GetCaretPos
    POINT pt;
    if (GetCaretPos(&pt))
    {
        // Get the foreground window to convert coordinates
        HWND hwnd = GetForegroundWindow();
        if (hwnd != nullptr)
        {
            ClientToScreen(hwnd, &pt);

            // Validate position
            if (pt.x > 0 || pt.y > 0)
            {
                *px = pt.x;
                *py = pt.y + 20;  // Estimate caret height
                *pHeight = 20;

                // Save as last known good position
                s_lastCaretX = *px;
                s_lastCaretY = *py;
                s_lastCaretHeight = *pHeight;
                s_hasLastCaretPos = TRUE;

                if (pSource)
                {
                    *pSource = CARET_SRC_GUI_CARET;
                }
                return TRUE;
            }
        }
    }

    // Method 4: For browsers/WebView2, try to get focus window position
    // Browsers often don't expose caret position properly, so we use the focus window
    HWND hwndFocus = GetForegroundWindow();
    if (hwndFocus != nullptr)
    {
        RECT rc;
        if (GetWindowRect(hwndFocus, &rc))
        {
            // If we have a last known position within this window, use it
            if (s_hasLastCaretPos &&
                s_lastCaretX >= rc.left && s_lastCaretX <= rc.right &&
                s_lastCaretY >= rc.top && s_lastCaretY <= rc.bottom)
            {
                *px = s_lastCaretX;
                *py = s_lastCaretY;
                *pHeight = s_lastCaretHeight;
                if (pSource)
                {
                    *pSource = CARET_SRC_LAST_KNOWN;
                }
                return TRUE;
            }

            // Otherwise, position near the center-left of the window
            // This is a fallback for browsers that don't report caret position
            *px = rc.left + 100;  // Some offset from left edge
            *py = rc.top + (rc.bottom - rc.top) / 2;  // Vertical center
            *pHeight = 20;

            WIND_LOG_DEBUG(L"GetCaretPosition: Using window position fallback\n");
            if (pSource)
            {
                *pSource = CARET_SRC_LAST_KNOWN;
            }
            return TRUE;
        }
    }

    // Method 5: Use last known good position if available
    if (s_hasLastCaretPos)
    {
        *px = s_lastCaretX;
        *py = s_lastCaretY;
        *pHeight = s_lastCaretHeight;
        WIND_LOG_DEBUG(L"GetCaretPosition: Using last known position\n");
        return TRUE;
    }

    WIND_LOG_DEBUG(L"GetCaretPosition: Failed to get caret position\n");
    return FALSE;
}

// 宿主进程不是 Per-Monitor DPI aware 时把逻辑坐标换成物理屏幕坐标：DPI-unaware 宿主拿到的是
// 虚拟化的 96-DPI 坐标，而服务端（wind_input.exe）是 Per-Monitor DPI aware、按物理像素工作。
static void ConvertToPhysicalCoordinates(LONG& x, LONG& y, LONG& height,
                                         LONG& compStartX, LONG& compStartY)
{
    // Dynamically load to support older Windows versions
    static auto pGetProcessDpiAwareness =
        reinterpret_cast<decltype(&GetProcessDpiAwareness)>(
            GetProcAddress(GetModuleHandleW(L"shcore.dll"), "GetProcessDpiAwareness"));
    static auto pLogicalToPhysicalPointForPerMonitorDPI =
        reinterpret_cast<BOOL(WINAPI*)(HWND, LPPOINT)>(
            GetProcAddress(GetModuleHandleW(L"user32.dll"), "LogicalToPhysicalPointForPerMonitorDPI"));
    static auto pGetDpiForMonitor =
        reinterpret_cast<decltype(&GetDpiForMonitor)>(
            GetProcAddress(GetModuleHandleW(L"shcore.dll"), "GetDpiForMonitor"));

    if (!pGetProcessDpiAwareness || !pLogicalToPhysicalPointForPerMonitorDPI)
        return;

    PROCESS_DPI_AWARENESS awareness = PROCESS_PER_MONITOR_DPI_AWARE;
    if (FAILED(pGetProcessDpiAwareness(nullptr, &awareness)))
        return;

    if (awareness == PROCESS_PER_MONITOR_DPI_AWARE)
        return; // Already physical coordinates, no conversion needed

    WIND_LOG_DEBUG_FMT(L"ConvertToPhysicalCoordinates: host DPI awareness=%d, before: caret(%ld,%ld h=%ld) comp(%ld,%ld)",
                       (int)awareness, x, y, height, compStartX, compStartY);

    HWND hwnd = GetForegroundWindow();
    if (!hwnd)
        return;

    // Convert caret position
    POINT ptCaret = { x, y };
    if (!pLogicalToPhysicalPointForPerMonitorDPI(hwnd, &ptCaret))
        return;

    // Convert a second point to derive the physical height
    POINT ptCaretTop = { x, y - height };
    if (!pLogicalToPhysicalPointForPerMonitorDPI(hwnd, &ptCaretTop))
    {
        // Fallback: scale height using monitor DPI
        if (pGetDpiForMonitor)
        {
            HMONITOR hMon = MonitorFromPoint(ptCaret, MONITOR_DEFAULTTONEAREST);
            UINT dpiX = 96, dpiY = 96;
            if (hMon && SUCCEEDED(pGetDpiForMonitor(hMon, MDT_EFFECTIVE_DPI, &dpiX, &dpiY)))
            {
                height = MulDiv(height, (int)dpiX, 96);
            }
        }
    }
    else
    {
        height = ptCaret.y - ptCaretTop.y;
    }

    x = ptCaret.x;
    y = ptCaret.y;

    // Convert composition start position if present
    if (compStartX != 0 || compStartY != 0)
    {
        POINT ptComp = { compStartX, compStartY };
        if (pLogicalToPhysicalPointForPerMonitorDPI(hwnd, &ptComp))
        {
            compStartX = ptComp.x;
            compStartY = ptComp.y;
        }
    }

    WIND_LOG_DEBUG_FMT(L"ConvertToPhysicalCoordinates: after: caret(%ld,%ld h=%ld) comp(%ld,%ld)",
                       x, y, height, compStartX, compStartY);
}

// 与 ConvertToPhysicalCoordinates 同源的单点转换：组合矩形的两个角点各走一次。
//
// 组合矩形必须与 caret / compStart 走同一套转换，否则同一帧混进两个坐标系，服务端互相比较的差值毫无意义。
static void ConvertPointToPhysical(LONG& x, LONG& y)
{
    static auto pGetProcessDpiAwareness =
        reinterpret_cast<decltype(&GetProcessDpiAwareness)>(
            GetProcAddress(GetModuleHandleW(L"shcore.dll"), "GetProcessDpiAwareness"));
    static auto pLogicalToPhysicalPointForPerMonitorDPI =
        reinterpret_cast<BOOL(WINAPI*)(HWND, LPPOINT)>(
            GetProcAddress(GetModuleHandleW(L"user32.dll"), "LogicalToPhysicalPointForPerMonitorDPI"));

    if (!pGetProcessDpiAwareness || !pLogicalToPhysicalPointForPerMonitorDPI)
        return;

    PROCESS_DPI_AWARENESS awareness = PROCESS_PER_MONITOR_DPI_AWARE;
    if (FAILED(pGetProcessDpiAwareness(nullptr, &awareness)))
        return;

    if (awareness == PROCESS_PER_MONITOR_DPI_AWARE)
        return; // 已是物理坐标

    HWND hwnd = GetForegroundWindow();
    if (!hwnd)
        return;

    POINT pt = { x, y };
    if (pLogicalToPhysicalPointForPerMonitorDPI(hwnd, &pt))
    {
        x = pt.x;
        y = pt.y;
    }
}

// 坐标出口：DPI 归一 → 记为 last known → 发 IPC。同步与异步两条取坐标路径共用，保证「记住的坐标」与
// 「发出去的坐标」是同一份。
void CTextService::_EmitCaretUpdate(LONG x, LONG y, LONG height, LONG compStartX, LONG compStartY, int source,
                                   const RECT* pCompRect)
{
    ConvertToPhysicalCoordinates(x, y, height, compStartX, compStartY);

    // 组合矩形与上面同批转换。四值全 0 = 本帧没取到，此时不转换：转换会把 (0,0) 映射成某个真实坐标。
    LONG rectL = 0, rectT = 0, rectR = 0, rectB = 0;
    if (pCompRect != nullptr &&
        (pCompRect->left != 0 || pCompRect->top != 0 || pCompRect->right != 0 || pCompRect->bottom != 0))
    {
        rectL = pCompRect->left;
        rectT = pCompRect->top;
        rectR = pCompRect->right;
        rectB = pCompRect->bottom;
        ConvertPointToPhysical(rectL, rectT);
        ConvertPointToPhysical(rectR, rectB);
    }

    if (source == CARET_SRC_TSF_SELECTION || source == CARET_SRC_TSF_COMPOSITION || source == CARET_SRC_TSF_CACHED)
    {
        _hostGaveTsfCaret = TRUE;
    }
    _hasLastKnownCaretPos = TRUE;
    _lastKnownCaretX = x;
    _lastKnownCaretY = y;
    _lastKnownCaretHeight = height > 0 ? height : DEFAULT_CARET_HEIGHT;

    if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
    {
        _pIPCClient->SendCaretUpdate((int)x, (int)y, (int)height, (int)compStartX, (int)compStartY, source,
                                     (int)rectL, (int)rectT, (int)rectR, (int)rectB);
    }
}

// 非按键上下文里发起取坐标：见 CCaretEditSession::RequestCaretRectAsync 的说明。
// 返回 FALSE 表示请求根本没发出去（无焦点 context 等），调用方需自己兜底。
BOOL CTextService::RequestCaretPositionUpdateAsync(CaretProbeKind kind)
{
    // 恢复中组合已被宿主收走、编码仍在候选窗里：照样要取坐标（无组合时按选区取）。
    if (_pThreadMgr == nullptr || !HasCompositionOrRecover())
    {
        return FALSE;
    }

    ITfDocumentMgr* pDocMgr = nullptr;
    HRESULT hr = _pThreadMgr->GetFocus(&pDocMgr);
    if (FAILED(hr) || pDocMgr == nullptr)
    {
        return FALSE;
    }

    ITfContext* pContext = nullptr;
    hr = pDocMgr->GetTop(&pContext);
    pDocMgr->Release();
    if (FAILED(hr) || pContext == nullptr)
    {
        return FALSE;
    }

    BOOL requested = CCaretEditSession::RequestCaretRectAsync(
        pContext, _tfClientId, _pComposition, (LONG)GetPendingCommitPrefixLength(), this,
        kind, 0);
    pContext->Release();

    return requested;
}

// OnSetFocus 专用的异步取坐标，见头文件说明。
//
// 焦点路径必须走异步：OnSetFocus 不是按键上下文，同步编辑会话会被拒（TS_E_SYNCHRONOUS），
// GetCaretPosition 随之下坠到 GetGUIThreadInfo 这个跨窗口的 Win32 光标，在 Word 标题行上是别处的陈旧值。
BOOL CTextService::RequestFocusCaretAsync(ITfDocumentMgr* pDocMgrFocus)
{
    if (pDocMgrFocus == nullptr)
    {
        return FALSE;
    }
    // UI-less / 宿主接管绘制：焦点期这次取坐标也省了（不弹窗，独占全屏下一次 GetTextExt 都不发）。
    if (_CaretQuerySuppressed())
    {
        return FALSE;
    }

    // 同一焦点会话只探一次：OnSetFocus 在 DocMgr 抖动时会被反复调用（Excel 单元格切换、
    // 浏览器 SPA 导航），不去重就是给宿主刷 edit session 请求。
    if (_focusCaretProbedSession == _focusSessionId)
    {
        return FALSE;
    }
    _focusCaretProbedSession = _focusSessionId;

    ITfContext* pContext = nullptr;
    HRESULT hr = pDocMgrFocus->GetTop(&pContext);
    if (FAILED(hr) || pContext == nullptr)
    {
        return FALSE;
    }

    // 传 nullptr 组合：焦点刚到达时本就没有组合，会话只取 selection 的 GetTextExt，「组合起点降级」走不到；
    // 取不到就由回调如实丢弃，不在这条路上造回退值。
    BOOL requested = CCaretEditSession::RequestCaretRectAsync(
        pContext, _tfClientId, nullptr, 0, this,
        CaretProbeKind::Focus, _focusSessionId);
    pContext->Release();

    return requested;
}

// 异步编辑会话的回调出口。刻意不做任何回退：取不到就不发，服务端用按键时缓存的坐标兜底（来自按键路径的
// 同步会话），比回退到 GetGUIThreadInfo 拿到的无关窗口 caret 可信得多。
void CTextService::OnAsyncCaretRectReady(const AsyncCaretResult& result)
{
    const RECT& caretRect     = result.caretRect;
    const RECT& compStartRect = result.compStartRect;
    // 组合矩形只在真取到时下传；没取到传 nullptr，让 _EmitCaretUpdate 上报四值全 0。
    const RECT* pCompRect     = result.hasCompRect ? &result.compRect : nullptr;
    const BOOL  isFocusProbe  = (result.kind == CaretProbeKind::Focus);

    // 归属校验，两种用途判据不同、不能合并：
    //   Composition —— 排队期间用户可能已上屏，坐标属于上一轮组合，发出去会钉在已消失的组合上；
    //   Focus       —— 此时本就没有组合（拿 _pComposition 判会静默全丢），改判焦点会话号：
    //                  回调到达前用户可能已切走，坐标属于上一个应用。
    if (isFocusProbe)
    {
        if (result.sessionTag != _focusSessionId)
        {
            WIND_LOG_DEBUG_FMT(L"OnAsyncCaretRectReady(focus): 焦点已翻篇 tag=%llu now=%llu, dropping\n",
                               result.sessionTag, _focusSessionId);
            return;
        }
    }
    else if (!HasCompositionOrRecover())
    {
        WIND_LOG_DEBUG(L"OnAsyncCaretRectReady: composition already ended, dropping\n");
        return;
    }

    const wchar_t* kindName = isFocusProbe                                      ? L"focus"
                              : (result.kind == CaretProbeKind::FirstShowProbe) ? L"first_show_probe"
                              : (result.kind == CaretProbeKind::RetryDeadline)  ? L"retry_deadline"
                                                                                : L"composition";

    LONG height = caretRect.bottom - caretRect.top;
    // 「本 context 没有插入点可报」：退化矩形在重试窗口过完之后仍是它。
    //
    // 普通 probe 见到退化矩形一律丢弃（宿主还在排版）；RetryDeadline 已等过一整个重试窗口，
    // 仍是同一个退化矩形就是宿主在答「没有插入点」（如 Illustrator 画布）。丢弃会让回退链跌到
    // GUIThreadInfo 的无关光标、候选窗钉在左上角（见 BinaryProtocol.h 的 CARET_SRC_TSF_DEFAULT_POS）。
    // 高度须合成：0 高会被服务端 `caret_is_valid` 丢掉，补 WIND_DEFAULT_CARET_HEIGHT，与 GUI 回退同口径。
    // 判据是指纹（选区 / 组合起点 / 组合矩形三者全同，wind::caret::IsHostDefaultPosition），不是
    // 「退化就采信」：证据只来自一家宿主，全局放行有弄坏别的宿主的先例。
    BOOL isDefaultPos = FALSE;
    if (height <= 0)
    {
        const BOOL fingerprint = wind::caret::IsHostDefaultPosition(
            caretRect, result.hasCompStart, compStartRect, result.hasCompRect, result.compRect);
        if (result.kind != CaretProbeKind::RetryDeadline || !fingerprint)
        {
            // 退化矩形 = 宿主尚未完成排版，判据同 GetCaretPositionFromTSF。
            WIND_LOG_DEBUG_FMT(L"OnAsyncCaretRectReady: degenerate rect (height=0), dropping"
                               L" (retry_deadline=%d fingerprint=%d)\n",
                               result.kind == CaretProbeKind::RetryDeadline ? 1 : 0,
                               fingerprint ? 1 : 0);
            return;
        }
        isDefaultPos = TRUE;
        height = DEFAULT_CARET_HEIGHT;
        // 闩住判决供本次组合内的同步路径复用：每个按键都会走 GetCaretPositionFromTSF 拿到同一个
        // 退化矩形，不闩住就跌到 GUI_CARET，服务端大偏移逃生阀会把锚点重锁过去，第二个键又跳回左上角。
        // StartComposition 处复位。
        _caretDefaultPosLatched = TRUE;
        WIND_LOG_INFO_FMT(L"OnAsyncCaretRectReady(retry_deadline): 重试窗口过完仍退化 "
                          L"rect=(%ld,%ld,%ld,%ld)，按宿主默认位置采信，补高度 h=%ld\n",
                          caretRect.left, caretRect.top, caretRect.right, caretRect.bottom,
                          height);
    }
    else if (result.kind == CaretProbeKind::Composition || result.kind == CaretProbeKind::RetryDeadline)
    {
        // 宿主拿得出有高度的矩形 ⇒ 有插入点，之前「没有插入点」的判决作废。与同步路径同口径：
        // 任何一帧有效矩形都撤销判决。
        //
        // 白名单而非黑名单：只有本组合正式取坐标的这两个 kind 有资格作废判决。
        //   FirstShowProbe —— 约定不改变任何现有行为；它组合刚启动就发，若与 RetryDeadline 回调交错
        //     会无声撤销刚作出的判决（「有时好有时钉在左上角」）。
        //   Focus —— 讲的是另一个 context，拿它作废旧组合的判决没有依据。
        // 内层再判一次只为日志：热路径，只在判决作废那一刻打。
        if (_caretDefaultPosLatched)
        {
            WIND_LOG_DEBUG_FMT(L"退化判决作废: by=async_valid_rect(%s) h=%ld\n", kindName, height);
            _caretDefaultPosLatched = FALSE;
        }
    }

    // 本路径丢弃即终点（没有回退链）。三处越界判据已统一：同步路径虽有回退链，但回退到的
    // GUIThreadInfo 值是别的 shell 窗口残留的 caret，比被丢弃的 TSF 坐标差得多。
    if (IsScreenPointOutsideAllMonitors(caretRect.left, caretRect.top))
    {
        WIND_LOG_DEBUG_FMT(L"OnAsyncCaretRectReady: caret(%ld,%ld) outside all monitors, dropping\n",
                           caretRect.left, caretRect.top);
        return;
    }

    LONG compStartX = 0, compStartY = 0;
    if (result.hasCompStart)
    {
        compStartX = compStartRect.left;
        compStartY = compStartRect.bottom;
    }

    // 默认位置优先级最高：它说的是「这里根本没有插入点」，比 selection / 组合起点降级
    // 都更靠后（走到这里两者都已退化），标错会让服务端把它当真插入点参与跟行。
    const int source = isDefaultPos                  ? CARET_SRC_TSF_DEFAULT_POS
                       : result.usedImmCandidateForm ? CARET_SRC_IMM_CANDIDATE_FORM
                       : result.usedCompStartAsCaret ? CARET_SRC_TSF_COMPOSITION
                                                     : CARET_SRC_TSF_SELECTION;
    WIND_LOG_DEBUG_FMT(L"OnAsyncCaretRectReady(%s): caret(%ld,%ld h=%ld) compStart=(%ld,%ld) src=%d\n",
                       kindName,
                       caretRect.left, caretRect.bottom, height, compStartX, compStartY, source);

    // 首显试探：以 CARET_SRC_PRE_REFLOW 发出，服务端只拿它刷新缓存、记作「本轮重排前的坐标」当否决基准
    // （后续 probe 与它相同即判未重排），不拿它定位置。
    // 多数宿主内联执行这次请求，拿到的是 reflow 前的坐标：拿它做首显决策会把错位固定下来；但连续快速上屏
    // 时 OnLayoutChange 被 debounce 压住、权威坐标一条都不来，它是那段时间唯一的位置信息。
    // 不要改回普通 probe 通道，见 docs/architecture/caret-position-host-compat-notes.md 的「已否定的方向」。
    if (result.kind == CaretProbeKind::FirstShowProbe)
    {
        if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
        {
            _pIPCClient->SendCaretProbe(caretRect.left, caretRect.bottom, height, compStartX,
                                        compStartY, CARET_SRC_PRE_REFLOW);
        }
        return;
    }

    if (isFocusProbe)
    {
        // 刷新焦点 caret 缓存。内联档此刻仍在 OnSetFocus 栈里，这次写入赶在同一次 SendFocusGained 读取之前，
        // 权威坐标直接随焦点包发出去。
        _lastFocusCaretX      = caretRect.left;
        _lastFocusCaretY      = caretRect.bottom;
        _lastFocusCaretHeight = height;
        _lastFocusCaretSource = source;

        // 排队档（Word 为 TF_S_ASYNC）此刻 focus_gained 已发走，只能补一条 caret_update 修正服务端缓存。
        // 内联档跳过：那条会早于 focus_gained 到达，随后被焦点包里的坐标覆写，白多一次 IPC。
        if (_focusGainedSentForSession == _focusSessionId)
        {
            _EmitCaretUpdate(caretRect.left, caretRect.bottom, height, compStartX, compStartY, source,
                             pCompRect);
        }
        else
        {
            WIND_LOG_DEBUG(L"OnAsyncCaretRectReady(focus): 内联返回，坐标随 focus_gained 一并发出，跳过补发\n");
        }
        return;
    }

    _EmitCaretUpdate(caretRect.left, caretRect.bottom, height, compStartX, compStartY, source, pCompRect);
}

void CTextService::SendCaretPositionUpdate()
{
    // UI-less / 宿主接管绘制：不弹自己的窗 ⇒ 不需要光标；反复取坐标会在独占全屏拖死
    // 游戏（见 _CaretQuerySuppressed 注释）。timer 兜底也只在本函数里设，提前返回即不设。
    if (_CaretQuerySuppressed())
    {
        return;
    }
    // 组合刚创建后第一次调用不立即发 IPC：宿主尚未完成 reflow，GetTextExt 可能是旧坐标（WPS h>0 但陈旧），
    // 先发会让候选窗先错位再跳。等 OnLayoutChange（reflow 完成的信号），50ms timer 兜底。
    if (_compositionJustStarted && _pComposition != nullptr)
    {
        if (_pLangBarItemButton != nullptr)
        {
            _pLangBarItemButton->PostDelayedCaretPositionUpdate();
        }
        // 首显试探：发一次异步取坐标，结果以 PRE_REFLOW 上报（见 OnAsyncCaretRectReady）。
        RequestCaretPositionUpdateAsync(CaretProbeKind::FirstShowProbe);

        // 通知服务端组合刚启动、真坐标会在 reflow 后到达，服务端据此延长首显等待，
        // 避免回退到按键前的旧坐标（OnLayoutChange burst 跨度长的宿主如 EverEdit）。
        if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
        {
            _pIPCClient->SendCaretPending();
        }
        return;
    }

    LONG x = 0, y = 0, height = 0;
    LONG compStartX = 0, compStartY = 0;
    BOOL hasPosition = FALSE;
    int  source = CARET_SRC_UNKNOWN;

    // Priority 1：用编辑会话里缓存的位置（WebView 类宿主会拒绝 TF_INVALID_COOKIE 的独立
    // CaretEditSession）。缓存在 CUpdateCompositionEditSession::DoEditSession 里写入，
    // 保证 caret 与组合起点出自同一会话、同一坐标空间。
    // 整个组合 range 的包围矩形：两条分支各自填充，最后一并上报（四值全 0 = 本帧没有）。
    RECT compRect = {};
    BOOL hasCompRect = FALSE;

    if (_hasCachedCaretPos)
    {
        x = _cachedCaretRect.left;
        y = _cachedCaretRect.bottom;
        height = _cachedCaretRect.bottom - _cachedCaretRect.top;
        hasPosition = TRUE;
        // 缓存写自 UpdateComposition 的 edit session，与 caret/compStart 同源同 cookie，属 TSF 域
        source = CARET_SRC_TSF_CACHED;

        if (_hasCachedCompStartPos)
        {
            compStartX = _cachedCompStartRect.left;
            compStartY = _cachedCompStartRect.bottom;
        }

        if (_hasCachedCompRect)
        {
            compRect = _cachedCompRect;
            hasCompRect = TRUE;
        }

        // Clear cache (one-shot: next call falls back to normal methods)
        _hasCachedCaretPos = FALSE;
        _hasCachedCompStartPos = FALSE;
        _hasCachedCompRect = FALSE;
    }

    // Priority 2: Normal method (separate edit session + fallbacks)
    if (!hasPosition)
    {
        BOOL rejectedOutsideView = FALSE;
        if (!GetCaretPosition(&x, &y, &height, &source, &compRect, &hasCompRect, &rejectedOutsideView))
        {
            if (rejectedOutsideView)
            {
                WIND_LOG_DEBUG(L"SendCaretPositionUpdate: 坐标越出编辑区，跳过本次位置更新\n");
                return;
            }
            if (_hasLastKnownCaretPos)
            {
                x = _lastKnownCaretX;
                y = _lastKnownCaretY;
                height = _lastKnownCaretHeight;
                hasPosition = TRUE;
                source = CARET_SRC_LAST_KNOWN;
                WIND_LOG_INFO_FMT(L"SendCaretPositionUpdate: using last known caret x=%ld y=%ld h=%ld", x, y, height);
            }
            else
            {
                return; // No position available at all
            }
        }
        else
        {
            hasPosition = TRUE;
        }

        // 宿主给过 TSF 坐标、这次却落到 Win32 光标：是同步会话被拒后的回退，那个光标是跨窗口的陈旧位置，
        // 发出去会把候选窗拽走。不发，等异步探针 / UpdateComposition 缓存补真坐标。从未给过 TSF 坐标的
        // 宿主（纯 Win32 控件）不受影响。
        if (hasPosition && source == CARET_SRC_GUI_CARET && _hostGaveTsfCaret)
        {
            WIND_LOG_DEBUG_FMT(L"SendCaretPositionUpdate: 宿主有 TSF 坐标但本次回退到 GUI 光标 (%ld,%ld)，丢弃", x, y);
            return;
        }

        if (_pComposition != nullptr)
        {
            GetCompositionStartPosition(&compStartX, &compStartY);
        }
    }

    if (hasPosition)
    {
        // DPI 归一 + 记 last known + 发 IPC（fire-and-forget，无响应）
        _EmitCaretUpdate(x, y, height, compStartX, compStartY, source,
                         hasCompRect ? &compRect : nullptr);
    }
}

BOOL CTextService::GetCompositionStartPosition(LONG* px, LONG* py)
{
    if (_pComposition == nullptr || _pThreadMgr == nullptr)
    {
        return FALSE;
    }

    ITfDocumentMgr* pDocMgr = nullptr;
    HRESULT hr = _pThreadMgr->GetFocus(&pDocMgr);
    if (FAILED(hr) || pDocMgr == nullptr)
    {
        return FALSE;
    }

    ITfContext* pContext = nullptr;
    hr = pDocMgr->GetTop(&pContext);
    pDocMgr->Release();
    if (FAILED(hr) || pContext == nullptr)
    {
        return FALSE;
    }

    RECT caretRect = {}, compStartRect = {};
    BOOL hasCompStart = FALSE;
    BOOL result = CCaretEditSession::GetCaretAndCompositionStartRect(
        pContext, _tfClientId, _pComposition, &caretRect, &compStartRect, &hasCompStart,
        (LONG)GetPendingCommitPrefixLength());
    pContext->Release();

    if (result && hasCompStart)
    {
        *px = compStartRect.left;
        *py = compStartRect.bottom;
        WIND_LOG_DEBUG_FMT(L"GetCompositionStartPosition: x=%ld, y=%ld\n", *px, *py);
        return TRUE;
    }

    return FALSE;
}

BOOL CTextService::_InitLangBarButton()
{
    _pLangBarItemButton = new CLangBarItemButton(this);
    if (_pLangBarItemButton == nullptr)
        return FALSE;

    if (!_pLangBarItemButton->Initialize())
    {
        _pLangBarItemButton->Release();
        _pLangBarItemButton = nullptr;
        return FALSE;
    }

    return TRUE;
}

void CTextService::_UninitLangBarButton()
{
    if (_pLangBarItemButton != nullptr)
    {
        _pLangBarItemButton->Uninitialize();
        _pLangBarItemButton->Release();
        _pLangBarItemButton = nullptr;
    }
}

void CTextService::ToggleInputMode()
{
    WIND_LOG_INFO(L"ToggleInputMode called (local fallback)\n");

    if (!_bChineseMode && _pKeyEventSink != nullptr)
    {
        _pKeyEventSink->FlushEnglishStats();
    }

    // 本地切换模式（服务端不可用时的兜底；正常切换走按键 → 服务端 → 状态推送）
    EndComposition();
    NotifyCandidatesVisibilityChanged(FALSE);  // 同 _ApplyModeSwitch：收口候选 UIElement
    _SetChineseMode(!_bChineseMode);

    WIND_LOG_INFO_FMT(L"Switched to %s mode\n", _bChineseMode ? L"Chinese" : L"English");

    // compartment 如实反映中英模式（值语义），见 _SetOpenCloseCompartment 定义处的说明。
    // The actual key pass-through is handled by pfEaten=FALSE in OnTestKeyDown.
    _SetOpenCloseCompartment(_bChineseMode);
    _SetConversionMode(_bChineseMode);

    // Update language bar button
    if (_pLangBarItemButton != nullptr)
    {
        _pLangBarItemButton->UpdateLangBarButton(_bChineseMode);
    }
}

void CTextService::SetInputMode(BOOL bChineseMode)
{
    // Set mode directly from service response (no IPC call)
    if (!_bChineseMode && bChineseMode && _pKeyEventSink != nullptr)
    {
        _pKeyEventSink->FlushEnglishStats();
    }

    _SetChineseMode(bChineseMode);

    WIND_LOG_INFO_FMT(L"Mode set to %s (from service)\n", _bChineseMode ? L"Chinese" : L"English");

    // compartment 如实反映中英模式（值语义），见 _SetOpenCloseCompartment 定义处的说明。
    _SetOpenCloseCompartment(_bChineseMode);
    _SetConversionMode(_bChineseMode);

    // Update language bar button
    if (_pLangBarItemButton != nullptr)
    {
        _pLangBarItemButton->UpdateLangBarButton(_bChineseMode);
    }
}

void CTextService::UpdateCapsLockState(BOOL bCapsLock)
{
    if (_pLangBarItemButton != nullptr)
    {
        _pLangBarItemButton->UpdateCapsLockState(bCapsLock);
    }
}

void CTextService::SendMenuCommand(const char* command)
{
    WIND_LOG_INFO_FMT(L"SendMenuCommand: command=%hs\n", command);

    CIPCClient* pClient = _pIPCClient;
    CIPCClient* pTempClient = nullptr;

    // If main IPC client is null (Deactivate was called), create temporary connection
    if (pClient == nullptr)
    {
        WIND_LOG_INFO(L"SendMenuCommand: Main IPC null, creating temporary connection\n");
        pTempClient = new CIPCClient();
        if (pTempClient == nullptr)
        {
            WIND_LOG_ERROR(L"SendMenuCommand: Failed to create temporary IPC client\n");
            return;
        }
        if (!pTempClient->Connect())
        {
            WIND_LOG_WARN(L"SendMenuCommand: Temporary connection failed\n");
            delete pTempClient;
            return;
        }
        pClient = pTempClient;
        WIND_LOG_INFO(L"SendMenuCommand: Temporary connection established\n");
    }
    else if (!pClient->IsConnected())
    {
        // Main client exists but disconnected, try to reconnect
        WIND_LOG_INFO(L"SendMenuCommand: IPC disconnected, attempting reconnect\n");
        if (!pClient->Connect())
        {
            WIND_LOG_WARN(L"SendMenuCommand: Reconnect failed\n");
            return;
        }
        WIND_LOG_INFO(L"SendMenuCommand: Reconnected successfully\n");
    }

    // Send menu command via IPC (command is UTF-8 string)
    size_t commandLen = strlen(command);
    ServiceResponse response;
    if (pClient->SendSync(CMD_MENU_COMMAND, command, (uint32_t)commandLen, response))
    {
        WIND_LOG_INFO(L"SendMenuCommand: Command sent successfully\n");

        // Apply any status updates from response
        if (response.type == ResponseType::StatusUpdate)
        {
            BOOL bChineseMode = response.IsChineseMode();
            BOOL bFullWidth = response.IsFullWidth();
            BOOL bChinesePunct = response.IsChinesePunct();
            BOOL bToolbarVisible = response.IsToolbarVisible();
            BOOL bCapsLock = response.IsCapsLock();

            UpdateFullStatus(bChineseMode, bFullWidth, bChinesePunct, bToolbarVisible, bCapsLock,
                            response.iconLabel.empty() ? nullptr : response.iconLabel.c_str());
        }
    }
    else
    {
        WIND_LOG_WARN(L"SendMenuCommand: Failed to send command\n");
    }

    // Clean up temporary client if we created one
    if (pTempClient != nullptr)
    {
        pTempClient->Disconnect();
        delete pTempClient;
        WIND_LOG_DEBUG(L"SendMenuCommand: Temporary connection closed\n");
    }
}

void CTextService::SendShowContextMenu(int screenX, int screenY)
{
    WIND_LOG_INFO_FMT(L"SendShowContextMenu: x=%d, y=%d\n", screenX, screenY);

    CIPCClient* pClient = _pIPCClient;
    CIPCClient* pTempClient = nullptr;

    // If main IPC client is null (Deactivate was called), create temporary connection
    if (pClient == nullptr)
    {
        WIND_LOG_INFO(L"SendShowContextMenu: Main IPC null, creating temporary connection\n");
        pTempClient = new CIPCClient();
        if (pTempClient == nullptr)
        {
            WIND_LOG_ERROR(L"SendShowContextMenu: Failed to create temporary IPC client\n");
            return;
        }
        if (!pTempClient->Connect())
        {
            WIND_LOG_WARN(L"SendShowContextMenu: Temporary connection failed\n");
            delete pTempClient;
            return;
        }
        pClient = pTempClient;
        WIND_LOG_INFO(L"SendShowContextMenu: Temporary connection established\n");
    }
    else if (!pClient->IsConnected())
    {
        WIND_LOG_INFO(L"SendShowContextMenu: IPC disconnected, attempting reconnect\n");
        if (!pClient->Connect())
        {
            WIND_LOG_WARN(L"SendShowContextMenu: Reconnect failed\n");
            return;
        }
        WIND_LOG_INFO(L"SendShowContextMenu: Reconnected successfully\n");
    }

    // Build payload: int32 x + int32 y = 8 bytes
    struct {
        int32_t x;
        int32_t y;
    } payload;
    payload.x = (int32_t)screenX;
    payload.y = (int32_t)screenY;

    // 异步发送（fire-and-forget，由服务端弹出菜单）
    pClient->SendAsync(CMD_SHOW_CONTEXT_MENU, &payload, sizeof(payload));

    // Clean up temporary client if we created one
    if (pTempClient != nullptr)
    {
        pTempClient->Disconnect();
        delete pTempClient;
        WIND_LOG_DEBUG(L"SendShowContextMenu: Temporary connection closed\n");
    }
}

void CTextService::UpdateFullStatus(BOOL bChineseMode, BOOL bFullWidth, BOOL bChinesePunct, BOOL bToolbarVisible, BOOL bCapsLock, const wchar_t* iconLabel)
{
    _SetChineseMode(bChineseMode);
    _bFullWidth = bFullWidth;
    _bChinesePunct = bChinesePunct;

    // compartment 如实反映中英模式（值语义），见 _SetOpenCloseCompartment 定义处的说明。
    _SetOpenCloseCompartment(_bChineseMode);
    _SetConversionMode(_bChineseMode);

    if (_pLangBarItemButton != nullptr)
    {
        _pLangBarItemButton->UpdateFullStatus(bChineseMode, bFullWidth, bChinesePunct, bToolbarVisible, bCapsLock, iconLabel);
    }

    WIND_LOG_DEBUG_FMT(L"UpdateFullStatus: mode=%d, width=%d, punct=%d, toolbar=%d, caps=%d, label=%ls\n",
                 bChineseMode, bFullWidth, bChinesePunct, bToolbarVisible, bCapsLock,
                 iconLabel ? iconLabel : L"(none)");
}

// ITfCompositionSink implementation
STDAPI CTextService::OnCompositionTerminated(TfEditCookie ecWrite, ITfComposition* pComposition)
{
    // 组合被强制终止时（焦点切换、宿主 EndComposition 等），取消 HoldComposition 计时器。
    // 先记住 hold 是否活跃：活跃说明组合内容是智能符号（预上屏语义），下方须保留而非清空。
    BOOL holdWasActive = (_hHoldTimer != 0);
    CancelHoldTimer();
    // 宿主已强杀组合：待重开的余码暂存作废，避免野定时器回调开一个无主组合。
    CancelDeferredComposition();

    WIND_LOG_DEBUG(L"OnCompositionTerminated called\n");

    // Clear composition text cache
    _lastCompositionText.clear();
    _lastCaretPos = -1;

    // 宿主刚写完组合就终止（受控输入框回写 value）：保留缓冲、残留不动，下一键接上。
    // 不通知 KeyEventSink / 服务端，两边的组合态与候选原样保留。见 CompositionRecoverPolicy.h。
    if (_pComposition != nullptr && _pComposition == pComposition)
    {
        std::wstring leftover;
        LONG leftoverAcp = 0;
        ITfContext* pLeftContext = nullptr;
        ITfRange* pLeftClone = nullptr;
        ITfRange* pLeftRange = nullptr;
        if (SUCCEEDED(pComposition->GetRange(&pLeftRange)) && pLeftRange != nullptr)
        {
            WCHAR buf[wind::recover::kMaxLeftoverLen + 1] = {};
            ULONG got = 0;
            if (SUCCEEDED(pLeftRange->GetText(ecWrite, 0, buf, (ULONG)wind::recover::kMaxLeftoverLen + 1, &got)))
                leftover.assign(buf, got);
            // 记下残留的位置：之后核对要求光标恰在它末尾，光标前碰巧是同样的字母不算。
            // 宿主不给 ACP 偏移就无从核对位置，不进恢复。
            ITfRangeACP* pAcp = nullptr;
            LONG acpStart = 0, cch = 0;
            if (FAILED(pLeftRange->QueryInterface(IID_ITfRangeACP, (void**)&pAcp)) || pAcp == nullptr ||
                FAILED(pAcp->GetExtent(&acpStart, &cch)) || cch != (LONG)leftover.length())
                leftover.clear();
            if (pAcp != nullptr)
                pAcp->Release();
            leftoverAcp = acpStart;
            (void)pLeftRange->GetContext(&pLeftContext);
            if (FAILED(pLeftRange->Clone(&pLeftClone)))
                pLeftClone = nullptr;
            pLeftRange->Release();
        }
        const ULONGLONG since = GetTickCount64() - _lastCompositionWriteTick;
        if (pLeftContext != nullptr && pLeftClone != nullptr &&
            wind::recover::ShouldRecover(since, holdWasActive, _pendingCommitPrefix.length(), leftover.length()))
        {
            _ArmRecover(pLeftContext, pLeftClone, leftover, leftoverAcp);
            pLeftContext->Release();
            _hasCachedCaretPos = FALSE; // 缓存的是已终止那次组合的坐标
            WIND_LOG_INFO_FMT(L"OnCompositionTerminated: host ended composition %llums after write, keeping buffer (leftoverLen=%zu)\n",
                              since, leftover.length());
            _pComposition->Release();
            _pComposition = nullptr;
            _compositionJustStarted = FALSE;
            return S_OK;
        }
        if (pLeftContext != nullptr)
            pLeftContext->Release();
        if (pLeftClone != nullptr)
            pLeftClone->Release();
    }

    // Only release if this is the same composition we're tracking
    // It may have already been released in DoEditSession
    if (_pComposition != nullptr && _pComposition == pComposition)
    {
        // 意外终止（不是我们调的 EndComposition）：快速打字时新组合先于上一次写入完成，
        // 或宿主强行终止。必须清空组合文字，否则它会作为正文泄漏进文档（首字母直接上屏）。
        //
        // 例外：智能符号 hold 中被宿主终止（切窗口等）。组合内容语义上是预上屏，宿主终止时
        // 默认 finalize 留在文档，不清空即等于提交，正是期望行为；清空反而丢字。
        if (holdWasActive)
        {
            // 文本已随 finalize 留在文档，prefix 记账必须清，防下次 CommitText 双写。
            _pendingCommitPrefix.clear();
            WIND_LOG_DEBUG(L"OnCompositionTerminated: hold active, keeping composition text (finalized as committed)\n");
        }
        else
        {
        ITfRange* pRange = nullptr;
        if (SUCCEEDED(pComposition->GetRange(&pRange)) && pRange != nullptr)
        {
            // Clear the composition text by setting it to empty
            HRESULT hr = pRange->SetText(ecWrite, 0, L"", 0);
            if (SUCCEEDED(hr))
            {
                WIND_LOG_DEBUG(L"OnCompositionTerminated: Cleared composition text (unexpected termination)\n");
            }
            else
            {
                WIND_LOG_ERROR_FMT(L"OnCompositionTerminated: SetText failed hr=0x%08X\n", hr);
            }
            pRange->Release();
        }
        }

        WIND_LOG_DEBUG(L"OnCompositionTerminated: Releasing composition\n");
        _pComposition->Release();
        _pComposition = nullptr;
        _compositionJustStarted = FALSE;

        // Notify KeyEventSink that composition was unexpectedly terminated
        // This ensures _isComposing and _hasCandidates flags are properly reset
        if (_pKeyEventSink != nullptr)
        {
            _pKeyEventSink->OnCompositionUnexpectedlyTerminated();
        }
    }
    else if (_pComposition == nullptr)
    {
        WIND_LOG_DEBUG(L"OnCompositionTerminated: Already released\n");
    }

    return S_OK;
}

// ITfTextLayoutSink - called by TSF when the text layout changes.
// This fires after the app has reflowed text (processed WM_PAINT etc.),
// so GetTextExt now returns the correct, up-to-date coordinates.
STDAPI CTextService::OnLayoutChange(ITfContext* pContext, TfLayoutCode lCode, ITfContextView* pView)
{
    if (lCode == TF_LC_CHANGE && _pComposition != nullptr)
    {
        // UI-less / 宿主接管绘制：跳过整条 caret 重探。SDL 回无效光标，OnLayoutChange 反复触发
        // 同步 GetTextExt 会拖死独占全屏游戏的渲染线程；我们不弹自己的窗，本不需要这些坐标。
        if (_CaretQuerySuppressed())
        {
            return S_OK;
        }
        // 首次 reflow 阶段：WPS 等宿主在 reflow 完成前连续触发多次 OnLayoutChange，前几次
        // GetTextExt 仍是旧坐标。故 debounce：每次重置 timer，burst 结束后再 flush。
        // timer 兜底也覆盖完全不发 OnLayoutChange 的应用。
        if (_compositionJustStarted)
        {
            _hasCachedCaretPos = FALSE;
            _hasCachedCompStartPos = FALSE;
            if (_pLangBarItemButton != nullptr)
            {
                _pLangBarItemButton->PostDelayedCaretPositionUpdate();
            }
            // 首帧 reflow 期间的试探采样：每次 layout change 取一次坐标发给服务端，DLL 不做判断，
            // 哪一帧可信由服务端按 per-app 策略定（服务端默认忽略 probe）。不改变本地组合状态，
            // 也不替代上面的 debounce + timer 兜底。限前 5 次，防 IPC 洪泛与嵌套会话引发额外 layout change。
            // 各宿主表现与「Word 不发 OnLayoutChange」见 docs/architecture/caret-position-host-compat-notes.md
            // 的「首帧试探采样」一节。
            if (++_firstShowProbeSeq <= 5 && _pIPCClient != nullptr && _pIPCClient->IsConnected())
            {
                RECT probeCaret = {};
                RECT probeCs = {};
                BOOL probeHasCs = FALSE;
                BOOL probeUsedCs = FALSE;
                BOOL probeUsedImm = FALSE;
                if (CCaretEditSession::GetCaretAndCompositionStartRect(
                        pContext, _tfClientId, _pComposition,
                        &probeCaret, &probeCs, &probeHasCs, 0, &probeUsedCs, nullptr, nullptr, &probeUsedImm))
                {
                    // 与 SendCaretPositionUpdate 同口径：y 取 bottom、height 由 rect 高度算。
                    LONG px = probeCaret.left;
                    LONG py = probeCaret.bottom;
                    LONG ph = probeCaret.bottom - probeCaret.top;
                    LONG csx = probeHasCs ? probeCs.left : 0;
                    LONG csy = probeHasCs ? probeCs.bottom : 0;
                    // DPI 归一必须在此单独做一次：本路径刻意不走 _EmitCaretUpdate，那个出口会写权威兜底
                    // 缓存并发 CMD_CARET_UPDATE，喂进试探值会让首显闸门失效。漏掉归一时 DPI-unaware 宿主在
                    // 缩放≠100% 的屏上会把逻辑坐标当物理坐标，候选窗定位到屏幕外（见上面同一节）。
                    ConvertToPhysicalCoordinates(px, py, ph, csx, csy);
                    const int probeSrc = probeUsedImm  ? CARET_SRC_IMM_CANDIDATE_FORM
                                         : probeUsedCs ? CARET_SRC_TSF_COMPOSITION
                                                       : CARET_SRC_TSF_SELECTION;
                    _pIPCClient->SendCaretProbe(px, py, ph, csx, csy, probeSrc);
                }
            }
            WIND_LOG_DEBUG(L"OnLayoutChange (first show): debouncing caret flush\n");
            return S_OK;
        }
        WIND_LOG_DEBUG(L"OnLayoutChange: TF_LC_CHANGE with active composition, updating caret position\n");
        SendCaretPositionUpdate();
        return S_OK;
    }
    // 无活跃组合期的 layout change 无需处理（无组合＝无候选窗要跟）。
    return S_OK;
}

void CTextService::_AdviseTextLayoutSink(ITfContext* pContext)
{
    // Unadvise previous if any
    _UnadviseTextLayoutSink();

    if (pContext == nullptr)
        return;

    ITfSource* pSource = nullptr;
    if (SUCCEEDED(pContext->QueryInterface(IID_ITfSource, (void**)&pSource)) && pSource != nullptr)
    {
        if (SUCCEEDED(pSource->AdviseSink(IID_ITfTextLayoutSink, (ITfTextLayoutSink*)this, &_dwLayoutSinkCookie)))
        {
            _pLayoutSinkContext = pContext;
            _pLayoutSinkContext->AddRef();
            WIND_LOG_DEBUG(L"TextLayoutSink advised successfully\n");
        }
        pSource->Release();
    }
}

void CTextService::_UnadviseTextLayoutSink()
{
    if (_pLayoutSinkContext != nullptr && _dwLayoutSinkCookie != TF_INVALID_COOKIE)
    {
        ITfSource* pSource = nullptr;
        if (SUCCEEDED(_pLayoutSinkContext->QueryInterface(IID_ITfSource, (void**)&pSource)) && pSource != nullptr)
        {
            pSource->UnadviseSink(_dwLayoutSinkCookie);
            pSource->Release();
        }
        _pLayoutSinkContext->Release();
        _pLayoutSinkContext = nullptr;
        _dwLayoutSinkCookie = TF_INVALID_COOKIE;
        WIND_LOG_DEBUG(L"TextLayoutSink unadvised\n");
    }
}

// ============================================================================
// ITfTextEditSink implementation
// ============================================================================

STDAPI CTextService::OnEndEdit(ITfContext* pContext, TfEditCookie ecReadOnly, ITfEditRecord* pEditRecord)
{
    // Always update cached prevChar (character before caret) for smart punctuation
    WCHAR prevChar = 0;

    TF_SELECTION sel[1];
    ULONG fetched = 0;
    HRESULT hr = pContext->GetSelection(ecReadOnly, TF_DEFAULT_SELECTION, 1, sel, &fetched);

    if (SUCCEEDED(hr) && fetched > 0 && sel[0].range != nullptr)
    {
        // Clone range and shift start back by 1 character to get the char before caret
        ITfRange* pRange = nullptr;
        hr = sel[0].range->Clone(&pRange);
        if (SUCCEEDED(hr) && pRange != nullptr)
        {
            LONG shifted = 0;
            hr = pRange->ShiftStart(ecReadOnly, -1, &shifted, nullptr);
            if (SUCCEEDED(hr) && shifted == -1)
            {
                WCHAR buf[2] = {0};
                ULONG charCount = 0;
                hr = pRange->GetText(ecReadOnly, 0, buf, 1, &charCount);
                if (SUCCEEDED(hr) && charCount > 0)
                {
                    prevChar = buf[0];
                }
            }
            pRange->Release();
        }
        sel[0].range->Release();
    }

    _cachedPrevChar = prevChar;

    // Check if selection changed (cursor moved)
    BOOL selChanged = FALSE;
    pEditRecord->GetSelectionStatus(&selChanged);

    // 恢复中光标离开了残留末尾（点到别处、方向键交给宿主等）：不再接上，按宿主终止组合复位，
    // 服务端随之清缓冲。否则下一键会在新位置拿光标前碰巧相同的字当残留删。
    if (selChanged && !_recoverLeftover.empty())
    {
        ITfRange* pLeft = nullptr;
        if (_LeftoverBeforeCaret(ecReadOnly, pContext, &pLeft))
        {
            pLeft->Release();
        }
        else
        {
            _DisarmRecover(L"caret moved");
            if (_pKeyEventSink != nullptr)
                _pKeyEventSink->OnCompositionUnexpectedlyTerminated();
        }
    }

    // Shift/Ctrl 正等 keyup 切换时选区变了 ⇒ 按住期间有鼠标点选，那次 keyup 不算单击。
    // 不受 `_pComposition` 限制：组合中 Shift+点击同样是点选。
    if (selChanged && _pKeyEventSink != nullptr)
    {
        _pKeyEventSink->NoteSelectionChangedDuringToggle();
    }

    // 鼠标点击 / 拖选挪了光标：输入序列记账看不见，「上一笔」作废（补空格、英文后标点）。
    // 鼠标按着时才去查这次编辑有没有改文本（透传字符和自己的上屏都会改文本）。只读「此刻按着」
    // 位，不碰「按下过」位——那一位留给单击切换键的鼠标判定（NoteSelectionChangedDuringToggle）。
    const bool inComposition = _pComposition != nullptr;
    const bool mouseDown =
        ((GetAsyncKeyState(VK_LBUTTON) | GetAsyncKeyState(VK_RBUTTON) | GetAsyncKeyState(VK_MBUTTON)) & 0x8000) != 0;
    if (_pKeyEventSink != nullptr && WindInputSeq::CursorMovedByMouse(selChanged, inComposition, mouseDown, false))
    {
        bool textChanged = true; // 查不出来按「改过」算：宁可沿用记账，也不误作废
        IEnumTfRanges* pEnum = nullptr;
        if (SUCCEEDED(pEditRecord->GetTextAndPropertyUpdates(TF_GTP_INCL_TEXT, nullptr, 0, &pEnum)) && pEnum != nullptr)
        {
            ITfRange* pChanged = nullptr;
            ULONG fetched = 0;
            textChanged = pEnum->Next(1, &pChanged, &fetched) == S_OK && fetched > 0;
            if (pChanged != nullptr)
                pChanged->Release();
            pEnum->Release();
        }
        if (!textChanged)
            _pKeyEventSink->NoteCursorMovedByMouse();
    }

    if (selChanged && _pComposition == nullptr)
    {
        // 组合外选区变化（鼠标点击、方向键）：通知服务端复位智能标点状态。
        // 组合中由服务端从按键事件自行跟踪。
        if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
        {
            _pIPCClient->SendSelectionChanged((uint16_t)prevChar);
        }
    }

    return S_OK;
}

void CTextService::_AdviseTextEditSink(ITfContext* pContext)
{
    _UnadviseTextEditSink();

    if (pContext == nullptr)
        return;

    ITfSource* pSource = nullptr;
    if (SUCCEEDED(pContext->QueryInterface(IID_ITfSource, (void**)&pSource)) && pSource != nullptr)
    {
        if (SUCCEEDED(pSource->AdviseSink(IID_ITfTextEditSink, (ITfTextEditSink*)this, &_dwTextEditSinkCookie)))
        {
            _pTextEditSinkContext = pContext;
            _pTextEditSinkContext->AddRef();
            WIND_LOG_DEBUG(L"TextEditSink advised successfully\n");
        }
        pSource->Release();
    }
}

void CTextService::_UnadviseTextEditSink()
{
    if (_pTextEditSinkContext != nullptr && _dwTextEditSinkCookie != TF_INVALID_COOKIE)
    {
        ITfSource* pSource = nullptr;
        if (SUCCEEDED(_pTextEditSinkContext->QueryInterface(IID_ITfSource, (void**)&pSource)) && pSource != nullptr)
        {
            pSource->UnadviseSink(_dwTextEditSinkCookie);
            pSource->Release();
        }
        _pTextEditSinkContext->Release();
        _pTextEditSinkContext = nullptr;
        _dwTextEditSinkCookie = TF_INVALID_COOKIE;
        WIND_LOG_DEBUG(L"TextEditSink unadvised\n");
    }
}

// Update composition text
BOOL CTextService::UpdateComposition(const std::wstring& text, int caretPos, BOOL noUnderline)
{
    // 顶码聚合（微软 IME 行为）：组合实际显示 = 待提交前缀 + 引擎组合文本，
    // 光标位置随前缀偏移。前缀为空时与原行为完全一致。
    std::wstring full = _pendingCommitPrefix + text;
    int fullCaret = (caretPos >= 0) ? (int)(_pendingCommitPrefix.length() + caretPos) : caretPos;

    WIND_LOG_DEBUG_FMT(L"UpdateComposition called, textLen=%zu, prefixLen=%zu, _pComposition=%p\n",
                 text.length(), _pendingCommitPrefix.length(), _pComposition);

    // 优化：文本与光标都与上次相同则跳过，避免多余的 RequestEditSession
    //（必须同时比较光标，否则左右移动光标会被跳过）
    if (full == _lastCompositionText && fullCaret == _lastCaretPos && _pComposition != nullptr)
    {
        WIND_LOG_DEBUG(L"UpdateComposition: Skipping duplicate (same text and caret)\n");
        return TRUE;
    }

    // Need a document manager
    ITfDocumentMgr* pDocMgr = nullptr;
    if (_pThreadMgr == nullptr || FAILED(_pThreadMgr->GetFocus(&pDocMgr)) || pDocMgr == nullptr)
    {
        WIND_LOG_ERROR(L"UpdateComposition: Failed to get DocMgr\n");
        return FALSE;
    }

    // 空文本兜底占位用哪个字符：由 core 按本进程 compat 规则 `composition_placeholder` 决定，经
    // CONFIG_KEY_COMPOSITION_PLACEHOLDER 下发（默认空格；浏览器受控 `<input>` 会 trim 组合中的 value，
    // 出厂给浏览器发 ZWSP）。core 自己发来的占位（非空文本）原样写入。
    //
    // 写组合区只有这一个口子（顶码、延迟组合、hold 都汇到这里）；上屏 / 取消 / 宿主终止都是整段
    // 替换或清空组合 range，ZWSP 与空格一样不会残留。
    const wchar_t* placeholder = wind::placeholder::PlaceholderForKind(
        static_cast<uint8_t>(InterlockedCompareExchange(&_compositionPlaceholderKind, 0, 0)));

    ITfContext* pContext = nullptr;
    HRESULT hr = pDocMgr->GetTop(&pContext);
    pDocMgr->Release();

    if (FAILED(hr) || pContext == nullptr)
    {
        WIND_LOG_ERROR(L"UpdateComposition: Failed to get Context\n");
        return FALSE;
    }

    CUpdateCompositionEditSession* pEditSession =
        new CUpdateCompositionEditSession(this, pContext, full, fullCaret, noUnderline, placeholder);

    // Timing: measure RequestEditSession duration
    LARGE_INTEGER startTime, endTime, freq;
    QueryPerformanceCounter(&startTime);
    QueryPerformanceFrequency(&freq);

    HRESULT hrSession;
    hr = pContext->RequestEditSession(_tfClientId, pEditSession, TF_ES_ASYNCDONTCARE | TF_ES_READWRITE, &hrSession);

    QueryPerformanceCounter(&endTime);
    int durationMs = (int)((endTime.QuadPart - startTime.QuadPart) * 1000 / freq.QuadPart);

    // Track if this was async (Weasel optimization pattern)
    _asyncEdit = (hrSession == TF_S_ASYNC);

    WIND_LOG_DEBUG_FMT(L"UpdateComposition: RequestEditSession hr=0x%08X, hrSession=0x%08X, async=%d, duration=%dms\n",
                 hr, hrSession, _asyncEdit ? 1 : 0, durationMs);

    pEditSession->Release();
    pContext->Release();

    // 成功后更新去重缓存（缓存的是含前缀的完整显示文本）
    if (SUCCEEDED(hr))
    {
        _lastCompositionText = full;
        _lastCaretPos = fullCaret;
    }

    return SUCCEEDED(hr);
}

void CTextService::HandlePairCommitPush(const std::wstring& text, uint32_t moveLeft)
{
    if (_pKeyEventSink == nullptr)
    {
        WIND_LOG_WARN(L"HandlePairCommitPush: KeyEventSink 未装配，丢弃 ime.pair 推送\n");
        return;
    }
    _pKeyEventSink->HandlePairCommitPush(text, moveLeft);
}

BOOL CTextService::CommitTextViaSyntheticKey(const std::wstring& text, BOOL replacingHeld)
{
    if (_pKeyEventSink != nullptr &&
        _pKeyEventSink->QueueAsyncCommitViaSyntheticKey(text, replacingHeld))
    {
        return TRUE;
    }
    // KeyEventSink 未装配，或合成按键注入失败（SendInput 出错，极罕见）：退回旧的
    // 直接异步提交，保证至少不丢字（代价是回到 nonKeyContext 的已知问题面）。
    WIND_LOG_WARN(L"CommitTextViaSyntheticKey: falling back to direct async CommitText\n");
    return CommitText(text, TRUE, replacingHeld);
}

// 把光标前 count 个已上屏字符替换为 text（智能符号 DeleteReplace 方案）。
//
// 优先走 TSF 同步范围替换（CReplaceBackwardEditSession：ShiftStart+SetText，原子、不发按键，
// 不受修饰键是否按住影响）；失败才回退到合成按键（Backspace × count + Unicode 注入）。
// 已知例外：Tabby/微信的 TSFTextStore 对范围替换报成功却画面不符，而全局改用合成按键又会在
// EverEdit 撞上 Shift 类标点的发键抑制；若要处理，应按宿主特判 kTryTsfRangeReplace 而不是全局切换。
// 见 docs/architecture/smart-symbol-compat-notes.md。
BOOL CTextService::ReplacePrecedingChars(int count, const std::wstring& text)
{
    if (count <= 0)
    {
        // 无删除需求，等价于直接上屏。
        return CommitText(text);
    }

    _lastCompositionText.clear();
    _lastCaretPos = -1;

    constexpr bool kTryTsfRangeReplace = true;
    if (kTryTsfRangeReplace)
    {
        ITfDocumentMgr* pDocMgr = nullptr;
        if (_pThreadMgr != nullptr && SUCCEEDED(_pThreadMgr->GetFocus(&pDocMgr)) && pDocMgr != nullptr)
        {
            ITfContext* pContext = nullptr;
            HRESULT hr = pDocMgr->GetTop(&pContext);
            pDocMgr->Release();

            if (SUCCEEDED(hr) && pContext != nullptr)
            {
                CReplaceBackwardEditSession* pEditSession =
                    new CReplaceBackwardEditSession(this, pContext, count, text);

                HRESULT hrSession;
                hr = pContext->RequestEditSession(_tfClientId, pEditSession,
                                                  TF_ES_SYNC | TF_ES_READWRITE, &hrSession);
                BOOL success = pEditSession->GetSuccess();
                pEditSession->Release();
                pContext->Release();

                // 同 CommitText：只信 GetSuccess()。部分终端模拟器外层报失败但替换其实已完成，
                // 按 hr/hrSession 判失败再退格重打会让文字被复制。
                if (success)
                {
                    WIND_LOG_DEBUG_FMT(L"ReplacePrecedingChars: TSF range replace succeeded count=%d\n", count);
                    return TRUE;
                }
                WIND_LOG_DEBUG_FMT(L"ReplacePrecedingChars: TSF failed (hr=0x%08X, hrSession=0x%08X), falling back to SendInput\n",
                                   hr, hrSession);
            }
        }
    }

    // 真实合成按键：count 次 Backspace + Unicode 注入 text。每个键须先 MarkSyntheticKey，
    // 否则会被自己的 OnTestKeyDown 当真实按键再处理一遍，表现为替换后的符号重复上屏。
    std::vector<INPUT> inputs;
    inputs.reserve((size_t)count * 2 + text.length() * 2);
    for (int i = 0; i < count; i++)
    {
        if (_pKeyEventSink != nullptr) _pKeyEventSink->MarkSyntheticKey(VK_BACK);

        INPUT down = {};
        down.type = INPUT_KEYBOARD;
        down.ki.wVk = VK_BACK;
        inputs.push_back(down);

        INPUT up = {};
        up.type = INPUT_KEYBOARD;
        up.ki.wVk = VK_BACK;
        up.ki.dwFlags = KEYEVENTF_KEYUP;
        inputs.push_back(up);
    }
    for (wchar_t ch : text)
    {
        if (_pKeyEventSink != nullptr) _pKeyEventSink->MarkSyntheticKey(VK_PACKET);

        INPUT down = {};
        down.type = INPUT_KEYBOARD;
        down.ki.wScan = ch;
        down.ki.dwFlags = KEYEVENTF_UNICODE;
        inputs.push_back(down);

        INPUT up = {};
        up.type = INPUT_KEYBOARD;
        up.ki.wScan = ch;
        up.ki.dwFlags = KEYEVENTF_UNICODE | KEYEVENTF_KEYUP;
        inputs.push_back(up);
    }
    if (!inputs.empty())
    {
        SendInput((UINT)inputs.size(), inputs.data(), sizeof(INPUT));
    }
    WIND_LOG_DEBUG_FMT(L"ReplacePrecedingChars: SendInput fallback count=%d textLen=%zu\n", count, text.length());
    return TRUE;
}

// 给剪贴板追加一个注册格式，值为 DWORD。须在 OpenClipboard 之后、CloseClipboard 之前调用。
static void SetClipboardDwordFormat(const wchar_t* name, DWORD value)
{
    const UINT fmt = RegisterClipboardFormatW(name);
    if (fmt == 0)
        return;
    HGLOBAL h = GlobalAlloc(GMEM_MOVEABLE, sizeof(DWORD));
    if (h == nullptr)
        return;
    DWORD* p = static_cast<DWORD*>(GlobalLock(h));
    if (p == nullptr)
    {
        GlobalFree(h);
        return;
    }
    *p = value;
    GlobalUnlock(h);
    if (SetClipboardData(fmt, h) == nullptr)
        GlobalFree(h);
}

// 多行文本的粘贴上屏（compat `multiline_commit = "paste"`），由 CCommitTextEditSession 在组合结束后调用。
// 剪贴板不备份也不恢复：恢复会让剪贴板历史工具多记一条，且宿主何时处理 Ctrl+V 不可知，恢复早了会贴出旧内容。
BOOL CTextService::_PasteTextViaClipboard(const std::wstring& text)
{
    const std::wstring crlf = wind::multiline::NormalizeToCrlf(text);
    const size_t newlines = (size_t)std::count(crlf.begin(), crlf.end(), L'\n');

    // 以下两种情况注入的 Ctrl+V 会变质或落到别处，在写剪贴板、压 skip 条目之前就放弃，由调用方插入原文。
    // ① 物理按住 Shift / Alt / Win：叠成 Ctrl+Shift+V 之类的别的快捷键（Ctrl 本身按着无妨）。
    const int heldMods[] = {VK_LSHIFT, VK_RSHIFT, VK_LMENU, VK_RMENU, VK_LWIN, VK_RWIN};
    for (int vk : heldMods)
    {
        if (GetAsyncKeyState(vk) & 0x8000)
        {
            WIND_LOG_DEBUG_FMT(L"MultilinePaste: modifier vk=0x%02X held, inserting instead, len=%zu\n", vk,
                               text.length());
            return FALSE;
        }
    }
    // ② 前台不是本线程：SendInput 进的是前台窗口的输入队列。
    if (GetWindowThreadProcessId(GetForegroundWindow(), nullptr) != GetCurrentThreadId())
    {
        WIND_LOG_DEBUG_FMT(L"MultilinePaste: foreground is another thread, inserting instead, len=%zu\n",
                           text.length());
        return FALSE;
    }

    // 剪贴板可能正被别的进程占着（剪贴板历史 / 同步工具），短暂重试，总等待不超过约 40ms。
    // 用本 DLL 自己的消息窗作所有者：传 nullptr 时 EmptyClipboard 会把所有者置空。
    BOOL opened = FALSE;
    for (int attempt = 0; attempt < 5 && !opened; attempt++)
    {
        if (attempt > 0)
            Sleep(8);
        opened = OpenClipboard(_hHotkeyWnd);
    }
    if (!opened)
    {
        WIND_LOG_WARN_FMT(L"MultilinePaste: OpenClipboard failed err=%lu, len=%zu\n", GetLastError(), text.length());
        return FALSE;
    }
    BOOL written = FALSE;
    const size_t cb = (crlf.length() + 1) * sizeof(wchar_t);
    HGLOBAL hMem = GlobalAlloc(GMEM_MOVEABLE, cb);
    if (hMem != nullptr)
    {
        wchar_t* pDst = static_cast<wchar_t*>(GlobalLock(hMem));
        if (pDst != nullptr)
        {
            memcpy(pDst, crlf.c_str(), cb);
            GlobalUnlock(hMem);
            written = EmptyClipboard() && SetClipboardData(CF_UNICODETEXT, hMem) != nullptr;
        }
        if (!written)
            GlobalFree(hMem); // SetClipboardData 成功后内存归系统，失败才由我们释放
    }
    if (written)
    {
        // 这份内容只是上屏的载体：不进 Win+V 历史、不同步云剪贴板，并请第三方剪贴板工具忽略。
        SetClipboardDwordFormat(L"CanIncludeInClipboardHistory", 0);
        SetClipboardDwordFormat(L"CanUploadToCloudClipboard", 0);
        SetClipboardDwordFormat(L"ExcludeClipboardContentFromMonitorProcessing", 0);
    }
    const DWORD clipErr = written ? 0 : GetLastError();
    CloseClipboard();
    if (!written)
    {
        WIND_LOG_WARN_FMT(L"MultilinePaste: SetClipboardData failed err=%lu, len=%zu\n", clipErr, text.length());
        return FALSE;
    }

    // Ctrl↓ V↓ V↑ Ctrl↑ 一次注入，中间不被真实键鼠穿插；纯 VK、无 UNICODE 标志，宿主按快捷键处理。
    // 两个键都凭空造出，down / up 都用 MarkSyntheticKey 压条目（见 SkipKeyTable.h），不能像
    // _ReplayKeyToHost 只压 down：注入的 Ctrl keyup 若走正常路径会落进修饰键释放处理，可能让宿主里
    // Ctrl 卡住或误切模式。宿主不调 OnTestKeyUp 时残留的 keyup 条目由 skip 表的 TTL 收掉。
    if (_pKeyEventSink != nullptr)
    {
        _pKeyEventSink->MarkSyntheticKey(VK_CONTROL);
        _pKeyEventSink->MarkSyntheticKey('V');
    }
    INPUT inputs[4] = {};
    for (INPUT& in : inputs)
        in.type = INPUT_KEYBOARD;
    const WORD vks[4] = {VK_CONTROL, 'V', 'V', VK_CONTROL};
    for (int i = 0; i < 4; i++)
    {
        inputs[i].ki.wVk = vks[i];
        // 补上扫描码：有的宿主按扫描码认键。不加 KEYEVENTF_SCANCODE，仍以 VK 为准。
        inputs[i].ki.wScan = (WORD)MapVirtualKeyW(vks[i], MAPVK_VK_TO_VSC);
    }
    inputs[2].ki.dwFlags = KEYEVENTF_KEYUP;
    inputs[3].ki.dwFlags = KEYEVENTF_KEYUP;
    const UINT sent = SendInput(4, inputs, sizeof(INPUT));
    if (sent == 0)
    {
        WIND_LOG_WARN_FMT(L"MultilinePaste: SendInput failed err=%lu, len=%zu\n", GetLastError(), text.length());
        return FALSE;
    }
    if (sent != 4)
    {
        // 已注入了一部分，再插入原文会重复出字，不补；但 Ctrl↓ 已发出，补一个 Ctrl↑ 免得宿主里 Ctrl 卡住。
        // 不另压 skip 条目：上面 MarkSyntheticKey 给 Ctrl↑ 压的那条因第 4 个事件没发出而仍在表里，正好对应它。
        const UINT upSent = SendInput(1, &inputs[3], sizeof(INPUT));
        WIND_LOG_WARN_FMT(L"MultilinePaste: SendInput sent %u of 4, ctrl-up resent=%u, len=%zu\n", sent, upSent,
                          text.length());
    }
    WIND_LOG_DEBUG_FMT(L"MultilinePaste: clipboard set + Ctrl+V injected, len=%zu, newlines=%zu, sent=%u\n",
                       crlf.length(), newlines, sent);
    return TRUE;
}

// 原子上屏：结束组合 + 写入文本在同一个编辑会话里完成，避免浏览器里异步 EndComposition 清掉
// 随后同步 InsertText 写入的文字。
//
// 换行形式（CR / LF / CRLF）不在这一层决定，收到什么写什么：该用哪种取决于宿主的文本模型，
// 只有服务端认得宿主（协调器 `apply_commit_newline`，见 docs/architecture/app-compat-host-notes.md
// 的「上屏换行」），统一在推送前改写，各条出口天然一致。
// 未解决：SendInput 兜底把换行当 Unicode 字符注入，多数控件既不换行也不报错（见 wind-keys 的
// `split_type_text`），修法是拆成 VK_RETURN，单独立案。
BOOL CTextService::CommitText(const std::wstring& text, BOOL nonKeyContext, BOOL replacingHeld,
                              BOOL* pAsyncDeferred)
{
    // 默认"已落定"，只有真的把编辑交给异步会话时才置位（两处 RequestEditSession 成功后）。
    if (pAsyncDeferred != nullptr) *pAsyncDeferred = FALSE;

    // hold 预览态活跃时，本次提交必须交代那个符号的去向：下面走的是组合 range 的 SetText，
    // 不处置它就会被静默覆盖。
    //   replacingHeld=TRUE （press2）：要拿英文符号换掉它 → 丢弃，让 SetText 覆盖。
    //   replacingHeld=FALSE（其余一切）：并入 prefix，与本次文本一起上屏。
    // 默认取追加：hold 期间可能触发提交的路径很多（全角空格/数字、临时英文、独占模式出字……），
    // 安全的一侧做默认，新增路径自动正确。超时收口那条路上两个分支都是 no-op。
    if (replacingHeld)
        CancelHoldTimer();
    else
        AbsorbHeldIntoPrefix();

    // 顶码聚合：真正提交 = 待提交前缀 + 本次文本（微软 IME 的延迟提交在此收口）。
    std::wstring full = _pendingCommitPrefix + text;
    _pendingCommitPrefix.clear();

    // 只数换行，不改写（改写在服务端完成，见函数头）。
    int newlineCount = 0;
    for (size_t i = 0; i < full.length(); i++)
    {
        if (full[i] != L'\r' && full[i] != L'\n') continue;
        // CRLF 是一个换行的两字符写法，跳过紧随的 LF 只算一次。
        if (full[i] == L'\r' && i + 1 < full.length() && full[i + 1] == L'\n') i++;
        newlineCount++;
    }

    // 含换行且本进程 compat 配了 paste：组合照常结束，正文改经剪贴板 + Ctrl+V 交给宿主
    // （见 MultilineCommit.h）。会话请求流程不变，粘贴在 DoEditSession 里组合结束之后发生。
    const BOOL paste = wind::multiline::ContainsNewline(full) && wind::multiline::IsPasteMode(
        static_cast<uint8_t>(InterlockedCompareExchange(&_multilineCommitMode, 0, 0)));

    // 诊断用：只统计换行符个数、不打印正文（日志隐私红线）。这里已为 0 说明丢换行在 Rust/IPC 一侧，
    // 不为 0 而宿主没分段则问题在 TSF/宿主。
    {
        WIND_LOG_DEBUG_FMT(L"CommitText: textLen=%zu, newlines=%d, nonKeyContext=%d, replacingHeld=%d, paste=%d\n",
                           full.length(), newlineCount, (int)nonKeyContext, (int)replacingHeld, (int)paste);
    }

    LARGE_INTEGER startTime, endTime, freq;
    QueryPerformanceCounter(&startTime);
    QueryPerformanceFrequency(&freq);

    // Clear composition text cache
    _lastCompositionText.clear();
    _lastCaretPos = -1;

    // Transfer ownership of _pComposition to the EditSession
    ITfComposition* pCompToEnd = _pComposition;
    _pComposition = nullptr;

    if (full.empty() && pCompToEnd == nullptr)
    {
        _DisarmRecover(L"empty commit");
        WIND_LOG_DEBUG(L"CommitText: Nothing to do (no text, no composition)\n");
        return TRUE;
    }

    // Need a document manager to request edit session
    ITfDocumentMgr* pDocMgr = nullptr;
    if (_pThreadMgr == nullptr || FAILED(_pThreadMgr->GetFocus(&pDocMgr)) || pDocMgr == nullptr)
    {
        WIND_LOG_DEBUG(L"CommitText: Can't get DocMgr, falling back\n");
        if (pCompToEnd != nullptr) pCompToEnd->Release();
        _DisarmRecover(L"commit fallback");
        goto fallback;
    }

    {
        ITfContext* pContext = nullptr;
        HRESULT hr = pDocMgr->GetTop(&pContext);
        pDocMgr->Release();

        if (FAILED(hr) || pContext == nullptr)
        {
            WIND_LOG_DEBUG(L"CommitText: Can't get Context, falling back\n");
            if (pCompToEnd != nullptr) pCompToEnd->Release();
            _DisarmRecover(L"commit fallback");
            goto fallback;
        }

        // 恢复中且宿主是 Chromium：残留先在单独一把锁里删掉，上屏会话再插入。删和插混在同一把锁里，
        // 受控输入框上屏文字会重复一份。非按键上下文不能发同步会话，改发异步，TSF 按请求顺序执行。
        BOOL recoverDropped = FALSE;
        if (pCompToEnd == nullptr && HasRecoverRecord() && CCaretEditSession::IsChromiumContext(pContext))
        {
            CDropLeftoverEditSession* pDrop = new CDropLeftoverEditSession(this, pContext);
            HRESULT hrDropSession = S_OK;
            HRESULT hrDrop = pContext->RequestEditSession(
                _tfClientId, pDrop, (nonKeyContext ? TF_ES_ASYNC : TF_ES_SYNC) | TF_ES_READWRITE, &hrDropSession);
            pDrop->Release();
            recoverDropped = SUCCEEDED(hrDrop);
            WIND_LOG_DEBUG_FMT(L"Recover: drop in its own lock hr=0x%08X hrSession=0x%08X\n", hrDrop, hrDropSession);
        }

        CCommitTextEditSession* pEditSession = new CCommitTextEditSession(this, pContext, pCompToEnd, full, paste);
        // pCompToEnd ownership transferred to pEditSession
        if (recoverDropped)
            pEditSession->SetRecoverDropped();

        if (nonKeyContext)
        {
            // 非按键上下文（WM_TIMER / 窗口消息 / COM 回调）里 TF_ES_SYNC 不合法，Word 会拒发
            // 同步会话；此时 Release 组合会让宿主把组合里的内容 finalize 成正文，再走 SendInput
            // 就重复上屏。故改用异步会话，由 TSF 在可授予锁时原地 SetText+EndComposition，绝不
            // SendInput。代价是鼠标上屏时用户可能多看到原码若干毫秒。
            // 见 docs/architecture/tsf-commit-edit-session-notes.md。
            HRESULT hrSession = S_OK;
            hr = pContext->RequestEditSession(_tfClientId, pEditSession,
                                              TF_ES_ASYNCDONTCARE | TF_ES_READWRITE, &hrSession);
            pEditSession->Release();
            pContext->Release();

            if (SUCCEEDED(hr))
            {
                if (pAsyncDeferred != nullptr) *pAsyncDeferred = TRUE;
                WIND_LOG_DEBUG_FMT(L"CommitText(async): commit requested, hrSession=0x%08X\n", hrSession);
                return TRUE;
            }
            // 极少数：异步请求本身就被拒（组合已随会话析构释放）。落到 SendInput 末路兜底。
            WIND_LOG_DEBUG_FMT(L"CommitText(async): request rejected hr=0x%08X, falling back to SendInput\n", hr);
        }
        else
        {
            HRESULT hrSession;
            hr = pContext->RequestEditSession(_tfClientId, pEditSession, TF_ES_SYNC | TF_ES_READWRITE, &hrSession);
            BOOL success = pEditSession->GetSuccess();

            QueryPerformanceCounter(&endTime);
            int durationMs = (int)((endTime.QuadPart - startTime.QuadPart) * 1000 / freq.QuadPart);

            // 只信 GetSuccess()：它在 SetText/EndComposition 真正执行后才置 TRUE。外层 hr/hrSession
            // 在部分宿主里会在编辑已执行时仍报失败，按它判就会 SendInput 再打一遍。
            if (success)
            {
                pEditSession->Release();
                pContext->Release();
                WIND_LOG_DEBUG_FMT(L"CommitText: TSF atomic commit succeeded, duration=%dms\n", durationMs);
                return TRUE;
            }

            // 同步会话没跑过（宿主拒发锁，TF_ES_SYNC 只是「期待成功」）时绝不能 SendInput 兜底：
            // Release 让组合成孤儿，宿主把组合里的原码 finalize 进文档，正文再追加在后面。
            // 改为退到异步会话，与 nonKeyContext 分支同构；代价是可能多看到原码若干毫秒。
            //
            // 判据是「会话跑过没有」（WasExecuted），不是「组合还在不在」：无组合的提交（空缓冲
            // 直接上屏符号、鼠标上屏）里 HasPendingComposition() 恒 FALSE，会错过异步重试。
            // 跑过但没写成时再请求只会重复出字，照旧走 SendInput。
            if (!pEditSession->WasExecuted())
            {
                HRESULT hrAsyncSession = S_OK;
                HRESULT hrAsyncReq = pContext->RequestEditSession(
                    _tfClientId, pEditSession, TF_ES_ASYNCDONTCARE | TF_ES_READWRITE, &hrAsyncSession);
                pEditSession->Release();
                pContext->Release();

                if (SUCCEEDED(hrAsyncReq))
                {
                    if (pAsyncDeferred != nullptr) *pAsyncDeferred = TRUE;
                    WIND_LOG_WARN_FMT(L"CommitText: sync rejected (hr=0x%08X, hrSession=0x%08X), retried async hrSession=0x%08X, duration=%dms\n",
                                      hr, hrSession, hrAsyncSession, durationMs);
                    return TRUE;
                }

                // 同步与异步都被拒：组合只能孤儿释放，编码泄漏无从避免；落 SendInput 至少不丢字。
                WIND_LOG_ERROR_FMT(L"CommitText: sync AND async both rejected (sync hrSession=0x%08X, async hr=0x%08X) - composition will leak\n",
                                   hrSession, hrAsyncReq);
            }
            else
            {
                pEditSession->Release();
                pContext->Release();
                // 会话跑过了但没写成（组合已收尾 / SetText 与 InsertTextAtSelection 双双失败）。
                // 重试无意义，落 SendInput 保证不丢字。
                WIND_LOG_WARN_FMT(L"CommitText: TSF method failed (hr=0x%08X, hrSession=0x%08X), session executed but commit failed, falling back to SendInput, duration=%dms\n",
                             hr, hrSession, durationMs);
            }
        }
    }

fallback:
    // 兜底：用 SendInput 注入（与 InsertText 的兜底路径一致）
    if (full.empty())
    {
        return TRUE;
    }

    WIND_LOG_WARN_FMT(L"CommitText: Using SendInput fallback for textLen=%zu\n", full.length());

    std::vector<INPUT> inputs;
    inputs.reserve(full.length() * 2);

    for (wchar_t ch : full)
    {
        // 标记为自生成，避免被自己的 OnTestKeyDown 当成真实按键二次处理。
        if (_pKeyEventSink != nullptr) _pKeyEventSink->MarkSyntheticKey(VK_PACKET);

        INPUT inputDown = {};
        inputDown.type = INPUT_KEYBOARD;
        inputDown.ki.wVk = 0;
        inputDown.ki.wScan = ch;
        inputDown.ki.dwFlags = KEYEVENTF_UNICODE;
        inputs.push_back(inputDown);

        INPUT inputUp = {};
        inputUp.type = INPUT_KEYBOARD;
        inputUp.ki.wVk = 0;
        inputUp.ki.wScan = ch;
        inputUp.ki.dwFlags = KEYEVENTF_UNICODE | KEYEVENTF_KEYUP;
        inputs.push_back(inputUp);
    }

    UINT sent = SendInput((UINT)inputs.size(), inputs.data(), sizeof(INPUT));
    if (sent != inputs.size())
    {
        WIND_LOG_WARN_FMT(L"CommitText: SendInput sent %u of %u inputs\n", sent, (UINT)inputs.size());
    }

    return TRUE;
}

// 以 VK_PACKET（KEYEVENTF_UNICODE）把文本注入宿主，产生真实 WM_CHAR。
// 用于 AutoCAD 表格在位编辑器这类丢弃 TSF 写入却报 S_OK 的宿主，
// 见 docs/architecture/pair-commit-host-compat-notes.md。
// 配对插入后的光标回移由调用方走 _SimulatePairKey：修饰键按住时延后到松开再发，
// 否则物理 Shift 会把注入的 VK_LEFT 变成 Shift+Left 选中右括号。
BOOL CTextService::InjectUnicodeText(const std::wstring& text)
{
    std::vector<INPUT> inputs;
    inputs.reserve(text.length() * 2);
    for (wchar_t ch : text)
    {
        // 标记为自生成，避免被自己的 OnTestKeyDown 当成真实按键二次处理
        if (_pKeyEventSink != nullptr) _pKeyEventSink->MarkSyntheticKey(VK_PACKET);
        INPUT down = {};
        down.type = INPUT_KEYBOARD;
        down.ki.wScan = ch;
        down.ki.dwFlags = KEYEVENTF_UNICODE;
        inputs.push_back(down);
        INPUT up = down;
        up.ki.dwFlags = KEYEVENTF_UNICODE | KEYEVENTF_KEYUP;
        inputs.push_back(up);
    }
    const UINT sent = SendInput((UINT)inputs.size(), inputs.data(), sizeof(INPUT));
    WIND_LOG_DEBUG_FMT(L"InjectUnicodeText: sent=%u of %zu\n", sent, inputs.size());
    return sent == inputs.size() ? TRUE : FALSE;
}

// FOCUS_LOST_REASON_* 的日志名。与协议值共用一处，避免两边各写一份而说反。
static const wchar_t* FocusLostReasonName(uint8_t reason)
{
    switch (reason)
    {
    case FOCUS_LOST_REASON_THREAD:      return L"thread_focus_lost";
    case FOCUS_LOST_REASON_DOC_CHANGED: return L"doc_changed";
    case FOCUS_LOST_REASON_CTX_LOST:    return L"ctx_lost";
    default:                            return L"?";
    }
}

// 输入态整体清理。只应由「离开了原来那个文档」的两条路径调用（OnKillThreadFocus /
// doc_changed），不要挂回 DocMgr 级失焦：那是噪声层，在那里销毁输入会让首字符直接上屏，
// 见 docs/architecture/tsf-docmgr-focus-semantics.md。
// DocMgr 级失焦要隐藏工具栏时走 _ReportEditContextLost()，它只翻可见性标志、不碰输入态。
void CTextService::CleanupInputStateForDocChange(ITfDocumentMgr* pDocMgrHint, uint8_t reason,
                                                 BOOL sendFocusLost)
{
    WIND_LOG_DEBUG_FMT(L"CleanupInputStateForDocChange reason=%ls hint=0x%p focusLostSent=%d sendLost=%d",
                       FocusLostReasonName(reason), pDocMgrHint, _focusLostSent ? 1 : 0, sendFocusLost ? 1 : 0);

    // 换了文档：残留属于旧文档，不能拿新文档的光标去核对删除。
    _DisarmRecover(L"doc change");

    // 先结束 composition 再发 focus_lost：EndComposition 会清空 composition 范围的文本，
    // 顺序反了则服务端已清 buffer 而宿主里仍留着未清的 composition 文本。
    EndComposition(pDocMgrHint);

    // sendFocusLost=FALSE：本地清理照做但不通知服务端失焦。用于新 DocMgr 会被 XamlIsland
    // locked 守卫跳过 focus_gained 的情形：发了无人配对，ime_active 会被永久清掉。
    if (sendFocusLost && !_focusLostSent && _pIPCClient != nullptr && _pIPCClient->IsConnected())
    {
        _pIPCClient->SendFocusLost(reason);
        _focusLostSent = TRUE;
        // THREAD / DOC_CHANGED 都意味着已离开原可编辑上下文，一并复位上报态，
        // 否则紧接着的 DocMgr 失焦会再补一条 CTX_LOST。
        _editCtxReported = FALSE;
    }

    if (_pKeyEventSink != nullptr)
    {
        // 失焦一律复位，配对状态也不例外（keepPair 取默认 FALSE）：本 DLL 的配对栈是每宿主
        // 进程一份，core 侧是全局单栈，作用域对不齐，按 reason 保留会时灵时不灵。
        _pKeyEventSink->ResetComposingState();
    }
}

// 焦点离开可编辑控件（DocMgr 级失焦）时通知服务端隐藏工具栏。
//
// 只翻可见性标志、绝不碰输入态，所以能在 DocMgr 噪声层安全调用：抖动最多让工具栏闪一下
// （UI 层隐藏防抖会吸收）。在这里调 CleanupInputStateForDocChange 会让首字符直接上屏。
// 不设 _focusLostSent：CTX_LOST 不是真失焦，置位会吞掉随后真正的 thread_focus_lost。
// 靠 _editCtxReported 只在「有上下文 → 没有」翻转沿发一次，DocMgr 级失焦频率高，避免 IPC 洪泛。
void CTextService::_ReportEditContextLost()
{
    if (!_editCtxReported)
    {
        return; // 本来就没上报过有上下文，无需再说一遍
    }
    _editCtxReported = FALSE;

    if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
    {
        _pIPCClient->SendFocusLost(FOCUS_LOST_REASON_CTX_LOST);
    }
}

// 结束组合。异步：立即返回，_pComposition 当场清空，HasActiveComposition() 随即为 FALSE，
// 新组合可以马上开始。
void CTextService::EndComposition(ITfDocumentMgr* pDocMgrHint, BOOL nonKeyContext)
{
    // direct_commit 顶码：失焦/强制收口前，先把待重开的余码组合落定，语义与下方
    // HoldComposition flush 一致——收口时不能让余码组合悬空未开。
    StartDeferredCompositionIfPending();

    // 智能符号 hold 中（语义=待提交）：主动结束组合（失焦/Deactivate 等）时转为提交而非放弃，
    // 切换窗口时符号应直接上屏。CommitText 内 full = prefix + held，定格的旧符号一并收口。
    if (_hHoldTimer != 0 && _pComposition != nullptr)
    {
        WIND_LOG_DEBUG(L"EndComposition: flushing held smart symbol as commit\n");
        FlushHoldCompositionIfActive(nonKeyContext);
        return;
    }

    // 顶码聚合中（组合头部有待提交前缀）：引擎已按上屏记账，不能放弃组合，转为提交前缀、
    // 丢弃余码。此路径用当前焦点 context 提交，失焦竞态下可能落到 SendInput 兜底。
    if (!_pendingCommitPrefix.empty() && _pComposition != nullptr)
    {
        WIND_LOG_DEBUG(L"EndComposition: flushing pending top-code prefix as commit\n");
        CommitText(L"", nonKeyContext);
        return;
    }

    LARGE_INTEGER startTime, endTime, freq;
    QueryPerformanceCounter(&startTime);
    QueryPerformanceFrequency(&freq);

    // Clear composition text cache
    _lastCompositionText.clear();
    _lastCaretPos = -1;

    // 无组合但有残留前缀（异常路径）：直接丢弃，防止污染下一次组合。
    _pendingCommitPrefix.clear();

    // If there's no active composition, nothing to do
    if (_pComposition == nullptr)
    {
        if (!_recoverLeftover.empty())
            _RequestDropLeftover();
        WIND_LOG_DEBUG(L"EndComposition: No active composition\n");
        return;
    }

    WIND_LOG_DEBUG(L"EndComposition: Ending active composition\n");
    wind::caret::PostImmCandidateRefreshIfHost(L"组合结束");

    // CRITICAL: Transfer ownership of _pComposition immediately
    // This allows new compositions to start while the old one is being ended async
    ITfComposition* pCompToEnd = _pComposition;
    _pComposition = nullptr;  // Clear immediately - HasActiveComposition() now returns FALSE
    _compositionJustStarted = FALSE;

    // pDocMgrHint 给出即具有权威性：composition 属于创建它的 context，而此刻 GetFocus()
    // 可能已指向新文档（doc_changed 路径在新文档获焦后才收口），拿新 context 的 cookie
    // 清旧 context 的 range 轻则失败、重则改到新文档。无 hint 时回落 GetFocus。
    ITfDocumentMgr* pDocMgr = nullptr;
    if (pDocMgrHint != nullptr)
    {
        WIND_LOG_DEBUG(L"EndComposition: using pDocMgrHint (authoritative)\n");
        pDocMgr = pDocMgrHint;
        pDocMgr->AddRef();
    }
    else if (_pThreadMgr == nullptr || FAILED(_pThreadMgr->GetFocus(&pDocMgr)) || pDocMgr == nullptr)
    {
        // Can't get document manager, force cleanup
        WIND_LOG_DEBUG(L"EndComposition: Can't get DocMgr, forcing cleanup\n");
        pCompToEnd->Release();
        return;
    }

    ITfContext* pContext = nullptr;
    HRESULT hr = pDocMgr->GetTop(&pContext);
    pDocMgr->Release();

    if (FAILED(hr) || pContext == nullptr)
    {
        // Can't get context, force cleanup
        WIND_LOG_DEBUG(L"EndComposition: Can't get Context, forcing cleanup\n");
        pCompToEnd->Release();
        return;
    }

    // Create edit session with ownership transfer of pCompToEnd
    CEndCompositionEditSession* pEditSession = new CEndCompositionEditSession(this, pCompToEnd);

    HRESULT hrSession;
    // Use TF_ES_ASYNCDONTCARE for non-blocking operation
    // The edit session will complete asynchronously, and pCompToEnd will be
    // released in DoEditSession or in ~CEndCompositionEditSession if the request fails
    hr = pContext->RequestEditSession(_tfClientId, pEditSession, TF_ES_ASYNCDONTCARE | TF_ES_READWRITE, &hrSession);

    QueryPerformanceCounter(&endTime);
    int durationMs = (int)((endTime.QuadPart - startTime.QuadPart) * 1000 / freq.QuadPart);
    WIND_LOG_DEBUG_FMT(L"EndComposition: RequestEditSession hr=0x%08X, hrSession=0x%08X, duration=%dms\n",
                 hr, hrSession, durationMs);

    if (FAILED(hr))
    {
        // Request failed - pEditSession destructor will release pCompToEnd
        WIND_LOG_DEBUG(L"EndComposition: RequestEditSession failed\n");
    }

    pEditSession->Release();
    pContext->Release();
}

void CTextService::_ArmRecover(ITfContext* pContext, ITfRange* pRange, const std::wstring& leftover, LONG acpStart)
{
    _ClearRecoverRecord();
    _recoverLeftover = leftover;
    _recoverAcpStart = acpStart;
    _recoverContext = pContext;
    _recoverContext->AddRef();
    _recoverRange = pRange; // 接管调用方的引用
    _recoverArmTick = GetTickCount64();
}

void CTextService::_ClearRecoverRecord()
{
    _recoverLeftover.clear();
    if (_recoverContext != nullptr)
    {
        _recoverContext->Release();
        _recoverContext = nullptr;
    }
    if (_recoverRange != nullptr)
    {
        _recoverRange->Release();
        _recoverRange = nullptr;
    }
}

void CTextService::_DisarmRecover(const wchar_t* why)
{
    if (_recoverLeftover.empty())
        return;
    // 先取走再清记录：清残留的会话可能当场执行，它触发的 OnEndEdit 不能再见到这条记录。
    const std::wstring text = _recoverLeftover;
    ITfContext* pContext = _recoverContext;
    ITfRange* pRange = _recoverRange;
    pContext->AddRef();
    pRange->AddRef();
    _ClearRecoverRecord();

    // 不再接上：按「宿主意外终止就清掉组合文字」的语义清残留。Chromium 例外：它把无组合的删改
    // 译成相对页面当前光标的删除，光标已挪开时会删到别处的字。
    HRESULT hr = S_FALSE;
    if (!CCaretEditSession::IsChromiumContext(pContext))
    {
        CPurgeLeftoverEditSession* pPurge = new CPurgeLeftoverEditSession(this, pRange, text);
        pRange = nullptr; // 引用交给会话
        HRESULT hrSession = S_OK;
        // Deactivate 里请求时宿主若排队，会话可能被 TSF 丢弃，残留就留在文档里（与宿主自己 finalize 等同）。
        hr = pContext->RequestEditSession(_tfClientId, pPurge, TF_ES_ASYNCDONTCARE | TF_ES_READWRITE, &hrSession);
        pPurge->Release();
    }
    if (pRange != nullptr)
        pRange->Release();
    pContext->Release();
    WIND_LOG_DEBUG_FMT(L"Recover: disarmed (%ls), purge hr=0x%08X\n", why, hr);
}

void CTextService::_PurgeLeftover(TfEditCookie ec, ITfRange* pRange, const std::wstring& text)
{
    // 区间随宿主的编辑漂移（与组合区间同一机制）；里面仍是那段残留才清。
    WCHAR buf[wind::recover::kMaxLeftoverLen + 1] = {};
    ULONG got = 0;
    HRESULT hr = E_FAIL;
    if (SUCCEEDED(pRange->GetText(ec, 0, buf, (ULONG)wind::recover::kMaxLeftoverLen + 1, &got)) &&
        std::wstring(buf, got) == text)
    {
        hr = pRange->SetText(ec, 0, L"", 0);
    }
    WIND_LOG_DEBUG_FMT(L"Recover: purge leftover len=%zu hr=0x%08X\n", text.length(), hr);
}

void CTextService::_RequestDropLeftover()
{
    if (_recoverContext == nullptr)
        return;
    CDropLeftoverEditSession* pEditSession = new CDropLeftoverEditSession(this, _recoverContext);
    HRESULT hrSession = S_OK;
    HRESULT hr = _recoverContext->RequestEditSession(_tfClientId, pEditSession, TF_ES_ASYNCDONTCARE | TF_ES_READWRITE,
                                                     &hrSession);
    pEditSession->Release();
    if (FAILED(hr))
        _DisarmRecover(L"edit session refused");
}

BOOL CTextService::_LeftoverBeforeCaret(TfEditCookie ec, ITfContext* pContext, ITfRange** ppLeftover)
{
    *ppLeftover = nullptr;
    if (_recoverLeftover.empty() || pContext == nullptr || pContext != _recoverContext)
        return FALSE;
    TF_SELECTION sel = {};
    ULONG fetched = 0;
    if (FAILED(pContext->GetSelection(ec, TF_DEFAULT_SELECTION, 1, &sel, &fetched)) || fetched == 0 ||
        sel.range == nullptr)
        return FALSE;
    ITfRange* pRange = sel.range; // 取得所有权
    const LONG len = (LONG)_recoverLeftover.length();
    BOOL empty = FALSE;
    LONG caretAcp = -1, cch = 0, shifted = 0;
    ITfRangeACP* pAcp = nullptr;
    if (SUCCEEDED(pRange->QueryInterface(IID_ITfRangeACP, (void**)&pAcp)) && pAcp != nullptr)
    {
        if (FAILED(pAcp->GetExtent(&caretAcp, &cch)))
            caretAcp = -1;
        pAcp->Release();
    }
    WCHAR buf[wind::recover::kMaxLeftoverLen + 1] = {};
    ULONG got = 0;
    if (FAILED(pRange->IsEmpty(ec, &empty)) || !empty || caretAcp != _recoverAcpStart + len ||
        FAILED(pRange->ShiftStart(ec, -len, &shifted, nullptr)) || shifted != -len ||
        FAILED(pRange->GetText(ec, 0, buf, (ULONG)wind::recover::kMaxLeftoverLen + 1, &got)) ||
        std::wstring(buf, got) != _recoverLeftover)
    {
        WIND_LOG_DEBUG_FMT(L"Recover: leftover not before caret (len=%ld caret=%ld start=%ld got=%lu)\n",
                           len, caretAcp, _recoverAcpStart, got);
        pRange->Release();
        return FALSE;
    }
    *ppLeftover = pRange;
    return TRUE;
}

void CTextService::_DropRecoverLeftover(TfEditCookie ec, ITfContext* pContext)
{
    if (_recoverLeftover.empty() || pContext == nullptr)
        return;
    ITfRange* pRange = nullptr;
    const BOOL found = _LeftoverBeforeCaret(ec, pContext, &pRange);
    const LONG len = (LONG)_recoverLeftover.length();
    if (!found)
    {
        // 光标不在残留末尾或不是残留所在的 context：不接上，残留另开会话处理，不与本会话的写入混在同一把锁里。
        _DisarmRecover(L"leftover not before caret");
        return;
    }
    _ClearRecoverRecord();
    HRESULT hr = pRange->SetText(ec, 0, L"", 0);
    if (SUCCEEDED(hr))
    {
        pRange->Collapse(ec, TF_ANCHOR_START);
        TF_SELECTION newSel = {};
        newSel.range = pRange;
        newSel.style.ase = TF_AE_NONE;
        newSel.style.fInterimChar = FALSE;
        pContext->SetSelection(ec, 1, &newSel);
    }
    WIND_LOG_DEBUG_FMT(L"Recover: dropped leftover len=%ld hr=0x%08X\n", len, hr);
    pRange->Release();
}

void CTextService::ResetComposingState(BOOL keepPairState)
{
    if (_pKeyEventSink != nullptr)
    {
        _pKeyEventSink->ResetComposingState(keepPairState);
    }
}

// 新开组合时插入点该放哪：常规放末尾。
//
// 例外是占位组合（内容恰为空格或 ZWSP，见 CompositionPlaceholder.h 与 Rust 侧
// `COMPOSITION_PLACEHOLDER`）：TSF 不接受空组合，它只为维持一个活着的组合，插入点必须落在
// 它前面，否则光标凭空右移一格。联想态没有「下一键」把插入点拉回 0，所以只在那里显形。
// 取值须与 Rust 侧 `COMPOSITION_PLACEHOLDER` 一致。
int CTextService::_CompositionCaretFor(const std::wstring& composition)
{
    return wind::placeholder::IsPlaceholderText(composition) ? 0 : static_cast<int>(composition.length());
}

BOOL CTextService::InsertTextAndStartComposition(const std::wstring& insertText, const std::wstring& newComposition)
{
    WIND_LOG_DEBUG_FMT(L"InsertTextAndStartComposition: insert='%s', newComp='%s', prefixLen=%zu, _pComposition=%p\n",
                 insertText.c_str(), newComposition.c_str(), _pendingCommitPrefix.length(), _pComposition);

    // 顶码聚合（对齐微软五笔的事件流）：顶出的文本不立即提交，累入待提交前缀、留在组合头部
    // 显示，推迟到最终上屏（CommitText）一次提交。宿主全程只见 compositionupdate 和最后一次
    // compositionend，快照式（xterm.js）与 diff 式（Chromium TSFTextStore）宿主都不会把余码
    // 并入提交；EndComposition→StartComposition 的拆法会双写余码。
    //
    // 有活动组合：纯组合内容更新；无活动组合（罕见）：UpdateComposition 新建组合显示 前缀+余码。
    _pendingCommitPrefix += insertText;
    return UpdateComposition(newComposition, _CompositionCaretFor(newComposition));
}

// ============================================================================
// ITfDisplayAttributeProvider implementation
// ============================================================================

STDAPI CTextService::EnumDisplayAttributeInfo(IEnumTfDisplayAttributeInfo** ppEnum)
{
    if (ppEnum == nullptr)
        return E_INVALIDARG;

    *ppEnum = new CEnumDisplayAttributeInfo();
    return (*ppEnum != nullptr) ? S_OK : E_OUTOFMEMORY;
}

STDAPI CTextService::GetDisplayAttributeInfo(REFGUID guid, ITfDisplayAttributeInfo** ppInfo)
{
    if (ppInfo == nullptr)
        return E_INVALIDARG;

    *ppInfo = nullptr;

    if (IsEqualGUID(guid, c_guidDisplayAttributeInput))
    {
        *ppInfo = new CDisplayAttributeInfoInput();
        return (*ppInfo != nullptr) ? S_OK : E_OUTOFMEMORY;
    }

    return E_INVALIDARG;
}

// ============================================================================
// Display Attribute initialization
// ============================================================================

BOOL CTextService::_InitDisplayAttribute()
{
    // Get category manager
    ITfCategoryMgr* pCategoryMgr = nullptr;
    HRESULT hr = CoCreateInstance(CLSID_TF_CategoryMgr, nullptr, CLSCTX_INPROC_SERVER,
                                   IID_ITfCategoryMgr, (void**)&pCategoryMgr);
    if (FAILED(hr) || pCategoryMgr == nullptr)
    {
        WIND_LOG_ERROR(L"Failed to create category manager\n");
        return FALSE;
    }

    // Register display attribute GUID
    hr = pCategoryMgr->RegisterGUID(c_guidDisplayAttributeInput, &_gaDisplayAttributeInput);
    if (FAILED(hr))
    {
        WIND_LOG_ERROR(L"Failed to register display attribute GUID\n");
        pCategoryMgr->Release();
        return FALSE;
    }

    WIND_LOG_DEBUG_FMT(L"Display attribute registered, atom=%lu\n", (unsigned long)_gaDisplayAttributeInput);

    pCategoryMgr->Release();
    return TRUE;
}

void CTextService::_UninitDisplayAttribute()
{
    // Reset the GUID atom
    _gaDisplayAttributeInput = TF_INVALID_GUIDATOM;
}

// ─── HoldComposition ─────────────────────────────────────────────────────────

BOOL CTextService::HoldComposition(const std::wstring& text, UINT timeoutMs)
{
    WIND_LOG_DEBUG_FMT(L"HoldComposition: text=%s timeoutMs=%u\n",
                       text.c_str(), timeoutMs);

    // 旧 held 符号定格并入 prefix（不 commit、不动文档），与新符号在同一次 UpdateComposition
    // 内完成显示。「先 CommitText 旧符号再开新组合」会被 Chromium TSFTextStore 按整锁 diff
    // 误读成替换、在 WPS 同步锁失败走 SendInput 乱序，表现为后一个符号顶掉前一个。
    AbsorbHeldIntoPrefix();

    // 将中文符号放入 TSF 组合态（caretPos = 文本长度，光标置末；显示 = prefix + text）。
    // noUnderline：符号观感与已上屏一致（预上屏），实际仍在组合态内可替换。
    if (!UpdateComposition(text, static_cast<int>(text.length()), TRUE))
    {
        WIND_LOG_ERROR(L"HoldComposition: UpdateComposition failed\n");
        return FALSE;
    }

    _heldCompositionText = text;
    g_holdTimerInstance  = this;
    _hHoldTimer = SetTimer(NULL, 0, timeoutMs, HoldTimerProc);

    if (_hHoldTimer == 0)
    {
        WIND_LOG_ERROR(L"HoldComposition: SetTimer failed\n");
        g_holdTimerInstance  = nullptr;
        _heldCompositionText.clear();
        return FALSE;
    }

    WIND_LOG_DEBUG_FMT(L"HoldComposition: timer started id=%llu\n",
                       static_cast<unsigned long long>(_hHoldTimer));
    return TRUE;
}

void CTextService::FlushHoldCompositionIfActive(BOOL nonKeyContext)
{
    if (_hHoldTimer != 0)
        OnHoldTimerExpired(nonKeyContext);
}

// 把当前 held 的智能符号定格并入 _pendingCommitPrefix（不 commit、不动文档），最终由
// CommitText 一次收口，规避「commit + 立即重启组合」在 Chromium/WPS 下的替换误读。
// 定格后的符号不可再被 press2 替换，与服务端状态机一致（press2 只作用于最新 armed 的符号）。
void CTextService::AbsorbHeldIntoPrefix()
{
    if (_hHoldTimer == 0)
        return;

    _pendingCommitPrefix += _heldCompositionText;
    KillTimer(NULL, _hHoldTimer);
    _hHoldTimer          = 0;
    _heldCompositionText.clear();
    g_holdTimerInstance  = nullptr;
    WIND_LOG_DEBUG_FMT(L"AbsorbHeldIntoPrefix: held symbol pinned, prefixLen=%zu\n",
                       _pendingCommitPrefix.length());
}

void CTextService::CancelHoldTimer()
{
    if (_hHoldTimer == 0)
        return;

    KillTimer(NULL, _hHoldTimer);
    WIND_LOG_DEBUG_FMT(L"CancelHoldTimer: killed timer id=%llu\n",
                       static_cast<unsigned long long>(_hHoldTimer));
    _hHoldTimer          = 0;
    _heldCompositionText.clear();
    g_holdTimerInstance  = nullptr;
}

// static
VOID CALLBACK CTextService::HoldTimerProc(HWND /*hwnd*/, UINT /*uMsg*/,
                                           UINT_PTR idEvent, DWORD /*dwTime*/)
{
    if (g_holdTimerInstance != nullptr
        && idEvent == g_holdTimerInstance->_hHoldTimer)
    {
        // 真正的 WM_TIMER 回调：拿不到同步编辑会话，须异步收口。
        g_holdTimerInstance->OnHoldTimerExpired(TRUE);
    }
}

void CTextService::OnHoldTimerExpired(BOOL nonKeyContext)
{
    WIND_LOG_DEBUG_FMT(L"OnHoldTimerExpired: committing chinese text, nonKeyContext=%d\n",
                       nonKeyContext);

    UINT_PTR timerId = _hHoldTimer;
    std::wstring textToCommit = std::move(_heldCompositionText);
    _hHoldTimer         = 0;
    g_holdTimerInstance = nullptr;
    KillTimer(NULL, timerId);

    // 判据原样透传：WM_TIMER 回调与经窗口消息/COM 回调进来的 Flush 走异步会话收口；
    // 按键上下文里的 Flush 保持同步，确保收口先于后续透传字符。
    CommitText(textToCommit, nonKeyContext);
    // 待定符号落定 = 文档里多了这个符号：seqChar 跟上（否则「hello，」之后的汉字前会被误补空格）。
    if (_pKeyEventSink != nullptr)
        _pKeyEventSink->NoteCommittedSeqChar(textToCommit);
}

// ─── DeferredComposition（direct_commit 顶码，延迟到 keyup 才开新组合）───────────

// 延迟组合（direct_commit 顶码）定时器的 thread_local 实例指针。
static thread_local CTextService* g_deferredTimerInstance = nullptr;

void CTextService::StashDeferredComposition(const std::wstring& composition, UINT fallbackMs, UINT triggerVk)
{
    // 异常保护：若已有待重开的余码，先把旧的落定，避免丢失。
    StartDeferredCompositionIfPending();

    _deferredCompText = composition;
    _deferredTriggerVk = triggerVk;
    g_deferredTimerInstance = this;
    _hDeferredTimer = SetTimer(NULL, 0, fallbackMs, DeferredTimerProc);

    if (_hDeferredTimer == 0)
    {
        WIND_LOG_ERROR(L"StashDeferredComposition: SetTimer failed\n");
        g_deferredTimerInstance = nullptr;
        _deferredCompText.clear();
        return;
    }

    WIND_LOG_DEBUG_FMT(L"StashDeferredComposition: text=%s fallbackMs=%u triggerVk=0x%02X timer=%llu\n",
                       composition.c_str(), fallbackMs, triggerVk,
                       static_cast<unsigned long long>(_hDeferredTimer));
}

void CTextService::StartDeferredCompositionIfPending()
{
    if (_deferredCompText.empty())
    {
        // 仅残留定时器（理论不达）也一并清掉。
        if (_hDeferredTimer != 0) { KillTimer(NULL, _hDeferredTimer); _hDeferredTimer = 0; }
        g_deferredTimerInstance = nullptr;
        return;
    }
    std::wstring text = std::move(_deferredCompText);
    _deferredCompText.clear();
    if (_hDeferredTimer != 0) { KillTimer(NULL, _hDeferredTimer); _hDeferredTimer = 0; }
    g_deferredTimerInstance = nullptr;

    WIND_LOG_DEBUG_FMT(L"StartDeferredComposition: opening new composition text=%s\n", text.c_str());
    // 此刻 CommitText 已结束旧组合、_pComposition 为空 → UpdateComposition 新建组合并显示余码
    //（有下划线的正常编码态）。对齐真实输入法 compositionstart@keyup。
    const BOOL opened = UpdateComposition(text, _CompositionCaretFor(text));
    // 延迟组合在 keyup / 定时器里开，没人替它上报坐标；不报的话候选窗首显用上屏前的陈旧坐标，
    // 直到下一键才跳。组合没建成（失败或宿主排成异步）就不报：非按键上下文同步取坐标会被拒，
    // 退到 GUI 光标的值会被服务端当权威坐标；异步落地的由 OnLayoutChange 去报。
    if (opened && HasActiveComposition())
    {
        SendCaretPositionUpdate();
    }
}

void CTextService::CancelDeferredComposition()
{
    if (_hDeferredTimer != 0) { KillTimer(NULL, _hDeferredTimer); _hDeferredTimer = 0; }
    _deferredCompText.clear();
    g_deferredTimerInstance = nullptr;
}

// static
VOID CALLBACK CTextService::DeferredTimerProc(HWND, UINT, UINT_PTR idEvent, DWORD)
{
    if (g_deferredTimerInstance != nullptr
        && idEvent == g_deferredTimerInstance->_hDeferredTimer)
    {
        WIND_LOG_DEBUG(L"DeferredTimerProc: keyup 未达，兜底开余码组合\n");
        g_deferredTimerInstance->StartDeferredCompositionIfPending();
    }
}
