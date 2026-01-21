#include "IPCClient.h"
#include <sstream>
#include <vector>

#pragma comment(lib, "advapi32.lib")

CIPCClient::CIPCClient()
    : _hPipe(INVALID_HANDLE_VALUE)
    , _serviceStartAttempted(FALSE)
{
}

CIPCClient::~CIPCClient()
{
    Disconnect();
}

BOOL CIPCClient::_StartService()
{
    OutputDebugStringW(L"[WindInput] Attempting to start service...\n");

    // Get service executable path (same directory as DLL)
    WCHAR dllPath[MAX_PATH];

    // Use global hInstance from DllMain
    if (GetModuleFileNameW(g_hInstance, dllPath, MAX_PATH) == 0)
    {
        OutputDebugStringW(L"[WindInput] Failed to get module path\n");
        return FALSE;
    }

    // Replace DLL filename with wind_input.exe
    WCHAR* lastSlash = wcsrchr(dllPath, L'\\');
    if (lastSlash)
    {
        wcscpy_s(lastSlash + 1, MAX_PATH - (lastSlash - dllPath + 1), L"wind_input.exe");
    }

    WCHAR debug[512];
    wsprintfW(debug, L"[WindInput] Starting service: %s\n", dllPath);
    OutputDebugStringW(debug);

    // Start the service process
    // Note: During development, show console window for debugging
    // Change to CREATE_NO_WINDOW and SW_HIDE for release
    STARTUPINFOW si = { sizeof(STARTUPINFOW) };
    si.dwFlags = STARTF_USESHOWWINDOW;
    si.wShowWindow = SW_SHOW;  // Show console window for debugging

    PROCESS_INFORMATION pi = {};

    if (!CreateProcessW(
        dllPath,
        nullptr,
        nullptr,
        nullptr,
        FALSE,
        CREATE_NEW_CONSOLE,  // Create new console window for debugging
        nullptr,
        nullptr,
        &si,
        &pi))
    {
        DWORD error = GetLastError();
        wsprintfW(debug, L"[WindInput] Failed to start service: error=%d\n", error);
        OutputDebugStringW(debug);
        return FALSE;
    }

    // Close process handles (we don't need to wait for it)
    CloseHandle(pi.hProcess);
    CloseHandle(pi.hThread);

    OutputDebugStringW(L"[WindInput] Service started successfully\n");
    return TRUE;
}

BOOL CIPCClient::Connect()
{
    if (_hPipe != INVALID_HANDLE_VALUE)
    {
        OutputDebugStringW(L"[WindInput] Already connected to pipe\n");
        return TRUE;
    }

    OutputDebugStringW(L"[WindInput] Connecting to Go Service...\n");

    // Try to connect multiple times, starting service if needed
    for (int attempt = 0; attempt < 3; attempt++)
    {
        _hPipe = CreateFileW(
            PIPE_NAME,
            GENERIC_READ | GENERIC_WRITE,
            0,
            nullptr,
            OPEN_EXISTING,
            0,
            nullptr);

        if (_hPipe != INVALID_HANDLE_VALUE)
        {
            break;  // Connected successfully
        }

        DWORD error = GetLastError();
        WCHAR debug[256];
        wsprintfW(debug, L"[WindInput] Connection attempt %d failed: error=%d\n", attempt + 1, error);
        OutputDebugStringW(debug);

        // If pipe doesn't exist, try to start the service
        if (error == ERROR_FILE_NOT_FOUND && !_serviceStartAttempted)
        {
            _serviceStartAttempted = TRUE;
            if (_StartService())
            {
                // Wait a bit for service to initialize
                Sleep(500);
                continue;
            }
        }
        else if (error == ERROR_PIPE_BUSY)
        {
            // Pipe is busy, wait and retry
            if (!WaitNamedPipeW(PIPE_NAME, 1000))
            {
                continue;
            }
        }
        else
        {
            // Other error, don't retry
            break;
        }
    }

    if (_hPipe == INVALID_HANDLE_VALUE)
    {
        OutputDebugStringW(L"[WindInput] Failed to connect to Go Service after retries\n");
        return FALSE;
    }

    // Set pipe mode to byte
    DWORD mode = PIPE_READMODE_BYTE;
    if (!SetNamedPipeHandleState(_hPipe, &mode, nullptr, nullptr))
    {
        OutputDebugStringW(L"[WindInput] Failed to set pipe mode\n");
    }

    OutputDebugStringW(L"[WindInput] Connected to Go Service successfully\n");
    return TRUE;
}

void CIPCClient::Disconnect()
{
    if (_hPipe != INVALID_HANDLE_VALUE)
    {
        CloseHandle(_hPipe);
        _hPipe = INVALID_HANDLE_VALUE;
        OutputDebugStringW(L"[WindInput] Disconnected from Go Service\n");
    }
}

