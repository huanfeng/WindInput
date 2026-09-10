#include "Register.h"
#include "DisplayAttributeInfo.h"
#include "Globals.h"
#include "InstallPaths.h"   // WindResolveInstallRoot: 图标源要落在安装目录, 见 _ResolveIconFile
#include <shlwapi.h>
#include <strsafe.h>
#include <inputscope.h>

#pragma comment(lib, "shlwapi.lib")

// --- GUID 定义区 ---
// DEFINE_GUID 在未定义 INITGUID 时只展开为声明，实际取值由链接期解析（uuid.lib
// 提供 SDK 真值），手写数值并不进链接产物。下方两个类别的手抄数值历史上与 SDK
// 真值不符，现对照 msctf.h 改为真值，属防御性修正：避免将来某个 TU 引入
// initguid.h 后错值突然生效。修改此处数值必须对照 msctf.h 逐字核对，勿凭记忆手写。

// Windows 8+ 沉浸式支持
DEFINE_GUID(GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
    0x13A016DF, 0x560B, 0x46CD, 0x94, 0x7A, 0x4C, 0x3A, 0xF1, 0xE0, 0xE3, 0x5D);

// 系统托盘支持 (用于在输入指示器显示图标)
DEFINE_GUID(GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
    0x25504FB4, 0x7BAB, 0x4BC1, 0x9C, 0x69, 0xCF, 0x81, 0x89, 0x0F, 0x0E, 0xF5);

// UI 元素支持 (候选窗口等现代 UI 渲染必备)
// {49D2F9CF-1F5E-11D7-A6D3-00065B84435C}
DEFINE_GUID(GUID_TFCAT_TIPCAP_UIELEMENTENABLED,
    0x49D2F9CF, 0x1F5E, 0x11D7, 0xA6, 0xD3, 0x00, 0x06, 0x5B, 0x84, 0x43, 0x5C);

// 安全模式支持 (开始菜单搜索框/锁屏等高权限宿主可能会筛选此能力)
// {49D2F9CE-1F5E-11D7-A6D3-00065B84435C}（与 UIELEMENTENABLED 为相邻的同族 GUID 对）
DEFINE_GUID(GUID_TFCAT_TIPCAP_SECUREMODE,
    0x49D2F9CE, 0x1F5E, 0x11D7, 0xA6, 0xD3, 0x00, 0x06, 0x5B, 0x84, 0x43, 0x5C);

// --- 1. COM 服务器注册与卸载 ---
static HRESULT RegisterCOMServer()
{
    HRESULT hr = E_FAIL;
    WCHAR szModule[MAX_PATH];
    WCHAR szCLSID[39];

    if (GetModuleFileNameW(g_hInstance, szModule, ARRAYSIZE(szModule)) == 0)
        return E_FAIL;

    // 转换 CLSID 为字符串，整个文件严格依赖 c_clsidTextService，避免手误
    StringFromGUID2(c_clsidTextService, szCLSID, ARRAYSIZE(szCLSID));

    WCHAR szKey[256];
    StringCchPrintfW(szKey, ARRAYSIZE(szKey), L"CLSID\\%s", szCLSID);

    HKEY hKey;
    LONG result = RegCreateKeyExW(HKEY_CLASSES_ROOT, szKey, 0, nullptr,
                                   REG_OPTION_NON_VOLATILE, KEY_WRITE, nullptr, &hKey, nullptr);

    if (result == ERROR_SUCCESS)
    {
        RegSetValueExW(hKey, nullptr, 0, REG_SZ, (BYTE*)TEXTSERVICE_DESC,
                       (lstrlenW(TEXTSERVICE_DESC) + 1) * sizeof(WCHAR));
        RegCloseKey(hKey);

        // 注册 InprocServer32
        StringCchPrintfW(szKey, ARRAYSIZE(szKey), L"CLSID\\%s\\InprocServer32", szCLSID);
        result = RegCreateKeyExW(HKEY_CLASSES_ROOT, szKey, 0, nullptr,
                                 REG_OPTION_NON_VOLATILE, KEY_WRITE, nullptr, &hKey, nullptr);

        if (result == ERROR_SUCCESS)
        {
            RegSetValueExW(hKey, nullptr, 0, REG_SZ, (BYTE*)szModule,
                           (lstrlenW(szModule) + 1) * sizeof(WCHAR));

            // TSF 标准线程模型：Apartment（weasel / SampleIME 同为 Apartment）。
            // 历史上的 Both 系 Go 时代为「Win11 现代应用兼容」引入的 workaround 带入，
            // 原始症状已失传；此处对齐 TSF 标准做法改回 Apartment。
            RegSetValueExW(hKey, L"ThreadingModel", 0, REG_SZ, (BYTE*)L"Apartment",
                           (lstrlenW(L"Apartment") + 1) * sizeof(WCHAR));
            RegCloseKey(hKey);
            hr = S_OK;
        }
    }

    return hr;
}

