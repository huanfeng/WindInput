#include "LangBarItemButton.h"
#include "TextService.h"
#include "Globals.h"
#include <olectl.h>  // For CONNECT_E_* constants

// GUID_LBI_INPUTMODE - 用于在 Windows 10/11 输入指示器显示模式图标
// {2C77A81E-41CC-4178-A3A7-5F8A987568E1}
DEFINE_GUID(GUID_LBI_INPUTMODE,
    0x2C77A81E, 0x41CC, 0x4178, 0xA3, 0xA7, 0x5F, 0x8A, 0x98, 0x75, 0x68, 0xE1);

// 使用 GUID_LBI_INPUTMODE 使图标显示在 Windows 11 输入指示器中
const GUID CLangBarItemButton::_guidLangBarItemButton = GUID_LBI_INPUTMODE;

CLangBarItemButton::CLangBarItemButton(CTextService* pTextService)
    : _refCount(1)
    , _pTextService(pTextService)
    , _pLangBarItemSink(nullptr)
    , _dwCookie(0)
    , _bChineseMode(TRUE)
    , _bCapsLock(FALSE)
{
    // Initialize Caps Lock state
    _bCapsLock = (GetKeyState(VK_CAPITAL) & 0x0001) != 0;
    DllAddRef();
}

CLangBarItemButton::~CLangBarItemButton()
{
    DllRelease();
}

STDAPI CLangBarItemButton::QueryInterface(REFIID riid, void** ppvObj)
{
    if (ppvObj == nullptr)
        return E_INVALIDARG;

    *ppvObj = nullptr;

    if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITfLangBarItem) || IsEqualIID(riid, IID_ITfLangBarItemButton))
    {
        *ppvObj = (ITfLangBarItemButton*)this;
    }
    else if (IsEqualIID(riid, IID_ITfSource))
    {
        *ppvObj = (ITfSource*)this;
    }

    if (*ppvObj)
    {
        AddRef();
        return S_OK;
    }

    return E_NOINTERFACE;
}

STDAPI_(ULONG) CLangBarItemButton::AddRef()
{
    return InterlockedIncrement(&_refCount);
}

STDAPI_(ULONG) CLangBarItemButton::Release()
{
    LONG cr = InterlockedDecrement(&_refCount);
    if (cr == 0)
    {
        delete this;
    }
    return cr;
}

STDAPI CLangBarItemButton::GetInfo(TF_LANGBARITEMINFO* pInfo)
{
    if (pInfo == nullptr)
        return E_INVALIDARG;

    pInfo->clsidService = c_clsidTextService;
    pInfo->guidItem = _guidLangBarItemButton;

    // TF_LBI_STYLE_BTN_BUTTON: 显示为可点击按钮
    // TF_LBI_STYLE_SHOWNINTRAY: 在系统托盘/输入指示器区域显示
    // TF_LBI_STYLE_TEXTCOLORICON: 图标颜色随主题变化
    pInfo->dwStyle = TF_LBI_STYLE_BTN_BUTTON |
                     TF_LBI_STYLE_SHOWNINTRAY |
                     TF_LBI_STYLE_TEXTCOLORICON;

    pInfo->ulSort = 0;  // 排序顺序 (0 = 最左边, 用于输入模式指示器)

    // 设置描述 - 显示为工具提示
    wcscpy_s(pInfo->szDescription, TEXTSERVICE_NAME);

    OutputDebugStringW(L"[WindInput] LangBarItemButton::GetInfo called\n");

    return S_OK;
}

STDAPI CLangBarItemButton::GetStatus(DWORD* pdwStatus)
{
    if (pdwStatus == nullptr)
        return E_INVALIDARG;

    *pdwStatus = 0;
    return S_OK;
}

STDAPI CLangBarItemButton::Show(BOOL fShow)
{
    return E_NOTIMPL;
}

