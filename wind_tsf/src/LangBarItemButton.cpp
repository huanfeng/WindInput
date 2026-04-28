#include "LangBarItemButton.h"
#include "TextService.h"
#include "IPCClient.h"
#include "Globals.h"
#include <olectl.h>  // For CONNECT_E_* constants
#include <dwrite.h>

#pragma comment(lib, "dwrite.lib")
#pragma comment(lib, "advapi32.lib")

// Detect if the system taskbar uses dark mode by reading the registry.
// Returns true if dark mode is active (SystemUsesLightTheme == 0).
static bool IsSystemDarkMode()
{
    DWORD value = 1; // default to light mode
    DWORD size = sizeof(value);
    RegGetValueW(
        HKEY_CURRENT_USER,
        L"Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize",
        L"SystemUsesLightTheme",
        RRF_RT_REG_DWORD,
        nullptr,
        &value,
        &size);
    return value == 0;
}

// DirectWrite factory (lazy-initialized, per-process lifetime)
static IDWriteFactory* g_pDWriteFactory = nullptr;

static bool EnsureDWriteFactory()
{
    if (!g_pDWriteFactory)
    {
        if (FAILED(DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED,
            __uuidof(IDWriteFactory),
            reinterpret_cast<IUnknown**>(&g_pDWriteFactory))))
            return false;
    }
    return true;
}

// Minimal IDWriteTextRenderer that delegates DrawGlyphRun to IDWriteBitmapRenderTarget.
// Matches the Go-side rendering path for consistent text quality.
class IconTextRenderer : public IDWriteTextRenderer
{
public:
    IconTextRenderer(IDWriteBitmapRenderTarget* pTarget, IDWriteRenderingParams* pParams, COLORREF color)
        : _refCount(1), _pTarget(pTarget), _pParams(pParams), _color(color) {}

    // IUnknown
    STDMETHOD(QueryInterface)(REFIID riid, void** ppv) override
    {
        if (IsEqualIID(riid, IID_IUnknown) ||
            IsEqualIID(riid, __uuidof(IDWriteTextRenderer)) ||
            IsEqualIID(riid, __uuidof(IDWritePixelSnapping)))
        {
            *ppv = this;
            AddRef();
            return S_OK;
        }
        *ppv = nullptr;
        return E_NOINTERFACE;
    }
    STDMETHOD_(ULONG, AddRef)() override { return InterlockedIncrement(&_refCount); }
    STDMETHOD_(ULONG, Release)() override
    {
        ULONG c = InterlockedDecrement(&_refCount);
        if (c == 0) delete this;
        return c;
    }

    // IDWritePixelSnapping
    STDMETHOD(IsPixelSnappingDisabled)(void*, BOOL* isDisabled) override
    {
        *isDisabled = FALSE;  // Pixel snapping enabled for sharp small text
        return S_OK;
    }
    STDMETHOD(GetCurrentTransform)(void*, DWRITE_MATRIX* transform) override
    {
        *transform = { 1.0f, 0, 0, 1.0f, 0, 0 };  // Identity
        return S_OK;
    }
    STDMETHOD(GetPixelsPerDip)(void*, FLOAT* pixelsPerDip) override
    {
        *pixelsPerDip = 1.0f;
        return S_OK;
    }

    // IDWriteTextRenderer
    STDMETHOD(DrawGlyphRun)(void*, FLOAT baselineOriginX, FLOAT baselineOriginY,
        DWRITE_MEASURING_MODE measuringMode, const DWRITE_GLYPH_RUN* glyphRun,
        const DWRITE_GLYPH_RUN_DESCRIPTION*, IUnknown*) override
    {
        RECT blackBoxRect;
        return _pTarget->DrawGlyphRun(baselineOriginX, baselineOriginY,
            measuringMode, glyphRun, _pParams, _color, &blackBoxRect);
    }
    STDMETHOD(DrawUnderline)(void*, FLOAT, FLOAT, const DWRITE_UNDERLINE*, IUnknown*) override { return S_OK; }
    STDMETHOD(DrawStrikethrough)(void*, FLOAT, FLOAT, const DWRITE_STRIKETHROUGH*, IUnknown*) override { return S_OK; }
    STDMETHOD(DrawInlineObject)(void*, FLOAT, FLOAT, IDWriteInlineObject*, BOOL, BOOL, IUnknown*) override { return S_OK; }

private:
    LONG _refCount;
    IDWriteBitmapRenderTarget* _pTarget;
    IDWriteRenderingParams* _pParams;
    COLORREF _color;
};

// GUID_LBI_INPUTMODE - 用于在 Windows 10/11 输入指示器显示模式图标
// {2C77A81E-41CC-4178-A3A7-5F8A987568E1}
DEFINE_GUID(GUID_LBI_INPUTMODE,
    0x2C77A81E, 0x41CC, 0x4178, 0xA3, 0xA7, 0x5F, 0x8A, 0x98, 0x75, 0x68, 0xE1);

