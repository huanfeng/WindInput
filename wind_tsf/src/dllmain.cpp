#include "Globals.h"
#include "ClassFactory.h"
#include "Register.h"
#include "FileLogger.h"

BOOL WINAPI DllMain(HINSTANCE hInstance, DWORD dwReason, LPVOID pvReserved)
{
    switch (dwReason)
    {
        case DLL_PROCESS_ATTACH:
            g_hInstance = hInstance;
            DisableThreadLibraryCalls(hInstance);
            CFileLogger::Instance().Init();
            {
                WCHAR hostExe[MAX_PATH] = {};
                DWORD len = GetModuleFileNameW(nullptr, hostExe, ARRAYSIZE(hostExe));
                // build=<编译时刻> 是版本指纹：TSF DLL 常驻宿主进程，部署后未重启的宿主
                // 仍跑旧代码，而日志里各进程混在一起，靠时间戳与文件时间根本分不清谁新谁旧
                // （2026-08-04 排查 DBX 焦点问题时在此空转一轮）。有了它，一眼就能确认
                // 某个 PID 加载的到底是哪次构建的产物。
                WIND_LOG_INFO_FMT(
                    L"DllMain PROCESS_ATTACH pid=%lu tid=%lu hInstance=0x%p build=%hs_%hs hostExe=%ls",
                    GetCurrentProcessId(),
                    GetCurrentThreadId(),
                    hInstance,
                    __DATE__, __TIME__,
                    len > 0 ? hostExe : L"(unknown)"
                );
            }
            break;

        case DLL_PROCESS_DETACH:
            WIND_LOG_INFO_FMT(L"DllMain PROCESS_DETACH pid=%lu tid=%lu", GetCurrentProcessId(), GetCurrentThreadId());
            CFileLogger::Instance().Shutdown();
            break;
    }

    return TRUE;
}

// DLL 导出函数
STDAPI DllCanUnloadNow()
{
    return (g_lServerLock == 0) ? S_OK : S_FALSE;
}

STDAPI DllGetClassObject(REFCLSID rclsid, REFIID riid, LPVOID* ppv)
{
    // COM 激活第一入口：游戏等宿主里 Win+Space 选中无效时，靠这条日志区分
    // 「msctf 根本没来问」与「问了但 CLSID/RIID 不被接受被拒」。
    {
        WCHAR szClsid[64] = {};
        WCHAR szIid[64] = {};
        StringFromGUID2(rclsid, szClsid, ARRAYSIZE(szClsid));
        StringFromGUID2(riid, szIid, ARRAYSIZE(szIid));
        WIND_LOG_DEBUG_FMT(
            L"DllGetClassObject rclsid=%ls riid=%ls clsidMatch=%d",
            szClsid, szIid, IsEqualCLSID(rclsid, c_clsidTextService) ? 1 : 0);
    }

    if (ppv == nullptr)
        return E_INVALIDARG;

    *ppv = nullptr;

    if (!IsEqualCLSID(rclsid, c_clsidTextService))
        return CLASS_E_CLASSNOTAVAILABLE;

    CClassFactory* pClassFactory = new CClassFactory();
    if (pClassFactory == nullptr)
        return E_OUTOFMEMORY;

    HRESULT hr = pClassFactory->QueryInterface(riid, ppv);
    pClassFactory->Release();

    return hr;
}

STDAPI DllRegisterServer()
{
    return RegisterServer();
}

STDAPI DllUnregisterServer()
{
    return UnregisterServer();
}
