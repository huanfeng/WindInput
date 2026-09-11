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
            // reason=exit|unload 是「进程死了」与「DLL 被卸了」的唯一分水岭：DllMain 的
            // pvReserved 非 NULL 表示进程正在退出（加载器统一派发，COM 不做清理），为 NULL
            // 表示有人主动 FreeLibrary（COM 正常回收，或被第三方强制卸载）。缺了这一位，
            // 「宿主进程退出」与「输入法被卸载后再没被请求」在日志里长得一模一样——
            // 2026-09-11 分析 #115 彩虹六号（BattlEye）日志时正卡在这里分不开。
            // 配合本行判读：正常 TSF 收尾一定先有 TextService::Deactivate，
            // 只有 DETACH 没有 Deactivate ⇒ 宿主是被掐掉的，不是正常切走输入法。
            WIND_LOG_INFO_FMT(
                L"DllMain PROCESS_DETACH pid=%lu tid=%lu reason=%ls",
                GetCurrentProcessId(),
                GetCurrentThreadId(),
                pvReserved != nullptr ? L"exit" : L"unload"
            );
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