// 使用 GUID_LBI_INPUTMODE 使图标显示在 Windows 11 输入指示器中
const GUID CLangBarItemButton::_guidLangBarItemButton = GUID_LBI_INPUTMODE;

// Custom messages for cross-thread updates
const UINT CLangBarItemButton::WM_UPDATE_STATUS = WM_USER + 100;
const UINT CLangBarItemButton::WM_COMMIT_TEXT = WM_USER + 101;
const UINT CLangBarItemButton::WM_CLEAR_COMPOSITION = WM_USER + 102;
const UINT CLangBarItemButton::WM_UPDATE_COMPOSITION = WM_USER + 103;

static const UINT_PTR TIMER_ID_CARET_RETRY = 0xC401;

CLangBarItemButton::CLangBarItemButton(CTextService* pTextService)
    : _refCount(1)
    , _pTextService(pTextService)
    , _pLangBarItemSink(nullptr)
    , _dwCookie(0)
    , _bChineseMode(TRUE)
    , _bCapsLock(FALSE)
    , _bFullWidth(FALSE)
    , _bChinesePunct(TRUE)
    , _bToolbarVisible(FALSE)
    , _bKeyboardDisabled(FALSE)
    , _bDarkMode(IsSystemDarkMode() ? TRUE : FALSE)
    , _hMsgWnd(NULL)
{
    // Default input type label
    wcscpy_s(_inputTypeLabel, L"中");
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
    // TF_LBI_STYLE_BTN_MENU: 支持右键菜单 (InitMenu/OnMenuSelect)
    // TF_LBI_STYLE_SHOWNINTRAY: 在系统托盘/输入指示器区域显示
    // TF_LBI_STYLE_TEXTCOLORICON: 图标颜色随主题变化
    pInfo->dwStyle = TF_LBI_STYLE_BTN_BUTTON |
                     TF_LBI_STYLE_BTN_MENU |
                     TF_LBI_STYLE_SHOWNINTRAY |
                     TF_LBI_STYLE_TEXTCOLORICON;

    pInfo->ulSort = 0;  // 排序顺序 (0 = 最左边, 用于输入模式指示器)

    // 设置描述 - 显示为工具提示
    wcscpy_s(pInfo->szDescription, TEXTSERVICE_NAME);

    WIND_LOG_TRACE(L"GetInfo called\n");

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

    if (_bKeyboardDisabled)
    {
        *pbstrToolTip = SysAllocString(L"清风输入法 - 已禁用");
        return (*pbstrToolTip != nullptr) ? S_OK : E_OUTOFMEMORY;
    }

    // Use effective mode: Chinese mode + CapsLock ON = English Upper (temporary)
    BOOL effectiveChinese = _bChineseMode && !_bCapsLock;

    if (effectiveChinese)
    {
        *pbstrToolTip = SysAllocString(L"清风输入法 - 中文模式");
    }
    else if (_bCapsLock)
    {
        if (_bChineseMode)
        {
            // Chinese mode with CapsLock = temporary English uppercase
            *pbstrToolTip = SysAllocString(L"清风输入法 - 英文大写 (中文模式, Caps Lock)");
        }
        else
        {
            *pbstrToolTip = SysAllocString(L"清风输入法 - 英文模式 (Caps Lock 开)");
        }
    }
    else
    {
        *pbstrToolTip = SysAllocString(L"清风输入法 - 英文模式 (Caps Lock 关)");
    }

    return (*pbstrToolTip != nullptr) ? S_OK : E_OUTOFMEMORY;
}

STDAPI CLangBarItemButton::OnClick(TfLBIClick click, POINT pt, const RECT* prcArea)
{
    // TfLBIClick values: TF_LBI_CLK_RIGHT=1, TF_LBI_CLK_LEFT=2
    WIND_LOG_INFO_FMT(L"OnClick: click=%d (1=right, 2=left), pt=(%ld,%ld)\n", click, pt.x, pt.y);

    // TF_LBI_CLK_RIGHT = 1 (right click) - show popup menu
    // NOTE: Windows 11 changed the Language Bar implementation and no longer calls InitMenu.
    // We need to create and show the popup menu ourselves.
    if (click == TF_LBI_CLK_RIGHT)
    {
        WIND_LOG_INFO(L"OnClick: Right click - showing popup menu manually (Windows 11 workaround)\n");
        _ShowPopupMenu(pt);
        return S_OK;
    }

    // When keyboard is disabled by system, ignore left click toggle
    if (_bKeyboardDisabled)
        return S_OK;

    // Left click: Toggle mode via Go service (all state changes go through Go)
    if (_pTextService != nullptr)
    {
        CIPCClient* pIPCClient = _pTextService->GetIPCClient();
        if (pIPCClient != nullptr && pIPCClient->IsConnected())
        {
            ServiceResponse response;
            if (pIPCClient->SendToggleMode(response))
            {
                // Apply mode change from Go service response
                if (response.type == ResponseType::ModeChanged)
                {
                    _pTextService->SetInputMode(response.chineseMode);
                }
            }
            // If IPC fails, don't toggle locally - keep state consistent with Go
        }
    }
    return S_OK;
}

