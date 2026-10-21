#include "LangBarItemButton.h"
#include "TextService.h"
#include "IPCClient.h"
#include "Globals.h"
#include "LoadingIcon.h"
#include "ServiceReadyRetryPolicy.h"
#include <olectl.h>  // For CONNECT_E_* constants
#include <shellscalingapi.h>  // GetDpiForMonitor / MDT_EFFECTIVE_DPI（符号动态取，不静态链接 shcore）

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
const UINT CLangBarItemButton::WM_SERVICE_READY = WM_USER + 104;
const UINT CLangBarItemButton::WM_ACTIVATION_STATUS = WM_USER + 105;
const UINT CLangBarItemButton::WM_REPLACE_BACKWARD = WM_USER + 106;
const UINT CLangBarItemButton::WM_PAIR_COMMIT = WM_USER + 107;
const UINT CLangBarItemButton::WM_REFRESH_ICON = WM_USER + 108;
const UINT CLangBarItemButton::WM_TITLE_MATCH_RESYNC = WM_USER + 109;

static const UINT_PTR TIMER_ID_CARET_RETRY    = 0xC401;
static const UINT_PTR TIMER_ID_SERVICE_READY  = 0xC402;
static const UINT_PTR TIMER_ID_FOCUS_RECOVERY = 0xC403;

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
    , _bIconLoading(TRUE)
    , _bLoggedLoading(FALSE)
    , _serviceReadyFailures(0)
    , _focusRecoveryFailures(0)
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

    // 文案与选择逻辑全在服务端（Rust `langbar_tooltip`），经 CONFIG_KEY_LANGBAR_TOOLTIP
    // 推来，这里只负责原样返回。
    //
    // 收归的理由：本 DLL 手里只有 _bChineseMode / _bCapsLock 两个量，判不出「密码框」
    // 「输入法被系统禁用」这些成因——而图标只能表达「不可用」，说清是哪一种正是 tooltip
    // 的职责。那些成因服务端全都有（见 Rust 侧 InputBlock），留在这边只会让同一件事有
    // 两个负责者、各说各话。
    //
    // 例外是图标正显示加载中（SHM 取不到）时：服务端推来的旧文案描述的是某个模式，
    // 与一个「…」图标对不上，这时只说正在加载。
    if (_bIconLoading)
    {
        *pbstrToolTip = SysAllocString(L"清风输入法：正在加载…");
        return (*pbstrToolTip != nullptr) ? S_OK : E_OUTOFMEMORY;
    }
    if (_pTextService != nullptr)
    {
        const std::wstring text = _pTextService->GetLangBarTooltip();
        if (!text.empty())
        {
            *pbstrToolTip = SysAllocString(text.c_str());
            return (*pbstrToolTip != nullptr) ? S_OK : E_OUTOFMEMORY;
        }
    }

    // 回落：仅在「连接建立前」这段极短窗口内走到（服务端握手时必推一次）。
    // 刻意只给一个中性文案而不在这里重建那套分支——留一份简化版判定，就是留一个会与
    // 服务端漂移的第二真相源，而漂移了也没有任何信号。
    *pbstrToolTip = SysAllocString(L"清风输入法");
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

    // Left click: Toggle mode via Go service (all state changes go through Go).
    // Go 端以 StatusUpdate 回应（含 iconLabel 完整状态），C++ 端走 UpdateFullStatus
    // 一并同步 _bChineseMode/_bFullWidth 等 mirror + TSF compartments + LangBar UI。
    if (_pTextService != nullptr)
    {
        CIPCClient* pIPCClient = _pTextService->GetIPCClient();
        if (pIPCClient != nullptr && pIPCClient->IsConnected())
        {
            ServiceResponse response;
            if (pIPCClient->SendToggleMode(response))
            {
                // 服务端 CMD_TOGGLE_MODE 的应答二选一（server.rs）：有待提交文本时回
                // CommitText，否则回 StatusUpdate。而**两种应答都意味着这段输入结束了**
                // ——服务端在 handle_toggle_mode 里已经清了自己的输入态。DLL 侧的组合是
                // 我们自己维护的，两条路都必须显式收口，否则组合残留在宿主里。
                //
                // 旧代码只在 CommitText 分支动组合，StatusUpdate 分支纯刷 UI，于是
                // keys.commit_on_switch=false（切英文不上屏原码）时编码留在原地不走，
                // 直到焦点回来才被别的清理路径收掉。
                const BOOL hasCommitText =
                    (response.type == ResponseType::CommitText && !response.text.empty());

                if (hasCommitText)
                {
                    // CommitOnSwitch 边路: 先把 pending 输入 commit，状态由随后到达的 push pipe 同步
                    // nonKeyContext=TRUE：OnClick 是 TSF 的 COM 回调（鼠标点语言栏图标），
                    // 不在按键上下文；且此路径之前没有 EndComposition，组合仍在，
                    // 走同步会话被 Word 拒时会漏编码（同 WM_COMMIT_TEXT 的病理）。
                    _pTextService->CommitText(response.text, TRUE);
                    _pTextService->SetInputMode(response.IsChineseMode());
                }
                else
                {
                    // 无待提交文本 = 服务端已丢弃这段输入：终止组合、丢弃编码。
                    // EndComposition 本身走异步会话且无组合时是 no-op，故这里无条件调用
                    // 安全；nonKeyContext 仍要传，因为它的顶码/智能符号支路会转调 CommitText。
                    WIND_LOG_DEBUG(L"OnClick: no commit text, discarding composition\n");
                    _pTextService->EndComposition(nullptr, TRUE);
                }

                // 与 compartment 模式切换路径（TextService.cpp 的 CONVERSION/OPENCLOSE）
                // 对齐：切换即一段输入结束，KeyEventSink 的组合态标志必须跟着清，否则
                // 后续快捷键仍被会话门控挡下。keepPairState=TRUE 的理由见那两处注释
                // （切换既不移光标也不消除已插入的右符号）。
                _pTextService->ResetComposingState(TRUE);

                if (response.type == ResponseType::StatusUpdate)
                {
                    _pTextService->UpdateFullStatus(
                        response.IsChineseMode(),
                        response.IsFullWidth(),
                        response.IsChinesePunct(),
                        response.IsToolbarVisible(),
                        response.IsCapsLock(),
                        response.iconLabel.empty() ? nullptr : response.iconLabel.c_str()
                    );
                }
            }
            // If IPC fails, don't toggle locally - keep state consistent with Go
        }
    }
    return S_OK;
}