BOOL CIPCClient::SendKeyEvent(const std::wstring& key, int keyCode, int modifiers)
{
    if (!IsConnected())
    {
        if (!Connect())
            return FALSE;
    }

    // Build JSON message
    std::wostringstream oss;
    oss << L"{";
    oss << L"\"type\":\"key_event\",";
    oss << L"\"data\":{";
    oss << L"\"key\":\"" << key << L"\",";
    oss << L"\"keycode\":" << keyCode << L",";
    oss << L"\"modifiers\":" << modifiers << L",";
    oss << L"\"event\":\"down\"";
    oss << L"}}";

    std::wstring json = oss.str();

    WCHAR debug[512];
    wsprintfW(debug, L"[WindInput] Sending key event: key=%s, keycode=%d\n", key.c_str(), keyCode);
    OutputDebugStringW(debug);

    return _SendMessage(json);
}

BOOL CIPCClient::SendCaretUpdate(int x, int y, int height)
{
    if (!IsConnected())
    {
        if (!Connect())
            return FALSE;
    }

    // Build JSON message
    std::wostringstream oss;
    oss << L"{";
    oss << L"\"type\":\"caret_update\",";
    oss << L"\"data\":{";
    oss << L"\"x\":" << x << L",";
    oss << L"\"y\":" << y << L",";
    oss << L"\"height\":" << height;
    oss << L"}}";

    return _SendMessage(oss.str());
}

BOOL CIPCClient::SendFocusLost()
{
    if (!IsConnected())
    {
        return FALSE;
    }

    OutputDebugStringW(L"[WindInput] Sending focus_lost\n");

    std::wstring json = L"{\"type\":\"focus_lost\",\"data\":{}}";
    return _SendMessage(json);
}

BOOL CIPCClient::SendToggleMode()
{
    if (!IsConnected())
    {
        if (!Connect())
            return FALSE;
    }

    OutputDebugStringW(L"[WindInput] Sending toggle_mode\n");

    std::wstring json = L"{\"type\":\"toggle_mode\",\"data\":{}}";
    return _SendMessage(json);
}

BOOL CIPCClient::ReceiveResponse(ServiceResponse& response)
{
    std::wstring json;
    if (!_ReceiveMessage(json))
    {
        return FALSE;
    }

    return _ParseResponse(json, response);
}

BOOL CIPCClient::_SendMessage(const std::wstring& message)
{
    // Convert to UTF-8
    int utf8Size = WideCharToMultiByte(CP_UTF8, 0, message.c_str(), -1, nullptr, 0, nullptr, nullptr);
    if (utf8Size <= 0)
    {
        OutputDebugStringW(L"[WindInput] Failed to calculate UTF-8 size\n");
        return FALSE;
    }

    std::vector<char> utf8Buffer(utf8Size);
    WideCharToMultiByte(CP_UTF8, 0, message.c_str(), -1, utf8Buffer.data(), utf8Size, nullptr, nullptr);

    // Message length (excluding null terminator)
    DWORD messageLength = static_cast<DWORD>(utf8Size - 1);

    // Write length prefix (4 bytes, little-endian)
    DWORD bytesWritten;
    if (!WriteFile(_hPipe, &messageLength, sizeof(DWORD), &bytesWritten, nullptr))
    {
        OutputDebugStringW(L"[WindInput] Failed to write message length\n");
        Disconnect();
        return FALSE;
    }

    // Write message content
    if (!WriteFile(_hPipe, utf8Buffer.data(), messageLength, &bytesWritten, nullptr))
    {
        OutputDebugStringW(L"[WindInput] Failed to write message content\n");
        Disconnect();
        return FALSE;
    }

    return TRUE;
}