STDAPI CLangBarItemButton::InitMenu(ITfMenu* pMenu)
{
    WIND_LOG_INFO(L"InitMenu called by TSF - returning empty menu (unified menu handled by Go service)\n");

    if (pMenu == nullptr)
    {
        WIND_LOG_ERROR(L"InitMenu: pMenu is null\n");
        return E_INVALIDARG;
    }

    // Return S_OK with empty menu - the unified menu is rendered by Go service
    // On Win10, TSF may still call InitMenu, but we don't add any items
    // so no native menu will be displayed
    return S_OK;
}

STDAPI CLangBarItemButton::OnMenuSelect(UINT wID)
{
    WIND_LOG_DEBUG_FMT(L"OnMenuSelect: wID=%d\n", wID);

    if (_pTextService == nullptr)
        return E_FAIL;

    const char* command = nullptr;

    switch (wID)
    {
    case MENU_ID_TOGGLE_MODE:
        command = "toggle_mode";
        break;
    case MENU_ID_TOGGLE_WIDTH:
        command = "toggle_width";
        break;
    case MENU_ID_TOGGLE_PUNCT:
        command = "toggle_punct";
        break;
    case MENU_ID_TOGGLE_TOOLBAR:
        command = "toggle_toolbar";
        break;
    case MENU_ID_OPEN_SETTINGS:
        command = "open_settings";
        break;
    case MENU_ID_DICTIONARY:
        command = "open_dictionary";
        break;
    case MENU_ID_ABOUT:
        command = "show_about";
        break;
    // Note: MENU_ID_EXIT removed - IME exit is meaningless
    default:
        return E_INVALIDARG;
    }

    // Send menu command to Go service via IPC
    if (command != nullptr)
    {
        _pTextService->SendMenuCommand(command);
    }

    return S_OK;
}

