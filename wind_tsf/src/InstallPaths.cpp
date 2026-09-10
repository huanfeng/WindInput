#include "InstallPaths.h"
#include "Globals.h"   // g_hInstance / WIND_APP_REGKEY

BOOL WindResolveInstallRoot(WCHAR* outDir, DWORD cchOutDir)
{
    if (outDir == nullptr || cchOutDir == 0)
        return FALSE;
    outDir[0] = L'\0';

    HKEY hKey = NULL;
    // ★ KEY_WOW64_64KEY 不可省：WIND_APP_REGKEY 是 HKLM\Software 下的普通键，32 位进程
    // 读它会被 WOW64 重定向到 Software\Wow6432Node\。三个部署方（wind-installer /
    // scripts\dev.ps1 / wind-portable）都是 64 位程序，只写得进 64 位视图，SysWOW64 里的
    // x86 DLL 不加这个标志就**永远读不到 InstallDir**，静默回退到下面那条 GetModuleFileName
    // 分支 —— 而 DLL 进了系统目录之后，那条回退给出的是 SysWOW64\IME\<app>\，不是安装目录。
    // 后果是 32 位宿主里服务拉不起来、便携判据恒 FALSE。DLL 还在安装目录时回退恰好等价，
    // 所以这个缺陷是随「TSF 组件搬进系统目录」一起进来的，且无任何报错。
    if (RegOpenKeyExW(HKEY_LOCAL_MACHINE, WIND_APP_REGKEY, 0, KEY_READ | KEY_WOW64_64KEY,
                      &hKey) == ERROR_SUCCESS)
    {
        DWORD type = REG_SZ;
        DWORD cb = cchOutDir * sizeof(WCHAR);
        LONG r = RegQueryValueExW(hKey, L"InstallDir", nullptr, &type,
                                  reinterpret_cast<LPBYTE>(outDir), &cb);
        RegCloseKey(hKey);

        if (r == ERROR_SUCCESS && (type == REG_SZ || type == REG_EXPAND_SZ) && cb >= sizeof(WCHAR))
        {
            // REG_SZ 不保证以 NUL 收尾（写入方可能未把结尾计入长度），显式收口。
            DWORD cch = cb / sizeof(WCHAR);
            if (cch >= cchOutDir) { cch = cchOutDir - 1; }
            outDir[cch] = L'\0';

            size_t len = wcslen(outDir);
            while (len > 0 && outDir[len - 1] == L'\\') { outDir[--len] = L'\0'; }
            if (len > 0) { return TRUE; }
        }
        // 读到了键但内容不可用（类型不对/空串/只有反斜杠）：清干净再走回退，
        // 否则 RegQueryValueExW 可能已经往缓冲里写了半截数据。
        outDir[0] = L'\0';
    }

    if (GetModuleFileNameW(g_hInstance, outDir, cchOutDir) == 0) { outDir[0] = L'\0'; return FALSE; }
    WCHAR* lastSlash = wcsrchr(outDir, L'\\');
    if (lastSlash == nullptr) { outDir[0] = L'\0'; return FALSE; }
    *lastSlash = L'\0';
    return TRUE;
}

BOOL WindIsPortableRoot(const WCHAR* root)
{
    if (root == nullptr || root[0] == L'\0')
        return FALSE;

    static const WCHAR* const kMarkerNames[] = { L"portable_mode", L"wind_portable_mode" };
    for (size_t i = 0; i < ARRAYSIZE(kMarkerNames); ++i)
    {
        WCHAR markerPath[MAX_PATH];
        if (_snwprintf_s(markerPath, _countof(markerPath), _TRUNCATE, L"%ls\\%ls",
                         root, kMarkerNames[i]) < 0)
        {
            continue;
        }
        // 判据是「文件存在」，**不读内容**：标记文件里的 `stopped=1` 是另一件事
        // （服务该不该被拉起，见 CIPCClient::_StartService），与「这是不是便携部署」无关。
        // 此前只有那一处读取，照抄过来就会得到「用户按了停止 ⇒ 日志改写 %LOCALAPPDATA%」
        // 这种荒唐行为。
        DWORD attr = GetFileAttributesW(markerPath);
        if (attr != INVALID_FILE_ATTRIBUTES && !(attr & FILE_ATTRIBUTE_DIRECTORY))
            return TRUE;
    }
    return FALSE;
}
