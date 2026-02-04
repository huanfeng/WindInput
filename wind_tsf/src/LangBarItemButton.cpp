#include "LangBarItemButton.h"
#include "TextService.h"
#include "IPCClient.h"
#include "Globals.h"
#include <olectl.h>  // For CONNECT_E_* constants

// GUID_LBI_INPUTMODE - 用于在 Windows 10/11 输入指示器显示模式图标
// {2C77A81E-41CC-4178-A3A7-5F8A987568E1}
DEFINE_GUID(GUID_LBI_INPUTMODE,
    0x2C77A81E, 0x41CC, 0x4178, 0xA3, 0xA7, 0x5F, 0x8A, 0x98, 0x75, 0x68, 0xE1);

// 使用 GUID_LBI_INPUTMODE 使图标显示在 Windows 11 输入指示器中
const GUID CLangBarItemButton::_guidLangBarItemButton = GUID_LBI_INPUTMODE;

// Custom messages for cross-thread updates
const UINT CLangBarItemButton::WM_UPDATE_STATUS = WM_USER + 100;
const UINT CLangBarItemButton::WM_COMMIT_TEXT = WM_USER + 101;

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
    , _hMsgWnd(NULL)
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

    // Use effective mode: Chinese mode + CapsLock ON = English Upper (temporary)
    BOOL effectiveChinese = _bChineseMode && !_bCapsLock;

    if (effectiveChinese)
    {
        *pbstrToolTip = SysAllocString(L"WindInput - 中文模式");
    }
    else if (_bCapsLock)
    {
        if (_bChineseMode)
        {
            // Chinese mode with CapsLock = temporary English uppercase
            *pbstrToolTip = SysAllocString(L"WindInput - 英文大写 (中文模式, Caps Lock)");
        }
        else
        {
            *pbstrToolTip = SysAllocString(L"WindInput - English Mode (Caps Lock ON)");
        }
    }
    else
    {
        *pbstrToolTip = SysAllocString(L"WindInput - English Mode (Caps Lock OFF)");
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
    WIND_LOG_INFO(L"InitMenu called by TSF - setting up right-click menu\n");

    if (pMenu == nullptr)
    {
        WIND_LOG_ERROR(L"InitMenu: pMenu is null\n");
        return E_INVALIDARG;
    }

    // Add menu items
    // 中文模式
    pMenu->AddMenuItem(MENU_ID_TOGGLE_MODE,
        _bChineseMode ? TF_LBMENUF_CHECKED : 0,
        NULL, NULL,
        L"\x4E2D\x6587\x6A21\x5F0F", 4,  // 中文模式
        NULL);

    // 全角
    pMenu->AddMenuItem(MENU_ID_TOGGLE_WIDTH,
        _bFullWidth ? TF_LBMENUF_CHECKED : 0,
        NULL, NULL,
        L"\x5168\x89D2", 2,  // 全角
        NULL);

    // 中文标点
    pMenu->AddMenuItem(MENU_ID_TOGGLE_PUNCT,
        _bChinesePunct ? TF_LBMENUF_CHECKED : 0,
        NULL, NULL,
        L"\x4E2D\x6587\x6807\x70B9", 4,  // 中文标点
        NULL);

    // Separator
    pMenu->AddMenuItem(0, TF_LBMENUF_SEPARATOR, NULL, NULL, NULL, 0, NULL);

    // 显示工具栏
    pMenu->AddMenuItem(MENU_ID_TOGGLE_TOOLBAR,
        _bToolbarVisible ? TF_LBMENUF_CHECKED : 0,
        NULL, NULL,
        L"\x663E\x793A\x5DE5\x5177\x680F", 5,  // 显示工具栏
        NULL);

    // Separator
    pMenu->AddMenuItem(0, TF_LBMENUF_SEPARATOR, NULL, NULL, NULL, 0, NULL);

    // 词库管理...
    pMenu->AddMenuItem(MENU_ID_DICTIONARY, 0,
        NULL, NULL,
        L"\x8BCD\x5E93\x7BA1\x7406...", 5,  // 词库管理...
        NULL);

    // 设置...
    pMenu->AddMenuItem(MENU_ID_OPEN_SETTINGS, 0,
        NULL, NULL,
        L"\x8BBE\x7F6E...", 3,  // 设置...
        NULL);

    // Separator
    pMenu->AddMenuItem(0, TF_LBMENUF_SEPARATOR, NULL, NULL, NULL, 0, NULL);

    // 关于
    pMenu->AddMenuItem(MENU_ID_ABOUT, 0,
        NULL, NULL,
        L"\x5173\x4E8E", 2,  // 关于
        NULL);

    // Note: "Exit" menu item removed - IME exit is meaningless

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

    // Draw background based on effective mode:
    // - Chinese mode + CapsLock OFF = Chinese (蓝底"中")
    // - Chinese mode + CapsLock ON = English Upper (灰底"A") - temporary English for caps
    // - English mode + CapsLock OFF = English Lower (灰底"a")
    // - English mode + CapsLock ON = English Upper (灰底"A")
    BOOL effectiveChinese = _bChineseMode && !_bCapsLock;

    RECT rc = { 0, 0, iconSize, iconSize };
    HBRUSH hBrush;
    if (effectiveChinese)
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
        CLEARTYPE_QUALITY,  // Use ClearType for better rendering
        DEFAULT_PITCH | FF_DONTCARE,
        L"Microsoft YaHei"  // Microsoft YaHei has better coverage
    );

    if (hFont == NULL)
    {
        // Fallback to SimHei
        hFont = CreateFontW(
            fontSize, 0, 0, 0, FW_BOLD,
            FALSE, FALSE, FALSE,
            GB2312_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            DEFAULT_PITCH | FF_DONTCARE,
            L"SimHei"
        );
    }

    if (hFont == NULL)
    {
        // Final fallback to system font
        hFont = (HFONT)GetStockObject(DEFAULT_GUI_FONT);
    }

    HFONT hOldFont = (HFONT)SelectObject(hdcMem, hFont);

    // Display text based on effective mode:
    // - effectiveChinese = true: "中"
    // - effectiveChinese = false + CapsLock ON: "A"
    // - effectiveChinese = false + CapsLock OFF: "a"
    const wchar_t* text;
    if (effectiveChinese)
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

    // Set alpha channel to fully opaque for all pixels
    // DIB section pixels are in BGRA format
    BYTE* pixels = (BYTE*)pBits;
    for (int i = 0; i < iconSize * iconSize; i++)
    {
        pixels[i * 4 + 3] = 255;  // Set alpha to opaque
    }

    // Create monochrome mask bitmap (all zeros = fully opaque)
    // Use CreateDIBSection for proper initialization
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

    // Fill mask with zeros (0 = opaque in mask)
    // Each row is padded to DWORD boundary
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

    WIND_LOG_DEBUG_FMT(L"GetIcon: size=%d, mode=%s, icon=%p\n",
              iconSize, _bChineseMode ? L"Chinese" : L"English", *phIcon);

    return (*phIcon != nullptr) ? S_OK : E_FAIL;
}

