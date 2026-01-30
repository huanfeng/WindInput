#include "IPCClient.h"
#include <sstream>
#include <vector>
#include <cstdarg>

#pragma comment(lib, "advapi32.lib")

// Static member initialization
// Default to Info level; can be changed via SetLogLevel()
IPCLogLevel CIPCClient::s_logLevel = IPCLogLevel::Info;

CIPCClient::CIPCClient()
    : _hPipe(INVALID_HANDLE_VALUE)
    , _hEvent(NULL)
    , _serviceStartAttempted(FALSE)
    , _circuitState(CircuitState::Closed)
    , _consecutiveFailures(0)
    , _lastFailureTime(0)
{
    // Create event for overlapped I/O
    _hEvent = CreateEventW(NULL, TRUE, FALSE, NULL);
    if (_hEvent == NULL)
    {
        _LogError(L"Failed to create overlapped event: %d", GetLastError());
    }
}

CIPCClient::~CIPCClient()
{
    Disconnect();
    if (_hEvent != NULL)
    {
        CloseHandle(_hEvent);
        _hEvent = NULL;
    }
}

// ============================================================================
// Logging helpers
// ============================================================================

void CIPCClient::_Log(IPCLogLevel level, const wchar_t* format, ...)
{
    if (static_cast<int>(level) > static_cast<int>(s_logLevel))
        return;

    wchar_t buffer[1024];
    va_list args;
    va_start(args, format);
    _vsnwprintf_s(buffer, _countof(buffer), _TRUNCATE, format, args);
    va_end(args);

    OutputDebugStringW(buffer);
}

void CIPCClient::_LogError(const wchar_t* format, ...)
{
    if (s_logLevel < IPCLogLevel::Error)
        return;

    wchar_t msgBuffer[1024];
    va_list args;
    va_start(args, format);
    _vsnwprintf_s(msgBuffer, _countof(msgBuffer), _TRUNCATE, format, args);
    va_end(args);

    wchar_t buffer[1100];
    _snwprintf_s(buffer, _countof(buffer), _TRUNCATE, L"[WindInput][ERROR] %s\n", msgBuffer);
    OutputDebugStringW(buffer);
}

void CIPCClient::_LogInfo(const wchar_t* format, ...)
{
    if (s_logLevel < IPCLogLevel::Info)
        return;

    wchar_t msgBuffer[1024];
    va_list args;
    va_start(args, format);
    _vsnwprintf_s(msgBuffer, _countof(msgBuffer), _TRUNCATE, format, args);
    va_end(args);

    wchar_t buffer[1100];
    _snwprintf_s(buffer, _countof(buffer), _TRUNCATE, L"[WindInput] %s\n", msgBuffer);
    OutputDebugStringW(buffer);
}

void CIPCClient::_LogDebug(const wchar_t* format, ...)
{
    if (s_logLevel < IPCLogLevel::Debug)
        return;

    wchar_t msgBuffer[1024];
    va_list args;
    va_start(args, format);
    _vsnwprintf_s(msgBuffer, _countof(msgBuffer), _TRUNCATE, format, args);
    va_end(args);

    wchar_t buffer[1100];
    _snwprintf_s(buffer, _countof(buffer), _TRUNCATE, L"[WindInput][DBG] %s\n", msgBuffer);
    OutputDebugStringW(buffer);
}

// ============================================================================
// Circuit Breaker
// ============================================================================

void CIPCClient::_RecordSuccess()
{
    _consecutiveFailures = 0;
    if (_circuitState != CircuitState::Closed)
    {
        _LogInfo(L"Circuit breaker closed (service recovered)");
        _circuitState = CircuitState::Closed;
    }
}

void CIPCClient::_RecordFailure()
{
    _consecutiveFailures++;
    _lastFailureTime = GetTickCount();

    if (_consecutiveFailures >= IPCConfig::MAX_CONSECUTIVE_FAILURES)
    {
        if (_circuitState != CircuitState::Open)
        {
            _LogError(L"Circuit breaker OPEN after %d consecutive failures", _consecutiveFailures);
            _circuitState = CircuitState::Open;
        }
    }
}