STDAPI CLangBarItemButton::GetIcon(HICON* phIcon)
{
    if (phIcon == nullptr)
        return E_INVALIDARG;

    *phIcon = nullptr;

    WIND_LOG_TRACE(L"GetIcon called\n");

    // Get DPI scaling
    HDC hdcScreen = GetDC(NULL);
    if (hdcScreen == NULL)
    {
        WIND_LOG_ERROR(L"GetIcon: GetDC failed\n");
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
        WIND_LOG_ERROR(L"GetIcon: CreateCompatibleDC failed\n");
        return E_FAIL;
    }

    // Create 32-bit DIB section for better compatibility with Windows 10/11
    BITMAPINFO bmi = { 0 };
    bmi.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
    bmi.bmiHeader.biWidth = iconSize;
    bmi.bmiHeader.biHeight = -iconSize;  // Top-down DIB
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB;

    void* pBits = nullptr;
    HBITMAP hBitmap = CreateDIBSection(hdcMem, &bmi, DIB_RGB_COLORS, &pBits, NULL, 0);
    if (hBitmap == NULL || pBits == nullptr)
    {
        DeleteDC(hdcMem);
        ReleaseDC(NULL, hdcScreen);
        WIND_LOG_ERROR(L"GetIcon: CreateDIBSection failed\n");
        return E_FAIL;
    }
    HBITMAP hOldBitmap = (HBITMAP)SelectObject(hdcMem, hBitmap);

    // Fill with opaque black (BGRA = 0,0,0,255) so GDI can properly anti-alias
    // against a solid background. Alpha will be replaced later from text luminance.
    {
        BYTE* initPixels = (BYTE*)pBits;
        for (int i = 0; i < iconSize * iconSize; i++)
        {
            initPixels[i * 4 + 0] = 0;    // B
            initPixels[i * 4 + 1] = 0;    // G
            initPixels[i * 4 + 2] = 0;    // R
            initPixels[i * 4 + 3] = 255;  // A = opaque
        }
    }

    // Display text is determined by Go service via _inputTypeLabel
    // (e.g., "中", "英", "A", "拼", "五", "双")
    const wchar_t* text = _inputTypeLabel;

    // Draw white text on black using DirectWrite GDI-interop path
    // (IDWriteBitmapRenderTarget + IDWriteTextRenderer — same as Go-side candidate window)
    bool textRendered = false;
    float fontSizeDIP = (float)(iconSize - 2);

    if (EnsureDWriteFactory())
    {
        IDWriteGdiInterop* pGdiInterop = nullptr;
        HRESULT hr = g_pDWriteFactory->GetGdiInterop(&pGdiInterop);
        if (SUCCEEDED(hr))
        {
            IDWriteBitmapRenderTarget* pBitmapTarget = nullptr;
            hr = pGdiInterop->CreateBitmapRenderTarget(NULL, iconSize, iconSize, &pBitmapTarget);
            if (SUCCEEDED(hr))
            {
                // 1 DIP = 1 pixel (bitmap is already DPI-scaled)
                pBitmapTarget->SetPixelsPerDip(1.0f);

                // Fill bitmap target with black background
                HDC hdcBitmap = pBitmapTarget->GetMemoryDC();
                RECT rcFill = { 0, 0, iconSize, iconSize };
                FillRect(hdcBitmap, &rcFill, (HBRUSH)GetStockObject(BLACK_BRUSH));

                // Grayscale rendering params: disable ClearType to avoid subpixel
                // color artifacts in luminance-to-alpha conversion for icon rendering
                IDWriteRenderingParams* pRenderParams = nullptr;
                {
                    IDWriteRenderingParams* pDefault = nullptr;
                    g_pDWriteFactory->CreateRenderingParams(&pDefault);
                    if (pDefault)
                    {
                        g_pDWriteFactory->CreateCustomRenderingParams(
                            pDefault->GetGamma(),
                            pDefault->GetEnhancedContrast(),
                            0.0f,  // clearTypeLevel = 0: force grayscale AA
                            pDefault->GetPixelGeometry(),
                            DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,
                            &pRenderParams
                        );
                        pDefault->Release();
                    }
                }

                // Create text format and layout
                IDWriteTextFormat* pTextFormat = nullptr;
                hr = g_pDWriteFactory->CreateTextFormat(
                    L"Microsoft YaHei UI",
                    nullptr,
                    DWRITE_FONT_WEIGHT_LIGHT,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    fontSizeDIP,
                    L"zh-cn",
                    &pTextFormat
                );

                if (SUCCEEDED(hr))
                {
                    pTextFormat->SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                    pTextFormat->SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

                    IDWriteTextLayout* pLayout = nullptr;
                    hr = g_pDWriteFactory->CreateTextLayout(
                        text, (UINT32)wcslen(text), pTextFormat,
                        (float)iconSize, (float)iconSize, &pLayout);

                    if (SUCCEEDED(hr))
                    {
                        // Render via IconTextRenderer → BitmapRenderTarget::DrawGlyphRun
                        IconTextRenderer* pRenderer = new IconTextRenderer(
                            pBitmapTarget, pRenderParams, RGB(255, 255, 255));
                        pLayout->Draw(nullptr, pRenderer, 0, 0);
                        pRenderer->Release();

                        // Copy rendered text from bitmap target to our DIB section
                        BitBlt(hdcMem, 0, 0, iconSize, iconSize, hdcBitmap, 0, 0, SRCCOPY);
                        textRendered = true;

                        pLayout->Release();
                    }
                    pTextFormat->Release();
                }
                if (pRenderParams) pRenderParams->Release();
                pBitmapTarget->Release();
            }
            pGdiInterop->Release();
        }
    }

    // GDI fallback if DirectWrite unavailable
    if (!textRendered)
    {
        SetBkMode(hdcMem, TRANSPARENT);
        SetTextColor(hdcMem, RGB(255, 255, 255));
        int fontSize = iconSize - 2;
        HFONT hFont = CreateFontW(
            -fontSize, 0, 0, 0, FW_MEDIUM,
            FALSE, FALSE, FALSE,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            DEFAULT_PITCH | FF_DONTCARE,
            L"Microsoft YaHei"
        );
        if (hFont == NULL)
            hFont = (HFONT)GetStockObject(DEFAULT_GUI_FONT);

        HFONT hOldFont = (HFONT)SelectObject(hdcMem, hFont);
        RECT rc = { 0, 0, iconSize, iconSize };
        DrawTextW(hdcMem, text, -1, &rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        SelectObject(hdcMem, hOldFont);
        if (hFont != GetStockObject(DEFAULT_GUI_FONT))
            DeleteObject(hFont);
    }

    // Convert white-on-black text to alpha mask for theme-aware rendering.
    // Text luminance becomes alpha; RGB is set based on system theme:
    //   Light mode: RGB(0,0,0)       → black text on light taskbar
    //   Dark mode:  RGB(255,255,255) → white text on dark taskbar
    // TF_LBI_STYLE_TEXTCOLORICON should handle this automatically, but some
    // Windows versions don't reliably recolor, so we detect the theme ourselves.
    BYTE fgColor = _bDarkMode ? 255 : 0;
    BYTE* pixels = (BYTE*)pBits;
    for (int i = 0; i < iconSize * iconSize; i++)
    {
        BYTE b = pixels[i * 4 + 0];
        BYTE g = pixels[i * 4 + 1];
        BYTE r = pixels[i * 4 + 2];
        // max(r, g, b) as alpha - preserves anti-aliased edge transitions
        BYTE alpha = r > g ? (r > b ? r : b) : (g > b ? g : b);
        // When keyboard is disabled, reduce alpha to 35% for dimmed appearance
        if (_bKeyboardDisabled)
            alpha = (BYTE)(alpha * 90 / 255);
        pixels[i * 4 + 0] = fgColor; // B
        pixels[i * 4 + 1] = fgColor; // G
        pixels[i * 4 + 2] = fgColor; // R
        pixels[i * 4 + 3] = alpha;   // A = text coverage
    }

    // Create monochrome mask bitmap (all zeros for 32-bit alpha icon)
    BITMAPINFO bmiMask = { 0 };
    bmiMask.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
    bmiMask.bmiHeader.biWidth = iconSize;
    bmiMask.bmiHeader.biHeight = iconSize;  // Bottom-up for mask (positive height)
    bmiMask.bmiHeader.biPlanes = 1;
    bmiMask.bmiHeader.biBitCount = 1;
    bmiMask.bmiHeader.biCompression = BI_RGB;

    void* pMaskBits = nullptr;
    HBITMAP hMaskBitmap = CreateDIBSection(hdcMem, &bmiMask, DIB_RGB_COLORS, &pMaskBits, NULL, 0);
    if (hMaskBitmap == NULL || pMaskBits == nullptr)
    {
        SelectObject(hdcMem, hOldBitmap);
        DeleteObject(hBitmap);
        DeleteDC(hdcMem);
        ReleaseDC(NULL, hdcScreen);
        WIND_LOG_ERROR(L"GetIcon: CreateDIBSection for mask failed\n");
        return E_FAIL;
    }

    // Fill mask with zeros (alpha channel handles transparency for 32-bit icons)
    int maskRowBytes = ((iconSize + 31) / 32) * 4;
    memset(pMaskBits, 0, maskRowBytes * iconSize);

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

    WIND_LOG_DEBUG_FMT(L"GetIcon: size=%d, text=%ls, icon=%p\n",
              iconSize, text, *phIcon);

    return (*phIcon != nullptr) ? S_OK : E_FAIL;
}

STDAPI CLangBarItemButton::GetText(BSTR* pbstrText)
{
    if (pbstrText == nullptr)
        return E_INVALIDARG;

    // Display text is determined by Go service via _inputTypeLabel
    *pbstrText = SysAllocString(_inputTypeLabel);

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

// Message window class name
static const wchar_t* MSG_WND_CLASS = L"WindInputLangBarMsgWnd";
static ATOM s_msgWndClass = 0;

LRESULT CALLBACK CLangBarItemButton::_MsgWndProc(HWND hwnd, UINT msg, WPARAM wParam, LPARAM lParam)
{
    if (msg == WM_UPDATE_STATUS)
    {
        // lParam contains pointer to StatusUpdateData (allocated by sender)
        StatusUpdateData* pData = reinterpret_cast<StatusUpdateData*>(lParam);
        CLangBarItemButton* pThis = reinterpret_cast<CLangBarItemButton*>(GetWindowLongPtrW(hwnd, GWLP_USERDATA));

        if (pThis != nullptr && pData != nullptr)
        {
            WIND_LOG_DEBUG(L"MsgWndProc: Processing WM_UPDATE_STATUS\n");
            // Call UpdateFullStatus on the UI thread (with icon label from Go service)
            pThis->UpdateFullStatus(pData->bChineseMode, pData->bFullWidth,
                                     pData->bChinesePunct, pData->bToolbarVisible, pData->bCapsLock,
                                     pData->iconLabel[0] != L'\0' ? pData->iconLabel : nullptr);
        }

        // Free the data allocated by sender
        delete pData;
        return 0;
    }
    else if (msg == WM_COMMIT_TEXT)
    {
        // lParam contains pointer to CommitTextData (allocated by sender)
        CommitTextData* pData = reinterpret_cast<CommitTextData*>(lParam);
        CLangBarItemButton* pThis = reinterpret_cast<CLangBarItemButton*>(GetWindowLongPtrW(hwnd, GWLP_USERDATA));

        if (pThis != nullptr && pData != nullptr && pThis->_pTextService != nullptr)
        {
            WIND_LOG_DEBUG_FMT(L"MsgWndProc: Processing WM_COMMIT_TEXT, textLen=%zu\n", pData->text.length());

            // Use atomic CommitText which ends composition + inserts text in a single
            // TF_ES_SYNC EditSession. The previous separate EndComposition(async) +
            // InsertText(sync) approach caused a race condition where the async
            // EndComposition could clear the just-inserted text, especially in apps
            // like VSCode and browsers when InlinePreedit is disabled.
            pThis->_pTextService->CommitText(pData->text);
            // Reset KeyEventSink state so shortcut keys work again
            pThis->_pTextService->ResetComposingState();
        }

        // Free the data allocated by sender
        delete pData;
        return 0;
    }
    else if (msg == WM_CLEAR_COMPOSITION)
    {
        CLangBarItemButton* pThis = reinterpret_cast<CLangBarItemButton*>(GetWindowLongPtrW(hwnd, GWLP_USERDATA));

        if (pThis != nullptr && pThis->_pTextService != nullptr)
        {
            WIND_LOG_DEBUG(L"MsgWndProc: Processing WM_CLEAR_COMPOSITION\n");
            pThis->_pTextService->EndComposition();
            pThis->_pTextService->ResetComposingState();
        }
        return 0;
    }
    else if (msg == WM_UPDATE_COMPOSITION)
    {
        UpdateCompositionData* pData = reinterpret_cast<UpdateCompositionData*>(lParam);
        CLangBarItemButton* pThis = reinterpret_cast<CLangBarItemButton*>(GetWindowLongPtrW(hwnd, GWLP_USERDATA));

        if (pThis != nullptr && pData != nullptr && pThis->_pTextService != nullptr)
        {
            WIND_LOG_DEBUG_FMT(L"MsgWndProc: Processing WM_UPDATE_COMPOSITION, textLen=%zu, caret=%d\n",
                               pData->text.length(), pData->caretPos);
            pThis->_pTextService->UpdateComposition(pData->text, pData->caretPos);
        }

        delete pData;
        return 0;
    }
    else if (msg == WM_TIMER)
    {
        if (wParam == TIMER_ID_CARET_RETRY)
        {
            KillTimer(hwnd, wParam);
            CLangBarItemButton* pThis = reinterpret_cast<CLangBarItemButton*>(GetWindowLongPtrW(hwnd, GWLP_USERDATA));
            if (pThis != nullptr && pThis->_pTextService != nullptr && pThis->_pTextService->HasActiveComposition())
            {
                // Timer 兜底：清除 _compositionJustStarted 让 SendCaretPositionUpdate
                // 走正常路径（消费已缓存的坐标，应对不发 OnLayoutChange 的应用）。
                pThis->_pTextService->ClearCompositionJustStarted();
                pThis->_pTextService->SendCaretPositionUpdate();
            }
            return 0;
        }
    }

    return DefWindowProcW(hwnd, msg, wParam, lParam);
}

BOOL CLangBarItemButton::Initialize()
{
    WIND_LOG_INFO(L"LangBarItemButton::Initialize\n");

    if (_pTextService == nullptr)
    {
        WIND_LOG_ERROR(L"LangBarItemButton: _pTextService is null\n");
        return FALSE;
    }

    // Register message window class if not already registered
    if (s_msgWndClass == 0)
    {
        WNDCLASSEXW wc = { sizeof(WNDCLASSEXW) };
        wc.lpfnWndProc = _MsgWndProc;
        wc.hInstance = g_hInstance;
        wc.lpszClassName = MSG_WND_CLASS;
        s_msgWndClass = RegisterClassExW(&wc);
        if (s_msgWndClass == 0)
        {
            WIND_LOG_WARN(L"Failed to register message window class\n");
        }
    }

    // Create message-only window for cross-thread updates
    if (s_msgWndClass != 0)
    {
        _hMsgWnd = CreateWindowExW(0, MSG_WND_CLASS, L"", 0, 0, 0, 0, 0,
                                    HWND_MESSAGE, NULL, g_hInstance, NULL);
        if (_hMsgWnd != NULL)
        {
            // Store this pointer in window data
            SetWindowLongPtrW(_hMsgWnd, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(this));
            WIND_LOG_DEBUG(L"Message window created for cross-thread updates\n");
        }
        else
        {
            WIND_LOG_WARN(L"Failed to create message window\n");
        }
    }

    ITfThreadMgr* pThreadMgr = _pTextService->GetThreadMgr();
    if (pThreadMgr == nullptr)
    {
        WIND_LOG_ERROR(L"LangBarItemButton: pThreadMgr is null\n");
        return FALSE;
    }

    ITfLangBarItemMgr* pLangBarItemMgr = nullptr;
    HRESULT hr = pThreadMgr->QueryInterface(IID_ITfLangBarItemMgr, (void**)&pLangBarItemMgr);
    if (FAILED(hr) || pLangBarItemMgr == nullptr)
    {
        WIND_LOG_ERROR_FMT(L"Failed to get ITfLangBarItemMgr, hr=0x%08X\n", hr);
        return FALSE;
    }

    hr = pLangBarItemMgr->AddItem(this);

    WIND_LOG_DEBUG_FMT(L"LangBarItemMgr->AddItem returned hr=0x%08X\n", hr);

    pLangBarItemMgr->Release();

    if (FAILED(hr))
    {
        WIND_LOG_ERROR(L"Failed to add LangBarItem\n");
        return FALSE;
    }

    WIND_LOG_INFO(L"LangBarItemButton initialized successfully\n");
    return TRUE;
}

void CLangBarItemButton::Uninitialize()
{
    WIND_LOG_INFO(L"LangBarItemButton::Uninitialize\n");

    // Destroy message window
    if (_hMsgWnd != NULL)
    {
        KillTimer(_hMsgWnd, TIMER_ID_CARET_RETRY);
        DestroyWindow(_hMsgWnd);
        _hMsgWnd = NULL;
    }

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

void CLangBarItemButton::UpdateKeyboardDisabled(BOOL bDisabled)
{
    if (_bKeyboardDisabled == bDisabled)
        return;

    _bKeyboardDisabled = bDisabled;

    if (_pLangBarItemSink != nullptr)
    {
        _pLangBarItemSink->OnUpdate(TF_LBI_ICON | TF_LBI_TEXT | TF_LBI_TOOLTIP);
    }
}

void CLangBarItemButton::UpdateState(BOOL bChineseMode, BOOL bCapsLock)
{
    // With effective mode, CapsLock affects display in Chinese mode too
    // (Chinese + CapsLock = English Upper)
    BOOL needUpdate = (_bChineseMode != bChineseMode) ||
                      (_bCapsLock != bCapsLock);

    _bChineseMode = bChineseMode;
    _bCapsLock = bCapsLock;

    if (needUpdate && _pLangBarItemSink != nullptr)
    {
        _pLangBarItemSink->OnUpdate(TF_LBI_ICON | TF_LBI_TEXT | TF_LBI_TOOLTIP);
    }
}

void CLangBarItemButton::UpdateFullStatus(BOOL bChineseMode, BOOL bFullWidth, BOOL bChinesePunct, BOOL bToolbarVisible, BOOL bCapsLock, const wchar_t* iconLabel)
{
    // Update icon label from Go service (if provided)
    BOOL labelChanged = FALSE;
    if (iconLabel != nullptr && iconLabel[0] != L'\0')
    {
        if (wcscmp(_inputTypeLabel, iconLabel) != 0)
        {
            wcscpy_s(_inputTypeLabel, iconLabel);
            labelChanged = TRUE;
        }
    }

    // Refresh dark mode state (cached for GetIcon)
    BOOL bDarkMode = IsSystemDarkMode() ? TRUE : FALSE;

    BOOL needUpdate = (_bChineseMode != bChineseMode) ||
                      (_bFullWidth != bFullWidth) ||
                      (_bChinesePunct != bChinesePunct) ||
                      (_bToolbarVisible != bToolbarVisible) ||
                      (_bCapsLock != bCapsLock) ||
                      (_bDarkMode != bDarkMode) ||
                      labelChanged;

    _bChineseMode = bChineseMode;
    _bFullWidth = bFullWidth;
    _bChinesePunct = bChinesePunct;
    _bToolbarVisible = bToolbarVisible;
    _bCapsLock = bCapsLock;
    _bDarkMode = bDarkMode;

    if (needUpdate && _pLangBarItemSink != nullptr)
    {
        _pLangBarItemSink->OnUpdate(TF_LBI_ICON | TF_LBI_TEXT | TF_LBI_TOOLTIP);
    }

    WIND_LOG_DEBUG_FMT(L"UpdateFullStatus: mode=%d, width=%d, punct=%d, toolbar=%d, caps=%d, dark=%d, label=%ls, needUpdate=%d\n",
              bChineseMode, bFullWidth, bChinesePunct, bToolbarVisible, bCapsLock, bDarkMode, _inputTypeLabel, needUpdate);
}

void CLangBarItemButton::PostUpdateFullStatus(BOOL bChineseMode, BOOL bFullWidth, BOOL bChinesePunct, BOOL bToolbarVisible, BOOL bCapsLock, const wchar_t* iconLabel)
{
    // Thread-safe update: post message to message window which runs on UI thread
    if (_hMsgWnd == NULL)
    {
        WIND_LOG_WARN(L"PostUpdateFullStatus: No message window, falling back to direct call\n");
        // Fallback to direct call (may not work from async thread)
        UpdateFullStatus(bChineseMode, bFullWidth, bChinesePunct, bToolbarVisible, bCapsLock, iconLabel);
        return;
    }

    // Allocate data on heap (will be freed by message handler)
    StatusUpdateData* pData = new StatusUpdateData();
    pData->bChineseMode = bChineseMode;
    pData->bFullWidth = bFullWidth;
    pData->bChinesePunct = bChinesePunct;
    pData->bToolbarVisible = bToolbarVisible;
    pData->bCapsLock = bCapsLock;
    // Copy icon label
    if (iconLabel != nullptr && iconLabel[0] != L'\0')
    {
        wcscpy_s(pData->iconLabel, iconLabel);
    }
    else
    {
        pData->iconLabel[0] = L'\0';
    }

    // Post message to UI thread
    if (!PostMessageW(_hMsgWnd, WM_UPDATE_STATUS, 0, reinterpret_cast<LPARAM>(pData)))
    {
        // PostMessage failed, free data and fallback
        delete pData;
        WIND_LOG_WARN(L"PostUpdateFullStatus: PostMessage failed\n");
    }
    else
    {
        WIND_LOG_DEBUG(L"PostUpdateFullStatus: Message posted to UI thread\n");
    }
}

void CLangBarItemButton::PostCommitText(const std::wstring& text)
{
    // Thread-safe commit: post message to message window which runs on UI thread
    // This ensures EndComposition is called before InsertText on the correct thread
    if (_hMsgWnd == NULL)
    {
        WIND_LOG_WARN(L"PostCommitText: No message window, using direct InsertText\n");
        // Fallback to direct InsertText (composition won't be ended properly)
        if (_pTextService != nullptr)
        {
            _pTextService->InsertText(text);
        }
        return;
    }

    // Allocate data on heap (will be freed by message handler)
    CommitTextData* pData = new CommitTextData();
    pData->text = text;

    // Post message to UI thread
    if (!PostMessageW(_hMsgWnd, WM_COMMIT_TEXT, 0, reinterpret_cast<LPARAM>(pData)))
    {
        // PostMessage failed, free data and fallback
        delete pData;
        WIND_LOG_WARN(L"PostCommitText: PostMessage failed, using direct InsertText\n");
        if (_pTextService != nullptr)
        {
            _pTextService->InsertText(text);
        }
    }
    else
    {
        WIND_LOG_DEBUG_FMT(L"PostCommitText: Message posted to UI thread, textLen=%zu\n", text.length());
    }
}

void CLangBarItemButton::PostClearComposition()
{
    // Thread-safe: post message to message window which runs on UI thread
    if (_hMsgWnd == NULL)
    {
        WIND_LOG_WARN(L"PostClearComposition: No message window, using direct EndComposition\n");
        if (_pTextService != nullptr)
        {
            _pTextService->EndComposition();
        }
        return;
    }

    if (!PostMessageW(_hMsgWnd, WM_CLEAR_COMPOSITION, 0, 0))
    {
        WIND_LOG_WARN(L"PostClearComposition: PostMessage failed, using direct EndComposition\n");
        if (_pTextService != nullptr)
        {
            _pTextService->EndComposition();
        }
    }
    else
    {
        WIND_LOG_DEBUG(L"PostClearComposition: Message posted to UI thread\n");
    }
}

void CLangBarItemButton::PostUpdateComposition(const std::wstring& text, int caretPos)
{
    if (_hMsgWnd == NULL)
    {
        WIND_LOG_WARN(L"PostUpdateComposition: No message window, using direct UpdateComposition\n");
        if (_pTextService != nullptr)
        {
            _pTextService->UpdateComposition(text, caretPos);
        }
        return;
    }

    UpdateCompositionData* pData = new UpdateCompositionData();
    pData->text = text;
    pData->caretPos = caretPos;

    if (!PostMessageW(_hMsgWnd, WM_UPDATE_COMPOSITION, 0, reinterpret_cast<LPARAM>(pData)))
    {
        delete pData;
        WIND_LOG_WARN(L"PostUpdateComposition: PostMessage failed, using direct UpdateComposition\n");
        if (_pTextService != nullptr)
        {
            _pTextService->UpdateComposition(text, caretPos);
        }
    }
    else
    {
        WIND_LOG_DEBUG_FMT(L"PostUpdateComposition: Message posted to UI thread, textLen=%zu, caret=%d\n",
                           text.length(), caretPos);
    }
}

void CLangBarItemButton::PostDelayedCaretPositionUpdate()
{
    if (_hMsgWnd == NULL)
    {
        WIND_LOG_WARN(L"PostDelayedCaretPositionUpdate: No message window\n");
        return;
    }

    // Weasel 模式：StartComposition 后第一次 SendCaretPositionUpdate 推迟 50ms 兜底。
    // OnLayoutChange 触发时会清掉 _compositionJustStarted 并取消此 timer；
    // 若 50ms 内未收到 OnLayoutChange（如某些 CUAS 路径），timer 到期用 cache 兜底发一次。
    KillTimer(_hMsgWnd, TIMER_ID_CARET_RETRY);
    if (SetTimer(_hMsgWnd, TIMER_ID_CARET_RETRY, 50, nullptr) == 0)
    {
        WIND_LOG_WARN(L"PostDelayedCaretPositionUpdate: failed to schedule timer\n");
    }
}

void CLangBarItemButton::CancelDelayedCaretPositionUpdate()
{
    if (_hMsgWnd != NULL)
    {
        KillTimer(_hMsgWnd, TIMER_ID_CARET_RETRY);
    }
}

void CLangBarItemButton::ForceRefresh()
{
    WIND_LOG_DEBUG(L"ForceRefresh called\n");

    // Update current Caps Lock state
    _bCapsLock = (GetKeyState(VK_CAPITAL) & 0x0001) != 0;

    // Force update the language bar icon unconditionally
    if (_pLangBarItemSink != nullptr)
    {
        _pLangBarItemSink->OnUpdate(TF_LBI_ICON | TF_LBI_TEXT | TF_LBI_TOOLTIP | TF_LBI_STATUS);
    }

    WIND_LOG_DEBUG_FMT(L"ForceRefresh: mode=%d, caps=%d\n", _bChineseMode, _bCapsLock);
}

void CLangBarItemButton::SetInputTypeLabel(const wchar_t* label)
{
    if (label == nullptr)
        return;

    wcscpy_s(_inputTypeLabel, label);

    // Refresh icon to show the new label
    if (_pLangBarItemSink != nullptr)
    {
        _pLangBarItemSink->OnUpdate(TF_LBI_ICON | TF_LBI_TEXT | TF_LBI_TOOLTIP);
    }
}

// Show popup menu by sending screen coordinates to Go service
// Go service renders the unified menu with consistent styling
void CLangBarItemButton::_ShowPopupMenu(POINT pt)
{
    WIND_LOG_INFO_FMT(L"_ShowPopupMenu: Sending context menu request to Go service at (%ld, %ld)\n", pt.x, pt.y);

    if (_pTextService != nullptr)
    {
        _pTextService->SendShowContextMenu(pt.x, pt.y);
    }
}
