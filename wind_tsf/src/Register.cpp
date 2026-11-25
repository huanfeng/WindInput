#include "Register.h"
#include "DisplayAttributeInfo.h"
#include "Globals.h"
#include "InstallPaths.h"   // WindResolveInstallRoot: 图标源要落在安装目录, 见 _ResolveIconFile
#include <shlwapi.h>
#include <strsafe.h>
#include <inputscope.h>

#pragma comment(lib, "shlwapi.lib")

// --- GUID 定义区 ---
// DEFINE_GUID 在未定义 INITGUID 时只展开为声明，取值由 uuid.lib 在链接期提供，手写数值
// 不进产物；但某个 TU 一旦引入 initguid.h 就会生效，所以数值必须与 msctf.h 逐字一致。

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

            // TSF 标准线程模型 Apartment（weasel / SampleIME 相同）。
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
//
// 别名不可配置、也没有记录值时写死的 Dota 2 兼容别名，只用于认出老装机（当前值恰好等于它）。
// 它是历史常量，不跟 wind-config 的出厂值走，改了会认不出老装机。
static const wchar_t* const kLegacyDota2CompatAlias = L"中文 (简体) - 郑码";

// 「当前登记的名字是我们自己写上去的」这条记录，由 core 在落地开关时写下。
// 值名必须与写入方 wind-coordinator 的 `tsf_profile_name::ALIAS_VALUE` 一致。
static const wchar_t* const kAliasValueName = L"Dota2CompatAlias";

// 读 core 记下的别名。取不到（未开启 / 老版本 / 键不在）回 FALSE。
//
// KEY_WOW64_64KEY 不可省（同 InstallPaths.cpp 读 InstallDir）：写入方 core 是 64 位，
// 32 位的 wind_tsf_x86.dll 不加它会被重定向到 Wow6432Node 读不到别名；x86 后注册，
// 会把 x64 刚保留的 Description 覆盖回真名。
static BOOL _ReadRecordedAlias(wchar_t* out, DWORD cchOut)
{
    if (out == nullptr || cchOut == 0) return FALSE;
    out[0] = L'\0';

    HKEY hKey = nullptr;
    if (RegOpenKeyExW(HKEY_LOCAL_MACHINE, WIND_APP_REGKEY, 0, KEY_READ | KEY_WOW64_64KEY,
                      &hKey) != ERROR_SUCCESS)
        return FALSE;

    // 缓冲 256 个码元（含 NUL）是 wind-config `DOTA2_ALIAS_MAX_LEN` 的上限来源：更长的值
    // 回 ERROR_MORE_DATA，按读不到处理、静默写回真名。放宽那个上限须先放宽这里。
    DWORD type = 0;
    DWORD cb = cchOut * sizeof(wchar_t);
    LSTATUS st = RegQueryValueExW(hKey, kAliasValueName, nullptr, &type,
                                  reinterpret_cast<LPBYTE>(out), &cb);
    RegCloseKey(hKey);

    // REG_SZ 不保证以 NUL 收尾，显式收口（同 InstallPaths.cpp）。
    if (st != ERROR_SUCCESS || type != REG_SZ || cb < sizeof(wchar_t))
    {
        out[0] = L'\0';
        return FALSE;
    }
    DWORD cch = cb / sizeof(wchar_t);
    if (cch >= cchOut) { cch = cchOut - 1; }
    out[cch] = L'\0';
    return out[0] != L'\0';
}