STDAPI CLangBarItemButton::GetText(BSTR* pbstrText)
{
    if (pbstrText == nullptr)
        return E_INVALIDARG;

    // Use effective mode: Chinese mode + CapsLock ON = English Upper
    BOOL effectiveChinese = _bChineseMode && !_bCapsLock;

    if (effectiveChinese)
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
            // Call UpdateFullStatus on the UI thread
            pThis->UpdateFullStatus(pData->bChineseMode, pData->bFullWidth,
                                     pData->bChinesePunct, pData->bToolbarVisible, pData->bCapsLock);
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

            // IMPORTANT: On UI thread, first end composition, then insert text
            // This ensures the composition text is cleared before inserting final text
            pThis->_pTextService->EndComposition();
            pThis->_pTextService->InsertText(pData->text);
        }

        // Free the data allocated by sender
        delete pData;
        return 0;
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

void CLangBarItemButton::UpdateFullStatus(BOOL bChineseMode, BOOL bFullWidth, BOOL bChinesePunct, BOOL bToolbarVisible, BOOL bCapsLock)
{
    // With effective mode, CapsLock affects display in Chinese mode too
    // (Chinese + CapsLock = English Upper)
    BOOL needUpdate = (_bChineseMode != bChineseMode) ||
                      (_bFullWidth != bFullWidth) ||
                      (_bChinesePunct != bChinesePunct) ||
                      (_bToolbarVisible != bToolbarVisible) ||
                      (_bCapsLock != bCapsLock);

    _bChineseMode = bChineseMode;
    _bFullWidth = bFullWidth;
    _bChinesePunct = bChinesePunct;
    _bToolbarVisible = bToolbarVisible;
    _bCapsLock = bCapsLock;

    if (needUpdate && _pLangBarItemSink != nullptr)
    {
        _pLangBarItemSink->OnUpdate(TF_LBI_ICON | TF_LBI_TEXT | TF_LBI_TOOLTIP);
    }

    WIND_LOG_DEBUG_FMT(L"UpdateFullStatus: mode=%d, width=%d, punct=%d, toolbar=%d, caps=%d, needUpdate=%d\n",
              bChineseMode, bFullWidth, bChinesePunct, bToolbarVisible, bCapsLock, needUpdate);
}

