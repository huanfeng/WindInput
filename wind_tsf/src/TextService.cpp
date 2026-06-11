#include "TextService.h"
#include "KeyEventSink.h"
#include "IPCClient.h"
#include "LangBarItemButton.h"
#include "CaretEditSession.h"
#include "DisplayAttributeInfo.h"
#include "HotkeyManager.h"
#include "HostWindow.h"
#include <vector>
#include <ShellScalingApi.h>
#include <InputScope.h> // ITfInputScope / InputScope 枚举

// GUID_PROP_INPUTSCOPE 在 SDK 头中仅为 EXTERN_C 声明，其字节定义需某个 TU 启用 INITGUID
// 才会生成；直接引用会产生 LNK2019。这里本地定义该 GUID 值（与 inputscope.h 一致），
// 避免引入 <initguid.h> 把本文件所有 GUID 实体化而与已链接定义冲突。
static const GUID kGuidPropInputScope =
    { 0x1713dd5a, 0x68e7, 0x4a5b, { 0x9a, 0xf6, 0x59, 0x2a, 0x59, 0x5c, 0x77, 0x8d } };

// InputScope bit 常量（与 Go 端、inputscope.h 一致）
static const UINT64 kScopeBitPassword = 1ULL << 31; // IS_PASSWORD

// TSF 标准 compartment GUID（本地定义，避免链接 TSF GUID 静态库产生 LNK2019）。
// 宿主（含 Chromium 系浏览器密码框）会在 context 上置 KEYBOARD_DISABLED 表示"禁用输入法"；
// 这是比 InputScope 更可靠的密码框信号（小狼毫/Weasel 即用此判定），且无痕普通框不会置位。
static const GUID kGuidCompartmentKeyboardDisabled =
    { 0x71a5b253, 0x1951, 0x466b, { 0x9f, 0xbc, 0x9c, 0x88, 0x08, 0xfa, 0x84, 0xf2 } };

// 判断屏幕坐标点是否落在前台窗口矩形之外（原点错误的越界坐标）。个别宿主（某些 OpenGL /
// 异常渲染应用）的 GetTextExt 会返回越界屏幕坐标，直接用会把候选框甩到屏幕角落。检出后
// 调用方应丢弃该坐标、回退到窗口相对的方法。参考 Weasel enhanced_position；仅在明显越界时
// 返回 true，光标在窗口内的正常情形不受影响。
static bool IsScreenPointOutsideForegroundWindow(LONG x, LONG y)
{
    HWND hwndFg = GetForegroundWindow();
    if (hwndFg == nullptr)
        return false;
    RECT rcWin = {};
    if (!GetWindowRect(hwndFg, &rcWin))
        return false;
    return (x < rcWin.left || x > rcWin.right || y < rcWin.top || y > rcWin.bottom);
}

// EditSession for reading the focused context's TSF InputScope set.
// 用于识别密码框等语义控件：GetInputScopes 返回的枚举值按位编码为 bitmask
// （bit N 表示枚举值 N 存在，如 IS_PASSWORD=31 → bit 31），交由 Go 端决策。
// 需要读锁：InputScope 属性值通过 ITfReadOnlyProperty::GetValue 读取，必须在
// 编辑会话的 read cookie 下进行。同步读锁在 OnSetFocus 期间通常可获得（与
// CCaretEditSession 一致）；获取失败时返回 0（视为默认/未知，行为同既有逻辑）。
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

// EditSession for ending composition
// NOTE: This class takes ownership of the composition pointer passed to it.
// The composition will be ended and released when DoEditSession is called,
// or in the destructor if the edit session request fails.
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

// EditSession for committing text atomically (end composition + insert text in one session)
// This prevents race conditions where async EndComposition clears text inserted by a subsequent InsertText.
class CCommitTextEditSession : public ITfEditSession
{
public:
    // pComposition ownership is transferred to this object (may be nullptr if no active composition)
    CCommitTextEditSession(CTextService* pTextService, ITfContext* pContext,
                           ITfComposition* pComposition, const std::wstring& text)
        : _refCount(1), _pTextService(pTextService), _pContext(pContext),
          _pComposition(pComposition), _text(text), _success(FALSE)
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
            WIND_LOG_DEBUG(L"~CCommitTextEditSession: Releasing orphaned composition\n");
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
    STDMETHODIMP DoEditSession(TfEditCookie ec)
    {
        // 标准 IME 提交语义: 把最终上屏文字 SetText 到 composition range 后再
        // EndComposition, 让宿主应用通过 OnEndComposition 看到的 range 内容就是
        // 上屏文字, 与微软拼音/搜狗/Rime 一致。
        //
        // 历史实现 (SetText("") + EndComposition + InsertTextAtSelection) 等价于
        // "IME 取消了 composition + 旁路插入了一段普通文本", 跟打/统计类应用会
        // 把这次上屏误判为"非 IME 输入", 影响正确率统计。原子性由单一 EditSession
        // 保证 (历史 02a753f 修的浏览器异步竞态), 与此次模式调整正交。
        if (_pComposition != nullptr)
        {
            BOOL committedViaComposition = FALSE;
            ITfRange* pRange = nullptr;
            if (SUCCEEDED(_pComposition->GetRange(&pRange)))
            {
                // 把 composition range 内容替换为最终文字; _text 为空则等价于清空。
                pRange->SetText(ec, TF_ST_CORRECTION, _text.c_str(), (LONG)_text.length());
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
            if (committedViaComposition)
            {
                _success = TRUE;
                WIND_LOG_DEBUG(L"CCommitTextEditSession: SetText + EndComposition committed\n");
                return S_OK;
            }
            // GetRange 失败 (极少): composition 已结束但文字未写入, fallthrough
            // 到 InsertTextAtSelection 兜底, 避免静默丢字。
            WIND_LOG_DEBUG(L"CCommitTextEditSession: GetRange failed, falling back to InsertTextAtSelection\n");
        }

        // 无 active composition 的回退路径 (鼠标上屏 / 上述 GetRange 失败兜底): 走 InsertTextAtSelection。
        if (!_text.empty())
        {
            ITfInsertAtSelection* pInsertAtSel = nullptr;
            HRESULT hr = _pContext->QueryInterface(IID_ITfInsertAtSelection, (void**)&pInsertAtSel);
            if (FAILED(hr) || pInsertAtSel == nullptr)
            {
                WIND_LOG_DEBUG(L"CCommitTextEditSession: Failed to get ITfInsertAtSelection\n");
                return E_FAIL;
            }

            ITfRange* pRange = nullptr;
            hr = pInsertAtSel->InsertTextAtSelection(ec, 0, _text.c_str(), (LONG)_text.length(), &pRange);
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
        }

        _success = TRUE;
        WIND_LOG_DEBUG(L"CCommitTextEditSession: Text committed via InsertTextAtSelection fallback\n");
        return S_OK;
    }

    BOOL GetSuccess() const { return _success; }

private:
    LONG _refCount;
    CTextService* _pTextService;
    ITfContext* _pContext;
    ITfComposition* _pComposition;  // Owned composition pointer
    std::wstring _text;
    BOOL _success;
};

// EditSession for updating composition
class CUpdateCompositionEditSession : public ITfEditSession
{
public:
    CUpdateCompositionEditSession(CTextService* pTextService, ITfContext* pContext, const std::wstring& text, int caretPos = -1)
        : _refCount(1), _pTextService(pTextService), _pContext(pContext), _text(text), _caretPos(caretPos)
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
            // Get current selection (cursor position) to start composition there
            TF_SELECTION tfSelection;
            ULONG cFetched;
            if (FAILED(_pContext->GetSelection(ec, TF_DEFAULT_SELECTION, 1, &tfSelection, &cFetched)) || cFetched != 1)
            {
                return E_FAIL;
            }

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
            // Weasel 模式：标记 composition 刚刚创建。下一次 SendCaretPositionUpdate
            // 不会立即发 IPC，而是等 OnLayoutChange 提供 reflow 后的权威坐标，
            // 50ms timer 兜底（应对不发 OnLayoutChange 的应用，如某些 CUAS 路径）。
            _pTextService->_compositionJustStarted = TRUE;
        }

        // 2. Get range from composition
        ITfRange* pRange = nullptr;
        if (FAILED(_pTextService->_pComposition->GetRange(&pRange)))
        {
            return E_FAIL;
        }

        // 3. Set text
        // When composition text is empty (non-inline preedit mode), use a space as
        // placeholder so GetTextExt can return a valid caret rect. Without this,
        // apps like WPS return a degenerate rect (height=0) for zero-length ranges.
        // The cursor is positioned before the space (step 5), so visually there's
        // no offset. The placeholder is cleared on EndComposition/CommitText.
        BOOL isPlaceholder = _text.empty();
        static const wchar_t PLACEHOLDER[] = L" ";
        const wchar_t* textPtr = isPlaceholder ? PLACEHOLDER : _text.c_str();
        LONG textLen = isPlaceholder ? 1 : (LONG)_text.length();

        hr = pRange->SetText(ec, TF_ST_CORRECTION, textPtr, textLen);

        if (SUCCEEDED(hr))
        {
            // 4. Apply display attribute to show underline
            // Skip for placeholder text to avoid any visual artifacts
            if (!isPlaceholder)
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

        // Cache caret position from within this valid edit session.
        // This is critical for WebView apps where a separate CCaretEditSession
        // with TF_INVALID_COOKIE would be rejected.
        // 但 composition 刚刚创建（_compositionJustStarted）时跳过缓存：宿主
        // 在此刻尚未完成 reflow，GetTextExt 返回的是 pre-reflow 旧坐标，写入
        // 缓存会让后续 timer 兜底取到陈旧值。等 timer/OnLayoutChange 路径走
        // GetCaretPosition fresh 查询。
        if (SUCCEEDED(hr) && !_pTextService->_compositionJustStarted)
        {
            _CacheCaretPosition(ec);
        }

        return hr;
    }

private:
    int _caretPos;  // Cursor position within composition (-1 = at end)

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
                // Skip degenerate rects (height=0) — apps like WPS may return
                // an invalid rect on the first composition before layout reflow.
                // Cache 仅作为 timer 兜底使用；OnLayoutChange 路径会清掉 cache
                // 并重新通过 fallback 查询，因此这里不再需要标记延迟重试。
                LONG h = caretRect.bottom - caretRect.top;
                // 同 GetCaretPositionFromTSF：跳过退化矩形(h<=0)与越界坐标，避免把不可信坐标
                // 缓存后被 timer 兜底取用。越界时不缓存，留待 fallback 链重新求解。
                if (h > 0 && !IsScreenPointOutsideForegroundWindow(caretRect.left, caretRect.top))
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