BOOL CIPCClient::_ShouldAttemptOperation()
{
    if (_circuitState == CircuitState::Closed)
    {
        return TRUE;
    }

    if (_circuitState == CircuitState::Open)
    {
        // Check if enough time has passed to try again
        DWORD elapsed = GetTickCount() - _lastFailureTime;
        if (elapsed >= IPCConfig::CIRCUIT_RESET_INTERVAL_MS)
        {
            _LogInfo(L"Circuit breaker half-open, attempting reconnection...");
            _circuitState = CircuitState::HalfOpen;
            return TRUE;
        }
        return FALSE;
    }

    // HalfOpen state - allow one attempt
    return TRUE;
}

void CIPCClient::ResetCircuitBreaker()
{
    _consecutiveFailures = 0;
    _circuitState = CircuitState::Closed;
    _LogInfo(L"Circuit breaker manually reset");
}

BOOL CIPCClient::IsServiceAvailable()
{
    return _ShouldAttemptOperation() && (IsConnected() || Connect());
}

// ============================================================================
// Overlapped I/O helpers
// ============================================================================

BOOL CIPCClient::_WriteWithTimeout(const void* data, DWORD size, DWORD timeoutMs)
{
    if (_hPipe == INVALID_HANDLE_VALUE || _hEvent == NULL)
        return FALSE;

    OVERLAPPED overlapped = {};
    overlapped.hEvent = _hEvent;
    ResetEvent(_hEvent);

    DWORD bytesWritten = 0;
    BOOL result = WriteFile(_hPipe, data, size, &bytesWritten, &overlapped);

    if (result)
    {
        // Completed synchronously
        return bytesWritten == size;
    }

    DWORD error = GetLastError();
    if (error != ERROR_IO_PENDING)
    {
        _LogError(L"WriteFile failed immediately: %d", error);
        return FALSE;
    }

    // Wait for completion with timeout
    DWORD waitResult = WaitForSingleObject(_hEvent, timeoutMs);

    if (waitResult == WAIT_TIMEOUT)
    {
        _LogError(L"Write operation timed out after %dms", timeoutMs);
        CancelIo(_hPipe);
        return FALSE;
    }

    if (waitResult != WAIT_OBJECT_0)
    {
        _LogError(L"WaitForSingleObject failed: %d", GetLastError());
        CancelIo(_hPipe);
        return FALSE;
    }

    // Get the result
    if (!GetOverlappedResult(_hPipe, &overlapped, &bytesWritten, FALSE))
    {
        _LogError(L"GetOverlappedResult failed: %d", GetLastError());
        return FALSE;
    }

    return bytesWritten == size;
}

BOOL CIPCClient::_ReadWithTimeout(void* buffer, DWORD size, DWORD* bytesRead, DWORD timeoutMs)
{
    if (_hPipe == INVALID_HANDLE_VALUE || _hEvent == NULL)
        return FALSE;

    OVERLAPPED overlapped = {};
    overlapped.hEvent = _hEvent;
    ResetEvent(_hEvent);

    *bytesRead = 0;
    BOOL result = ReadFile(_hPipe, buffer, size, bytesRead, &overlapped);

    if (result)
    {
        // Completed synchronously
        return TRUE;
    }

    DWORD error = GetLastError();
    if (error != ERROR_IO_PENDING)
    {
        _LogDebug(L"ReadFile failed immediately: %d", error);
        return FALSE;
    }

    // Wait for completion with timeout
    DWORD waitResult = WaitForSingleObject(_hEvent, timeoutMs);

    if (waitResult == WAIT_TIMEOUT)
    {
        _LogError(L"Read operation timed out after %dms", timeoutMs);
        CancelIo(_hPipe);
        return FALSE;
    }

    if (waitResult != WAIT_OBJECT_0)
    {
        _LogError(L"WaitForSingleObject failed: %d", GetLastError());
        CancelIo(_hPipe);
        return FALSE;
    }

    // Get the result
    if (!GetOverlappedResult(_hPipe, &overlapped, bytesRead, FALSE))
    {
        _LogError(L"GetOverlappedResult failed: %d", GetLastError());
        return FALSE;
    }

    return TRUE;
}