static HRESULT UnregisterCOMServer()
{
    WCHAR szCLSID[39];
    WCHAR szKey[256];

    StringFromGUID2(c_clsidTextService, szCLSID, ARRAYSIZE(szCLSID));
    StringCchPrintfW(szKey, ARRAYSIZE(szKey), L"CLSID\\%s", szCLSID);

    // 递归删除 CLSID 键
    SHDeleteKeyW(HKEY_CLASSES_ROOT, szKey);

    return S_OK;
}

// --- 2. 语言配置文件 (Profile) 注册与卸载 ---
// Dota 2 兼容别名。⛔ 必须与 wind-coordinator 的 `tsf_profile_name::DOTA2_ALIAS`
// **逐字一致**（那边有一条测试把从游戏二进制里提取的字节钉死，以那份为准）。
static const wchar_t* const kDota2CompatAlias = L"中文 (简体) - 郑码";

// 注册时应当写入的显示名。
//
// ★ 用户可能开着「Dota 2 兼容」——那个开关把本输入法在系统里登记的名称改成了上面
// 那个别名（Dota 2 按名称查一张硬编码白名单，决定要不要由游戏自己绘制候选，见
// docs/design/game-compat-tsf-uielement.md §1.1）。而 RegisterProfile 是**无条件覆盖**
// Description 的：安装/升级重跑一次就把用户的设置冲掉了，没有任何提示，用户只会在
// 某次更新之后发现游戏里又打不出字，而设置页仍显示「已开启」。
//
// 故：已登记的名称若正是那个别名，就原样保留；其余情况（首次安装、名称是真名、
// 或被别的东西改成了第三种值）一律写回 TEXTSERVICE_NAME。
// ⛔ 别放宽成「保留任何非真名的值」——那会把误写与脏值也一并固化下来。
static const wchar_t* _ProfileNameToRegister()
{
    wchar_t clsid[64] = {};
    wchar_t profile[64] = {};
    if (StringFromGUID2(c_clsidTextService, clsid, ARRAYSIZE(clsid)) == 0) return TEXTSERVICE_NAME;
    if (StringFromGUID2(c_guidProfile, profile, ARRAYSIZE(profile)) == 0) return TEXTSERVICE_NAME;

    wchar_t path[512] = {};
    swprintf_s(path, L"SOFTWARE\\Microsoft\\CTF\\TIP\\%s\\LanguageProfile\\0x%08X\\%s",
               clsid, (unsigned)TEXTSERVICE_LANGID, profile);

    HKEY hKey = nullptr;
    if (RegOpenKeyExW(HKEY_LOCAL_MACHINE, path, 0, KEY_READ, &hKey) != ERROR_SUCCESS)
        return TEXTSERVICE_NAME;

    wchar_t cur[256] = {};
    DWORD cb = sizeof(cur);
    DWORD type = 0;
    LSTATUS st = RegQueryValueExW(hKey, L"Description", nullptr, &type, (LPBYTE)cur, &cb);
    RegCloseKey(hKey);
    if (st != ERROR_SUCCESS || type != REG_SZ) return TEXTSERVICE_NAME;

    if (wcscmp(cur, kDota2CompatAlias) == 0)
    {
        WIND_LOG_INFO(L"RegisterProfile: 保留用户已开启的 Dota 2 兼容别名\n");
        return kDota2CompatAlias;
    }
    return TEXTSERVICE_NAME;
}