BOOL CIPCClient::_ReceiveMessage(std::wstring& message)
{
    // Read length prefix (4 bytes)
    DWORD messageLength;
    DWORD bytesRead;

    if (!ReadFile(_hPipe, &messageLength, sizeof(DWORD), &bytesRead, nullptr) || bytesRead != sizeof(DWORD))
    {
        OutputDebugStringW(L"[WindInput] Failed to read message length\n");
        Disconnect();
        return FALSE;
    }

    if (messageLength == 0 || messageLength > 1024 * 1024)
    {
        WCHAR debug[128];
        wsprintfW(debug, L"[WindInput] Invalid message length: %d\n", messageLength);
        OutputDebugStringW(debug);
        return FALSE;
    }

    // Read message content
    std::vector<char> utf8Buffer(messageLength + 1);
    if (!ReadFile(_hPipe, utf8Buffer.data(), messageLength, &bytesRead, nullptr) || bytesRead != messageLength)
    {
        OutputDebugStringW(L"[WindInput] Failed to read message content\n");
        Disconnect();
        return FALSE;
    }
    utf8Buffer[messageLength] = '\0';

    // Convert from UTF-8
    int wideSize = MultiByteToWideChar(CP_UTF8, 0, utf8Buffer.data(), -1, nullptr, 0);
    if (wideSize <= 0)
    {
        OutputDebugStringW(L"[WindInput] Failed to calculate wide string size\n");
        return FALSE;
    }

    std::vector<wchar_t> wideBuffer(wideSize);
    MultiByteToWideChar(CP_UTF8, 0, utf8Buffer.data(), -1, wideBuffer.data(), wideSize);

    message = wideBuffer.data();

    // Log raw JSON response
    WCHAR debug[1024];
    wsprintfW(debug, L"[WindInput] Received raw JSON (len=%d): %.500s\n", (int)message.length(), message.c_str());
    OutputDebugStringW(debug);

    return TRUE;
}

BOOL CIPCClient::_ParseResponse(const std::wstring& json, ServiceResponse& response)
{
    response.type = ResponseType::Unknown;
    response.text.clear();
    response.composition.clear();
    response.caretPos = 0;
    response.chineseMode = FALSE;
    response.error.clear();

    OutputDebugStringW(L"[WindInput] Parsing response JSON...\n");

    // Parse type field
    if (json.find(L"\"type\":\"ack\"") != std::wstring::npos)
    {
        response.type = ResponseType::Ack;
        OutputDebugStringW(L"[WindInput] Response type: Ack\n");
    }
    else if (json.find(L"\"type\":\"insert_text\"") != std::wstring::npos)
    {
        response.type = ResponseType::InsertText;
        OutputDebugStringW(L"[WindInput] Response type: InsertText\n");

        // Extract text from data.text
        size_t textPos = json.find(L"\"text\":\"");
        if (textPos != std::wstring::npos)
        {
            textPos += 8;
            size_t textEnd = json.find(L"\"", textPos);
            if (textEnd != std::wstring::npos)
            {
                response.text = json.substr(textPos, textEnd - textPos);
            }
        }

        WCHAR debug[256];
        wsprintfW(debug, L"[WindInput] InsertText: text=%s (len=%d)\n", response.text.c_str(), (int)response.text.length());
        OutputDebugStringW(debug);
    }
    else if (json.find(L"\"type\":\"update_composition\"") != std::wstring::npos)
    {
        response.type = ResponseType::UpdateComposition;

        // Extract composition text
        size_t textPos = json.find(L"\"text\":\"");
        if (textPos != std::wstring::npos)
        {
            textPos += 8;
            size_t textEnd = json.find(L"\"", textPos);
            if (textEnd != std::wstring::npos)
            {
                response.composition = json.substr(textPos, textEnd - textPos);
            }
        }

        // Extract caret position
        size_t caretPos = json.find(L"\"caret_pos\":");
        if (caretPos != std::wstring::npos)
        {
            caretPos += 12;
            std::wstring caretStr;
            while (caretPos < json.length() && json[caretPos] >= L'0' && json[caretPos] <= L'9')
            {
                caretStr += json[caretPos];
                caretPos++;
            }
            if (!caretStr.empty())
            {
                response.caretPos = _wtoi(caretStr.c_str());
            }
        }
    }
    else if (json.find(L"\"type\":\"clear_composition\"") != std::wstring::npos)
    {
        response.type = ResponseType::ClearComposition;
    }
    else if (json.find(L"\"type\":\"mode_changed\"") != std::wstring::npos)
    {
        response.type = ResponseType::ModeChanged;
        OutputDebugStringW(L"[WindInput] Response type: ModeChanged\n");

        // Extract chinese_mode from data
        if (json.find(L"\"chinese_mode\":true") != std::wstring::npos)
        {
            response.chineseMode = TRUE;
        }
        else
        {
            response.chineseMode = FALSE;
        }

        WCHAR debug[128];
        wsprintfW(debug, L"[WindInput] ModeChanged: chineseMode=%s\n",
                  response.chineseMode ? L"true" : L"false");
        OutputDebugStringW(debug);
    }

    // Check for error field
    size_t errorPos = json.find(L"\"error\":\"");
    if (errorPos != std::wstring::npos)
    {
        errorPos += 9;
        size_t errorEnd = json.find(L"\"", errorPos);
        if (errorEnd != std::wstring::npos && errorEnd > errorPos)
        {
            response.error = json.substr(errorPos, errorEnd - errorPos);
        }
    }

    return TRUE;
}