        // Set the display attribute on the composition range
        VARIANT var;
        var.vt = VT_I4;
        var.lVal = gaDisplayAttr;

        HRESULT hr = pDisplayAttrProp->SetValue(ec, pRange, &var);
        if (FAILED(hr))
        {
            WIND_LOG_DEBUG(L"Failed to set display attribute\n");
        }
        else
        {
            WIND_LOG_DEBUG(L"Display attribute set successfully\n");
        }

        pDisplayAttrProp->Release();
    }

private:
    LONG _refCount;
    CTextService* _pTextService;
    ITfContext* _pContext;
    std::wstring _text;
};

// EditSession for inserting text and starting new composition (for top code commit)
class CInsertAndComposeEditSession : public ITfEditSession
{
public:
    // pOldComposition ownership is transferred (may be nullptr)
    CInsertAndComposeEditSession(CTextService* pTextService, ITfContext* pContext,
                                  ITfComposition* pOldComposition,
                                  const std::wstring& insertText, const std::wstring& newComposition)
        : _refCount(1), _pTextService(pTextService), _pContext(pContext),
          _pOldComposition(pOldComposition), _insertText(insertText), _newComposition(newComposition)
    {
        _pTextService->AddRef();
        _pContext->AddRef();
    }

    ~CInsertAndComposeEditSession()
    {
        _pTextService->Release();
        _pContext->Release();
        if (_pOldComposition != nullptr)
        {
            WIND_LOG_DEBUG(L"~CInsertAndComposeEditSession: Releasing orphaned old composition\n");
            _pOldComposition->Release();
            _pOldComposition = nullptr;
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
    STDMETHODIMP DoEditSession(TfEditCookie ec)
    {
        HRESULT hr = S_OK;

        WIND_LOG_DEBUG_FMT(L"InsertAndCompose: insert='%s', newComp='%s'\n",
                     _insertText.c_str(), _newComposition.c_str());

        // 0. End old composition (if present) — 与 CCommitTextEditSession 一致采用
        // "SetText(最终文字) + EndComposition" 模式, 让宿主感知到一次完整 IME 上屏
        // 而非"取消 composition + 旁路 Insert"。光标自动落在插入文字之后, 作为新
        // composition 的起点。原子性由单一 EditSession 保证, 浏览器异步竞态修复不
        // 受影响 (历史 02a753f)。
        bool insertedViaOldComposition = false;
        if (_pOldComposition != nullptr)
        {
            ITfRange* pRange = nullptr;
            if (SUCCEEDED(_pOldComposition->GetRange(&pRange)))
            {
                // _insertText 为空也允许 (语义=清空旧 composition 后另起新 composition)。
                pRange->SetText(ec, TF_ST_CORRECTION, _insertText.c_str(), (LONG)_insertText.length());
                pRange->Collapse(ec, TF_ANCHOR_END);
                TF_SELECTION sel = {};
                sel.range = pRange;
                sel.style.ase = TF_AE_NONE;
                sel.style.fInterimChar = FALSE;
                _pContext->SetSelection(ec, 1, &sel);
                pRange->Release();
                insertedViaOldComposition = true;
            }
            _pOldComposition->EndComposition(ec);
            _pOldComposition->Release();
            _pOldComposition = nullptr;
            WIND_LOG_DEBUG(L"InsertAndCompose: Old composition ended via SetText + EndComposition\n");
        }

        // 1. Get current selection to anchor new composition
        TF_SELECTION tfSelection;
        ULONG cFetched;
        if (FAILED(_pContext->GetSelection(ec, TF_DEFAULT_SELECTION, 1, &tfSelection, &cFetched)) || cFetched != 1)
        {
            WIND_LOG_DEBUG(L"InsertAndCompose: Failed to get selection\n");
            return E_FAIL;
        }

        // 2. 仅在无旧 composition 的回退路径下才需要在 selection 处插入文字
        // (旧 composition 路径已经在第 0 步通过 SetText 写入并把光标移到了文本末)。
        if (!insertedViaOldComposition && !_insertText.empty())
        {
            hr = tfSelection.range->SetText(ec, 0, _insertText.c_str(), (LONG)_insertText.length());
            if (FAILED(hr))
            {
                WIND_LOG_DEBUG(L"InsertAndCompose: Failed to insert text\n");
                tfSelection.range->Release();
                return hr;
            }
            WIND_LOG_DEBUG(L"InsertAndCompose: Text inserted at selection (fallback path)\n");

            // Collapse range to end (after inserted text)
            tfSelection.range->Collapse(ec, TF_ANCHOR_END);
        }

        // 3. Now start a new composition for the new input
        // Always start composition (even when _newComposition is empty = non-inline placeholder mode)
        {
            ITfContextComposition* pContextComp = nullptr;
            if (FAILED(_pContext->QueryInterface(IID_ITfContextComposition, (void**)&pContextComp)))
            {
                WIND_LOG_DEBUG(L"InsertAndCompose: Failed to get ITfContextComposition\n");
                tfSelection.range->Release();
                return E_FAIL;
            }

            // Start new composition at current position (after inserted text)
            hr = pContextComp->StartComposition(
                ec,
                tfSelection.range,
                (ITfCompositionSink*)_pTextService,
                &_pTextService->_pComposition);

            pContextComp->Release();

            if (FAILED(hr) || _pTextService->_pComposition == nullptr)
            {
                WIND_LOG_DEBUG(L"InsertAndCompose: Failed to start new composition\n");
                tfSelection.range->Release();
                return E_FAIL;
            }

            WIND_LOG_DEBUG(L"InsertAndCompose: New composition started\n");
            // Weasel 模式：与 CUpdateCompositionEditSession 一致，标记延迟首次 IPC。
            _pTextService->_compositionJustStarted = TRUE;

            // 4. Set the composition text
            // Non-inline preedit: _newComposition is empty → use space placeholder (same as
            // CUpdateCompositionEditSession), cursor positioned BEFORE the placeholder so
            // GetTextExt returns valid coordinates without showing any visible text.
            BOOL isPlaceholder = _newComposition.empty();
            static const wchar_t PLACEHOLDER[] = L" ";
            const wchar_t* textPtr = isPlaceholder ? PLACEHOLDER : _newComposition.c_str();
            LONG textLen = isPlaceholder ? 1 : (LONG)_newComposition.length();

            ITfRange* pCompRange = nullptr;
            if (SUCCEEDED(_pTextService->_pComposition->GetRange(&pCompRange)))
            {
                hr = pCompRange->SetText(ec, TF_ST_CORRECTION, textPtr, textLen);
                if (SUCCEEDED(hr))
                {
                    if (!isPlaceholder)
                        _SetDisplayAttribute(ec, pCompRange);

                    ITfRange* pRangeForSel = nullptr;
                    if (SUCCEEDED(_pTextService->_pComposition->GetRange(&pRangeForSel)))
                    {
                        if (isPlaceholder)
                        {
                            // Placeholder: position cursor BEFORE the space (same as UpdateComposition placeholder logic)
                            pRangeForSel->Collapse(ec, TF_ANCHOR_START);
                        }
                        else
                        {
                            // Inline preedit: cursor at end of composition text
                            pRangeForSel->Collapse(ec, TF_ANCHOR_END);
                        }
                        TF_SELECTION sel = {};
                        sel.range = pRangeForSel;
                        sel.style.ase = TF_AE_NONE;
                        sel.style.fInterimChar = FALSE;
                        _pContext->SetSelection(ec, 1, &sel);
                        pRangeForSel->Release();
                    }
                    WIND_LOG_DEBUG_FMT(L"InsertAndCompose: Composition text set (placeholder=%d)\n", isPlaceholder);
                }
                pCompRange->Release();
            }
        }

        tfSelection.range->Release();
        return S_OK;
    }

private:
    ITfComposition* _pOldComposition;  // Owned old composition pointer

    void _SetDisplayAttribute(TfEditCookie ec, ITfRange* pRange)
    {
        TfGuidAtom gaDisplayAttr = _pTextService->GetDisplayAttributeInputAtom();
        if (gaDisplayAttr == TF_INVALID_GUIDATOM) return;

        ITfProperty* pDisplayAttrProp = nullptr;
        if (FAILED(_pContext->GetProperty(GUID_PROP_ATTRIBUTE, &pDisplayAttrProp))) return;

        VARIANT var;
        var.vt = VT_I4;
        var.lVal = gaDisplayAttr;
        pDisplayAttrProp->SetValue(ec, pRange, &var);
        pDisplayAttrProp->Release();
    }

private:
    LONG _refCount;
    CTextService* _pTextService;
    ITfContext* _pContext;
    std::wstring _insertText;
    std::wstring _newComposition;
};

static const LONG DEFAULT_CARET_HEIGHT = 20;

CTextService::CTextService()
    : _refCount(1)
    , _pThreadMgr(nullptr)
    , _tfClientId(TF_CLIENTID_NULL)
    , _dwThreadMgrEventSinkCookie(TF_INVALID_COOKIE)
    , _dwThreadFocusSinkCookie(TF_INVALID_COOKIE)
    , _uiElementId((DWORD)-1)
    , _uiElementShown(FALSE)
    , _pUIElementMgr(nullptr)
    , _pSourceSingle(nullptr)
    , _funcProviderRegistered(FALSE)
    , _hHotkeyWnd(nullptr)
    , _hotkeyWndClass(0)
    , _hotkeysActive(FALSE)
    , _hasThreadFocus(FALSE)
    , _activateFlags(0)
    , _pKeyEventSink(nullptr)
    , _pIPCClient(nullptr)
    , _pLangBarItemButton(nullptr)
    , _pHotkeyManager(nullptr)
    , _pHostWindow(nullptr)
    , _bChineseMode(TRUE)
    , _bFullWidth(FALSE)
    , _focusSessionId(0)
    , _hasFocus(FALSE)
    , _hasTextInputContext(FALSE)
    , _pComposition(nullptr)
    , _hasCachedCaretPos(FALSE)
    , _hasCachedCompStartPos(FALSE)
    , _compositionJustStarted(FALSE)
    , _needsFocusRecovery(FALSE)
    , _lastFocusCaretX(0)
    , _lastFocusCaretY(0)
    , _lastFocusCaretHeight(DEFAULT_CARET_HEIGHT)
    , _hasLastKnownCaretPos(FALSE)
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
    DllAddRef();
}

CTextService::~CTextService()
{
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
    else if (IsEqualIID(riid, IID_ITfFunctionProvider))
    {
        *ppvObj = (ITfFunctionProvider*)this;
    }

    if (*ppvObj)
    {
        AddRef();
        return S_OK;
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

    _activateFlags = dwFlags;

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

    // Notify Go service that IME is activated and sync full state.
    // Uses _DoFullStateSync which also handles lazy connect (service may
    // still be starting after first install).
    _DoFullStateSync();

    // NOTE: Using synchronous IPC mode (no reader thread)
    // Reference: Weasel uses sync IPC with librime and it works well
    // The reader thread is not started - responses are received synchronously in OnKeyDown

    WIND_LOG_INFO(L"TextService::Activate completed successfully (sync IPC mode)\n");
    return S_OK;
}

STDAPI CTextService::Deactivate()
{
    WIND_LOG_INFO(L"TextService::Deactivate called\n");

    if (_pKeyEventSink != nullptr)
    {
        _pKeyEventSink->FlushEnglishStats();
    }

    // End any active composition before deactivating
    EndComposition();

    // 清理候选 UI 元素（必须在 ThreadMgr 释放之前）
    NotifyCandidatesVisibilityChanged(FALSE);

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

    // Notify Go service that IME is being deactivated (before disconnecting)
    // This allows the service to hide the toolbar immediately
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

// ITfThreadFocusSink — 线程进入 foreground（应用窗口被激活）。
// 跨进程协调：当某个进程通过 race check 让出热键时，PostMessage 给当前前台
// 进程的 IME hidden window，让对方立即重试注册（避免对方等到下次 IME 事件
// 才发现热键空出来）。消息 ID 用 RegisterWindowMessage 全局注册，所有进程
// 取到相同 ID。
static UINT GetRetryHotkeyMessageId()
{
    static UINT s_msg = RegisterWindowMessageW(L"WindInputHotkeyRetry_v1");
    return s_msg;
}

// Foreground self-check timer：兜底 OnKillThreadFocus 在 Chromium / Wails 类
// 宿主里可能不触发的问题。只要本进程持有任何热键就需要定期校验，500ms 间隔
// 既能尽快让出（用户切走半秒内），开销又可忽略。
static constexpr UINT_PTR kFocusCheckTimerId = 0x57494E44; // 'WIND'
static constexpr UINT     kFocusCheckIntervalMs = 500;

// Win32 RegisterHotKey 热键 ID（前置声明给 OnKillThreadFocus 使用）
// 窗口类名按 Debug / Release 区分（class name 是 per-process 不会跨进程冲突，但
// 命名约定与 pipe / CLSID 等其他跨进程资源保持一致，便于 Spy++ 等工具区分两版本）。
#ifdef WIND_DEBUG_VARIANT
static const wchar_t* kHotkeyWndClassName = L"WindInputHotkeyWndDebug";
static const wchar_t* kHotkeyWndTitle     = L"WindInputHotkeyDebug";
#else
static const wchar_t* kHotkeyWndClassName = L"WindInputHotkeyWnd";
static const wchar_t* kHotkeyWndTitle     = L"WindInputHotkey";
#endif
static constexpr int  kHotkeyIdPinBase    = 0x4000; // Pin: Ctrl+N → id = kHotkeyIdPinBase + N
static constexpr int  kHotkeyIdDelBase    = 0x4010; // Delete: Ctrl+Shift+N → id = kHotkeyIdDelBase + N

STDAPI CTextService::OnSetThreadFocus()
{
    WIND_LOG_DEBUG(L"OnSetThreadFocus called\n");
    _hasThreadFocus = TRUE;
    // 拿回 thread focus：候选可见性热键由 NotifyCandidatesVisibilityChanged 驱动，
    // 这里不主动补——切焦点时候选窗通常已经消失。
    return S_OK;
}

// ITfThreadFocusSink — 线程退出 foreground。
STDAPI CTextService::OnKillThreadFocus()
{
    WIND_LOG_DEBUG(L"OnKillThreadFocus called\n");
    _hasThreadFocus = FALSE;
    // 立即让出所有热键，让前台应用的 IME 实例能注册成功。
    if (_hotkeysActive)
    {
        _UnregisterCandidateHotkeys();
    }
    // 注意：失焦时**不**销毁 HostWindow。SearchHost/任务管理器等用 XamlIsland
    // locked/transient DocMgr，OnSetFocus 对其跳过 focus_gained（防 composition
    // replay），而 HostWindow 重建依赖 focus_gained → 一旦销毁就再也不会重建，
    // 候选永久不显示。per-PID event 模型下 HostWindow 可常驻：失焦时 Go 发 WriteHide
    // 经本进程 event 隐藏窗口即可，无需销毁。
    return S_OK;
}

// ============================================================================
// Win32 RegisterHotKey 支持
// 候选可见时把 Ctrl+0..9 + Ctrl+Shift+0..9 注册为系统级热键，OS 在 WM_KEYDOWN
// 派发之前直接消费，规避 QQNT 等 Chromium 类宿主的加速键双处理。无候选时立即
// UnregisterHotKey 让宿主重获这些键。机制来自第三方输入法的实测验证。
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
        _hasThreadFocus = (fgPid == GetCurrentProcessId());
        WIND_LOG_DEBUG_FMT(L"_InitHotkeyWindow: initial thread focus seed=%d (fgPid=%u ownPid=%u)\n",
                           (int)_hasThreadFocus, fgPid, GetCurrentProcessId());
    }
    // 启动前台自检 timer：兜底 OnKillThreadFocus 不可靠的宿主（Wails/Chromium 等）
    SetTimer(_hHotkeyWnd, kFocusCheckTimerId, kFocusCheckIntervalMs, nullptr);
    return TRUE;
}

void CTextService::_UninitHotkeyWindow()
{
    if (_hotkeysActive)
    {
        _UnregisterCandidateHotkeys();
    }
    if (_hHotkeyWnd != nullptr)
    {
        DestroyWindow(_hHotkeyWnd);
        _hHotkeyWnd = nullptr;
    }
    if (_hotkeyWndClass != 0)
    {
        UnregisterClassW(kHotkeyWndClassName, g_hInstance);
        _hotkeyWndClass = 0;
    }
}

void CTextService::_RegisterCandidateHotkeys()
{
    if (_hHotkeyWnd == nullptr || _hotkeysActive) return;
    // 没拿到 thread focus 时绝不注册 — 多进程 IME 实例竞争同一组热键会引发
    // ERROR_HOTKEY_ALREADY_REGISTERED (1409)，让前台应用 IME 实例反而注册不上。
    if (!_hasThreadFocus) return;

    int registered = 0;
    // Ctrl+0..9 (Pin)
    for (int n = 0; n <= 9; ++n)
    {
        if (RegisterHotKey(_hHotkeyWnd, kHotkeyIdPinBase + n, MOD_CONTROL | MOD_NOREPEAT, '0' + n))
        {
            registered++;
        }
    }
    // Ctrl+Shift+0..9 (Delete)
    for (int n = 0; n <= 9; ++n)
    {
        if (RegisterHotKey(_hHotkeyWnd, kHotkeyIdDelBase + n, MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT, '0' + n))
        {
            registered++;
        }
    }
    _hotkeysActive = TRUE;
    WIND_LOG_DEBUG_FMT(L"RegisterCandidateHotkeys: registered=%d/20\n", registered);
}

void CTextService::_UnregisterCandidateHotkeys()
{
    if (_hHotkeyWnd == nullptr || !_hotkeysActive) return;

    for (int n = 0; n <= 9; ++n)
    {
        UnregisterHotKey(_hHotkeyWnd, kHotkeyIdPinBase + n);
        UnregisterHotKey(_hHotkeyWnd, kHotkeyIdDelBase + n);
    }
    _hotkeysActive = FALSE;
    WIND_LOG_DEBUG(L"UnregisterCandidateHotkeys\n");
}

LRESULT CALLBACK CTextService::_HotkeyWndProc(HWND hWnd, UINT msg, WPARAM wParam, LPARAM lParam)
{
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
                self->_hasThreadFocus = TRUE;
                // 候选可见性热键由 NotifyCandidatesVisibilityChanged 驱动；
                // 候选下一次出现时自然会重新注册。
                WIND_LOG_DEBUG(L"Received hotkey retry signal, marked thread focus\n");
            }
        }
        return 0;
    }
    // 定时自检：兜底 OnKillThreadFocus 不可靠的宿主。每 500ms 跑一次，
    // 如果发现本进程不再前台但仍持有热键，主动释放并通知前台进程重试。
    if (msg == WM_TIMER && wParam == kFocusCheckTimerId)
    {
        CTextService* self = reinterpret_cast<CTextService*>(GetWindowLongPtrW(hWnd, GWLP_USERDATA));
        if (self != nullptr)
        {
            HWND hFg = GetForegroundWindow();
            DWORD fgPid = 0;
            if (hFg != nullptr) GetWindowThreadProcessId(hFg, &fgPid);
            BOOL nowForeground = (fgPid == GetCurrentProcessId());
            BOOL holdsAnyHotkey = self->_hotkeysActive;
            if (!nowForeground && holdsAnyHotkey)
            {
                WIND_LOG_DEBUG_FMT(L"FocusCheck timer: not foreground (fgPid=%u ownPid=%u), releasing\n",
                                   fgPid, GetCurrentProcessId());
                self->_hasThreadFocus = FALSE;
                if (self->_hotkeysActive) self->_UnregisterCandidateHotkeys();
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
            else if (nowForeground && !self->_hasThreadFocus)
            {
                // 反向：成为前台但 _hasThreadFocus 还没更新（OnSetThreadFocus 也可能漏），
                // 标记一下；候选可见性热键由 NotifyCandidatesVisibilityChanged 驱动。
                self->_hasThreadFocus = TRUE;
            }
        }
        return 0;
    }
    if (msg == WM_HOTKEY)
    {
        CTextService* self = reinterpret_cast<CTextService*>(GetWindowLongPtrW(hWnd, GWLP_USERDATA));
        if (self != nullptr && self->_pKeyEventSink != nullptr)
        {
            // 焦点切换竞态防护：用户按下 Ctrl+= 的瞬间焦点可能正从 App A 切到 App B，
            // 此时 A 的 OnKillThreadFocus 还没来得及 unregister，WM_HOTKEY 会落到
            // A 的 hidden window；如果直接处理，会把加词动作下发到已经失焦的 IME
            // 实例（错的 PID / 错的光标位置）。
            // 实时复核当前前台进程是否是自己，不是就主动 unregister 让出热键，
            // 这一次按键丢弃（用户下一次按时新前台已经注册好）。
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
                self->_hasThreadFocus = FALSE;
                if (self->_hotkeysActive) self->_UnregisterCandidateHotkeys();
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
            if (id >= kHotkeyIdPinBase && id < kHotkeyIdPinBase + 10)
            {
                vk = '0' + (id - kHotkeyIdPinBase);
                mods = KEYMOD_CTRL;
            }
            else if (id >= kHotkeyIdDelBase && id < kHotkeyIdDelBase + 10)
            {
                vk = '0' + (id - kHotkeyIdDelBase);
                mods = KEYMOD_CTRL | KEYMOD_SHIFT;
            }
            if (vk != 0)
            {
                WIND_LOG_DEBUG_FMT(L"WM_HOTKEY id=0x%04X vk=0x%02X mods=0x%04X\n", id, vk, mods);
                self->_pKeyEventSink->DispatchHotkey(vk, mods);
            }
        }
        return 0;
    }
    return DefWindowProcW(hWnd, msg, wParam, lParam);
}