// 输入法列表里显示的图标从哪个文件取。
//
// ★ 不能用 szModule（系统副本）——CTF 的 TIP 键连同 IconFile 是 WOW64 两个视角**共享的
// 同一份字符串**: 64 位读者按字面解析 System32\...，32 位读者则被重定向到 SysWOW64\...。
// 这套机制隐含一个前提: 两个目录里的文件必须同名。系统里的输入法都满足它
// （weasel.dll、PalmInputTSF.dll 在 System32 与 SysWOW64 下各一份、同名），而本项目的
// x64/x86 刻意不同名（wind_tsf.dll / wind_tsf_x86.dll，为的是能并排放在同一个安装目录里）。
// 于是 x86 那次注册写下的 System32\IME\<app>\wind_tsf_x86.dll 在 64 位侧根本不存在,
// 输入法列表取不到图标, 只剩「简体」两个字。x86 是后注册的, 它覆盖 x64 写下的有效值。
//
// 安装目录不在 System32 之下, 不触发 WOW64 文件重定向: 两种位数拿到同一个真实文件,
// 且 x64/x86 各写各的副本都成立 —— 顺序依赖一并消失, 不必再去调 regsvr32 的先后。
// 便携形态同理: InstallDir 指向便携目录, 那儿两个 DLL 都在。
//
// ⛔ 别改成「让两边同名」: 便携版与安装版的产物命名要保持可并存, 见上。
// 取不到安装目录副本时回退 szModule: 维持原行为, 不往注册表写一个不存在的路径。
static void _ResolveIconFile(const WCHAR* szModule, WCHAR* outPath, DWORD cchOut)
{
    WCHAR baseDir[MAX_PATH];
    if (!WindResolveInstallRoot(baseDir, ARRAYSIZE(baseDir)))
    {
        StringCchCopyW(outPath, cchOut, szModule);
        return;
    }

    const WCHAR* name = wcsrchr(szModule, L'\\');
    name = (name != nullptr) ? name + 1 : szModule;

    if (FAILED(StringCchPrintfW(outPath, cchOut, L"%s\\%s", baseDir, name)))
    {
        StringCchCopyW(outPath, cchOut, szModule);
        return;
    }

    // 安装目录里没有这个副本(部署形态不同/文件被删)时不要写进去 —— 那等于换一个坏路径。
    DWORD attr = GetFileAttributesW(outPath);
    if (attr == INVALID_FILE_ATTRIBUTES || (attr & FILE_ATTRIBUTE_DIRECTORY))
        StringCchCopyW(outPath, cchOut, szModule);
}