STDAPI CLangBarItemButton::GetTooltipString(BSTR* pbstrToolTip)
{
    if (pbstrToolTip == nullptr)
        return E_INVALIDARG;

    if (_bChineseMode)
    {
        *pbstrToolTip = SysAllocString(L"WindInput - 中文模式");
    }
    else
    {
        if (_bCapsLock)
        {
            *pbstrToolTip = SysAllocString(L"WindInput - English Mode (Caps Lock ON)");
        }
        else
        {
            *pbstrToolTip = SysAllocString(L"WindInput - English Mode (Caps Lock OFF)");
        }
    }

    return (*pbstrToolTip != nullptr) ? S_OK : E_OUTOFMEMORY;
}

STDAPI CLangBarItemButton::OnClick(TfLBIClick click, POINT pt, const RECT* prcArea)
{
    // Toggle mode when clicked
    if (_pTextService != nullptr)
    {
        _pTextService->ToggleInputMode();
    }
    return S_OK;
}

STDAPI CLangBarItemButton::InitMenu(ITfMenu* pMenu)
{
    return E_NOTIMPL;
}

STDAPI CLangBarItemButton::OnMenuSelect(UINT wID)
{
    return E_NOTIMPL;
}

STDAPI CLangBarItemButton::GetIcon(HICON* phIcon)
{
    if (phIcon == nullptr)
        return E_INVALIDARG;

    *phIcon = nullptr;

    OutputDebugStringW(L"[WindInput] LangBarItemButton::GetIcon called\n");

    // Get DPI scaling
    HDC hdcScreen = GetDC(NULL);
    if (hdcScreen == NULL)
    {
        OutputDebugStringW(L"[WindInput] GetIcon: GetDC failed\n");
        return E_FAIL;
    }

    int dpi = GetDeviceCaps(hdcScreen, LOGPIXELSX);
    int iconSize = MulDiv(16, dpi, 96);  // Scale icon size based on DPI
    if (iconSize < 16) iconSize = 16;
    if (iconSize > 32) iconSize = 32;

    HDC hdcMem = CreateCompatibleDC(hdcScreen);
    if (hdcMem == NULL)
    {
        ReleaseDC(NULL, hdcScreen);
        OutputDebugStringW(L"[WindInput] GetIcon: CreateCompatibleDC failed\n");
        return E_FAIL;
    }

    // Create compatible bitmap (simpler, more reliable)
    HBITMAP hBitmap = CreateCompatibleBitmap(hdcScreen, iconSize, iconSize);
    if (hBitmap == NULL)
    {
        DeleteDC(hdcMem);
        ReleaseDC(NULL, hdcScreen);
        OutputDebugStringW(L"[WindInput] GetIcon: CreateCompatibleBitmap failed\n");
        return E_FAIL;
    }
    HBITMAP hOldBitmap = (HBITMAP)SelectObject(hdcMem, hBitmap);

    // Draw background
    RECT rc = { 0, 0, iconSize, iconSize };
    HBRUSH hBrush;
    if (_bChineseMode)
    {
        hBrush = CreateSolidBrush(RGB(66, 133, 244)); // Blue for Chinese
    }
    else
    {
        hBrush = CreateSolidBrush(RGB(128, 128, 128)); // Gray for English
    }
    FillRect(hdcMem, &rc, hBrush);
    DeleteObject(hBrush);

    // Draw text - use a font that supports Chinese
    SetBkMode(hdcMem, TRANSPARENT);
    SetTextColor(hdcMem, RGB(255, 255, 255));

    int fontSize = MulDiv(12, dpi, 96);
    HFONT hFont = CreateFontW(
        fontSize, 0, 0, 0, FW_BOLD,
        FALSE, FALSE, FALSE,
        GB2312_CHARSET,  // Chinese charset
        OUT_DEFAULT_PRECIS,
        CLIP_DEFAULT_PRECIS,
        DEFAULT_QUALITY,
        DEFAULT_PITCH | FF_DONTCARE,
        L"SimHei"  // SimHei supports Chinese
    );

    if (hFont == NULL)
    {
        // Fallback to system font
        hFont = (HFONT)GetStockObject(DEFAULT_GUI_FONT);
    }

    HFONT hOldFont = (HFONT)SelectObject(hdcMem, hFont);

    // Display text based on mode and Caps Lock state:
    // Chinese mode: "中"
    // English mode + Caps Lock ON: "A"
    // English mode + Caps Lock OFF: "a"
    const wchar_t* text;
    if (_bChineseMode)
    {
        text = L"中";
    }
    else
    {
        text = _bCapsLock ? L"A" : L"a";
    }
    DrawTextW(hdcMem, text, -1, &rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);

    SelectObject(hdcMem, hOldFont);
    if (hFont != GetStockObject(DEFAULT_GUI_FONT))
    {
        DeleteObject(hFont);
    }

    // Create monochrome mask bitmap (all black = all opaque)
    HDC hdcMask = CreateCompatibleDC(hdcScreen);
    HBITMAP hMaskBitmap = CreateCompatibleBitmap(hdcMask, iconSize, iconSize);
    if (hMaskBitmap == NULL)
    {
        // Fallback: create simple monochrome bitmap
        hMaskBitmap = CreateBitmap(iconSize, iconSize, 1, 1, NULL);
    }
    HBITMAP hOldMask = (HBITMAP)SelectObject(hdcMask, hMaskBitmap);

    // Fill mask with black (0 = opaque in mask)
    RECT rcMask = { 0, 0, iconSize, iconSize };
    FillRect(hdcMask, &rcMask, (HBRUSH)GetStockObject(BLACK_BRUSH));

    SelectObject(hdcMask, hOldMask);
    DeleteDC(hdcMask);

    SelectObject(hdcMem, hOldBitmap);
    DeleteDC(hdcMem);
    ReleaseDC(NULL, hdcScreen);

    // Create icon
    ICONINFO iconInfo = { 0 };
    iconInfo.fIcon = TRUE;
    iconInfo.hbmMask = hMaskBitmap;
    iconInfo.hbmColor = hBitmap;

    *phIcon = CreateIconIndirect(&iconInfo);

    DeleteObject(hBitmap);
    DeleteObject(hMaskBitmap);

    WCHAR debug[256];
    wsprintfW(debug, L"[WindInput] GetIcon: size=%d, mode=%s, icon=%p\n",
              iconSize, _bChineseMode ? L"Chinese" : L"English", *phIcon);
    OutputDebugStringW(debug);

    return (*phIcon != nullptr) ? S_OK : E_FAIL;
}