// ============================================================================
// ITfFunctionProvider
// 通过 ITfSourceSingle::AdviseSingleSink 把自己注册为该 IME 的 Function Provider。
// 其它成熟 TSF IME 都这么做，让 Chromium / QQNT 识别为完整 IME。
// 当前 stub 实现：GetFunction 一律返回 E_NOINTERFACE，不提供任何具体函数。
// 仅"注册存在"本身就足以达到识别效果。
// 注意 ITfFunctionProvider::GetDescription 与 ITfUIElement::GetDescription 同签名，
// C++ 多继承合并为单一实现，复用 ITfUIElement 那一份即可。
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
// 当前阶段：用 stub 数据验证 ITfUIElementMgr::BeginUIElement 注册本身能否让
// Chromium / QQNT 走完整 IME-first 调度路径，规避 Ctrl+数字 被宿主同时处理。
// 候选数据由 Go-side UI 渲染，C++ 这里返回占位数据即可。
// ============================================================================

static const GUID kWindCandidateUIElementGuid =
    { 0xb3e54a91, 0x7c20, 0x4b6a, { 0xa1, 0x5e, 0x82, 0x09, 0x77, 0x55, 0x44, 0x33 } };

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
    _uiElementShown = bShow;
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
    *pdwFlags = TF_CLUIE_DOCUMENTMGR | TF_CLUIE_COUNT | TF_CLUIE_SELECTION
              | TF_CLUIE_STRING | TF_CLUIE_PAGEINDEX | TF_CLUIE_CURRENTPAGE;
    return S_OK;
}