// ============================================================================
// Service management
// ============================================================================

BOOL CIPCClient::_StartService()
{
    _LogInfo(L"Attempting to start Go service...");

    // Get service executable path (same directory as DLL)
    WCHAR dllPath[MAX_PATH];

    if (GetModuleFileNameW(g_hInstance, dllPath, MAX_PATH) == 0)
    {
        _LogError(L"Failed to get module path");
        return FALSE;
    }

    // Replace DLL filename with wind_input.exe
    WCHAR* lastSlash = wcsrchr(dllPath, L'\\');
    if (lastSlash)
    {
        wcscpy_s(lastSlash + 1, MAX_PATH - (lastSlash - dllPath + 1), L"wind_input.exe");
    }

    _LogDebug(L"Starting service: %s", dllPath);

    // Start the service process
    STARTUPINFOW si = { sizeof(STARTUPINFOW) };
    si.dwFlags = STARTF_USESHOWWINDOW;
    si.wShowWindow = SW_HIDE;  // Hide console window, use log file for debugging

    PROCESS_INFORMATION pi = {};

    if (!CreateProcessW(
        dllPath,
        nullptr,
        nullptr,
        nullptr,
        FALSE,
        CREATE_NEW_CONSOLE,
        nullptr,
        nullptr,
        &si,
        &pi))
    {
        _LogError(L"Failed to start service: error=%d", GetLastError());
        return FALSE;
    }

    // Close process handles (we don't need to wait for it)
    CloseHandle(pi.hProcess);
    CloseHandle(pi.hThread);

    _LogInfo(L"Service started successfully");
    return TRUE;
}

// ============================================================================
// Connection management
// ============================================================================

BOOL CIPCClient::Connect()
{
    if (_hPipe != INVALID_HANDLE_VALUE)
    {
        _LogDebug(L"Already connected to pipe");
        return TRUE;
    }

    if (!_ShouldAttemptOperation())
    {
        _LogDebug(L"Circuit breaker open, skipping connection attempt");
        return FALSE;
    }

    _LogDebug(L"Connecting to Go Service...");

    // Try to connect with retries
    for (int attempt = 0; attempt < 3; attempt++)
    {
        // First check if pipe exists and wait if busy
        if (!WaitNamedPipeW(PIPE_NAME, IPCConfig::CONNECT_TIMEOUT_MS))
        {
            DWORD error = GetLastError();
            if (error == ERROR_FILE_NOT_FOUND)
            {
                // Pipe doesn't exist, try to start service
                if (!_serviceStartAttempted)
                {
                    _serviceStartAttempted = TRUE;
                    if (_StartService())
                    {
                        Sleep(500);  // Wait for service to initialize
                        continue;
                    }
                }
                _LogDebug(L"Pipe not found, service not available");
                break;
            }
            else if (error == ERROR_SEM_TIMEOUT)
            {
                _LogDebug(L"WaitNamedPipe timed out, attempt %d", attempt + 1);
                continue;
            }
            // Other error, proceed to try CreateFile anyway
        }

        // Open the pipe with FILE_FLAG_OVERLAPPED for async I/O
        _hPipe = CreateFileW(
            PIPE_NAME,
            GENERIC_READ | GENERIC_WRITE,
            0,
            nullptr,
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,  // Enable overlapped I/O
            nullptr);

        if (_hPipe != INVALID_HANDLE_VALUE)
        {
            // Set pipe mode
            DWORD mode = PIPE_READMODE_BYTE;
            SetNamedPipeHandleState(_hPipe, &mode, nullptr, nullptr);

            _LogInfo(L"Connected to Go Service");
            _RecordSuccess();
            return TRUE;
        }

        DWORD error = GetLastError();
        _LogDebug(L"Connection attempt %d failed: error=%d", attempt + 1, error);

        if (error == ERROR_PIPE_BUSY)
        {
            // Pipe busy, wait and retry
            Sleep(50);
            continue;
        }

        // Other errors
        break;
    }

    _LogError(L"Failed to connect to Go Service");
    _RecordFailure();
    return FALSE;
}