STDAPI CLangBarItemButton::GetText(BSTR* pbstrText)
{
    if (pbstrText == nullptr)
        return E_INVALIDARG;

    if (_bChineseMode)
    {
        *pbstrText = SysAllocString(L"中");
    }
    else
    {
        // English mode: show "A" or "a" based on Caps Lock state
        *pbstrText = SysAllocString(_bCapsLock ? L"A" : L"a");
    }

    return (*pbstrText != nullptr) ? S_OK : E_OUTOFMEMORY;
}

STDAPI CLangBarItemButton::AdviseSink(REFIID riid, IUnknown* punk, DWORD* pdwCookie)
{
    if (!IsEqualIID(riid, IID_ITfLangBarItemSink))
        return CONNECT_E_CANNOTCONNECT;

    if (_pLangBarItemSink != nullptr)
        return CONNECT_E_ADVISELIMIT;

    if (punk == nullptr || pdwCookie == nullptr)
        return E_INVALIDARG;

    if (FAILED(punk->QueryInterface(IID_ITfLangBarItemSink, (void**)&_pLangBarItemSink)))
        return E_NOINTERFACE;

    *pdwCookie = ++_dwCookie;
    return S_OK;
}

STDAPI CLangBarItemButton::UnadviseSink(DWORD dwCookie)
{
    if (dwCookie != _dwCookie || _pLangBarItemSink == nullptr)
        return CONNECT_E_NOCONNECTION;

    _pLangBarItemSink->Release();
    _pLangBarItemSink = nullptr;
    return S_OK;
}