STDAPI CTextService::GetDocumentMgr(ITfDocumentMgr** ppdim)
{
    if (ppdim == nullptr) return E_INVALIDARG;
    *ppdim = nullptr;
    if (_pThreadMgr)
    {
        _pThreadMgr->GetFocus(ppdim); // may set null when no focus; that's OK
    }
    return S_OK;
}

STDAPI CTextService::GetCount(UINT* puCount)
{
    if (puCount == nullptr) return E_INVALIDARG;
    *puCount = 1; // stub: 至少 1 个候选才能让 TSF 认为候选 UI "有意义"
    return S_OK;
}

STDAPI CTextService::GetSelection(UINT* puIndex)
{
    if (puIndex == nullptr) return E_INVALIDARG;
    *puIndex = 0;
    return S_OK;
}

STDAPI CTextService::GetString(UINT uIndex, BSTR* pstr)
{
    if (pstr == nullptr) return E_INVALIDARG;
    *pstr = SysAllocString(L"…"); // 占位
    return *pstr ? S_OK : E_OUTOFMEMORY;
}

STDAPI CTextService::GetPageIndex(UINT* pIndex, UINT uSize, UINT* puPageCnt)
{
    if (puPageCnt == nullptr) return E_INVALIDARG;
    *puPageCnt = 1;
    if (pIndex && uSize >= 1)
    {
        pIndex[0] = 0;
    }
    return S_OK;
}

STDAPI CTextService::SetPageIndex(UINT* pIndex, UINT uPageCnt)
{
    // no-op (read-only stub)
    return S_OK;
}

STDAPI CTextService::GetCurrentPage(UINT* puPage)
{
    if (puPage == nullptr) return E_INVALIDARG;
    *puPage = 0;
    return S_OK;
}

STDAPI CTextService::SetSelection(UINT nIndex)
{
    WIND_LOG_DEBUG_FMT(L"ITfCandidateListUIElementBehavior::SetSelection(%u)\n", nIndex);
    return S_OK; // no-op: TSF 不参与候选选择，Go 端处理
}

STDAPI CTextService::Finalize(void)
{
    WIND_LOG_DEBUG(L"ITfCandidateListUIElementBehavior::Finalize\n");
    return S_OK;
}

STDAPI CTextService::Abort(void)
{
    WIND_LOG_DEBUG(L"ITfCandidateListUIElementBehavior::Abort\n");
    return S_OK;
}