HRESULT RegisterProfile()
{
    HRESULT hr = E_FAIL;
    WCHAR szModule[MAX_PATH];
    WCHAR szIcon[MAX_PATH];

    if (GetModuleFileNameW(g_hInstance, szModule, ARRAYSIZE(szModule)) == 0)
        return E_FAIL;

    // 图标源与 COM 注册用的 szModule 是两件事: InprocServer32 必须指系统副本(位数要匹配),
    // 图标则必须指一个两种位数都解析得到的路径。
    _ResolveIconFile(szModule, szIcon, ARRAYSIZE(szIcon));

    // 已开启 Dota 2 兼容时保留别名，别把用户设置冲掉（见 _ProfileNameToRegister）。
    const wchar_t* profileName = _ProfileNameToRegister();

    // 首先尝试使用 Windows 8+ 的 ITfInputProcessorProfileMgr 接口
    ITfInputProcessorProfileMgr* pProfileMgr = nullptr;
    hr = CoCreateInstance(CLSID_TF_InputProcessorProfiles, nullptr, CLSCTX_INPROC_SERVER,
                          IID_ITfInputProcessorProfileMgr, (void**)&pProfileMgr);

    if (SUCCEEDED(hr) && pProfileMgr != nullptr)
    {
        // Windows 8+ 现代注册方式
        hr = pProfileMgr->RegisterProfile(
            c_clsidTextService,
            TEXTSERVICE_LANGID,
            c_guidProfile,
            profileName,
            (ULONG)wcslen(profileName),
            szIcon,
            (ULONG)wcslen(szIcon),
            TEXTSERVICE_ICON_INDEX,
            NULL,                   // hklSubstitute
            0,                      // dwPreferredLayout
            TRUE,                   // bEnabledByDefault
            0);                     // dwFlags

        if (SUCCEEDED(hr)) {
            WIND_LOG_INFO(L"RegisterProfile (ProfileMgr) succeeded\n");
        } else {
            WIND_LOG_WARN_FMT(L"RegisterProfile (ProfileMgr) failed hr=0x%08X\n", hr);
        }

        pProfileMgr->Release();
    }
    else
    {
        // 回退到旧的 ITfInputProcessorProfiles 接口 (Win7 及以下)
        ITfInputProcessorProfiles* pProfiles = nullptr;
        hr = CoCreateInstance(CLSID_TF_InputProcessorProfiles, nullptr, CLSCTX_INPROC_SERVER,
                              IID_ITfInputProcessorProfiles, (void**)&pProfiles);

        if (SUCCEEDED(hr))
        {
            hr = pProfiles->Register(c_clsidTextService);

            if (SUCCEEDED(hr))
            {
                hr = pProfiles->AddLanguageProfile(c_clsidTextService,
                                                   TEXTSERVICE_LANGID,
                                                   c_guidProfile,
                                                   profileName,
                                                   (ULONG)wcslen(profileName),
                                                   szIcon,
                                                   (ULONG)wcslen(szIcon),
                                                   TEXTSERVICE_ICON_INDEX);
            }

            if (SUCCEEDED(hr)) {
                WIND_LOG_INFO(L"RegisterProfile (legacy) succeeded\n");
            } else {
                WIND_LOG_WARN_FMT(L"RegisterProfile (legacy) failed hr=0x%08X\n", hr);
            }

            pProfiles->Release();
        }
    }

    return hr;
}

HRESULT UnregisterProfile()
{
    ITfInputProcessorProfiles* pProfiles = nullptr;
    HRESULT hr = CoCreateInstance(CLSID_TF_InputProcessorProfiles, nullptr, CLSCTX_INPROC_SERVER,
                                   IID_ITfInputProcessorProfiles, (void**)&pProfiles);

    if (SUCCEEDED(hr))
    {
        hr = pProfiles->Unregister(c_clsidTextService);
        pProfiles->Release();
    }

    return hr;
}

// --- 3. TSF 分类 (Categories) 注册与卸载 ---
HRESULT RegisterCategories()
{
    ITfCategoryMgr* pCategoryMgr = nullptr;
    HRESULT hr = CoCreateInstance(CLSID_TF_CategoryMgr, nullptr, CLSCTX_INPROC_SERVER,
                                   IID_ITfCategoryMgr, (void**)&pCategoryMgr);

    if (FAILED(hr)) return hr;

    // 与小狼毫 (weasel) 保持一致的完整分类列表，确保 Win11 开始菜单/UWP 兼容
    const GUID* categories[] = {
        &GUID_TFCAT_CATEGORY_OF_TIP,                // 基础：标识为 TIP
        &GUID_TFCAT_TIP_KEYBOARD,                   // 基础：键盘输入法
        &GUID_TFCAT_TIPCAP_SECUREMODE,              // 安全模式（锁屏/开始菜单搜索）
        &GUID_TFCAT_TIPCAP_UIELEMENTENABLED,        // UI 元素支持
        &GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT,    // 输入模式区间
        &GUID_TFCAT_TIPCAP_COMLESS,                 // 【关键】COM-less 支持，UWP/AppContainer 必需
        &GUID_TFCAT_TIPCAP_WOW16,                   // WOW16 兼容
        &GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,        // Win8+ 沉浸式应用支持
        &GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,          // 系统托盘支持
        &GUID_TFCAT_PROP_AUDIODATA,                  // 属性：音频数据
        &GUID_TFCAT_PROP_INKDATA,                    // 属性：墨迹数据
#ifndef __MINGW32__
        &GUID_TFCAT_PROPSTYLE_CUSTOM,                // 属性样式：自定义（MinGW 无权威 GUID 值，跳过；见 mingw_tsf_compat.h）
#endif
        &GUID_TFCAT_PROPSTYLE_STATIC,                // 属性样式：静态
#ifndef __MINGW32__
        &GUID_TFCAT_PROPSTYLE_STATICCOMPACT,         // 属性样式：静态紧凑（同上，MinGW 跳过）
#endif
        &GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER,        // 显示属性提供者
        &GUID_TFCAT_DISPLAYATTRIBUTEPROPERTY         // 显示属性
    };

    for (const GUID* pGuid : categories)
    {
        hr = pCategoryMgr->RegisterCategory(c_clsidTextService, *pGuid, c_clsidTextService);
        if (SUCCEEDED(hr)) {
            WIND_LOG_INFO(L"Registered category successfully\n");
        } else {
            WIND_LOG_WARN_FMT(L"Failed to register category, hr=0x%08X\n", hr);
        }
    }

    // 注册具体的显示属性关联 (Display Attribute Info)
    hr = pCategoryMgr->RegisterCategory(c_clsidTextService,
                                        GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER,
                                        c_guidDisplayAttributeInput);
    if (FAILED(hr)) {
        WIND_LOG_WARN_FMT(L"Failed to register display attribute to provider hr=0x%08X\n", hr);
    }

    pCategoryMgr->Release();
    return S_OK;
}

