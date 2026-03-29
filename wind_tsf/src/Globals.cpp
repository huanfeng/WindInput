#include "Globals.h"
#include <appmodel.h>
#include <vector>

// 链接必要的库
// 注意：msctf.lib 在新版 SDK 中不需要直接链接
#pragma comment(lib, "ole32.lib")
#pragma comment(lib, "oleaut32.lib")
#pragma comment(lib, "uuid.lib")
#pragma comment(lib, "user32.lib")
#pragma comment(lib, "gdi32.lib")

// 全局变量定义
HINSTANCE g_hInstance = nullptr;
LONG g_lServerLock = 0;

// GUID 定义
// {99C2EE30-5C57-45A2-9C63-FB54B34FD90A}
const CLSID c_clsidTextService =
    { 0x99c2ee30, 0x5c57, 0x45a2, { 0x9c, 0x63, 0xfb, 0x54, 0xb3, 0x4f, 0xd9, 0x0a } };

// {99C2EE31-5C57-45A2-9C63-FB54B34FD90A}
const GUID c_guidProfile =
    { 0x99c2ee31, 0x5c57, 0x45a2, { 0x9c, 0x63, 0xfb, 0x54, 0xb3, 0x4f, 0xd9, 0x0a } };

// {99C2EE32-5C57-45A2-9C63-FB54B34FD90A}
const GUID c_guidLangBarItemButton =
    { 0x99c2ee32, 0x5c57, 0x45a2, { 0x9c, 0x63, 0xfb, 0x54, 0xb3, 0x4f, 0xd9, 0x0a } };

// {99C2EE33-5C57-45A2-9C63-FB54B34FD90A}
const GUID c_guidDisplayAttributeInput =
    { 0x99c2ee33, 0x5c57, 0x45a2, { 0x9c, 0x63, 0xfb, 0x54, 0xb3, 0x4f, 0xd9, 0x0a } };

// {99C2EE34-5C57-45A2-9C63-FB54B34FD90A}
const GUID c_guidDisplayAttributeConverted =
    { 0x99c2ee34, 0x5c57, 0x45a2, { 0x9c, 0x63, 0xfb, 0x54, 0xb3, 0x4f, 0xd9, 0x0a } };

LONG DllAddRef()
{
    return InterlockedIncrement(&g_lServerLock);
}

LONG DllRelease()
{
    return InterlockedDecrement(&g_lServerLock);
}

namespace
{
    std::wstring _BaseNameFromPath(const std::wstring& path)
    {
        if (path.empty())
            return L"";

        size_t pos = path.find_last_of(L"\\/");
        if (pos == std::wstring::npos || pos + 1 >= path.length())
            return path;

        return path.substr(pos + 1);
    }

    BOOL _QueryProcessPath(HANDLE hProcess, std::wstring& path)
    {
        WCHAR buffer[MAX_PATH * 2] = {};
        DWORD size = ARRAYSIZE(buffer);
        if (!QueryFullProcessImageNameW(hProcess, 0, buffer, &size))
            return FALSE;

        path.assign(buffer, size);
        return TRUE;
    }

    void _QueryTokenMetadata(HANDLE hProcess, WindHostProcessInfo& info)
    {
        HANDLE hToken = nullptr;
        if (!OpenProcessToken(hProcess, TOKEN_QUERY, &hToken))
        {
            info.queryError = GetLastError();
            return;
        }

        DWORD isAppContainer = 0;
        DWORD returnLength = 0;
        if (GetTokenInformation(hToken, TokenIsAppContainer, &isAppContainer, sizeof(isAppContainer), &returnLength))
            info.isAppContainer = isAppContainer ? TRUE : FALSE;

        GetTokenInformation(hToken, TokenIntegrityLevel, nullptr, 0, &returnLength);
        if (returnLength > 0)
        {
            std::vector<BYTE> tokenBuffer(returnLength);
            if (GetTokenInformation(hToken, TokenIntegrityLevel, tokenBuffer.data(), returnLength, &returnLength))
            {
                auto* til = reinterpret_cast<TOKEN_MANDATORY_LABEL*>(tokenBuffer.data());
                DWORD subAuthCount = *GetSidSubAuthorityCount(til->Label.Sid);
                if (subAuthCount > 0)
                    info.integrityRid = *GetSidSubAuthority(til->Label.Sid, subAuthCount - 1);
            }
        }

        UINT32 packageLen = PACKAGE_FAMILY_NAME_MAX_LENGTH;
        WCHAR packageName[PACKAGE_FAMILY_NAME_MAX_LENGTH] = {};
        LONG packageResult = GetPackageFamilyName(hProcess, &packageLen, packageName);
        if (packageResult == ERROR_SUCCESS)
            info.packageFamilyName.assign(packageName, packageLen);

        CloseHandle(hToken);
    }