void CTextService::NotifyCandidatesVisibilityChanged(BOOL hasCandidates)
{
    // 候选可见 → 注册系统级热键拦截 Ctrl+0..9/Ctrl+Shift+0..9；候选消失 → 卸载，
    // 让宿主重新获得这些键。这是第三方输入法使用的成熟机制，规避 Chromium 类宿主
    // 的加速键双处理。
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
        // 通过 Behavior 路径解决菱形继承
        HRESULT hr = _pUIElementMgr->BeginUIElement(
            static_cast<ITfUIElement*>(static_cast<ITfCandidateListUIElementBehavior*>(this)),
            &bShow, &_uiElementId);
        if (SUCCEEDED(hr))
        {
            _uiElementShown = bShow;
            WIND_LOG_DEBUG_FMT(L"BeginUIElement ok id=%u show=%d\n", _uiElementId, (int)bShow);
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
    }
    else if (hasCandidates && _uiElementId != (DWORD)-1)
    {
        // 已注册，仅触发 update
        _pUIElementMgr->UpdateUIElement(_uiElementId);
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

    _hasFocus = (pDocMgrFocus != nullptr);

    // If gaining focus (pDocMgrFocus is not null)
    if (pDocMgrFocus != nullptr)
    {
        _focusSessionId++;
        WIND_LOG_DEBUG_FMT(L"Focus gained focusSession=%llu", _focusSessionId);

        // Register ITfTextLayoutSink on the new context to receive
        // layout change notifications (for accurate candidate window positioning)
        ITfContext* pContext = nullptr;
        if (SUCCEEDED(pDocMgrFocus->GetTop(&pContext)) && pContext != nullptr)
        {
            _AdviseTextLayoutSink(pContext);
            _AdviseTextEditSink(pContext);
            pContext->Release();
        }

        WindHostProcessInfo currentHost;
        if (WindQueryCurrentProcessInfo(&currentHost))
            WindLogHostProcessInfo(4, L"compat.focus.current_host", currentHost);

        WindLogForegroundProcessInfo(4, L"compat.focus.foreground_host");

        // Force refresh the language bar button to ensure it's visible
        if (_pLangBarItemButton != nullptr)
        {
            _pLangBarItemButton->ForceRefresh();
        }

        // Reset composing state on focus gained to ensure clean state
        // This prevents stale composition state from affecting new input
        if (_pKeyEventSink != nullptr)
        {
            _pKeyEventSink->ResetComposingState();
        }

        // Detect whether the focused doc manager has a real editable context.
        // Use TSF context status flags (TF_SD_READONLY / TF_SS_TRANSITORY) rather than
        // GetTextExt: GetTextExt is a layout API and is not implemented by many frameworks
        // (JetBrains/Java Swing). Chrome marks its "no text field" context as TF_SD_READONLY,
        // which is the correct TSF-standard signal for "no writable text input".
        DWORD docMgrDynFlags = 0;
        _hasTextInputContext = _DocMgrHasEditableContext(pDocMgrFocus, &docMgrDynFlags);
        WIND_LOG_DEBUG_FMT(L"OnSetFocus: hasTextCtx=%d focusSession=%llu", _hasTextInputContext, _focusSessionId);

        // 读取焦点控件的 InputScope（密码框/邮箱/URL 等语义），随 focus_gained 上报给 Go 决策。
        // 仅对真正有可编辑上下文的文档查询，避免对无文本控件的 DocMgr 多跑一次同步读锁。
        UINT64 inputScopeMask = _hasTextInputContext ? _QueryInputScopeMask(pDocMgrFocus) : 0;

        // 密码框信号（Weasel/小狼毫做法）：宿主在 context 上置 GUID_COMPARTMENT_KEYBOARD_DISABLED
        // 表示"此控件禁用输入法"。Chromium 系浏览器密码框会置位，而无痕模式普通可编辑框不会，
        // 因此能精确区分密码框与隐私字段。置位则补 IS_PASSWORD 位让 Go 抑制中文。
        // InputScope 原始位（IS_PRIVATE/IS_SEARCH 等）仍随 mask 上报，留作 Go 端将来扩展判断。
        if (_hasTextInputContext && _IsFocusKeyboardDisabled(pDocMgrFocus))
            inputScopeMask |= kScopeBitPassword;

        // Get caret position for toolbar placement (separate concern from _hasTextInputContext)
        LONG caretX = 0, caretY = 0, caretHeight = 0;
        if (!GetCaretPosition(&caretX, &caretY, &caretHeight) && _hasLastKnownCaretPos)
        {
            caretX = _lastKnownCaretX;
            caretY = _lastKnownCaretY;
            caretHeight = _lastKnownCaretHeight;
            WIND_LOG_INFO_FMT(L"OnSetFocus: using last known caret position x=%ld y=%ld h=%ld", caretX, caretY, caretHeight);
        }
        WIND_LOG_DEBUG_FMT(
            L"compat.focus.caret focusSession=%llu x=%ld y=%ld height=%ld",
            _focusSessionId, caretX, caretY, caretHeight
        );
        _lastFocusCaretX = caretX;
        _lastFocusCaretY = caretY;
        _lastFocusCaretHeight = caretHeight > 0 ? caretHeight : DEFAULT_CARET_HEIGHT;

        // XamlIsland/transient locked DocMgr guard: dynFlags=0x20 consistently
        // marks Explorer's XamlIsland container DocMgrs where RequestEditSession
        // returns TF_E_NOLOCK. Sending focus_gained would cause Go to replay
        // composition into this unstable DocMgr; when the user then clicks away
        // the composition text is committed at screen position (0,0).
        // Skip focus_gained for these DocMgrs — the subsequent stable DocMgr
        // focus_gained will arrive and handle composition replay correctly.
        const DWORD kXamlIslandLockedFlag = 0x20;
        if (docMgrDynFlags & kXamlIslandLockedFlag)
        {
            WIND_LOG_INFO_FMT(
                L"OnSetFocus: skipping focus_gained for locked/transient DocMgr dynFlags=0x%X focusSession=%llu",
                docMgrDynFlags, _focusSessionId);
            // Fall through — sinks and LangBar are already set up above.
            // Do not send focus_gained IPC.
        }
        // No editable context (QQ Ctrl+1 切会话场景等)：新 DocMgr 没有任何可输入的
        // 文本控件 (_DocMgrHasEditableCtx -> 0)。发 focus_gained 会让 Go 把上一次
        // composition 状态 replay 回来 (UpdateComposition with residual buffer)，
        // 而 QQ 这边根本没地方接，结果是 IME 候选框残留、Go 内部 buffer 滞留。
        // 显式发 focus_lost 让 Go 强制清空 (clearState + hideUI)。
        else if (!_hasTextInputContext)
        {
            WIND_LOG_INFO_FMT(
                L"OnSetFocus: new DocMgr has no editable context, sending focus_lost focusSession=%llu",
                _focusSessionId);
            if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
            {
                _pIPCClient->SendFocusLost();
            }
            _needsFocusRecovery = FALSE;
        }
        // 异步化：SendFocusGained 是 fire-and-forget。Go 端立即回 Ack 解除本调用阻塞,
        // HandleFocusGained 完成后通过 push pipe 推 CMD_ACTIVATION_STATUS_PUSH,
        // AsyncReader → WM_ACTIVATION_STATUS → ApplyActivationStatusResponse 完成同步。
        // Lazy connect: 服务在 TSF 加载之后才启动也能覆盖（SendFocusGained 内部已处理）。
        else if (_pIPCClient != nullptr)
        {
            if (_pIPCClient->SendFocusGained(caretX, caretY, caretHeight, inputScopeMask))
            {
                WIND_LOG_DEBUG_FMT(L"FocusGained sent (async) focusSession=%llu", _focusSessionId);
                _needsFocusRecovery = FALSE;
                _pIPCClient->ClearNeedsSyncFlag();
            }
            else
            {
                WIND_LOG_WARN_FMT(L"FocusGained IPC send failed focusSession=%llu", _focusSessionId);
                _needsFocusRecovery = TRUE;
            }
        }
    }

    // If losing focus (pDocMgrFocus is null)
    if (pDocMgrFocus == nullptr)
    {
        WIND_LOG_DEBUG_FMT(L"Focus lost focusSession=%llu, notifying service", _focusSessionId);

        if (_pKeyEventSink != nullptr)
        {
            _pKeyEventSink->FlushEnglishStats();
        }

        // End any active composition before sending focus_lost.
        // 传入 pDocMgrPrevFocus：此刻 GetFocus()=null，必须靠它兜底跑 EditSession，
        // 否则 forced cleanup 会让 composition 残留文本被提交（Excel/WPS 表格的 'd' 漏字）。
        EndComposition(pDocMgrPrevFocus);

        // Send focus_lost to service (async, no response expected)
        if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
        {
            _pIPCClient->SendFocusLost();
        }

        // Reset composing state
        if (_pKeyEventSink != nullptr)
        {
            _pKeyEventSink->ResetComposingState();
        }

        // Unregister layout sink when losing focus
        _UnadviseTextLayoutSink();
        _UnadviseTextEditSink();

        // 失焦时**不**销毁 HostWindow（见 OnKillThreadFocus 注释）：locked/transient
        // DocMgr 会跳过 focus_gained，销毁后无法重建。靠 Go 的 WriteHide 隐藏即可。

        _needsFocusRecovery = FALSE;
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

    _bChineseMode = response.IsChineseMode();
    _bFullWidth = response.IsFullWidth();

    // Keep compartment always OPEN so TSF calls OnTestKeyDown even in English mode.
    _SetOpenCloseCompartment(TRUE);
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
    if (_pHostWindow != nullptr)
    {
        _pHostWindow->Uninitialize();
        delete _pHostWindow;
        _pHostWindow = nullptr;
    }
}

void CTextService::_EnsureHostRenderSetup(const ServiceResponse& response, BOOL forceRefresh)
{
    if (_pIPCClient == nullptr || !_pIPCClient->IsConnected())
        return;

    BOOL hadHostWindow = (_pHostWindow != nullptr);
    BOOL hostRenderAvailable = response.IsHostRenderAvailable();
    BOOL shouldRetryExistingHost = forceRefresh && hadHostWindow && !hostRenderAvailable;

    if (!hostRenderAvailable && !shouldRetryExistingHost)
    {
        if (forceRefresh && _pHostWindow != nullptr)
        {
            WIND_LOG_INFO(L"Host render unavailable after refresh, disabling existing host window\n");
            _DestroyHostWindow();
        }
        return;
    }

    if (shouldRetryExistingHost)
    {
        WIND_LOG_WARN(L"Host render flag missing after reconnect, retrying setup because host window was previously active\n");
    }

    if (_pHostWindow != nullptr && !forceRefresh)
    {
        // Check if the host's band has changed (e.g., user switched from
        // Start Menu search band=6 to taskbar search band=13).
        // UpdateBand recreates only the display window, not the shared memory.
        DWORD currentHostBand = _pHostWindow->GetHostBand();
        if (currentHostBand > 1 && currentHostBand != _pHostWindow->GetCurrentBand())
        {
            _pHostWindow->UpdateBand(currentHostBand);
        }
        return;
    }

    if (_pHostWindow != nullptr)
    {
        WIND_LOG_INFO(L"Refreshing host render window after service reconnection\n");
        _DestroyHostWindow();
    }

    WIND_LOG_INFO(L"Host render available, requesting setup\n");

    ServiceResponse hrResponse;
    if (_pIPCClient->SendHostRenderRequest(hrResponse) &&
        hrResponse.type == ResponseType::HostRenderSetup &&
        !hrResponse.shmName.empty() && !hrResponse.eventName.empty())
    {
        _pHostWindow = new CHostWindow();
        if (!_pHostWindow->Initialize(
            hrResponse.shmName.c_str(),
            hrResponse.eventName.c_str(),
            hrResponse.maxBufferSize,
            _pIPCClient)) // weak ref: host window routes mouse events back to Go
        {
            WIND_LOG_WARN(L"Host window initialization failed, falling back to Go window\n");
            delete _pHostWindow;
            _pHostWindow = nullptr;
        }
        else
        {
            WIND_LOG_INFO(L"Host window initialized successfully\n");
        }
    }
    else
    {
        WIND_LOG_WARN(L"Host render setup request failed, falling back to Go window\n");
    }
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

    // Set guard to prevent re-entrant OnChange
    _bInCompartmentChange = TRUE;

    VARIANT var;
    var.vt = VT_I4;
    var.lVal = bOpen ? TRUE : FALSE;
    hr = pCompartment->SetValue(_tfClientId, &var);
    pCompartment->Release();

    _bInCompartmentChange = FALSE;

    return SUCCEEDED(hr);
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

        // 与 OPENCLOSE 路径一致：清状态、通知 Go 服务、刷新 LangBar。
        if (_pKeyEventSink != nullptr)
            _pKeyEventSink->FlushEnglishStats();

        EndComposition();
        ResetComposingState();

        BOOL newChineseMode = bWantChinese;
        if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
        {
            ServiceResponse response;
            if (_pIPCClient->SendSystemModeSwitch(newChineseMode != FALSE, response))
            {
                if (response.type == ResponseType::CommitText && !response.text.empty())
                    CommitText(response.text);
                if (response.type == ResponseType::StatusUpdate || response.type == ResponseType::CommitText)
                    newChineseMode = response.IsChineseMode() ? TRUE : FALSE;
            }
        }

        _bChineseMode = newChineseMode;
    
        if (_pLangBarItemButton != nullptr)
            _pLangBarItemButton->UpdateLangBarButton(_bChineseMode);

        // 若 Go 服务把模式仲裁成了与外部请求不同的值，回写 compartment 保持一致。
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

    // The system alternates compartment between 0 and 1 each Ctrl+Space press.
    // We always re-open to 1 after handling, but _SetOpenCloseCompartment may fail
    // inside OnChange (TSF reentrancy), leaving compartment at 0. Either way the
    // system fires bOpen=TRUE on the next press and bOpen=FALSE on the one after.
    // We must handle BOTH directions as mode toggle events.
    //
    // Guard: _bInCompartmentChange is set TRUE before we call _SetOpenCloseCompartment(TRUE),
    // preventing a synchronous re-entrant OnChange from causing a spurious extra toggle.

    BOOL newChineseMode = !_bChineseMode;

    WIND_LOG_INFO_FMT(L"Compartment %s: toggling mode %s -> %s\n",
        bOpen ? L"opened" : L"closed",
        _bChineseMode ? L"Chinese" : L"English",
        newChineseMode ? L"Chinese" : L"English");

    // Flush English stats before any mode switch
    if (_pKeyEventSink != nullptr)
        _pKeyEventSink->FlushEnglishStats();

    // End any active composition since we're switching modes
    EndComposition();
    ResetComposingState();

    // Notify Go service of the mode switch (sync: may return CommitText for pending input)
    if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
    {
        ServiceResponse response;
        if (_pIPCClient->SendSystemModeSwitch(newChineseMode != FALSE, response))
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

    _bChineseMode = newChineseMode;

    if (_pLangBarItemButton != nullptr)
        _pLangBarItemButton->UpdateLangBarButton(_bChineseMode);

    // Re-open compartment so TSF keeps calling OnTestKeyDown for English stats/auto-pair.
    // Set the re-entrance guard so that if SetValue fires synchronously, the resulting
    // OnChange(bOpen=TRUE) is suppressed and doesn't trigger a spurious extra toggle.
    _bInCompartmentChange = TRUE;
    _SetOpenCloseCompartment(TRUE);
    _bInCompartmentChange = FALSE;

    // 同步真实中英文模式到 INPUTMODE_CONVERSION（KBLSwitch / 任务栏读取此 compartment）
    _SetConversionMode(_bChineseMode);

    WIND_LOG_INFO_FMT(L"Mode toggled via system compartment -> %s (compartment kept open)\n",
        _bChineseMode ? L"Chinese" : L"English");

    return S_OK;
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
// 此 compartment 是 Windows 标准的「中/英文模式」对外通信通道：
//   - IME_CMODE_NATIVE 位置 1：当前为本地（中文）输入
//   - IME_CMODE_NATIVE 位置 0：当前为字母（英文）输入
// 第三方工具（KBLSwitch 等按应用锁中英文）与 Win11 任务栏语言指示器都
// 读写此 compartment。OPENCLOSE 在内部约定下始终为 TRUE，不应承担模式信号。
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

void CTextService::_DoFullStateSync()
{
    if (_pIPCClient == nullptr)
        return;

    // Lazy connect: push pipe may reconnect before main pipe is established (service restart).
    if (!_pIPCClient->IsConnected() && !_pIPCClient->Connect())
    {
        WIND_LOG_WARN(L"_DoFullStateSync: main pipe not connected, skipping\n");
        return;
    }

    // 异步化（Go 端 server.go::handleClient 走"先 Ack 后处理"）：本调用仅发 fire-and-forget
    // CMD_IME_ACTIVATED。Go 端收到立即回 Ack 解除本同步调用，HandleIMEActivated 在 Go
    // 端的 handler goroutine 中完成后通过 push pipe 推 CMD_ACTIVATION_STATUS_PUSH，
    // AsyncReader 线程上的回调 PostMessage 到 TSF 线程触发 ApplyActivationStatusResponse,
    // 完成 _SyncStateFromResponse + _EnsureHostRenderSetup 的全套状态同步动作。
    //
    // 这条路径消除了原同步 ReceiveResponse 在宿主 UI 线程上的 1500ms 阻塞窗口——
    // explorer.exe 等 shell 宿主进程不再因任何 Go 端 handler 内的跨进程调用形成环形等待。
    WIND_LOG_INFO(L"Sending IMEActivated (async); state will arrive via CMD_ACTIVATION_STATUS_PUSH\n");

    if (!_pIPCClient->SendIMEActivated())
    {
        // Stale pipe: write failed and Disconnect() was called; retry after fresh connect.
        if (_pIPCClient->Connect())
        {
            _pIPCClient->SendIMEActivated();
        }
        else
        {
            WIND_LOG_WARN(L"_DoFullStateSync: SendIMEActivated failed, toolbar may not show until next focus event\n");
        }
    }

    // 注：清除 _needsStateSync / _needsFocusRecovery 提前到此处，让后续 KeyEventSink 路径
    // 不再触发重复 state sync。即便 push 暂时未到，下一次焦点切换会重新拉起 activation 流程。
    _pIPCClient->ClearNeedsSyncFlag();
    _needsFocusRecovery = FALSE;
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

void CTextService::TryRecoverFocusState()
{
    if (!_needsFocusRecovery || _pIPCClient == nullptr || !_pIPCClient->IsConnected())
        return;

    LONG caretX = _lastFocusCaretX;
    LONG caretY = _lastFocusCaretY;
    LONG caretHeight = _lastFocusCaretHeight > 0 ? _lastFocusCaretHeight : DEFAULT_CARET_HEIGHT;

    if (GetCaretPosition(&caretX, &caretY, &caretHeight))
    {
        _lastFocusCaretX = caretX;
        _lastFocusCaretY = caretY;
        _lastFocusCaretHeight = caretHeight > 0 ? caretHeight : DEFAULT_CARET_HEIGHT;
    }
    else if (_hasLastKnownCaretPos)
    {
        caretX = _lastKnownCaretX;
        caretY = _lastKnownCaretY;
        caretHeight = _lastKnownCaretHeight;
        WIND_LOG_INFO_FMT(L"Recovering focus state with last known caret x=%ld y=%ld h=%ld", caretX, caretY, caretHeight);
    }

    WIND_LOG_INFO_FMT(L"Attempting deferred focus recovery focusSession=%llu x=%ld y=%ld h=%ld",
        _focusSessionId, caretX, caretY, caretHeight);

    // 异步化：SendFocusGained 现在是 fire-and-forget。状态由 push pipe 经
    // CMD_ACTIVATION_STATUS_PUSH 异步送达，AsyncReader 线程的回调走 PostMessage
    // 到 TSF 线程的 WM_ACTIVATION_STATUS, 最终触发 ApplyActivationStatusResponse。
    if (_pIPCClient->SendFocusGained((int)caretX, (int)caretY, (int)caretHeight))
    {
        _needsFocusRecovery = FALSE;
        _pIPCClient->ClearNeedsSyncFlag();
        SendCaretPositionUpdate();
        WIND_LOG_INFO(L"Deferred focus recovery sent (async), state will arrive via push\n");
    }
    else
    {
        WIND_LOG_WARN_FMT(L"Deferred focus recovery send failed focusSession=%llu", _focusSessionId);
        _needsFocusRecovery = FALSE;
    }
}

BOOL CTextService::_InitIPCClient()
{
    _pIPCClient = new CIPCClient();
    if (_pIPCClient == nullptr)
        return FALSE;

    // Try to connect to Go Service (failure is OK, will retry later)
    if (!_pIPCClient->Connect())
    {
        WIND_LOG_WARN(L"Failed to connect to Go Service, will retry later\n");
    }

    // Set up activation status push callback (CMD_ACTIVATION_STATUS_PUSH)
    // 触发链：Go HandleIMEActivated / HandleFocusGained 异步完成 → push pipe → 本回调。
    // 必须 Post 到 TSF 线程：_SyncStateFromResponse / _EnsureHostRenderSetup 会触碰 TSF
    // COM 对象（compartment、LangBar 等），它们都是 STA-bound。
    CTextService* pThis = this;
    _pIPCClient->SetActivationPushCallback([pThis](const ServiceResponse& response) {
        if (pThis->_pLangBarItemButton != nullptr)
        {
            pThis->_pLangBarItemButton->PostActivationStatus(response);
        }
    });

    // Set up state push callback
    _pIPCClient->SetStatePushCallback([pThis](const ServiceResponse& response) {
        // This callback is called from the async reader thread
        // We need to update our state and notify the language bar
        WIND_LOG_INFO_FMT(L"State push received: mode=%d, fullWidth=%d, punct=%d, caps=%d\n",
                     response.IsChineseMode(), response.IsFullWidth(),
                     response.IsChinesePunct(), response.IsCapsLock());

        // Update internal state (atomic operation, thread-safe)
        pThis->_bChineseMode = response.IsChineseMode();
        pThis->_bFullWidth = response.IsFullWidth();

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

    // Set up clear composition callback for mode toggle via menu
    _pIPCClient->SetClearCompositionCallback([pThis]() {
        // This callback is called from the async reader thread
        WIND_LOG_DEBUG(L"Clear composition received from Go service\n");

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
        WIND_LOG_DEBUG_FMT(L"Update composition received from Go service, textLen=%zu, caret=%d\n",
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

    // Start async reader thread for receiving state pushes from Go
    if (!_pIPCClient->StartAsyncReader())
    {
        WIND_LOG_WARN(L"Failed to start async reader thread (non-fatal)\n");
        // Non-fatal - we can still use sync IPC
    }
    else
    {
        WIND_LOG_INFO(L"Async reader thread started for state push\n");
    }

    // Service-ready callback: Go sends CMD_SERVICE_READY when push pipe connects.
    // Route through LangBarItemButton's proven message window (same TSF thread,
    // known-working cross-thread channel used by PostUpdateFullStatus et al.).
    _pIPCClient->SetServiceReadyCallback([pThis]() {
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

// Get caret position using TSF APIs (for browsers and modern apps)
BOOL CTextService::GetCaretPositionFromTSF(LONG* px, LONG* py, LONG* pHeight)
{
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

    // Use EditSession to get caret position
    RECT rc = {};
    BOOL result = CCaretEditSession::GetCaretRect(pContext, _tfClientId, &rc);
    pContext->Release();

    if (result)
    {
        // rc contains screen coordinates
        *px = rc.left;
        *py = rc.bottom;  // Position below the caret
        *pHeight = rc.bottom - rc.top;

        // A zero-height rect (top == bottom) means GetTextExt returned a degenerate
        // result — common in apps like WPS when no TSF composition is active (e.g.,
        // non-inline-preedit mode). Return FALSE so the caller falls through to
        // GetGUIThreadInfo which tracks the Win32 caret independently of composition.
        if (*pHeight <= 0)
        {
            WIND_LOG_DEBUG(L"GetCaretPositionFromTSF: Degenerate rect (height=0), falling back\n");
            return FALSE;
        }

        // 坐标越界保护：TSF 坐标落在前台窗口外时判定不可信，返回 FALSE，让 GetCaretPosition
        // 回退到 GUIThreadInfo / GetCaretPos（窗口相对、可靠）。详见 IsScreenPointOutsideForegroundWindow。
        if (IsScreenPointOutsideForegroundWindow(rc.left, rc.top))
        {
            WIND_LOG_DEBUG_FMT(L"GetCaretPositionFromTSF: caret(%ld,%ld) outside fg window, falling back\n", rc.left, rc.top);
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
        if (SUCCEEDED(_pThreadMgr->GetFocus(&pDocMgr)) && pDocMgr != nullptr)
        {
            _hasTextInputContext = _DocMgrHasEditableContext(pDocMgr);
            pDocMgr->Release();
            if (_hasTextInputContext)
                WIND_LOG_DEBUG_FMT(L"RefreshTextInputContext: late editable context focusSession=%llu", _focusSessionId);
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

BOOL CTextService::_DocMgrHasEditableContext(ITfDocumentMgr* pDocMgr, DWORD* pDynFlagsOut)
{
    if (pDynFlagsOut)
        *pDynFlagsOut = 0;

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
    BOOL result = TRUE;
    HRESULT hrStatus = pCtx->GetStatus(&status);
    if (SUCCEEDED(hrStatus))
    {
        // Only TF_SD_READONLY (bit 0 of dwDynamicFlags) reliably means "no writable text
        // input". Chrome dynamically sets/clears this bit when text fields gain/lose focus.
        // TF_SS_TRANSITORY (0x4 of dwStaticFlags) is NOT a reliable signal — Chrome and
        // JetBrains both set it on contexts that do have real text input.
        WIND_LOG_DEBUG_FMT(L"_DocMgrHasEditableCtx: dynFlags=0x%X statFlags=0x%X", status.dwDynamicFlags, status.dwStaticFlags);
        if (status.dwDynamicFlags & TF_SD_READONLY)
            result = FALSE;
        if (pDynFlagsOut)
            *pDynFlagsOut = status.dwDynamicFlags;
    }
    else
    {
        WIND_LOG_DEBUG_FMT(L"_DocMgrHasEditableCtx: GetStatus hr=0x%08X -> default TRUE", hrStatus);
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
            *pHeight = max(guiInfo.rcCaret.bottom - guiInfo.rcCaret.top, 16);

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

BOOL CTextService::GetCaretPosition(LONG* px, LONG* py, LONG* pHeight)
{
    // First, check if the foreground window is a console/terminal
    HWND hwndForeground = GetForegroundWindow();
    BOOL isConsole = IsConsoleWindow(hwndForeground);

    if (isConsole)
    {
        WIND_LOG_DEBUG(L"GetCaretPosition: Detected console window\n");
    }

    // Method 1: Try TSF APIs first - this is the most reliable for browsers and modern apps
    // ITfContextView::GetTextExt provides accurate caret position in Chrome, Edge, etc.
    if (GetCaretPositionFromTSF(px, py, pHeight))
    {
        return TRUE;
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
            return TRUE;
        }
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
                return TRUE;
            }

            // Otherwise, position near the center-left of the window
            // This is a fallback for browsers that don't report caret position
            *px = rc.left + 100;  // Some offset from left edge
            *py = rc.top + (rc.bottom - rc.top) / 2;  // Vertical center
            *pHeight = 20;

            WIND_LOG_DEBUG(L"GetCaretPosition: Using window position fallback\n");
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

// Convert logical coordinates to physical screen coordinates when the host process
// is not Per-Monitor DPI aware. DPI-unaware apps receive virtualized 96-DPI coordinates
// from Windows, but our Go service (wind_input.exe) is Per-Monitor DPI aware and works
// in physical pixels. This mismatch causes the candidate window to appear at the wrong
// position in legacy/old applications.
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

void CTextService::SendCaretPositionUpdate()
{
    // Weasel 模式：composition 刚创建后第一次调用，不立即发 IPC。
    // 应用尚未完成 layout reflow，GetTextExt 此时返回的可能是旧坐标
    // （WPS 中 h>0 但坐标陈旧），先发会导致候选窗显示在错误位置然后跳到正确位置。
    // 改为等 OnLayoutChange 触发（reflow 完成的权威信号），50ms timer 兜底。
    if (_compositionJustStarted && _pComposition != nullptr)
    {
        if (_pLangBarItemButton != nullptr)
        {
            _pLangBarItemButton->PostDelayedCaretPositionUpdate();
        }
        // 通知 Go 端：composition 刚启动, 真正的 caret 会在 reflow 后到达。
        // Go 端据此延长 pendingFirstShow 超时, 避免回退到按键前的旧坐标。
        // 适用于 OnLayoutChange burst 跨度较长的应用 (如 EverEdit ~200ms 间隔)。
        if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
        {
            _pIPCClient->SendCaretPending();
        }
        return;
    }

    LONG x = 0, y = 0, height = 0;
    LONG compStartX = 0, compStartY = 0;
    BOOL hasPosition = FALSE;

    // Priority 1: Use cached position from edit session (reliable for WebView apps
    // where separate CaretEditSession with TF_INVALID_COOKIE is rejected).
    // The cache is set inside CUpdateCompositionEditSession::DoEditSession, which
    // guarantees that caret and composition-start come from the SAME edit session
    // and thus the same coordinate space.
    if (_hasCachedCaretPos)
    {
        x = _cachedCaretRect.left;
        y = _cachedCaretRect.bottom;
        height = _cachedCaretRect.bottom - _cachedCaretRect.top;
        hasPosition = TRUE;

        if (_hasCachedCompStartPos)
        {
            compStartX = _cachedCompStartRect.left;
            compStartY = _cachedCompStartRect.bottom;
        }

        // Clear cache (one-shot: next call falls back to normal methods)
        _hasCachedCaretPos = FALSE;
        _hasCachedCompStartPos = FALSE;
    }

    // Priority 2: Normal method (separate edit session + fallbacks)
    if (!hasPosition)
    {
        if (!GetCaretPosition(&x, &y, &height))
        {
            if (_hasLastKnownCaretPos)
            {
                x = _lastKnownCaretX;
                y = _lastKnownCaretY;
                height = _lastKnownCaretHeight;
                hasPosition = TRUE;
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

        if (_pComposition != nullptr)
        {
            GetCompositionStartPosition(&compStartX, &compStartY);
        }
    }

    // Convert logical coordinates to physical if host is not Per-Monitor DPI aware
    if (hasPosition)
    {
        ConvertToPhysicalCoordinates(x, y, height, compStartX, compStartY);
    }

    if (hasPosition)
    {
        _hasLastKnownCaretPos = TRUE;
        _lastKnownCaretX = x;
        _lastKnownCaretY = y;
        _lastKnownCaretHeight = height > 0 ? height : DEFAULT_CARET_HEIGHT;
    }

    // SendCaretUpdate is async (fire-and-forget), no response expected
    if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
    {
        _pIPCClient->SendCaretUpdate((int)x, (int)y, (int)height, (int)compStartX, (int)compStartY);
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
        pContext, _tfClientId, _pComposition, &caretRect, &compStartRect, &hasCompStart);
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

void CTextService::HandleCtrlSpaceToggle()
{
    BOOL newChineseMode = !_bChineseMode;
    WIND_LOG_INFO_FMT(L"HandleCtrlSpaceToggle: %s -> %s\n",
        _bChineseMode ? L"Chinese" : L"English",
        newChineseMode ? L"Chinese" : L"English");

    // Flush English stats before mode switch
    if (_pKeyEventSink != nullptr)
        _pKeyEventSink->FlushEnglishStats();

    // End any active composition
    EndComposition();
    ResetComposingState();

    // Notify Go service of the mode switch
    if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
    {
        ServiceResponse response;
        if (_pIPCClient->SendSystemModeSwitch(newChineseMode != FALSE, response))
        {
            if (response.type == ResponseType::CommitText && !response.text.empty())
            {
                CommitText(response.text);
                WIND_LOG_INFO_FMT(L"HandleCtrlSpaceToggle: committed pending text (len=%zu)\n", response.text.size());
            }
            if (response.type == ResponseType::StatusUpdate || response.type == ResponseType::CommitText)
                newChineseMode = response.IsChineseMode() ? TRUE : FALSE;
        }
        else
        {
            WIND_LOG_WARN(L"HandleCtrlSpaceToggle: IPC failed, proceeding with local toggle\n");
        }
    }

    _bChineseMode = newChineseMode;

    if (_pLangBarItemButton != nullptr)
        _pLangBarItemButton->UpdateLangBarButton(_bChineseMode);

    // Do NOT call _SetOpenCloseCompartment here: compartment stays at 1 always,
    // so the system never gets a chance to desync its internal toggle state.
    // 但需要把真实模式写入 INPUTMODE_CONVERSION，让 KBLSwitch / 任务栏感知。
    _SetConversionMode(_bChineseMode);

    WIND_LOG_INFO_FMT(L"Mode toggled via Ctrl+Space interception -> %s (compartment unchanged)\n",
        _bChineseMode ? L"Chinese" : L"English");
}

void CTextService::ToggleInputMode()
{
    WIND_LOG_INFO(L"ToggleInputMode called (local fallback)\n");

    if (!_bChineseMode && _pKeyEventSink != nullptr)
    {
        _pKeyEventSink->FlushEnglishStats();
    }

    // Toggle mode locally (this is used as a fallback when Go service is unavailable)
    // The actual mode toggle is handled via KeyUp event -> Go service -> StatusUpdate response
    EndComposition();
    _bChineseMode = !_bChineseMode;

    WIND_LOG_INFO_FMT(L"Switched to %s mode\n", _bChineseMode ? L"Chinese" : L"English");

    // Keep compartment always OPEN so TSF calls OnTestKeyDown even in English mode.
    // This allows English stats collection, auto-pair, and other features to work.
    // The actual key pass-through is handled by pfEaten=FALSE in OnTestKeyDown.
    _SetOpenCloseCompartment(TRUE);
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

    _bChineseMode = bChineseMode;

    WIND_LOG_INFO_FMT(L"Mode set to %s (from service)\n", _bChineseMode ? L"Chinese" : L"English");

    // Keep compartment always OPEN so TSF calls OnTestKeyDown even in English mode.
    _SetOpenCloseCompartment(TRUE);
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

    // Send async (fire-and-forget, Go side will show the menu)
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
    _bChineseMode = bChineseMode;
    _bFullWidth = bFullWidth;

    // Keep compartment always OPEN so TSF calls OnTestKeyDown even in English mode.
    _SetOpenCloseCompartment(TRUE);
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
    WIND_LOG_DEBUG(L"OnCompositionTerminated called\n");

    // Clear composition text cache
    _lastCompositionText.clear();
    _lastCaretPos = -1;

    // Only release if this is the same composition we're tracking
    // It may have already been released in DoEditSession
    if (_pComposition != nullptr && _pComposition == pComposition)
    {
        // CRITICAL: This is an unexpected termination (we didn't call EndComposition)
        // This can happen when:
        // 1. Fast typing: new composition starts before previous InsertText completes
        // 2. Application forcefully terminates composition
        //
        // We MUST clear the composition text to prevent it from leaking to the document
        // as plain text (which would cause the "d being committed directly" bug)
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
        // 首次 reflow 阶段（_compositionJustStarted）：WPS 等宿主会在 reflow 完成前
        // 连续触发多次 OnLayoutChange，前几次 GetTextExt 仍返回旧坐标。这里改用
        // debounce：每次 OnLayoutChange 都重置 timer，等事件 burst 结束后再 flush，
        // 此时 reflow 已稳定。timer 兜底也覆盖了完全不发 OnLayoutChange 的应用。
        if (_compositionJustStarted)
        {
            _hasCachedCaretPos = FALSE;
            _hasCachedCompStartPos = FALSE;
            if (_pLangBarItemButton != nullptr)
            {
                _pLangBarItemButton->PostDelayedCaretPositionUpdate();
            }
            WIND_LOG_DEBUG(L"OnLayoutChange (first show): debouncing caret flush\n");
            return S_OK;
        }
        WIND_LOG_DEBUG(L"OnLayoutChange: TF_LC_CHANGE with active composition, updating caret position\n");
        SendCaretPositionUpdate();
    }
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

    // When selection changes outside of composition (e.g., mouse click, arrow keys),
    // notify Go to reset smart punct state.
    // During composition, Go tracks state internally via key events.
    // NOTE: Do NOT call ClearPassthroughDigit() here. OnEndEdit fires for normal digit
    // insertion too (cursor moves after typing '1'), which would incorrectly clear the
    // digit tracking that OnTestKeyDown just set. Mouse click detection relies on
    // caret Y comparison in _SendKeyToService instead.
    if (selChanged && _pComposition == nullptr)
    {
        // Notify Go side to reset its smart punct state
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
BOOL CTextService::UpdateComposition(const std::wstring& text, int caretPos)
{
    WIND_LOG_DEBUG_FMT(L"UpdateComposition called, textLen=%zu, _pComposition=%p\n",
                 text.length(), _pComposition);

    // OPTIMIZATION: Skip if both composition text AND caret position are the same as last time
    // This avoids unnecessary TSF RequestEditSession calls which can be slow in some apps
    // Note: must compare caretPos too, otherwise cursor movement (left/right arrow) gets skipped
    if (text == _lastCompositionText && caretPos == _lastCaretPos && _pComposition != nullptr)
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

    ITfContext* pContext = nullptr;
    HRESULT hr = pDocMgr->GetTop(&pContext);
    pDocMgr->Release();

    if (FAILED(hr) || pContext == nullptr)
    {
        WIND_LOG_ERROR(L"UpdateComposition: Failed to get Context\n");
        return FALSE;
    }

    CUpdateCompositionEditSession* pEditSession = new CUpdateCompositionEditSession(this, pContext, text, caretPos);

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

    // Update cache on success
    if (SUCCEEDED(hr))
    {
        _lastCompositionText = text;
        _lastCaretPos = caretPos;
    }

    return SUCCEEDED(hr);
}

// Commit text atomically: end composition + insert text in a single EditSession.
// This avoids race conditions in browsers where async EndComposition could clear
// text that was inserted by a subsequent synchronous InsertText.
BOOL CTextService::CommitText(const std::wstring& text)
{
    LARGE_INTEGER startTime, endTime, freq;
    QueryPerformanceCounter(&startTime);
    QueryPerformanceFrequency(&freq);

    // Clear composition text cache
    _lastCompositionText.clear();
    _lastCaretPos = -1;

    // Transfer ownership of _pComposition to the EditSession
    ITfComposition* pCompToEnd = _pComposition;
    _pComposition = nullptr;

    if (text.empty() && pCompToEnd == nullptr)
    {
        WIND_LOG_DEBUG(L"CommitText: Nothing to do (no text, no composition)\n");
        return TRUE;
    }

    // Need a document manager to request edit session
    ITfDocumentMgr* pDocMgr = nullptr;
    if (_pThreadMgr == nullptr || FAILED(_pThreadMgr->GetFocus(&pDocMgr)) || pDocMgr == nullptr)
    {
        WIND_LOG_DEBUG(L"CommitText: Can't get DocMgr, falling back\n");
        if (pCompToEnd != nullptr) pCompToEnd->Release();
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
            goto fallback;
        }

        CCommitTextEditSession* pEditSession = new CCommitTextEditSession(this, pContext, pCompToEnd, text);
        // pCompToEnd ownership transferred to pEditSession

        HRESULT hrSession;
        hr = pContext->RequestEditSession(_tfClientId, pEditSession, TF_ES_SYNC | TF_ES_READWRITE, &hrSession);

        BOOL success = pEditSession->GetSuccess();
        pEditSession->Release();
        pContext->Release();

        QueryPerformanceCounter(&endTime);
        int durationMs = (int)((endTime.QuadPart - startTime.QuadPart) * 1000 / freq.QuadPart);

        if (SUCCEEDED(hr) && SUCCEEDED(hrSession) && success)
        {
            WIND_LOG_DEBUG_FMT(L"CommitText: TSF atomic commit succeeded, duration=%dms\n", durationMs);
            return TRUE;
        }

        WIND_LOG_DEBUG_FMT(L"CommitText: TSF method failed (hr=0x%08X, hrSession=0x%08X), falling back to SendInput, duration=%dms\n",
                     hr, hrSession, durationMs);
    }

fallback:
    // Fallback: Use SendInput (same as InsertText fallback path)
    if (text.empty())
    {
        return TRUE;
    }

    WIND_LOG_DEBUG_FMT(L"CommitText: Using SendInput fallback for textLen=%zu\n", text.length());

    std::vector<INPUT> inputs;
    inputs.reserve(text.length() * 2);

    for (wchar_t ch : text)
    {
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

// End composition
// NOTE: This method is now ASYNC - it returns immediately without waiting for
// the composition to actually end. The _pComposition pointer is cleared immediately
// so that HasActiveComposition() returns FALSE and new compositions can start.
void CTextService::EndComposition(ITfDocumentMgr* pDocMgrHint)
{
    LARGE_INTEGER startTime, endTime, freq;
    QueryPerformanceCounter(&startTime);
    QueryPerformanceFrequency(&freq);

    // Clear composition text cache
    _lastCompositionText.clear();
    _lastCaretPos = -1;

    // If there's no active composition, nothing to do
    if (_pComposition == nullptr)
    {
        WIND_LOG_DEBUG(L"EndComposition: No active composition\n");
        return;
    }

    WIND_LOG_DEBUG(L"EndComposition: Ending active composition\n");

    // CRITICAL: Transfer ownership of _pComposition immediately
    // This allows new compositions to start while the old one is being ended async
    ITfComposition* pCompToEnd = _pComposition;
    _pComposition = nullptr;  // Clear immediately - HasActiveComposition() now returns FALSE
    _compositionJustStarted = FALSE;

    // Need a document manager to request edit session.
    // 优先使用 GetFocus 拿当前焦点 doc；失败时退回 pDocMgrHint（来自 OnSetFocus 的
    // pDocMgrPrevFocus），保证失焦时仍能在旧 doc 上跑 EditSession 清空 composition。
    ITfDocumentMgr* pDocMgr = nullptr;
    if (_pThreadMgr == nullptr || FAILED(_pThreadMgr->GetFocus(&pDocMgr)) || pDocMgr == nullptr)
    {
        if (pDocMgrHint != nullptr)
        {
            WIND_LOG_DEBUG(L"EndComposition: GetFocus null, using pDocMgrHint\n");
            pDocMgr = pDocMgrHint;
            pDocMgr->AddRef();
        }
        else
        {
            // Can't get document manager, force cleanup
            WIND_LOG_DEBUG(L"EndComposition: Can't get DocMgr, forcing cleanup\n");
            pCompToEnd->Release();
            return;
        }
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

void CTextService::ResetComposingState()
{
    if (_pKeyEventSink != nullptr)
    {
        _pKeyEventSink->ResetComposingState();
    }
}

// Insert text and start new composition (for top code commit)
BOOL CTextService::InsertTextAndStartComposition(const std::wstring& insertText, const std::wstring& newComposition)
{
    WIND_LOG_DEBUG_FMT(L"InsertTextAndStartComposition: insert='%s', newComp='%s', _pComposition=%p\n",
                 insertText.c_str(), newComposition.c_str(), _pComposition);

    // Clear composition text cache
    _lastCompositionText.clear();
    _lastCaretPos = -1;

    // Transfer ownership of old composition to EditSession (will be ended atomically inside DoEditSession)
    ITfComposition* pOldComp = _pComposition;
    _pComposition = nullptr;

    // Need a document manager
    ITfDocumentMgr* pDocMgr = nullptr;
    if (_pThreadMgr == nullptr || FAILED(_pThreadMgr->GetFocus(&pDocMgr)) || pDocMgr == nullptr)
    {
        WIND_LOG_ERROR(L"InsertTextAndStartComposition: Failed to get DocMgr\n");
        if (pOldComp != nullptr) pOldComp->Release();
        return FALSE;
    }

    ITfContext* pContext = nullptr;
    HRESULT hr = pDocMgr->GetTop(&pContext);
    pDocMgr->Release();

    if (FAILED(hr) || pContext == nullptr)
    {
        WIND_LOG_ERROR(L"InsertTextAndStartComposition: Failed to get Context\n");
        if (pOldComp != nullptr) pOldComp->Release();
        return FALSE;
    }

    CInsertAndComposeEditSession* pEditSession = new CInsertAndComposeEditSession(this, pContext, pOldComp, insertText, newComposition);

    HRESULT hrSession;
    // Use TF_ES_SYNC to ensure synchronous execution
    hr = pContext->RequestEditSession(_tfClientId, pEditSession, TF_ES_SYNC | TF_ES_READWRITE, &hrSession);

    WIND_LOG_DEBUG_FMT(L"InsertTextAndStartComposition: RequestEditSession hr=0x%08X, hrSession=0x%08X\n", hr, hrSession);

    pEditSession->Release();
    pContext->Release();

    return SUCCEEDED(hr) && SUCCEEDED(hrSession);
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