HRESULT UnregisterCategories()
{
    ITfCategoryMgr* pCategoryMgr = nullptr;
    HRESULT hr = CoCreateInstance(CLSID_TF_CategoryMgr, nullptr, CLSCTX_INPROC_SERVER,
                                   IID_ITfCategoryMgr, (void**)&pCategoryMgr);

    if (FAILED(hr)) return hr;

    // 必须与 RegisterCategories 中的列表完全一致，防止注册表残留
    const GUID* categories[] = {
        &GUID_TFCAT_CATEGORY_OF_TIP,
        &GUID_TFCAT_TIP_KEYBOARD,
        &GUID_TFCAT_TIPCAP_SECUREMODE,
        &GUID_TFCAT_TIPCAP_UIELEMENTENABLED,
        &GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT,
        &GUID_TFCAT_TIPCAP_COMLESS,
        &GUID_TFCAT_TIPCAP_WOW16,
        &GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
        &GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
        &GUID_TFCAT_PROP_AUDIODATA,
        &GUID_TFCAT_PROP_INKDATA,
#ifndef __MINGW32__
        &GUID_TFCAT_PROPSTYLE_CUSTOM,
#endif
        &GUID_TFCAT_PROPSTYLE_STATIC,
#ifndef __MINGW32__
        &GUID_TFCAT_PROPSTYLE_STATICCOMPACT,
#endif
        &GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER,
        &GUID_TFCAT_DISPLAYATTRIBUTEPROPERTY
    };

    for (const GUID* pGuid : categories)
    {
        pCategoryMgr->UnregisterCategory(c_clsidTextService, *pGuid, c_clsidTextService);
    }

    // 卸载具体的显示属性关联
    pCategoryMgr->UnregisterCategory(c_clsidTextService,
                                      GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER,
                                      c_guidDisplayAttributeInput);

    pCategoryMgr->Release();
    return S_OK;
}

// --- 4. 导出函数的统筹调用 ---
HRESULT RegisterServer()
{
    HRESULT hr;

    hr = CoInitialize(nullptr);
    if (FAILED(hr)) return hr;

    // 注册 COM 服务器
    hr = RegisterCOMServer();
    if (FAILED(hr)) goto Exit;

    // 注册配置文件
    hr = RegisterProfile();
    if (FAILED(hr)) goto Exit;

    // 注册分类
    hr = RegisterCategories();

Exit:
    CoUninitialize();
    return hr;
}

HRESULT UnregisterServer()
{
    HRESULT hr;

    hr = CoInitialize(nullptr);
    if (FAILED(hr)) return hr;

    // 严格按逆序卸载，确保清理干净
    UnregisterCategories();

    // 卸载配置文件
    UnregisterProfile();

    // 卸载 COM 服务器
    UnregisterCOMServer();

    CoUninitialize();
    return S_OK;
}