// 读当前登记的 Description。键或值不存在时回 FALSE，这是正常情况（反注册会删掉整个键）。
static BOOL _ReadCurrentDescription(const wchar_t* path, wchar_t* out, DWORD cchOut)
{
    if (out == nullptr || cchOut == 0) return FALSE;
    out[0] = L'\0';

    HKEY hKey = nullptr;
    if (RegOpenKeyExW(HKEY_LOCAL_MACHINE, path, 0, KEY_READ, &hKey) != ERROR_SUCCESS)
        return FALSE;

    DWORD type = 0;
    DWORD cb = cchOut * sizeof(wchar_t);
    LSTATUS st = RegQueryValueExW(hKey, L"Description", nullptr, &type,
                                  reinterpret_cast<LPBYTE>(out), &cb);
    RegCloseKey(hKey);
    // ERROR_MORE_DATA（值比缓冲长）也走这里：读不全就当读不到，绝不拿半截串去比对。
    if (st != ERROR_SUCCESS || type != REG_SZ || cb < sizeof(wchar_t))
    {
        out[0] = L'\0';
        return FALSE;
    }
    // REG_SZ 未必以 NUL 收尾，比对前显式收口，否则 wcscmp 会读出缓冲外。
    DWORD cch = cb / sizeof(wchar_t);
    if (cch >= cchOut) { cch = cchOut - 1; }
    out[cch] = L'\0';
    return TRUE;
}

// 注册时应当写入的显示名。
//
// 用户开着「Valve 游戏兼容」时，登记名是用户选定的别名（见 docs/design/game-compat-tsf-uielement.md
// §1.1），而 RegisterProfile 会无条件覆盖 Description，安装/升级时须保住它。
//
// 判据不能看当前 Description：安装/升级先反注册，`ITfInputProcessorProfiles::Unregister`
// 会删掉整个 TIP\{CLSID} 键，跑到这里时它已不存在（见同文档「别名跨重装能不能留住」）。
// 故按顺序判：
//   1. 有记录值（core 开启兼容时写、关闭时删）⇒ 用它；
//   2. 无记录值但当前 Description 恰是历史别名 ⇒ 老装机且键没被删，保住，core 下次落地会补记录值；
//   3. 其余 ⇒ TEXTSERVICE_NAME。
// 别放宽成「保留任何非真名的值」，那会把误写与脏值永久固化。
//
// 返回的指针指向函数内的静态缓冲，仅在注册流程（单线程、regsvr32 里跑一次）内有效。
static const wchar_t* _ProfileNameToRegister()
{
    static wchar_t s_kept[256] = {};

    wchar_t recorded[256] = {};
    if (_ReadRecordedAlias(recorded, ARRAYSIZE(recorded)))
    {
        WIND_LOG_INFO_FMT(L"RegisterProfile: 保留用户已登记的兼容别名 [%ls]\n", recorded);
        StringCchCopyW(s_kept, ARRAYSIZE(s_kept), recorded);
        return s_kept;
    }

    wchar_t clsid[64] = {};
    wchar_t profile[64] = {};
    if (StringFromGUID2(c_clsidTextService, clsid, ARRAYSIZE(clsid)) == 0) return TEXTSERVICE_NAME;
    if (StringFromGUID2(c_guidProfile, profile, ARRAYSIZE(profile)) == 0) return TEXTSERVICE_NAME;

    wchar_t path[512] = {};
    swprintf_s(path, L"SOFTWARE\\Microsoft\\CTF\\TIP\\%s\\LanguageProfile\\0x%08X\\%s",
               clsid, (unsigned)TEXTSERVICE_LANGID, profile);

    wchar_t cur[256] = {};
    if (!_ReadCurrentDescription(path, cur, ARRAYSIZE(cur))) return TEXTSERVICE_NAME;

    if (wcscmp(cur, kLegacyDota2CompatAlias) == 0)
    {
        WIND_LOG_INFO(L"RegisterProfile: 保留老版本写下的 Dota 2 兼容别名\n");
        return kLegacyDota2CompatAlias;
    }
    return TEXTSERVICE_NAME;
}

// 输入法列表里显示的图标从哪个文件取。
//
// 取安装目录里的副本，不用 szModule（系统副本）：IconFile 是 WOW64 两个视角共享的字符串，
// 而 x64/x86 的 DLL 刻意不同名，写系统副本路径会让 64 位侧取不到图标。详见
// docs/design/game-compat-tsf-uielement.md §4.6。别改成「让两边同名」，安装版与便携版的
// 产物命名要可并存。取不到安装目录副本时回退 szModule，不写一个不存在的路径。
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