void CLangBarItemButton::PostUpdateFullStatus(BOOL bChineseMode, BOOL bFullWidth, BOOL bChinesePunct, BOOL bToolbarVisible, BOOL bCapsLock)
{
    // Thread-safe update: post message to message window which runs on UI thread
    if (_hMsgWnd == NULL)
    {
        WIND_LOG_WARN(L"PostUpdateFullStatus: No message window, falling back to direct call\n");
        // Fallback to direct call (may not work from async thread)
        UpdateFullStatus(bChineseMode, bFullWidth, bChinesePunct, bToolbarVisible, bCapsLock);
        return;
    }

    // Allocate data on heap (will be freed by message handler)
    StatusUpdateData* pData = new StatusUpdateData();
    pData->bChineseMode = bChineseMode;
    pData->bFullWidth = bFullWidth;
    pData->bChinesePunct = bChinesePunct;
    pData->bToolbarVisible = bToolbarVisible;
    pData->bCapsLock = bCapsLock;

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

// Show popup menu manually (Windows 11 workaround)
// Windows 11 no longer calls InitMenu via TSF, so we create popup menu ourselves
void CLangBarItemButton::_ShowPopupMenu(POINT pt)
{
    // Log current state for debugging
    WIND_LOG_INFO_FMT(L"_ShowPopupMenu: Current state - chineseMode=%d, fullWidth=%d, chinesePunct=%d, toolbarVisible=%d\n",
                      _bChineseMode, _bFullWidth, _bChinesePunct, _bToolbarVisible);

    HMENU hMenu = CreatePopupMenu();
    if (hMenu == NULL)
    {
        WIND_LOG_ERROR(L"_ShowPopupMenu: CreatePopupMenu failed\n");
        return;
    }

    // Menu items (same as InitMenu)
    // 中文模式
    AppendMenuW(hMenu, _bChineseMode ? MF_CHECKED : MF_UNCHECKED,
                MENU_ID_TOGGLE_MODE, L"\x4E2D\x6587\x6A21\x5F0F");  // 中文模式

    // 全角
    AppendMenuW(hMenu, _bFullWidth ? MF_CHECKED : MF_UNCHECKED,
                MENU_ID_TOGGLE_WIDTH, L"\x5168\x89D2");  // 全角

    // 中文标点
    AppendMenuW(hMenu, _bChinesePunct ? MF_CHECKED : MF_UNCHECKED,
                MENU_ID_TOGGLE_PUNCT, L"\x4E2D\x6587\x6807\x70B9");  // 中文标点

    AppendMenuW(hMenu, MF_SEPARATOR, 0, NULL);

    // 显示工具栏
    AppendMenuW(hMenu, _bToolbarVisible ? MF_CHECKED : MF_UNCHECKED,
                MENU_ID_TOGGLE_TOOLBAR, L"\x663E\x793A\x5DE5\x5177\x680F");  // 显示工具栏

    AppendMenuW(hMenu, MF_SEPARATOR, 0, NULL);

    // 词库管理...
    AppendMenuW(hMenu, MF_STRING, MENU_ID_DICTIONARY, L"\x8BCD\x5E93\x7BA1\x7406...");  // 词库管理...

    // 设置...
    AppendMenuW(hMenu, MF_STRING, MENU_ID_OPEN_SETTINGS, L"\x8BBE\x7F6E...");  // 设置...

    AppendMenuW(hMenu, MF_SEPARATOR, 0, NULL);

    // 关于
    AppendMenuW(hMenu, MF_STRING, MENU_ID_ABOUT, L"\x5173\x4E8E");  // 关于

    // Note: "Exit" menu item removed - IME exit is meaningless

    WIND_LOG_INFO(L"_ShowPopupMenu: Showing popup menu\n");

    // IMPORTANT: Use WS_EX_NOACTIVATE to prevent focus change
    // Focus change would cause TextService::Deactivate and IPC disconnect
    // This means menu commands would fail to send

    // Create a temporary popup window for menu owner
    // WS_EX_NOACTIVATE prevents the window from activating and changing focus
    HWND hTempWnd = CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
        L"STATIC",  // Use static control class (always available)
        L"",
        WS_POPUP,
        pt.x, pt.y, 0, 0,  // Zero size at click position
        NULL, NULL, g_hInstance, NULL
    );

    if (hTempWnd == NULL)
    {
        WIND_LOG_ERROR(L"_ShowPopupMenu: Failed to create temp window\n");
        DestroyMenu(hMenu);
        return;
    }

    // Note: Do NOT call SetForegroundWindow - it would cause focus change
    // and trigger TextService::Deactivate, breaking IPC communication

    // Show the popup menu
    // TPM_RIGHTBUTTON: respond to right click in menu
    // TPM_RETURNCMD: return the menu item id
    UINT flags = TPM_RIGHTBUTTON | TPM_RETURNCMD;
    UINT cmd = TrackPopupMenu(hMenu, flags, pt.x, pt.y, 0, hTempWnd, NULL);

    // Post WM_NULL to ensure the menu closes properly (Microsoft KB article workaround)
    PostMessageW(hTempWnd, WM_NULL, 0, 0);

    // Clean up
    DestroyWindow(hTempWnd);
    DestroyMenu(hMenu);

    if (cmd != 0)
    {
        WIND_LOG_INFO_FMT(L"_ShowPopupMenu: Menu item selected: %d\n", cmd);
        // Call OnMenuSelect to handle the command
        OnMenuSelect(cmd);
    }
    else
    {
        WIND_LOG_DEBUG(L"_ShowPopupMenu: Menu cancelled\n");
    }
}