    BOOL _QueryProcessInfo(HANDLE hProcess, DWORD processId, DWORD threadId, HWND hwnd, WindHostProcessInfo* info)
    {
        if (info == nullptr)
            return FALSE;

        *info = WindHostProcessInfo{};
        info->processId = processId;
        info->threadId = threadId;
        info->hwnd = hwnd;

        if (hwnd != nullptr)
        {
            WCHAR className[256] = {};
            int classLen = GetClassNameW(hwnd, className, ARRAYSIZE(className));
            if (classLen > 0)
                info->windowClass.assign(className, classLen);

            WCHAR title[256] = {};
            int titleLen = GetWindowTextW(hwnd, title, ARRAYSIZE(title));
            if (titleLen > 0)
                info->windowTitle.assign(title, titleLen);
        }

        if (hProcess == nullptr)
        {
            info->queryError = ERROR_INVALID_HANDLE;
            return FALSE;
        }

        if (!_QueryProcessPath(hProcess, info->processPath))
            info->queryError = GetLastError();

        info->processName = _BaseNameFromPath(info->processPath);
        _QueryTokenMetadata(hProcess, *info);
        return info->queryError == ERROR_SUCCESS || !info->processPath.empty();
    }
}

BOOL WindQueryCurrentProcessInfo(WindHostProcessInfo* info)
{
    return _QueryProcessInfo(GetCurrentProcess(), GetCurrentProcessId(), GetCurrentThreadId(), nullptr, info);
}

BOOL WindQueryWindowProcessInfo(HWND hwnd, WindHostProcessInfo* info)
{
    if (hwnd == nullptr || info == nullptr)
        return FALSE;

    DWORD processId = 0;
    DWORD threadId = GetWindowThreadProcessId(hwnd, &processId);

    HANDLE hProcess = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, processId);
    BOOL ok = _QueryProcessInfo(hProcess, processId, threadId, hwnd, info);
    if (hProcess != nullptr)
        CloseHandle(hProcess);

    if (!ok && info->queryError == ERROR_SUCCESS)
        info->queryError = GetLastError();

    return ok;
}

void WindLogHostProcessInfo(int level, const wchar_t* prefix, const WindHostProcessInfo& info)
{
    WindLog::OutputFmt(
        level,
        L"%ls pid=%lu tid=%lu hwnd=0x%p appContainer=%d integrityRid=0x%04lX class=%ls title=%ls exe=%ls package=%ls queryError=%lu",
        prefix ? prefix : L"host",
        info.processId,
        info.threadId,
        info.hwnd,
        info.isAppContainer ? 1 : 0,
        info.integrityRid,
        info.windowClass.empty() ? L"-" : info.windowClass.c_str(),
        info.windowTitle.empty() ? L"-" : info.windowTitle.c_str(),
        info.processPath.empty() ? (info.processName.empty() ? L"-" : info.processName.c_str()) : info.processPath.c_str(),
        info.packageFamilyName.empty() ? L"-" : info.packageFamilyName.c_str(),
        info.queryError
    );
}

void WindLogForegroundProcessInfo(int level, const wchar_t* prefix)
{
    WindHostProcessInfo info;
    HWND hwndForeground = GetForegroundWindow();
    if (WindQueryWindowProcessInfo(hwndForeground, &info))
    {
        WindLogHostProcessInfo(level, prefix, info);
        return;
    }

    WindLog::OutputFmt(level, L"%ls hwnd=0x%p queryError=%lu", prefix ? prefix : L"foreground", hwndForeground, info.queryError);
}