BOOL CLangBarItemButton::Initialize()
{
    OutputDebugStringW(L"[WindInput] LangBarItemButton::Initialize\n");

    if (_pTextService == nullptr)
    {
        OutputDebugStringW(L"[WindInput] LangBarItemButton: _pTextService is null\n");
        return FALSE;
    }

    ITfThreadMgr* pThreadMgr = _pTextService->GetThreadMgr();
    if (pThreadMgr == nullptr)
    {
        OutputDebugStringW(L"[WindInput] LangBarItemButton: pThreadMgr is null\n");
        return FALSE;
    }

    ITfLangBarItemMgr* pLangBarItemMgr = nullptr;
    HRESULT hr = pThreadMgr->QueryInterface(IID_ITfLangBarItemMgr, (void**)&pLangBarItemMgr);
    if (FAILED(hr) || pLangBarItemMgr == nullptr)
    {
        WCHAR debug[256];
        wsprintfW(debug, L"[WindInput] Failed to get ITfLangBarItemMgr, hr=0x%08X\n", hr);
        OutputDebugStringW(debug);
        return FALSE;
    }

    hr = pLangBarItemMgr->AddItem(this);

    WCHAR debug[256];
    wsprintfW(debug, L"[WindInput] LangBarItemMgr->AddItem returned hr=0x%08X\n", hr);
    OutputDebugStringW(debug);

    pLangBarItemMgr->Release();

    if (FAILED(hr))
    {
        OutputDebugStringW(L"[WindInput] Failed to add LangBarItem\n");
        return FALSE;
    }

    OutputDebugStringW(L"[WindInput] LangBarItemButton initialized successfully\n");
    return TRUE;
}

void CLangBarItemButton::Uninitialize()
{
    OutputDebugStringW(L"[WindInput] LangBarItemButton::Uninitialize\n");

    if (_pTextService == nullptr)
        return;

    ITfThreadMgr* pThreadMgr = _pTextService->GetThreadMgr();
    if (pThreadMgr == nullptr)
        return;

    ITfLangBarItemMgr* pLangBarItemMgr = nullptr;
    if (SUCCEEDED(pThreadMgr->QueryInterface(IID_ITfLangBarItemMgr, (void**)&pLangBarItemMgr)))
    {
        pLangBarItemMgr->RemoveItem(this);
        pLangBarItemMgr->Release();
    }
}

void CLangBarItemButton::UpdateLangBarButton(BOOL bChineseMode)
{
    _bChineseMode = bChineseMode;

    // Notify sink that the button has changed
    if (_pLangBarItemSink != nullptr)
    {
        _pLangBarItemSink->OnUpdate(TF_LBI_ICON | TF_LBI_TEXT | TF_LBI_TOOLTIP);
    }
}

void CLangBarItemButton::UpdateCapsLockState(BOOL bCapsLock)
{
    if (_bCapsLock == bCapsLock)
        return;  // No change

    _bCapsLock = bCapsLock;

    // Only update if in English mode (Chinese mode doesn't show Caps Lock state)
    if (!_bChineseMode && _pLangBarItemSink != nullptr)
    {
        _pLangBarItemSink->OnUpdate(TF_LBI_ICON | TF_LBI_TEXT | TF_LBI_TOOLTIP);
    }
}

void CLangBarItemButton::UpdateState(BOOL bChineseMode, BOOL bCapsLock)
{
    BOOL needUpdate = (_bChineseMode != bChineseMode) ||
                      (!bChineseMode && _bCapsLock != bCapsLock);

    _bChineseMode = bChineseMode;
    _bCapsLock = bCapsLock;

    if (needUpdate && _pLangBarItemSink != nullptr)
    {
        _pLangBarItemSink->OnUpdate(TF_LBI_ICON | TF_LBI_TEXT | TF_LBI_TOOLTIP);
    }
}