STDAPI CLangBarItemButton::InitMenu(ITfMenu* pMenu)
{
    WIND_LOG_INFO(L"InitMenu called by TSF - returning empty menu (unified menu handled by service)\n");

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

// 用一块 BGRA（**非预乘**）像素建 HICON。
//
// 32bpp alpha 图标的单色掩码位图内容会被系统忽略（alpha 通道才是真正的透明度），
// 但 ICONINFO 仍要求提供一张，故建一张全 0 的。
static HICON _CreateIconFromBgra(const BYTE* bgra, int size)
{
    HDC hdcScreen = GetDC(NULL);
    if (hdcScreen == NULL)
        return NULL;
    HDC hdcMem = CreateCompatibleDC(hdcScreen);
    ReleaseDC(NULL, hdcScreen);
    if (hdcMem == NULL)
        return NULL;

    BITMAPINFO bmi = { 0 };
    bmi.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
    bmi.bmiHeader.biWidth = size;
    bmi.bmiHeader.biHeight = -size;  // Top-down DIB，与服务端行序一致
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB;

    void* pBits = nullptr;
    HBITMAP hColor = CreateDIBSection(hdcMem, &bmi, DIB_RGB_COLORS, &pBits, NULL, 0);
    if (hColor == NULL || pBits == nullptr)
    {
        if (hColor) DeleteObject(hColor);
        DeleteDC(hdcMem);
        return NULL;
    }
    memcpy(pBits, bgra, static_cast<size_t>(size) * size * 4);

    BITMAPINFO bmiMask = { 0 };
    bmiMask.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
    bmiMask.bmiHeader.biWidth = size;
    bmiMask.bmiHeader.biHeight = size;  // Bottom-up for mask (positive height)
    bmiMask.bmiHeader.biPlanes = 1;
    bmiMask.bmiHeader.biBitCount = 1;
    bmiMask.bmiHeader.biCompression = BI_RGB;

    void* pMaskBits = nullptr;
    HBITMAP hMask = CreateDIBSection(hdcMem, &bmiMask, DIB_RGB_COLORS, &pMaskBits, NULL, 0);
    if (hMask == NULL || pMaskBits == nullptr)
    {
        if (hMask) DeleteObject(hMask);
        DeleteObject(hColor);
        DeleteDC(hdcMem);
        return NULL;
    }
    const int maskRowBytes = ((size + 31) / 32) * 4;
    memset(pMaskBits, 0, static_cast<size_t>(maskRowBytes) * size);

    ICONINFO iconInfo = { 0 };
    iconInfo.fIcon = TRUE;
    iconInfo.hbmMask = hMask;
    iconInfo.hbmColor = hColor;
    HICON hIcon = CreateIconIndirect(&iconInfo);

    DeleteObject(hColor);
    DeleteObject(hMask);
    DeleteDC(hdcMem);
    return hIcon;
}

// 语言栏图标应有的边长（物理像素）。
//
// ⚠ **不能用 `GetDeviceCaps(GetDC(NULL), LOGPIXELSX)`**，那是本进程启动那一刻的
// **系统 DPI 快照**，Windows 之后一直按这个值骗它。本 DLL 加载在每一个宿主进程里，
// 于是同一台机器上会出现两种错：
//
//   · 用户改一次缩放后，所有没重启的程序图标一直糊到重启为止；
//   · 先开的程序与后开的程序给出**不同**的档位——实测记事本与 EverEdit 显示的
//     尺寸标记点数不同，也就是说其中至少一个必然是错的。
//
// 两条都靠调试用的尺寸标记（`IconRenderer::size_marks`）直接看出来的：切换缩放后
// 按 Shift 强制重画，图标确实变了、点数却不动 ⇒ 重取发生了，错在取到的值。
//
// 改取**主显示器的实时 DPI**：指示器画在任务栏上，而任务栏在主显示器
// （把窗口拖到别的屏时指示器并不跟着走，实测点数不变正与此一致）。
//
// ⚠⚠ **只换 API 不够，还必须临时抬高本线程的 DPI 感知级别。**
// `GetDpiForMonitor` 同样受调用进程的感知级别支配：DPI-unaware 进程一律得到 96，
// system-aware 进程一律得到进程启动时的系统 DPI，只有 per-monitor-aware 才拿到真值。
// 我起初以为它是不受虚拟化的显式查询，**实测被推翻**：改用它之后记事本
// （Win11 自带，per-monitor-v2）已能实时跟随，而 EverEdit（老程序）依旧不动、
// 且与记事本给出不同档位——同一台机器上对同一个全局事实出现两个答案，
// 就说明读到的不是那个事实。
//
// `SetThreadDpiAwarenessContext` 是这种「混合感知」场景的正解：它按线程临时改写
// 上下文，即使宿主进程整体声明为 unaware 也能拿到真值。窗口开得**尽可能窄**——
// 期间任何窗口/DC 操作都会跟着改变语义，故其中只放这一次查询。
//
// 动态取符号（Win10 1607+），与 `TextService.cpp` 的 `ConvertToPhysicalCoordinates`
// 同一惯例；取不到就退回原样调用，至少不比从前差。
//
// 已知局限：多任务栏时各屏 DPI 可能不同，而 `GetIcon` 只能返回**一个** HICON，
// 无法同时服侍两块屏，只能以主屏为准。
static int _LangBarIconSizePx()
{
    using SetThreadDpiAwarenessContextFn =
        DPI_AWARENESS_CONTEXT(WINAPI*)(DPI_AWARENESS_CONTEXT);
    static auto pSetThreadDpiAwarenessContext =
        reinterpret_cast<SetThreadDpiAwarenessContextFn>(
            GetProcAddress(GetModuleHandleW(L"user32.dll"), "SetThreadDpiAwarenessContext"));
    static auto pGetDpiForMonitor =
        reinterpret_cast<decltype(&GetDpiForMonitor)>(
            GetProcAddress(GetModuleHandleW(L"shcore.dll"), "GetDpiForMonitor"));

    UINT dpi = 0;
    if (pGetDpiForMonitor != nullptr)
    {
        DPI_AWARENESS_CONTEXT prevCtx = nullptr;
        if (pSetThreadDpiAwarenessContext != nullptr)
            prevCtx = pSetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        // 主显示器的左上角恒为 (0,0)，故这一点必落在主屏上。
        POINT origin = { 0, 0 };
        HMONITOR hMon = MonitorFromPoint(origin, MONITOR_DEFAULTTOPRIMARY);
        UINT dpiX = 0, dpiY = 0;
        if (hMon != nullptr && SUCCEEDED(pGetDpiForMonitor(hMon, MDT_EFFECTIVE_DPI, &dpiX, &dpiY)))
            dpi = dpiX;

        // 必须无条件还原：本函数由语言栏在宿主线程上回调，留下一个被抬高的上下文
        // 会让宿主后续的窗口/坐标操作换一套语义，那种缺陷与图标毫无表面关联。
        if (prevCtx != nullptr)
            pSetThreadDpiAwarenessContext(prevCtx);
    }
    if (dpi == 0)
    {
        // shcore 不可用（Win8 以前）时退回旧取法：不准，但好过没有。
        HDC hdc = GetDC(NULL);
        if (hdc != NULL)
        {
            dpi = (UINT)GetDeviceCaps(hdc, LOGPIXELSX);
            ReleaseDC(NULL, hdc);
        }
    }
    if (dpi == 0)
        dpi = 96;

    int iconSize = MulDiv(16, (int)dpi, 96);
    // 钳到档位表两端。上限 48 = 300%；再往上仍会被系统放大，而放大是实测最糊的情形
    // （同机对照：原生无缩放那档明显更清晰），故再加档要连 SHM 一起放大。
    if (iconSize < 16) iconSize = 16;
    if (iconSize > 48) iconSize = 48;
    return iconSize;
}

STDAPI CLangBarItemButton::GetIcon(HICON* phIcon)
{
    if (phIcon == nullptr)
        return E_INVALIDARG;

    *phIcon = nullptr;

    WIND_LOG_TRACE(L"GetIcon called\n");

    const int iconSize = _LangBarIconSizePx();

    // ── 优先取服务端预渲染的图标 ──
    //
    // 无条件优先取服务端预渲染图标。
    //
    // 这里曾有一道 localOnlyState 旁路：密码框 / 无可编辑上下文 / 键盘禁用三档改走本地
    // 绘制，理由是「服务端无从得知」。判定收归服务端之后该前提不再成立——那三档现在由
    // 服务端渲进 SHM（含变淡），本地只剩「服务没起来」时的加载中图标。
    // 留着旁路的代价是同一件事有两个渲染实现，迟早各说各话。
    {
        std::vector<BYTE> shmPixels;
        int shmSize = 0;
        uint32_t shmSeq = 0;
        if (_iconShm.ReadVariant(iconSize, _bDarkMode != FALSE, shmPixels, shmSize, &shmSeq))
        {
            HICON hIcon = _CreateIconFromBgra(shmPixels.data(), shmSize);
            if (hIcon != NULL)
            {
                _bIconLoading = FALSE;
                // seq 是判断「这一帧是不是最新版」的唯一可靠依据：与服务端
                // 「语言栏图标已发布 seq=N」直接对号。此前只能按两侧时间戳去凑，
                // 而毫秒级的陈旧读恰恰是时间戳最凑不准的场合（见 ReadVariant 注释）。
                WIND_LOG_DEBUG_FMT(L"GetIcon: from SHM seq=%u want=%d got=%d dark=%d\n",
                                   shmSeq,
                                   iconSize, shmSize, _bDarkMode);
                *phIcon = hIcon;
                return S_OK;
            }
        }
    }

    // ── 加载中 ──
    // 服务未启动、SHM 尚未发布、或读取失败时走这里。
    // 这条路径**不可删除**：DLL 加载在每一个宿主进程里，而服务的可用性无法保证。
    //
    // 这里**刻意不再画模式主字**。主字颜色由主题 / 用户配置决定，只有服务端知道；
    // 本地按黑白画出来的「中/英」与服务端那份时而不同，用户看到的就是颜色时有时无，
    // 且无从分辨「服务没就绪」与「正常状态」。画一个与任何主字都不像的「…」即可区分。
    // 键盘禁用的变淡也不适用：那是服务端才判得出的状态，由服务端渲进 SHM。
    //
    // 兼容旧版服务（不发布图标 SHM 的 v0.117 以前）时同样显示加载中，不回退主字：
    // 协议里没有能区分「旧服务」与「新服务尚未发布」的判据（PROTOCOL_VERSION 自
    // v1.1 起未变、握手不带服务版本；两者在 DLL 看来都只是 OpenFileMappingW 失败），
    // 而 DLL 与服务同一安装包发布，新 DLL + 旧服务只会出现在异常的半升级状态里。
    // 为这种状态保留一整套 DirectWrite 主字绘制，代价是继续背着上面那个颜色问题。
    const std::vector<uint8_t> loadingPixels =
        loading_icon::RenderBgra(iconSize, _bDarkMode != FALSE);
    HICON hIcon = _CreateIconFromBgra(loadingPixels.data(), iconSize);
    if (hIcon == NULL)
    {
        WIND_LOG_ERROR(L"GetIcon: create loading icon failed\n");
        return E_FAIL;
    }
    if (!_bIconLoading || !_bLoggedLoading)
    {
        // 只在进入加载中的那一下记一条：GetIcon 由系统按需回调，每次都记会刷屏。
        // _bLoggedLoading 兜住「服务从头到尾没起来」：那时 _bIconLoading 初值即 TRUE，
        // 永远不经过 FALSE→TRUE，却正是最需要这条日志的现场。
        _bLoggedLoading = TRUE;
        WIND_LOG_INFO_FMT(L"GetIcon: icon SHM unavailable, showing loading icon size=%d\n",
                          iconSize);
    }
    _bIconLoading = TRUE;
    *phIcon = hIcon;
    return S_OK;
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
            // 走 TextService::UpdateFullStatus 而非本类的 UpdateFullStatus —— 前者会
            // 顺带 _SetOpenCloseCompartment + _SetConversionMode 同步 TSF 全局 compartments,
            // 否则 push pipe 推过来的状态变更会让 _bChineseMode 与 TSF 系统 compartment
            // 出现 drift, 表现为 Ctrl+Space 失效 / 任务栏图标不刷新。
            // TextService::UpdateFullStatus 内部会 cascade 调本类的 UpdateFullStatus，
            // 因此 LangBar UI 仍然会被刷新。
            if (pThis->_pTextService != nullptr)
            {
                pThis->_pTextService->UpdateFullStatus(
                    pData->bChineseMode, pData->bFullWidth,
                    pData->bChinesePunct, pData->bToolbarVisible, pData->bCapsLock,
                    pData->iconLabel[0] != L'\0' ? pData->iconLabel : nullptr);
            }
            else
            {
                // Fallback: 没有 TextService 时只刷新 LangBar UI
                pThis->UpdateFullStatus(pData->bChineseMode, pData->bFullWidth,
                                         pData->bChinesePunct, pData->bToolbarVisible, pData->bCapsLock,
                                         pData->iconLabel[0] != L'\0' ? pData->iconLabel : nullptr);
            }
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
            // EditSession. The previous separate EndComposition(async) +
            // InsertText(sync) approach caused a race condition where the async
            // EndComposition could clear the just-inserted text, especially in apps
            // like VSCode and browsers when InlinePreedit is disabled.
            //
            // 改走合成提交键：缓冲文本 + 自注入触发键，把提交挪到 OnKeyDown 里以按键
            // 上下文同步执行，而不是在这里（裸窗口消息回调，鼠标点候选经 push 通道
            // 过来）直接发起异步会话。
            //
            // 这不只是把 `CommitText(text, TRUE)` 的已知问题（Word 拒同步会话导致
            // "Sfge杜甫" 那类组合孤儿 finalize + 重复上屏）换个方式绕开——按键上下文本身
            // 会触发宿主自己的输入时处理链路（AutoCorrect/内嵌 `\n` 转真实分段……），
            // 这些在纯异步 EditSession 里不会发生，是 CommitTextViaSyntheticKey 存在
            // 的主要原因。注入失败时内部退回旧的 `CommitText(text, TRUE)`，不会丢字。
            pThis->_pTextService->CommitTextViaSyntheticKey(pData->text);
            // Reset KeyEventSink state so shortcut keys work again.
            // 保留配对状态：上屏只是在光标处插入文本，已配对的右符号仍在光标右侧
            // （`（你好|）`）。此前一并清零 _pairPendingDepth，导致中文模式下
            // 「输左符号 → 打字上屏 → 按 Enter」跳不出去（Enter 被会话门控挡下不转发；
            // Tab 因中文模式无条件转发才幸免）。
            pThis->_pTextService->ResetComposingState(TRUE);
        }

        // Free the data allocated by sender
        delete pData;
        return 0;
    }
    else if (msg == WM_REPLACE_BACKWARD)
    {
        // lParam contains pointer to ReplaceBackwardData (allocated by sender)
        ReplaceBackwardData* pData = reinterpret_cast<ReplaceBackwardData*>(lParam);
        CLangBarItemButton* pThis = reinterpret_cast<CLangBarItemButton*>(GetWindowLongPtrW(hwnd, GWLP_USERDATA));

        if (pThis != nullptr && pData != nullptr && pThis->_pTextService != nullptr)
        {
            WIND_LOG_DEBUG_FMT(L"MsgWndProc: Processing WM_REPLACE_BACKWARD, count=%d, textLen=%zu\n",
                               pData->count, pData->text.length());
            // Same primitive as smart-symbol correction: atomic TSF range replace
            // first, synthetic-key fallback inside (undo commit push path).
            pThis->_pTextService->ReplacePrecedingChars(pData->count, pData->text);
        }

        // Free the data allocated by sender
        delete pData;
        return 0;
    }
    else if (msg == WM_PAIR_COMMIT)
    {
        // lParam contains pointer to PairCommitData (allocated by sender)
        PairCommitData* pData = reinterpret_cast<PairCommitData*>(lParam);
        CLangBarItemButton* pThis = reinterpret_cast<CLangBarItemButton*>(GetWindowLongPtrW(hwnd, GWLP_USERDATA));

        if (pThis != nullptr && pData != nullptr && pThis->_pTextService != nullptr)
        {
            WIND_LOG_DEBUG_FMT(L"MsgWndProc: Processing WM_PAIR_COMMIT, textLen=%zu, moveLeft=%u\n",
                               pData->text.length(), pData->moveLeft);
            // 必须在 UI（TSF）线程上做：CommitText 要开 EditSession，合成 VK_LEFT 也依赖
            // 本线程的输入状态。
            pThis->_pTextService->HandlePairCommitPush(pData->text, pData->moveLeft);
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
            // 部署指纹（勿删）：本分支的改动是「把一个默认参数改成 TRUE」，二进制里不产生
            // 任何新符号，光看时间戳无法区分「编进去了」与「增量跳过了」。这句话里的
            // keep_pair_state 就是那个可 grep 的锚点 —— DLL 里搜得到 = 编进去了，
            // 部署目录那份搜得到 = 换上了，真机日志出现本行 = 运行时确实加载了新 DLL。
            WIND_LOG_DEBUG(L"MsgWndProc: Processing WM_CLEAR_COMPOSITION keep_pair_state\n");
            pThis->_pTextService->EndComposition();
            // keepPairState=TRUE：**保留**自动配对状态。本消息只服务于服务端经 push 主动推来的
            // CMD_CLEAR_COMPOSITION，四个投递方（中英/方案切换的无按键路径、联想自动隐藏超时的
            // 收口、服务重启、鼠标点命令候选）都不移动光标、也不消除已插入的右符号，配对的前提
            // 「光标紧贴一个右符号」仍然成立 —— 与按键路径的中英切换取同一判据（见
            // CTextService 里两处 ResetComposingState(TRUE) 的注释）。
            //
            // ⚠️ 曾用默认值（连配对栈一起清），症状：把 jump_out_keys 配成 tab 的用户，在 ()
            // 里打字上屏后手停 5 秒，联想自动隐藏的收口把 _pairPendingDepth 清零，随后按 Tab
            // 不再被吃 —— 那个 depth 正是「Tab 要不要转发给协调器裁决」的唯一闸门（本文件
            // OnTestKeyDown 的 pair_jumpout_forward 分支），于是跳出退化成插入一个制表符。
            // 按键路径的 ResponseType::ClearComposition 分支从不碰 depth，两条路因此行为分叉。
            //
            // 服务重启那一条会让 DLL 保留 depth 而服务端 pair_tracker 是新的（空）——
            // 该 desync 由 OnKeyDown 的 pair_jumpout_desync_replay 分支自愈（以 core 为准把本地
            // depth 归零并重放键），不需要在此提前清。
            pThis->_pTextService->ResetComposingState(TRUE);
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
    else if (msg == WM_ACTIVATION_STATUS)
    {
        // lParam = heap-allocated ServiceResponse* (拥有权移交本 handler, 由本 handler delete)。
        // 触发链：Go push pipe → CIPCClient::_AsyncReaderLoop → _activationPushCallback
        //         → CTextService 的 lambda 调 PostActivationStatus → 本 handler。
        // 等价于原同步路径 _DoFullStateSync 收到 ReceiveResponse 后调
        // _SyncStateFromResponse + _EnsureHostRenderSetup。
        ServiceResponse* pResp = reinterpret_cast<ServiceResponse*>(lParam);
        CLangBarItemButton* pThis = reinterpret_cast<CLangBarItemButton*>(GetWindowLongPtrW(hwnd, GWLP_USERDATA));
        if (pThis != nullptr && pResp != nullptr && pThis->_pTextService != nullptr)
        {
            WIND_LOG_DEBUG(L"MsgWndProc: Processing WM_ACTIVATION_STATUS\n");
            pThis->_pTextService->ApplyActivationStatusResponse(*pResp);
        }
        delete pResp;
        return 0;
    }
    else if (msg == WM_TITLE_MATCH_RESYNC)
    {
        // 标题采集开关 0→1（core 握手 / compat 重载推来）：首个 focus_gained 可能早于开关到达、
        // 没带标题，在 TSF 线程上补发一次（SendFocusGained 是同步 IPC，不能在 reader 线程发）。
        CLangBarItemButton* pThis = reinterpret_cast<CLangBarItemButton*>(GetWindowLongPtrW(hwnd, GWLP_USERDATA));
        if (pThis != nullptr && pThis->_pTextService != nullptr)
            pThis->_pTextService->ResyncFocusForTitleMatch();
        return 0;
    }
    else if (msg == WM_SERVICE_READY)
    {
        CLangBarItemButton* pThis = reinterpret_cast<CLangBarItemButton*>(GetWindowLongPtrW(hwnd, GWLP_USERDATA));
        if (pThis != nullptr && pThis->_pTextService != nullptr)
        {
            // 服务刚证明自己活着：就绪前攒下的连接失败（管道不存在时）已打开的熔断器作废，
            // 否则下面的 Connect 会在熔断窗口里直接跳过。
            CIPCClient* pIPC = pThis->_pTextService->GetIPCClient();
            if (pIPC != nullptr)
            {
                pIPC->ResetCircuitBreaker();
                // 服务重启后主管道句柄是旧连接，无焦点时下面不同步，它会一直留到下次焦点 /
                // 按键才在写入时暴露。此刻就丢掉，下一次发送直接新建连接。活连接不受影响。
                pIPC->DiscardStaleConnection();
            }
            pThis->_serviceReadyFailures = 0;
            pThis->_RunServiceReadySync(hwnd);
        }
        return 0;
    }
    else if (msg == WM_REFRESH_ICON)
    {
        // 合并积压：演示动画以固定帧率持续投递，宿主 UI 线程一旦卡顿，队列里会攒下一串
        // 同类消息，恢复后连着重绘（表现为动画"追帧"）。它们彼此完全等价——图标内容的
        // 真相在共享内存里而不在消息里——所以只处理最后一条即可。
        // 可以在这里 Peek 是因为本函数就跑在拥有该队列的 TSF 线程上；投递侧（AsyncReader
        // 线程）做不到这件事。
        MSG discarded;
        while (PeekMessageW(&discarded, hwnd, WM_REFRESH_ICON, WM_REFRESH_ICON, PM_REMOVE))
        {
        }

        CLangBarItemButton* pThis = reinterpret_cast<CLangBarItemButton*>(GetWindowLongPtrW(hwnd, GWLP_USERDATA));
        if (pThis != nullptr && pThis->_pLangBarItemSink != nullptr)
        {
            // 平时只报 TF_LBI_ICON：TEXT/TOOLTIP 没变，一起报只会让系统多查几次，而本命令
            // 的调用频率可以很高（演示动画每帧一次）。
            // 例外：还在「加载中」时 tooltip 是「正在加载…」，服务首次发布图标正是走这里，
            // 只报 ICON 的话图标已恢复、悬停却一直停在「正在加载…」。GetIcon 会在这次
            // 回调里把 _bIconLoading 清掉，故这一带只发生一次。
            DWORD flags = TF_LBI_ICON;
            if (pThis->_bIconLoading)
                flags |= TF_LBI_TOOLTIP;
            pThis->_pLangBarItemSink->OnUpdate(flags);
        }
        return 0;
    }
    else if (msg == WM_TIMER)
    {
        if (wParam == TIMER_ID_FOCUS_RECOVERY)
        {
            KillTimer(hwnd, wParam);
            CLangBarItemButton* pThis = reinterpret_cast<CLangBarItemButton*>(GetWindowLongPtrW(hwnd, GWLP_USERDATA));
            if (pThis != nullptr && pThis->_pTextService != nullptr)
                pThis->_RunFocusRecoveryRetry(hwnd);
            return 0;
        }
        if (wParam == TIMER_ID_SERVICE_READY)
        {
            CLangBarItemButton* pThis = reinterpret_cast<CLangBarItemButton*>(GetWindowLongPtrW(hwnd, GWLP_USERDATA));
            if (pThis != nullptr && pThis->_pTextService != nullptr)
                pThis->_RunServiceReadySync(hwnd);
            else
                KillTimer(hwnd, wParam);
            return 0;
        }
        if (wParam == TIMER_ID_CARET_RETRY)
        {
            KillTimer(hwnd, wParam);
            CLangBarItemButton* pThis = reinterpret_cast<CLangBarItemButton*>(GetWindowLongPtrW(hwnd, GWLP_USERDATA));
            const BOOL hasComp = (pThis != nullptr && pThis->_pTextService != nullptr
                                  && pThis->_pTextService->HasActiveComposition());
            // ⚠ 这行日志不可省。`WM_TIMER` 是消息队列里**优先级最低的合成消息**，只在队列
            // 空闲时才生成，宿主忙时会被无限期饿死。2026-08-03 排查 Excel「点单元格后首字
            // 错位」时，本 timer 在 680ms 的重排窗口里一次都没触发，而当时成功路径**没有
            // 任何日志**，只能靠「这一整段零日志」反推它没跑——绕了一大圈。
            // **判决点必须自己说话，不能靠周围的沉默去猜**：无论走哪条分支都要留下痕迹。
            WIND_LOG_DEBUG_FMT(L"CARET_RETRY timer fired: hasComposition=%d\n", hasComp ? 1 : 0);
            if (hasComp)
            {
                // Timer 兜底：清除 _compositionJustStarted 让取坐标走正常路径
                // （消费已缓存的坐标，应对不发 OnLayoutChange 的应用）。
                pThis->_pTextService->ClearCompositionJustStarted();

                // ⚠ 这里是 WM_TIMER，不是按键上下文——同步 edit session 在这里会被宿主合法
                // 拒绝（Word 实测 TS_E_SYNCHRONOUS 15/15），而 SendCaretPositionUpdate 失败后
                // 会回退到 GetGUIThreadInfo 的 Win32 caret。Word 只在正文行维护那个 caret，
                // 标题等非正文样式行上它指向别处，候选窗因此错位数百像素。
                // 故这条路径改用异步 edit session：宿主会把请求排队，等文档可用时回调。
                // 发不出去才退回同步路径（非 TSF 宿主仍需要 GUIThreadInfo 那条链）。
                //
                // kind 必须显式给 RetryDeadline，不能沿用默认的 Composition：本次取坐标是
                // 「**重试窗口已经过完**」这个判决的唯一载体，回调据此把仍然退化的矩形当作
                // 宿主的「本 context 没有插入点可报」采信（CARET_SRC_TSF_DEFAULT_POS），
                // 而不是继续丢弃、跌到 GUIThreadInfo 的无关光标上。见 CaretProbeKind。
                if (!pThis->_pTextService->RequestCaretPositionUpdateAsync(CaretProbeKind::RetryDeadline))
                {
                    WIND_LOG_DEBUG(L"CARET_RETRY timer: async request not issued, falling back to sync path\n");
                    pThis->_pTextService->SendCaretPositionUpdate();
                }
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
            // _TRUNCATE 而非 wcscpy_s：后者遇超长**不是截断而是调用 invalid parameter
            // handler**，默认行为是终止进程——而这段代码跑在 Word / QQ 等宿主进程里。
            // Rust 侧已把标签卡在 2 个字符（wind_config::ICON_LABEL_MAX_CHARS），这条路
            // 本不该触发；留着它是防将来某条新路径绕过那道截断，把"显示被截断"和
            // "宿主进程崩溃"这两种后果的差距抹平。
            wcsncpy_s(_inputTypeLabel, iconLabel, _TRUNCATE);
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
        // 同 UpdateFullStatus：_TRUNCATE 把"超长即终止进程"降级成"超长即截断"。
        wcsncpy_s(pData->iconLabel, iconLabel, _TRUNCATE);
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

void CLangBarItemButton::PostPairCommit(const std::wstring& text, uint32_t moveLeft)
{
    // 同 PostReplaceBackward：必须回到 UI（TSF）线程，没有消息窗就只能丢弃并记日志。
    if (_hMsgWnd == NULL)
    {
        WIND_LOG_WARN(L"PostPairCommit: No message window, dropping pair push\n");
        return;
    }

    // Allocate data on heap (will be freed by message handler)
    PairCommitData* pData = new PairCommitData();
    pData->text = text;
    pData->moveLeft = moveLeft;

    if (!PostMessageW(_hMsgWnd, WM_PAIR_COMMIT, 0, reinterpret_cast<LPARAM>(pData)))
    {
        delete pData;
        WIND_LOG_WARN(L"PostPairCommit: PostMessage failed, dropping pair push\n");
    }
    else
    {
        WIND_LOG_DEBUG_FMT(L"PostPairCommit: Message posted to UI thread, moveLeft=%u\n", moveLeft);
    }
}

void CLangBarItemButton::PostReplaceBackward(int count, const std::wstring& text)
{
    // Thread-safe: post to the message window so ReplacePrecedingChars runs on
    // the UI (TSF) thread. Unlike PostCommitText there is no meaningful direct
    // fallback — ReplacePrecedingChars needs the TSF thread — so without a
    // message window the push is dropped (logged).
    if (_hMsgWnd == NULL)
    {
        WIND_LOG_WARN(L"PostReplaceBackward: No message window, dropping undo push\n");
        return;
    }

    // Allocate data on heap (will be freed by message handler)
    ReplaceBackwardData* pData = new ReplaceBackwardData();
    pData->count = count;
    pData->text = text;

    if (!PostMessageW(_hMsgWnd, WM_REPLACE_BACKWARD, 0, reinterpret_cast<LPARAM>(pData)))
    {
        delete pData;
        WIND_LOG_WARN(L"PostReplaceBackward: PostMessage failed, dropping undo push\n");
    }
    else
    {
        WIND_LOG_DEBUG_FMT(L"PostReplaceBackward: Message posted to UI thread, count=%d\n", count);
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

void CLangBarItemButton::PostRefreshIcon()
{
    if (_hMsgWnd == NULL)
    {
        // 不退回直接调用（对比 PostUpdateFullStatus 的兜底）：OnUpdate 是 COM 调用，
        // 必须在 TSF 线程上发，而本函数跑在 AsyncReader 线程。没有消息窗口就只能放弃
        // 这次刷新——代价仅仅是图标晚一步更新，而跨线程碰 COM 的代价是未定义行为。
        WIND_LOG_WARN(L"PostRefreshIcon: No message window, skipping\n");
        return;
    }
    if (!PostMessageW(_hMsgWnd, WM_REFRESH_ICON, 0, 0))
        WIND_LOG_WARN(L"PostRefreshIcon: PostMessage failed\n");
}

void CLangBarItemButton::PostTitleMatchResync()
{
    if (_hMsgWnd == NULL)
    {
        WIND_LOG_WARN(L"PostTitleMatchResync: No message window, skipping\n");
        return;
    }
    if (!PostMessageW(_hMsgWnd, WM_TITLE_MATCH_RESYNC, 0, 0))
        WIND_LOG_WARN(L"PostTitleMatchResync: PostMessage failed\n");
}

void CLangBarItemButton::_RunServiceReadySync(HWND hwnd)
{
    using wind::svcready::Action;

    KillTimer(hwnd, TIMER_ID_SERVICE_READY);
    const BOOL isRetry = _serviceReadyFailures > 0;
    CIPCClient* pIPC = _pTextService->GetIPCClient();
    const Action action = wind::svcready::Decide(
        isRetry != FALSE, _pTextService->HasFocus() != FALSE, _pTextService->HasThreadFocus() != FALSE,
        _pTextService->HasActiveComposition() != FALSE, pIPC != nullptr && pIPC->IsConnected(),
        pIPC != nullptr && pIPC->StateEstablished());

    switch (action)
    {
    case Action::StopNoFocus:
        // 当前实例无输入焦点，跳过同步以避免在无文本焦点时弹出工具栏。
        // 待焦点到来时 OnSetFocus → SendFocusGained 会完整建立服务端状态。
        WIND_LOG_INFO(L"MsgWndProc: SERVICE_READY — no focus, skipping state sync\n");
        _serviceReadyFailures = 0;
        return;
    case Action::StopAlreadySynced:
        WIND_LOG_INFO(L"MsgWndProc: SERVICE_READY retry — state already synced on this connection, stopping\n");
        _serviceReadyFailures = 0;
        return;
    case Action::StopBackground:
        WIND_LOG_INFO(L"MsgWndProc: SERVICE_READY retry — thread not in foreground, stopping\n");
        _serviceReadyFailures = 0;
        return;
    case Action::DeferComposition:
        // Composition active — defer sync to avoid clearing the service's input buffer.
        // TSF composition is always finite: user commits, cancels, or focus changes.
        // 推迟不计失败次数：它与发送失败的重试共用定时器，靠 _serviceReadyFailures 区分。
        WIND_LOG_DEBUG(L"MsgWndProc: SERVICE_READY deferred (composition active), scheduling retry\n");
        SetTimer(hwnd, TIMER_ID_SERVICE_READY, 500, nullptr);
        return;
    case Action::Sync:
        break;
    }

    WIND_LOG_INFO(L"MsgWndProc: SERVICE_READY — running full state sync\n");
    if (_pTextService->_DoFullStateSync(TRUE))
    {
        if (isRetry)
            WIND_LOG_INFO_FMT(L"MsgWndProc: SERVICE_READY sync succeeded after %u retries\n", _serviceReadyFailures);
        _serviceReadyFailures = 0;
        return;
    }

    // 宿主多时主管道会被同时就绪的各线程挤满（ERROR_PIPE_BUSY）。SERVICE_READY 只来一次，
    // 服务端又只在收到 IME_ACTIVATED 时显示工具栏，不重试就要等到用户按键。
    ++_serviceReadyFailures;
    if (wind::svcready::ShouldRetry(_serviceReadyFailures))
    {
        const UINT delay = wind::svcready::RetryDelayMs(GetCurrentThreadId(), _serviceReadyFailures);
        WIND_LOG_INFO_FMT(L"MsgWndProc: SERVICE_READY sync failed, retry %u/%u in %ums\n",
                          _serviceReadyFailures, wind::svcready::kMaxRetries, delay);
        SetTimer(hwnd, TIMER_ID_SERVICE_READY, delay, nullptr);
        return;
    }

    WIND_LOG_WARN_FMT(L"MsgWndProc: SERVICE_READY sync gave up after %u retries, toolbar waits for next key or focus\n",
                      wind::svcready::kMaxRetries);
    _serviceReadyFailures = 0;
}

void CLangBarItemButton::CancelServiceReadyRetry()
{
    if (_hMsgWnd == NULL)
        return;
    // KillTimer 对未挂的定时器返回 FALSE，借此只在真有待发同步时留痕。
    if (KillTimer(_hMsgWnd, TIMER_ID_SERVICE_READY))
        WIND_LOG_INFO_FMT(L"SERVICE_READY pending sync cancelled (state synced by key/focus path, failures=%u)\n",
                          _serviceReadyFailures);
    _serviceReadyFailures = 0;
    if (KillTimer(_hMsgWnd, TIMER_ID_FOCUS_RECOVERY))
        WIND_LOG_INFO(L"Focus resend retry cancelled (state synced by key/focus path)\n");
    _focusRecoveryFailures = 0;
}

void CLangBarItemButton::ScheduleFocusRecoveryRetry()
{
    if (_hMsgWnd == NULL)
        return;
    // 每次 focus_gained 失败都重新起一轮；与就绪重试各用各的定时器和计数。
    _focusRecoveryFailures = 1;
    const UINT delay = wind::svcready::RetryDelayMs(GetCurrentThreadId(), _focusRecoveryFailures);
    WIND_LOG_INFO_FMT(L"FocusGained failed, focus resend retry in %ums\n", delay);
    SetTimer(_hMsgWnd, TIMER_ID_FOCUS_RECOVERY, delay, nullptr);
}

void CLangBarItemButton::_RunFocusRecoveryRetry(HWND hwnd)
{
    if (_pTextService->RetryFocusRecovery())
    {
        _focusRecoveryFailures = 0;
        return;
    }
    ++_focusRecoveryFailures;
    if (wind::svcready::ShouldRetry(_focusRecoveryFailures))
    {
        const UINT delay = wind::svcready::RetryDelayMs(GetCurrentThreadId(), _focusRecoveryFailures);
        WIND_LOG_INFO_FMT(L"Focus resend failed, retry %u/%u in %ums\n", _focusRecoveryFailures,
                          wind::svcready::kMaxRetries, delay);
        SetTimer(hwnd, TIMER_ID_FOCUS_RECOVERY, delay, nullptr);
        return;
    }
    WIND_LOG_WARN_FMT(L"Focus resend gave up after %u retries, toolbar waits for next key or focus\n",
                      wind::svcready::kMaxRetries);
    _focusRecoveryFailures = 0;
}

void CLangBarItemButton::PostServiceReady()
{
    if (_hMsgWnd == NULL)
    {
        WIND_LOG_WARN(L"PostServiceReady: No message window, skipping\n");
        return;
    }
    if (!PostMessageW(_hMsgWnd, WM_SERVICE_READY, 0, 0))
        WIND_LOG_WARN(L"PostServiceReady: PostMessage failed\n");
    else
        WIND_LOG_DEBUG(L"PostServiceReady: Message posted to TSF thread\n");
}

void CLangBarItemButton::PostActivationStatus(const ServiceResponse& response)
{
    if (_hMsgWnd == NULL)
    {
        WIND_LOG_WARN(L"PostActivationStatus: No message window, skipping\n");
        return;
    }
    // 拷贝一份 ServiceResponse 到堆上, 让 TSF 线程的 handler 取走 ownership 后 delete。
    // 不能用栈对象——PostMessageW 是异步的, 调用栈解开后 response 引用会悬空。
    ServiceResponse* pCopy = new ServiceResponse(response);
    if (!PostMessageW(_hMsgWnd, WM_ACTIVATION_STATUS, 0, reinterpret_cast<LPARAM>(pCopy)))
    {
        delete pCopy;
        WIND_LOG_WARN(L"PostActivationStatus: PostMessage failed\n");
    }
    else
    {
        WIND_LOG_DEBUG(L"PostActivationStatus: Message posted to TSF thread\n");
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

    // 同 UpdateFullStatus：本方法收外部传入的任意长度字符串，wcscpy_s 遇超长会终止
    // 宿主进程。_TRUNCATE 把它降级成截断。
    wcsncpy_s(_inputTypeLabel, label, _TRUNCATE);

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
    WIND_LOG_INFO_FMT(L"_ShowPopupMenu: Sending context menu request to service at (%ld, %ld)\n", pt.x, pt.y);

    if (_pTextService != nullptr)
    {
        _pTextService->SendShowContextMenu(pt.x, pt.y);
    }
}
