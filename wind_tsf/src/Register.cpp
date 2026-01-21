#include "Register.h"
#include <shlwapi.h>
#include <strsafe.h>
#include <inputscope.h>

#pragma comment(lib, "shlwapi.lib")

// Windows 8+ 需要的 GUID
// {85F9F8EF-1B5E-4EC4-4CFE-56E86E4B6F05}
DEFINE_GUID(GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
    0x13A016DF, 0x560B, 0x46CD, 0x94, 0x7A, 0x4C, 0x3A, 0xF1, 0xE0, 0xE3, 0x5D);

// {25504FB4-7BAB-4BC1-9C69-CF81890F0EF5}
DEFINE_GUID(GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
    0x25504FB4, 0x7BAB, 0x4BC1, 0x9C, 0x69, 0xCF, 0x81, 0x89, 0x0F, 0x0E, 0xF5);

// {6D60FCCF-58D7-4B67-B13E-96BE706C3B6A}
DEFINE_GUID(GUID_TFCAT_TIPCAP_UIELEMENTENABLED,
    0x6D60FCCF, 0x58D7, 0x4B67, 0xB1, 0x3E, 0x96, 0xBE, 0x70, 0x6C, 0x3B, 0x6A);

// COM 服务器注册
static HRESULT RegisterCOMServer()
{
    HRESULT hr = E_FAIL;
    WCHAR szModule[MAX_PATH];
    WCHAR szCLSID[39];

    if (GetModuleFileNameW(g_hInstance, szModule, ARRAYSIZE(szModule)) == 0)
        return E_FAIL;

    // 转换 CLSID 为字符串
    StringFromGUID2(c_clsidTextService, szCLSID, ARRAYSIZE(szCLSID));

    // 注册 CLSID
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
    SHDeleteKeyW(HKEY_CLASSES_ROOT, szKey);

    return S_OK;
}

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
        // Windows 8+ 注册方式
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

        WCHAR debug[256];
        wsprintfW(debug, L"[WindInput] RegisterProfile (ProfileMgr) hr=0x%08X\n", hr);
        OutputDebugStringW(debug);

        pProfileMgr->Release();
    }
    else
    {
        // 回退到旧的 ITfInputProcessorProfiles 接口
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

            WCHAR debug[256];
            wsprintfW(debug, L"[WindInput] RegisterProfile (legacy) hr=0x%08X\n", hr);
            OutputDebugStringW(debug);

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

HRESULT RegisterCategories()
{
    ITfCategoryMgr* pCategoryMgr = nullptr;
    HRESULT hr = CoCreateInstance(CLSID_TF_CategoryMgr, nullptr, CLSCTX_INPROC_SERVER,
                                   IID_ITfCategoryMgr, (void**)&pCategoryMgr);

    if (SUCCEEDED(hr))
    {
        // 注册为 TIP (Text Input Processor)
        hr = pCategoryMgr->RegisterCategory(c_clsidTextService,
                                             GUID_TFCAT_TIP_KEYBOARD,
                                             c_clsidTextService);

        OutputDebugStringW(SUCCEEDED(hr) ?
            L"[WindInput] Registered GUID_TFCAT_TIP_KEYBOARD\n" :
            L"[WindInput] Failed to register GUID_TFCAT_TIP_KEYBOARD\n");

        // 注册 Windows 8+ 现代应用支持 (Immersive/Metro apps)
        HRESULT hr2 = pCategoryMgr->RegisterCategory(c_clsidTextService,
                                                      GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
                                                      c_clsidTextService);

        OutputDebugStringW(SUCCEEDED(hr2) ?
            L"[WindInput] Registered GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT\n" :
            L"[WindInput] Failed to register GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT\n");

        // 注册系统托盘支持 (用于在输入指示器显示图标)
        hr2 = pCategoryMgr->RegisterCategory(c_clsidTextService,
                                              GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
                                              c_clsidTextService);

        OutputDebugStringW(SUCCEEDED(hr2) ?
            L"[WindInput] Registered GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT\n" :
            L"[WindInput] Failed to register GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT\n");

        // 注册 UI 元素支持 (候选窗口等)
        hr2 = pCategoryMgr->RegisterCategory(c_clsidTextService,
                                              GUID_TFCAT_TIPCAP_UIELEMENTENABLED,
                                              c_clsidTextService);

        OutputDebugStringW(SUCCEEDED(hr2) ?
            L"[WindInput] Registered GUID_TFCAT_TIPCAP_UIELEMENTENABLED\n" :
            L"[WindInput] Failed to register GUID_TFCAT_TIPCAP_UIELEMENTENABLED\n");

        pCategoryMgr->Release();
    }

    return hr;
}

HRESULT UnregisterCategories()
{
    ITfCategoryMgr* pCategoryMgr = nullptr;
    HRESULT hr = CoCreateInstance(CLSID_TF_CategoryMgr, nullptr, CLSCTX_INPROC_SERVER,
                                   IID_ITfCategoryMgr, (void**)&pCategoryMgr);

    if (SUCCEEDED(hr))
    {
        // 卸载所有注册的分类
        pCategoryMgr->UnregisterCategory(c_clsidTextService,
                                          GUID_TFCAT_TIP_KEYBOARD,
                                          c_clsidTextService);

        pCategoryMgr->UnregisterCategory(c_clsidTextService,
                                          GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
                                          c_clsidTextService);

        pCategoryMgr->UnregisterCategory(c_clsidTextService,
                                          GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
                                          c_clsidTextService);

        pCategoryMgr->UnregisterCategory(c_clsidTextService,
                                          GUID_TFCAT_TIPCAP_UIELEMENTENABLED,
                                          c_clsidTextService);

        pCategoryMgr->Release();
    }

    return hr;
}

HRESULT RegisterServer()
{
    HRESULT hr;

    hr = CoInitialize(nullptr);
    if (FAILED(hr))
        return hr;

    // 注册 COM 服务器
    hr = RegisterCOMServer();
    if (FAILED(hr))
        goto Exit;

    // 注册配置文件
    hr = RegisterProfile();
    if (FAILED(hr))
        goto Exit;

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
    if (FAILED(hr))
        return hr;

    // 卸载分类
    UnregisterCategories();

    // 卸载配置文件
    UnregisterProfile();

    // 卸载 COM 服务器
    UnregisterCOMServer();

    CoUninitialize();
    return S_OK;
}
