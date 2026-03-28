#include "Register.h"
#include "DisplayAttributeInfo.h"
#include "Globals.h"
#include <shlwapi.h>
#include <strsafe.h>
#include <inputscope.h>

#pragma comment(lib, "shlwapi.lib")

// --- GUID 定义区 ---

// Windows 8+ 沉浸式支持
DEFINE_GUID(GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
    0x13A016DF, 0x560B, 0x46CD, 0x94, 0x7A, 0x4C, 0x3A, 0xF1, 0xE0, 0xE3, 0x5D);

// 系统托盘支持 (用于在输入指示器显示图标)
DEFINE_GUID(GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
    0x25504FB4, 0x7BAB, 0x4BC1, 0x9C, 0x69, 0xCF, 0x81, 0x89, 0x0F, 0x0E, 0xF5);

// UI 元素支持 (候选窗口等现代 UI 渲染必备)
DEFINE_GUID(GUID_TFCAT_TIPCAP_UIELEMENTENABLED,
    0x6D60FCCF, 0x58D7, 0x4B67, 0xB1, 0x3E, 0x96, 0xBE, 0x70, 0x6C, 0x3B, 0x6A);

// 安全模式支持 (开始菜单搜索框/锁屏等高权限宿主可能会筛选此能力)
// {3527B835-7383-4978-83C8-FD20C6F5A0E0}
DEFINE_GUID(GUID_TFCAT_TIPCAP_SECUREMODE,
    0x3527B835, 0x7383, 0x4978, 0x83, 0xC8, 0xFD, 0x20, 0xC6, 0xF5, 0xA0, 0xE0);

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

            // 【关键修复】：将 ThreadingModel 修改为 Both 以增强 Win11 现代应用兼容性
            RegSetValueExW(hKey, L"ThreadingModel", 0, REG_SZ, (BYTE*)L"Both",
                           (lstrlenW(L"Both") + 1) * sizeof(WCHAR));
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
HRESULT RegisterProfile()
{
    HRESULT hr = E_FAIL;
    WCHAR szModule[MAX_PATH];

    if (GetModuleFileNameW(g_hInstance, szModule, ARRAYSIZE(szModule)) == 0)
        return E_FAIL;

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
            TEXTSERVICE_NAME,
            (ULONG)wcslen(TEXTSERVICE_NAME),
            szModule,
            (ULONG)wcslen(szModule),
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
                                                   TEXTSERVICE_NAME,
                                                   (ULONG)wcslen(TEXTSERVICE_NAME),
                                                   szModule,
                                                   (ULONG)wcslen(szModule),
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
        &GUID_TFCAT_PROPSTYLE_CUSTOM,                // 属性样式：自定义
        &GUID_TFCAT_PROPSTYLE_STATIC,                // 属性样式：静态
        &GUID_TFCAT_PROPSTYLE_STATICCOMPACT,         // 属性样式：静态紧凑
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
        &GUID_TFCAT_PROPSTYLE_CUSTOM,
        &GUID_TFCAT_PROPSTYLE_STATIC,
        &GUID_TFCAT_PROPSTYLE_STATICCOMPACT,
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