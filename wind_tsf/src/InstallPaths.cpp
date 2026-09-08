#include "InstallPaths.h"
#include "Globals.h"   // g_hInstance / WIND_APP_REGKEY

BOOL WindResolveInstallRoot(WCHAR* outDir, DWORD cchOutDir)
{
    if (outDir == nullptr || cchOutDir == 0)
        return FALSE;
    outDir[0] = L'\0';

    HKEY hKey = NULL;
    if (RegOpenKeyExW(HKEY_LOCAL_MACHINE, WIND_APP_REGKEY, 0, KEY_READ, &hKey) == ERROR_SUCCESS)
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