void CIPCClient::Disconnect()
{
    if (_hPipe != INVALID_HANDLE_VALUE)
    {
        // Cancel any pending I/O before closing
        CancelIo(_hPipe);
        CloseHandle(_hPipe);
        _hPipe = INVALID_HANDLE_VALUE;
        _LogDebug(L"Disconnected from Go Service");
    }
}

// ============================================================================
// Message sending
// ============================================================================

BOOL CIPCClient::SendKeyEvent(const std::wstring& key, int keyCode, int modifiers,
                              const LONG* px, const LONG* py, const LONG* pHeight)
{
    if (!_ShouldAttemptOperation())
    {
        _LogDebug(L"Circuit open, skipping key event");
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
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

    // Include caret info if provided (avoids separate caret_update call)
    if (px != nullptr && py != nullptr && pHeight != nullptr)
    {
        oss << L",\"caret\":{";
        oss << L"\"x\":" << *px << L",";
        oss << L"\"y\":" << *py << L",";
        oss << L"\"height\":" << *pHeight;
        oss << L"}";
    }

    oss << L"}}";

    return _SendMessage(oss.str());
}

BOOL CIPCClient::SendCaretUpdate(int x, int y, int height)
{
    if (!_ShouldAttemptOperation())
    {
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
        return FALSE;
    }

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

    _LogDebug(L"Sending focus_lost");

    std::wstring json = L"{\"type\":\"focus_lost\",\"data\":{}}";
    return _SendMessage(json);
}

BOOL CIPCClient::SendFocusGained(LONG caretX, LONG caretY, LONG caretHeight)
{
    // Try to connect if not connected (important for first focus_gained)
    if (!_ShouldAttemptOperation())
    {
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
        return FALSE;
    }

    _LogDebug(L"Sending focus_gained with caret: x=%ld, y=%ld, h=%ld", caretX, caretY, caretHeight);

    // Include caret position so service knows which screen to show toolbar on
    wchar_t buffer[256];
    swprintf_s(buffer, L"{\"type\":\"focus_gained\",\"data\":{\"caret\":{\"x\":%ld,\"y\":%ld,\"height\":%ld}}}",
               caretX, caretY, caretHeight);
    std::wstring json = buffer;
    return _SendMessage(json);
}

BOOL CIPCClient::SendToggleMode()
{
    if (!_ShouldAttemptOperation())
    {
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
        return FALSE;
    }

    _LogDebug(L"Sending toggle_mode");

    std::wstring json = L"{\"type\":\"toggle_mode\",\"data\":{}}";
    return _SendMessage(json);
}

BOOL CIPCClient::SendCapsLockState(BOOL capsLockOn)
{
    if (!_ShouldAttemptOperation())
    {
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
        return FALSE;
    }

    std::wostringstream oss;
    oss << L"{\"type\":\"caps_lock_state\",\"data\":{\"caps_lock_on\":";
    oss << (capsLockOn ? L"true" : L"false");
    oss << L"}}";

    return _SendMessage(oss.str());
}

BOOL CIPCClient::SendMenuCommand(const char* command)
{
    if (!_ShouldAttemptOperation())
    {
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
        return FALSE;
    }

    _LogDebug(L"Sending menu_command: %S", command);

    // Convert command to wide string
    int wideSize = MultiByteToWideChar(CP_UTF8, 0, command, -1, nullptr, 0);
    std::vector<wchar_t> wideCommand(wideSize);
    MultiByteToWideChar(CP_UTF8, 0, command, -1, wideCommand.data(), wideSize);

    std::wostringstream oss;
    oss << L"{\"type\":\"menu_command\",\"data\":{\"command\":\"";
    oss << wideCommand.data();
    oss << L"\"}}";

    return _SendMessage(oss.str());
}

BOOL CIPCClient::SendIMEDeactivated()
{
    // Don't check circuit breaker here - we want to send this even if failing
    // This is important for cleanup
    if (!IsConnected())
    {
        return FALSE;
    }

    _LogInfo(L"Sending ime_deactivated");

    std::wstring json = L"{\"type\":\"ime_deactivated\",\"data\":{}}";
    return _SendMessage(json);
}

BOOL CIPCClient::SendIMEActivated()
{
    if (!_ShouldAttemptOperation())
    {
        return FALSE;
    }

    _LogInfo(L"Sending ime_activated");

    std::wstring json = L"{\"type\":\"ime_activated\",\"data\":{}}";
    return _SendMessage(json);
}

BOOL CIPCClient::_SendMessage(const std::wstring& message)
{
    // Convert to UTF-8
    int utf8Size = WideCharToMultiByte(CP_UTF8, 0, message.c_str(), -1, nullptr, 0, nullptr, nullptr);
    if (utf8Size <= 0)
    {
        _LogError(L"Failed to calculate UTF-8 size");
        return FALSE;
    }

    std::vector<char> utf8Buffer(utf8Size);
    WideCharToMultiByte(CP_UTF8, 0, message.c_str(), -1, utf8Buffer.data(), utf8Size, nullptr, nullptr);

    // Message length (excluding null terminator)
    DWORD messageLength = static_cast<DWORD>(utf8Size - 1);

    // Write length prefix with timeout
    if (!_WriteWithTimeout(&messageLength, sizeof(DWORD), IPCConfig::WRITE_TIMEOUT_MS))
    {
        _LogError(L"Failed to write message length");
        _RecordFailure();
        Disconnect();
        return FALSE;
    }

    // Write message content with timeout
    if (!_WriteWithTimeout(utf8Buffer.data(), messageLength, IPCConfig::WRITE_TIMEOUT_MS))
    {
        _LogError(L"Failed to write message content");
        _RecordFailure();
        Disconnect();
        return FALSE;
    }

    _RecordSuccess();
    return TRUE;
}

// ============================================================================
// Message receiving
// ============================================================================

BOOL CIPCClient::ReceiveResponse(ServiceResponse& response)
{
    std::wstring json;
    if (!_ReceiveMessage(json))
    {
        return FALSE;
    }

    return _ParseResponse(json, response);
}

BOOL CIPCClient::_ReceiveMessage(std::wstring& message)
{
    // Read length prefix with timeout
    DWORD messageLength;
    DWORD bytesRead;

    if (!_ReadWithTimeout(&messageLength, sizeof(DWORD), &bytesRead, IPCConfig::READ_TIMEOUT_MS))
    {
        _LogError(L"Failed to read message length");
        _RecordFailure();
        Disconnect();
        return FALSE;
    }

    if (bytesRead != sizeof(DWORD))
    {
        _LogError(L"Incomplete length read: got %d bytes", bytesRead);
        _RecordFailure();
        Disconnect();
        return FALSE;
    }

    if (messageLength == 0 || messageLength > IPCConfig::MAX_MESSAGE_SIZE)
    {
        _LogError(L"Invalid message length: %d", messageLength);
        _RecordFailure();
        return FALSE;
    }

    // Read message content with timeout
    std::vector<char> utf8Buffer(messageLength + 1);
    DWORD totalRead = 0;

    // May need multiple reads for large messages
    while (totalRead < messageLength)
    {
        DWORD chunkRead;
        if (!_ReadWithTimeout(utf8Buffer.data() + totalRead, messageLength - totalRead, &chunkRead, IPCConfig::READ_TIMEOUT_MS))
        {
            _LogError(L"Failed to read message content");
            _RecordFailure();
            Disconnect();
            return FALSE;
        }

        if (chunkRead == 0)
        {
            _LogError(L"Incomplete content read: expected %d, got %d", messageLength, totalRead);
            _RecordFailure();
            Disconnect();
            return FALSE;
        }

        totalRead += chunkRead;
    }

    utf8Buffer[messageLength] = '\0';

    // Convert from UTF-8
    int wideSize = MultiByteToWideChar(CP_UTF8, 0, utf8Buffer.data(), -1, nullptr, 0);
    if (wideSize <= 0)
    {
        _LogError(L"Failed to calculate wide string size");
        return FALSE;
    }

    std::vector<wchar_t> wideBuffer(wideSize);
    MultiByteToWideChar(CP_UTF8, 0, utf8Buffer.data(), -1, wideBuffer.data(), wideSize);

    message = wideBuffer.data();

    _LogDebug(L"Received JSON (len=%d): %.200s", (int)message.length(), message.c_str());

    _RecordSuccess();
    return TRUE;
}

BOOL CIPCClient::_ParseResponse(const std::wstring& json, ServiceResponse& response)
{
    response.type = ResponseType::Unknown;
    response.text.clear();
    response.composition.clear();
    response.caretPos = 0;
    response.chineseMode = FALSE;
    response.fullWidth = FALSE;
    response.chinesePunct = TRUE;
    response.toolbarVisible = FALSE;
    response.error.clear();

    // Parse type field
    if (json.find(L"\"type\":\"ack\"") != std::wstring::npos)
    {
        response.type = ResponseType::Ack;
    }
    else if (json.find(L"\"type\":\"insert_text\"") != std::wstring::npos)
    {
        response.type = ResponseType::InsertText;
        _LogDebug(L"Response type: InsertText");

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

        // Extract mode_changed flag (for CommitOnSwitch feature)
        response.modeChanged = (json.find(L"\"mode_changed\":true") != std::wstring::npos);

        // Extract chinese_mode if mode was changed
        if (response.modeChanged)
        {
            response.chineseMode = (json.find(L"\"chinese_mode\":true") != std::wstring::npos);
            _LogDebug(L"InsertText with mode change: chineseMode=%d", response.chineseMode);
        }

        // Extract new_composition (for top code commit)
        size_t newCompPos = json.find(L"\"new_composition\":\"");
        if (newCompPos != std::wstring::npos)
        {
            newCompPos += 19;
            size_t newCompEnd = json.find(L"\"", newCompPos);
            if (newCompEnd != std::wstring::npos)
            {
                response.newComposition = json.substr(newCompPos, newCompEnd - newCompPos);
                _LogDebug(L"InsertText with new composition: %s", response.newComposition.c_str());
            }
        }

        _LogDebug(L"InsertText: text=%s, modeChanged=%d, newComposition=%s", response.text.c_str(), response.modeChanged, response.newComposition.c_str());
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
        _LogDebug(L"Response type: ModeChanged");

        // Extract chinese_mode from data
        response.chineseMode = (json.find(L"\"chinese_mode\":true") != std::wstring::npos) ? TRUE : FALSE;

        _LogDebug(L"ModeChanged: chineseMode=%s", response.chineseMode ? L"true" : L"false");
    }
    else if (json.find(L"\"type\":\"status_update\"") != std::wstring::npos)
    {
        response.type = ResponseType::StatusUpdate;
        _LogDebug(L"Response type: StatusUpdate");

        // Extract status fields from data
        response.chineseMode = (json.find(L"\"chinese_mode\":true") != std::wstring::npos) ? TRUE : FALSE;
        response.fullWidth = (json.find(L"\"full_width\":true") != std::wstring::npos) ? TRUE : FALSE;
        response.chinesePunct = (json.find(L"\"chinese_punctuation\":true") != std::wstring::npos) ? TRUE : FALSE;
        response.toolbarVisible = (json.find(L"\"toolbar_visible\":true") != std::wstring::npos) ? TRUE : FALSE;

        _LogDebug(L"StatusUpdate: mode=%d, width=%d, punct=%d, toolbar=%d",
                  response.chineseMode, response.fullWidth, response.chinesePunct, response.toolbarVisible);

        // Parse hotkeys configuration if present (search for "hotkeys": with optional spaces)
        size_t hotkeysPos = json.find(L"\"hotkeys\":");
        if (hotkeysPos != std::wstring::npos)
        {
            // Skip whitespace after colon and check for object start
            size_t checkPos = hotkeysPos + 10; // length of "hotkeys":
            while (checkPos < json.length() && (json[checkPos] == L' ' || json[checkPos] == L'\t'))
                checkPos++;
            if (checkPos < json.length() && json[checkPos] == L'{')
            {
                response.hotkeys.hasData = TRUE;
                _LogDebug(L"StatusUpdate contains hotkeys config");

            // Helper lambda to parse string array (handles optional spaces after colon)
            auto parseStringArray = [&json](const wchar_t* fieldName) -> std::vector<std::wstring> {
                std::vector<std::wstring> result;
                // Search for "fieldName":
                std::wstring searchKey = std::wstring(L"\"") + fieldName + L"\":";
                size_t pos = json.find(searchKey);
                if (pos != std::wstring::npos)
                {
                    pos += searchKey.length();
                    // Skip any whitespace
                    while (pos < json.length() && (json[pos] == L' ' || json[pos] == L'\t' || json[pos] == L'\n' || json[pos] == L'\r'))
                        pos++;
                    // Check for array start
                    if (pos < json.length() && json[pos] == L'[')
                    {
                        pos++; // Skip '['
                        size_t endPos = json.find(L"]", pos);
                        if (endPos != std::wstring::npos)
                        {
                            std::wstring arrayContent = json.substr(pos, endPos - pos);
                            size_t strStart = 0;
                            while ((strStart = arrayContent.find(L"\"", strStart)) != std::wstring::npos)
                            {
                                strStart++;
                                size_t strEnd = arrayContent.find(L"\"", strStart);
                                if (strEnd != std::wstring::npos)
                                {
                                    result.push_back(arrayContent.substr(strStart, strEnd - strStart));
                                    strStart = strEnd + 1;
                                }
                                else break;
                            }
                        }
                    }
                }
                return result;
            };

            // Helper lambda to parse string value (handles optional spaces after colon)
            auto parseStringValue = [&json](const wchar_t* fieldName) -> std::wstring {
                std::wstring searchKey = std::wstring(L"\"") + fieldName + L"\":";
                size_t pos = json.find(searchKey);
                if (pos != std::wstring::npos)
                {
                    pos += searchKey.length();
                    // Skip any whitespace
                    while (pos < json.length() && (json[pos] == L' ' || json[pos] == L'\t' || json[pos] == L'\n' || json[pos] == L'\r'))
                        pos++;
                    // Check for string start
                    if (pos < json.length() && json[pos] == L'"')
                    {
                        pos++; // Skip opening quote
                        size_t endPos = json.find(L"\"", pos);
                        if (endPos != std::wstring::npos)
                        {
                            return json.substr(pos, endPos - pos);
                        }
                    }
                }
                return L"";
            };

            response.hotkeys.toggleModeKeys = parseStringArray(L"toggle_mode_keys");
            response.hotkeys.switchEngine = parseStringValue(L"switch_engine");
            response.hotkeys.toggleFullWidth = parseStringValue(L"toggle_full_width");
            response.hotkeys.togglePunct = parseStringValue(L"toggle_punct");
            response.hotkeys.selectKeyGroups = parseStringArray(L"select_key_groups");
            response.hotkeys.pageKeys = parseStringArray(L"page_keys");

            // Debug: log parsed toggle mode keys
            std::wstring toggleKeysStr;
            for (const auto& key : response.hotkeys.toggleModeKeys)
            {
                if (!toggleKeysStr.empty()) toggleKeysStr += L",";
                toggleKeysStr += key;
            }
            _LogInfo(L"Hotkeys parsed: toggleModeKeys=[%s] (%d items), selectKeyGroups=%d items, pageKeys=%d items",
                      toggleKeysStr.c_str(),
                      (int)response.hotkeys.toggleModeKeys.size(),
                      (int)response.hotkeys.selectKeyGroups.size(),
                      (int)response.hotkeys.pageKeys.size());
            }
        }
    }
    else if (json.find(L"\"type\":\"consumed\"") != std::wstring::npos)
    {
        response.type = ResponseType::Consumed;
        _LogDebug(L"Response type: Consumed (key consumed by hotkey)");
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
