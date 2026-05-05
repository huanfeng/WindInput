#include "IPCClient.h"
#include "FileLogger.h"
#include <sstream>
#include <cstdarg>
#include <cstring>

#pragma comment(lib, "advapi32.lib")

// Static member initialization
IPCLogLevel CIPCClient::s_logLevel = IPCLogLevel::Info;

CIPCClient::CIPCClient()
    : _hPipe(INVALID_HANDLE_VALUE)
    , _hEvent(NULL)
    , _serviceStartAttempted(FALSE)
    , _needsStateSync(TRUE)
    , _circuitState(CircuitState::Closed)
    , _consecutiveFailures(0)
    , _lastFailureTime(0)
    , _hAsyncThread(NULL)
    , _hStopEvent(NULL)
    , _hReadPipe(INVALID_HANDLE_VALUE)
    , _asyncReaderRunning(FALSE)
{
    // Create event for overlapped I/O
    _hEvent = CreateEventW(NULL, TRUE, FALSE, NULL);
    if (_hEvent == NULL)
    {
        _LogError(L"Failed to create overlapped event: %d", GetLastError());
    }

    // Initialize critical section for async reader
    InitializeCriticalSection(&_asyncLock);

    // Create stop event for async reader thread
    _hStopEvent = CreateEventW(NULL, TRUE, FALSE, NULL);
    if (_hStopEvent == NULL)
    {
        _LogError(L"Failed to create stop event: %d", GetLastError());
    }
}

CIPCClient::~CIPCClient()
{
    StopAsyncReader();
    Disconnect();

    if (_hEvent != NULL)
    {
        CloseHandle(_hEvent);
        _hEvent = NULL;
    }

    if (_hStopEvent != NULL)
    {
        CloseHandle(_hStopEvent);
        _hStopEvent = NULL;
    }

    DeleteCriticalSection(&_asyncLock);
}

// ============================================================================
// Logging helpers
// All levels are now controlled at runtime via FileLogger config.
// IPCClient logs are routed through WIND_LOG_* macros for unified output.
// ============================================================================

void CIPCClient::_Log(IPCLogLevel level, const wchar_t* format, ...)
{
    // Map IPCLogLevel to WIND_LOG level: Error=1, Info=3, Debug=4
    int windLevel = (level == IPCLogLevel::Error) ? 1 : (level == IPCLogLevel::Info) ? 3 : 4;
    if (!CFileLogger::Instance().IsEnabled(static_cast<CFileLogger::LogLevel>(windLevel)))
        return;

    wchar_t buffer[1024];
    va_list args;
    va_start(args, format);
    _vsnwprintf_s(buffer, _countof(buffer), _TRUNCATE, format, args);
    va_end(args);

    CFileLogger::Instance().Write(static_cast<CFileLogger::LogLevel>(windLevel), buffer);
}

void CIPCClient::_LogError(const wchar_t* format, ...)
{
    if (!CFileLogger::Instance().IsEnabled(CFileLogger::LogLevel::Error))
        return;

    wchar_t msgBuffer[1024];
    va_list args;
    va_start(args, format);
    _vsnwprintf_s(msgBuffer, _countof(msgBuffer), _TRUNCATE, format, args);
    va_end(args);

    CFileLogger::Instance().Write(CFileLogger::LogLevel::Error, msgBuffer);
}

void CIPCClient::_LogInfo(const wchar_t* format, ...)
{
    if (!CFileLogger::Instance().IsEnabled(CFileLogger::LogLevel::Info))
        return;

    wchar_t msgBuffer[1024];
    va_list args;
    va_start(args, format);
    _vsnwprintf_s(msgBuffer, _countof(msgBuffer), _TRUNCATE, format, args);
    va_end(args);

    CFileLogger::Instance().Write(CFileLogger::LogLevel::Info, msgBuffer);
}

void CIPCClient::_LogDebug(const wchar_t* format, ...)
{
    if (!CFileLogger::Instance().IsEnabled(CFileLogger::LogLevel::Debug))
        return;

    wchar_t msgBuffer[1024];
    va_list args;
    va_start(args, format);
    _vsnwprintf_s(msgBuffer, _countof(msgBuffer), _TRUNCATE, format, args);
    va_end(args);

    CFileLogger::Instance().Write(CFileLogger::LogLevel::Debug, msgBuffer);
}

// ============================================================================
// Encoding helpers
// ============================================================================

std::wstring CIPCClient::_Utf8ToWide(const char* utf8, size_t length)
{
    if (length == 0) return L"";

    int wideSize = MultiByteToWideChar(CP_UTF8, 0, utf8, (int)length, nullptr, 0);
    if (wideSize <= 0) return L"";

    std::wstring result(wideSize, L'\0');
    MultiByteToWideChar(CP_UTF8, 0, utf8, (int)length, &result[0], wideSize);
    return result;
}

std::string CIPCClient::_WideToUtf8(const std::wstring& wide)
{
    if (wide.empty()) return "";

    int utf8Size = WideCharToMultiByte(CP_UTF8, 0, wide.c_str(), (int)wide.length(), nullptr, 0, nullptr, nullptr);
    if (utf8Size <= 0) return "";

    std::string result(utf8Size, '\0');
    WideCharToMultiByte(CP_UTF8, 0, wide.c_str(), (int)wide.length(), &result[0], utf8Size, nullptr, nullptr);
    return result;
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
        DWORD elapsed = GetTickCount() - _lastFailureTime;
        if (elapsed >= IPCConfig::CIRCUIT_RESET_INTERVAL_MS)
        {
            _LogInfo(L"Circuit breaker half-open, attempting reconnection...");
            _circuitState = CircuitState::HalfOpen;
            _serviceStartAttempted = FALSE;  // 允许重新尝试拉起服务
            return TRUE;
        }
        return FALSE;
    }

    return TRUE;
}

void CIPCClient::ResetCircuitBreaker()
{
    _consecutiveFailures = 0;
    _circuitState = CircuitState::Closed;
    _serviceStartAttempted = FALSE;  // 允许重新尝试拉起服务
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
        return bytesWritten == size;
    }

    DWORD error = GetLastError();
    if (error != ERROR_IO_PENDING)
    {
        _LogError(L"WriteFile failed immediately: %d", error);
        return FALSE;
    }

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
        return TRUE;
    }

    DWORD error = GetLastError();
    if (error != ERROR_IO_PENDING)
    {
        _LogDebug(L"ReadFile failed immediately: %d", error);
        return FALSE;
    }

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

    WCHAR dllPath[MAX_PATH];

    if (GetModuleFileNameW(g_hInstance, dllPath, MAX_PATH) == 0)
    {
        _LogError(L"Failed to get module path");
        return FALSE;
    }

    WCHAR* lastSlash = wcsrchr(dllPath, L'\\');

    // Guard: check portable mode marker file for stopped flag
    {
        if (lastSlash)
        {
            WCHAR markerPath[MAX_PATH];
            wcsncpy_s(markerPath, dllPath, (lastSlash - dllPath + 1));
            wcscat_s(markerPath, MAX_PATH, L"wind_portable_mode");

            HANDLE hFile = CreateFileW(
                markerPath,
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                nullptr,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                nullptr);

            if (hFile != INVALID_HANDLE_VALUE)
            {
                char buf[256] = {};
                DWORD bytesRead = 0;
                ReadFile(hFile, buf, sizeof(buf) - 1, &bytesRead, nullptr);
                CloseHandle(hFile);
                buf[bytesRead] = '\0';

                if (strstr(buf, "stopped=1") != nullptr)
                {
                    _LogInfo(L"Portable mode stopped flag detected, not starting service");
                    return FALSE;
                }
            }
        }
    }

    if (lastSlash)
    {
#ifdef WIND_DEBUG_VARIANT
        wcscpy_s(lastSlash + 1, MAX_PATH - (lastSlash - dllPath + 1), L"wind_input_debug.exe");
#else
        wcscpy_s(lastSlash + 1, MAX_PATH - (lastSlash - dllPath + 1), L"wind_input.exe");
#endif
    }

    _LogDebug(L"Starting service: %s", dllPath);

    STARTUPINFOW si = { sizeof(STARTUPINFOW) };
    si.dwFlags = STARTF_USESHOWWINDOW;
    si.wShowWindow = SW_HIDE;

    PROCESS_INFORMATION pi = {};

    // CREATE_NEW_CONSOLE: 独立控制台，不继承宿主进程的控制台
    // CREATE_BREAKAWAY_FROM_JOB: 脱离宿主进程的 Job 对象（如 UWP 沙盒）
    // CREATE_DEFAULT_ERROR_MODE: 使用系统默认错误模式
    DWORD flags = CREATE_NEW_CONSOLE | CREATE_DEFAULT_ERROR_MODE;

    // 尝试脱离 Job 对象（某些宿主进程可能不允许，失败后回退）
    if (!CreateProcessW(dllPath, nullptr, nullptr, nullptr, FALSE,
        flags | CREATE_BREAKAWAY_FROM_JOB, nullptr, nullptr, &si, &pi))
    {
        // 回退：不使用 BREAKAWAY（部分进程可能限制此标志）
        if (!CreateProcessW(dllPath, nullptr, nullptr, nullptr, FALSE,
            flags, nullptr, nullptr, &si, &pi))
        {
            _LogError(L"Failed to start service: error=%d", GetLastError());
            return FALSE;
        }
    }

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

    _LogDebug(L"Connecting to Go Service (binary protocol)...");

    for (int attempt = 0; attempt < 3; attempt++)
    {
        if (!WaitNamedPipeW(PIPE_NAME, IPCConfig::CONNECT_TIMEOUT_MS))
        {
            DWORD error = GetLastError();
            if (error == ERROR_FILE_NOT_FOUND)
            {
                if (!_serviceStartAttempted)
                {
                    _serviceStartAttempted = TRUE;
                    if (_StartService())
                    {
                        Sleep(500);
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
        }

        _hPipe = CreateFileW(
            PIPE_NAME,
            GENERIC_READ | GENERIC_WRITE,
            0,
            nullptr,
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            nullptr);

        if (_hPipe != INVALID_HANDLE_VALUE)
        {
            // Use MESSAGE mode like Weasel for reliable message boundaries
            DWORD mode = PIPE_READMODE_MESSAGE;
            SetNamedPipeHandleState(_hPipe, &mode, nullptr, nullptr);

            _LogInfo(L"Connected to Go Service (binary protocol v%d.%d)",
                     PROTOCOL_VERSION >> 12, PROTOCOL_VERSION & 0xFFF);
            _RecordSuccess();
            _needsStateSync = TRUE;

            return TRUE;
        }

        DWORD error = GetLastError();
        _LogDebug(L"Connection attempt %d failed: error=%d", attempt + 1, error);
        if (error == ERROR_ACCESS_DENIED)
        {
            WindHostProcessInfo currentHost;
            if (WindQueryCurrentProcessInfo(&currentHost))
                WindLogHostProcessInfo(4, L"compat.ipc_connect_access_denied.current_host", currentHost);
            WindLogForegroundProcessInfo(4, L"compat.ipc_connect_access_denied.foreground_host");
        }

        if (error == ERROR_PIPE_BUSY)
        {
            Sleep(50);
            continue;
        }

        break;
    }

    _LogError(L"Failed to connect to Go Service (pipe=%s, lastErr=%d)", PIPE_NAME, GetLastError());
    {
        WindHostProcessInfo currentHost;
        if (WindQueryCurrentProcessInfo(&currentHost))
            WindLogHostProcessInfo(4, L"compat.ipc_connect_failed.current_host", currentHost);
        WindLogForegroundProcessInfo(4, L"compat.ipc_connect_failed.foreground_host");
    }
    _RecordFailure();
    return FALSE;
}

void CIPCClient::Disconnect()
{
    if (_hPipe != INVALID_HANDLE_VALUE)
    {
        CancelIo(_hPipe);
        CloseHandle(_hPipe);
        _hPipe = INVALID_HANDLE_VALUE;
        _LogDebug(L"Disconnected from Go Service");
    }
}

// ============================================================================
// Binary message sending
// ============================================================================

BOOL CIPCClient::_SendBinaryMessage(uint16_t command, const void* payload, uint32_t payloadSize, bool async)
{
    // Build header
    IpcHeader header;
    header.version = async ? (PROTOCOL_VERSION | ASYNC_FLAG) : PROTOCOL_VERSION;
    header.command = command;
    header.length = payloadSize;

    // In MESSAGE mode, we must write header + payload as a single message
    // Combine into a single buffer for atomic write
    std::vector<uint8_t> buffer(sizeof(header) + payloadSize);
    memcpy(buffer.data(), &header, sizeof(header));
    if (payloadSize > 0 && payload != nullptr)
    {
        memcpy(buffer.data() + sizeof(header), payload, payloadSize);
    }

    // Write complete message in one call
    if (!_WriteWithTimeout(buffer.data(), static_cast<DWORD>(buffer.size()), IPCConfig::WRITE_TIMEOUT_MS))
    {
        _LogError(L"Failed to write message");
        _RecordFailure();
        Disconnect();
        return FALSE;
    }

    _RecordSuccess();
    return TRUE;
}

// ============================================================================
// Message sending
// ============================================================================

BOOL CIPCClient::SendKeyEvent(uint32_t keyCode, uint32_t scanCode, uint32_t modifiers, uint8_t eventType,
                              uint8_t toggles, uint16_t eventSeq, uint16_t prevChar)
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

    KeyPayload payload;
    payload.keyCode = keyCode;
    payload.scanCode = scanCode;
    payload.modifiers = modifiers;
    payload.eventType = eventType;
    payload.toggles = toggles;
    payload.eventSeq = eventSeq;
    payload.prevChar = prevChar;

    _LogDebug(L"Sending key event: keyCode=0x%X, mods=0x%X, type=%d, toggles=0x%X, seq=%d, prevChar=0x%X",
              keyCode, modifiers, eventType, toggles, eventSeq, prevChar);

    return _SendBinaryMessage(CMD_KEY_EVENT, &payload, sizeof(payload));
}

BOOL CIPCClient::SendCommitRequest(const uint8_t* payload, uint32_t payloadSize)
{
    if (!_ShouldAttemptOperation())
    {
        _LogDebug(L"Circuit open, skipping commit request");
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
        return FALSE;
    }

    _LogDebug(L"Sending commit request: payloadSize=%d", payloadSize);

    return _SendBinaryMessage(CMD_COMMIT_REQUEST, payload, payloadSize);
}

BOOL CIPCClient::SendCaretUpdate(int x, int y, int height, int compositionStartX, int compositionStartY)
{
    if (!_ShouldAttemptOperation())
    {
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
        return FALSE;
    }

    CaretPayload payload;
    payload.x = x;
    payload.y = y;
    payload.height = height;
    payload.compositionStartX = compositionStartX;
    payload.compositionStartY = compositionStartY;

    _LogDebug(L"Sending caret update (async): x=%d, y=%d, h=%d, compStart=(%d,%d)", x, y, height, compositionStartX, compositionStartY);

    // Send async - no response needed for caret updates
    return _SendBinaryMessage(CMD_CARET_UPDATE, &payload, sizeof(payload), true /* async */);
}

BOOL CIPCClient::SendSelectionChanged(uint16_t prevChar)
{
    if (!_ShouldAttemptOperation())
    {
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
        return FALSE;
    }

    SelectionChangedPayload payload;
    payload.prevChar = prevChar;
    payload.reserved = 0;

    _LogDebug(L"Sending selection changed (async): prevChar=0x%X", prevChar);

    return _SendBinaryMessage(CMD_SELECTION_CHANGED, &payload, sizeof(payload), true /* async */);
}

BOOL CIPCClient::SendFocusLost()
{
    if (!IsConnected())
    {
        return FALSE;
    }

    _LogDebug(L"Sending focus_lost (async)");
    // Send async - no response needed for focus lost
    return _SendBinaryMessage(CMD_FOCUS_LOST, nullptr, 0, true /* async */);
}

BOOL CIPCClient::SendCaretPending()
{
    if (!_ShouldAttemptOperation())
    {
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
        return FALSE;
    }

    _LogDebug(L"Sending caret_pending (async): composition just started, awaiting reflow");
    return _SendBinaryMessage(CMD_CARET_PENDING, nullptr, 0, true /* async */);
}

BOOL CIPCClient::SendCompositionTerminated()
{
    if (!IsConnected())
    {
        return FALSE;
    }

    _LogDebug(L"Sending composition_terminated (async)");
    // Send async - no response needed for composition terminated
    return _SendBinaryMessage(CMD_COMPOSITION_TERMINATED, nullptr, 0, true /* async */);
}

BOOL CIPCClient::SendFocusGained(int caretX, int caretY, int caretHeight)
{
    if (!_ShouldAttemptOperation())
    {
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
        return FALSE;
    }

    _LogDebug(L"Sending focus_gained with caret: x=%d, y=%d, h=%d", caretX, caretY, caretHeight);

    CaretPayload payload = {};  // Zero-initialize to avoid garbage in compositionStartX/Y
    payload.x = caretX;
    payload.y = caretY;
    payload.height = caretHeight;

    return _SendBinaryMessage(CMD_FOCUS_GAINED, &payload, sizeof(payload));
}

BOOL CIPCClient::SendIMEDeactivated()
{
    if (!IsConnected())
    {
        return FALSE;
    }

    _LogInfo(L"Sending ime_deactivated (async)");
    // Send async - no response needed for IME deactivated
    return _SendBinaryMessage(CMD_IME_DEACTIVATED, nullptr, 0, true /* async */);
}

BOOL CIPCClient::SendIMEActivated()
{
    if (!_ShouldAttemptOperation())
    {
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
        return FALSE;
    }

    _LogInfo(L"Sending ime_activated");
    return _SendBinaryMessage(CMD_IME_ACTIVATED, nullptr, 0);
}

BOOL CIPCClient::SendHostRenderRequest(ServiceResponse& response)
{
    if (!_ShouldAttemptOperation())
    {
        return FALSE;
    }

    if (!IsConnected())
    {
        return FALSE;
    }

    _LogInfo(L"Sending host_render_request");
    if (!_SendBinaryMessage(CMD_HOST_RENDER_REQUEST, nullptr, 0))
    {
        return FALSE;
    }

    return ReceiveResponse(response);
}

BOOL CIPCClient::SendModeNotify(bool chineseMode, bool clearInput)
{
    if (!_ShouldAttemptOperation())
    {
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
        return FALSE;
    }

    // Build status flags
    uint32_t flags = 0;
    if (chineseMode) flags |= STATUS_CHINESE_MODE;
    if (clearInput) flags |= STATUS_MODE_CHANGED; // Reuse this flag to indicate input should be cleared

    _LogInfo(L"Sending mode_notify (async): chineseMode=%d, clearInput=%d", chineseMode, clearInput);

    // Send async - no response needed for mode notification
    return _SendBinaryMessage(CMD_MODE_NOTIFY, &flags, sizeof(flags), true /* async */);
}

BOOL CIPCClient::SendSystemModeSwitch(bool chineseMode, ServiceResponse& response)
{
    if (!_ShouldAttemptOperation())
    {
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
        return FALSE;
    }

    // Build status flags (same format as ModeNotify)
    uint32_t flags = 0;
    if (chineseMode) flags |= STATUS_CHINESE_MODE;

    _LogInfo(L"Sending system_mode_switch (sync): chineseMode=%d", chineseMode);

    // Send sync - wait for response (CommitText or ModeChanged)
    if (!_SendBinaryMessage(CMD_SYSTEM_MODE_SWITCH, &flags, sizeof(flags)))
    {
        return FALSE;
    }

    return ReceiveResponse(response);
}

BOOL CIPCClient::SendToggleMode(ServiceResponse& response)
{
    if (!_ShouldAttemptOperation())
    {
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
        return FALSE;
    }

    _LogInfo(L"Sending toggle_mode (sync)");

    // Send sync - wait for ModeChanged response
    if (!_SendBinaryMessage(CMD_TOGGLE_MODE, nullptr, 0))
    {
        return FALSE;
    }

    // Receive response
    return ReceiveResponse(response);
}

// ============================================================================
// Message receiving
// ============================================================================

BOOL CIPCClient::_ReceiveBinaryMessage(IpcHeader& header, std::vector<uint8_t>& payload)
{
    // In MESSAGE mode, read the complete message at once
    std::vector<uint8_t> messageBuffer(IPCConfig::PIPE_BUFFER_SIZE);
    DWORD bytesRead;

    if (!_ReadWithTimeout(messageBuffer.data(), static_cast<DWORD>(messageBuffer.size()), &bytesRead, IPCConfig::READ_TIMEOUT_MS))
    {
        _LogError(L"Failed to read message");
        _RecordFailure();
        Disconnect();
        return FALSE;
    }

    // Check if we have enough data for a header
    if (bytesRead < sizeof(IpcHeader))
    {
        _LogError(L"Message too short: got %d bytes", bytesRead);
        _RecordFailure();
        Disconnect();
        return FALSE;
    }

    // Parse header from the message buffer
    memcpy(&header, messageBuffer.data(), sizeof(IpcHeader));

    // Validate header
    if ((header.version >> 12) != (PROTOCOL_VERSION >> 12))
    {
        _LogError(L"Protocol version mismatch: got 0x%04X, expected 0x%04X", header.version, PROTOCOL_VERSION);
        _RecordFailure();
        return FALSE;
    }

    if (header.length > IPCConfig::MAX_MESSAGE_SIZE)
    {
        _LogError(L"Invalid payload length: %d", header.length);
        _RecordFailure();
        return FALSE;
    }

    _LogDebug(L"Received header: cmd=0x%04X, len=%d", header.command, header.length);

    // Check if the message contains the expected payload
    DWORD expectedSize = sizeof(IpcHeader) + header.length;
    if (bytesRead < expectedSize)
    {
        _LogError(L"Incomplete message: got %d bytes, expected %d", bytesRead, expectedSize);
        _RecordFailure();
        Disconnect();
        return FALSE;
    }

    // Extract payload from the message buffer
    if (header.length > 0)
    {
        payload.assign(messageBuffer.begin() + sizeof(IpcHeader),
                      messageBuffer.begin() + sizeof(IpcHeader) + header.length);
    }
    else
    {
        payload.clear();
    }

    _RecordSuccess();
    return TRUE;
}

BOOL CIPCClient::ReceiveResponse(ServiceResponse& response)
{
    IpcHeader header;
    std::vector<uint8_t> payload;

    if (!_ReceiveBinaryMessage(header, payload))
    {
        return FALSE;
    }

    return _ParseResponse(header, payload, response);
}

BOOL CIPCClient::_ParseResponse(const IpcHeader& header, const std::vector<uint8_t>& payload, ServiceResponse& response)
{
    // Reset response
    response = ServiceResponse();

    switch (header.command)
    {
    case CMD_ACK:
        response.type = ResponseType::Ack;
        _LogDebug(L"Response: Ack");
        break;

    case CMD_PASS_THROUGH:
        response.type = ResponseType::PassThrough;
        _LogDebug(L"Response: PassThrough (key not handled)");
        break;

    case CMD_CONSUMED:
        response.type = ResponseType::Consumed;
        _LogDebug(L"Response: Consumed");
        break;

    case CMD_CLEAR_COMPOSITION:
        response.type = ResponseType::ClearComposition;
        _LogDebug(L"Response: ClearComposition");
        break;

    case CMD_COMMIT_TEXT:
        {
            response.type = ResponseType::CommitText;

            if (payload.size() < sizeof(CommitTextHeader))
            {
                _LogError(L"CommitText payload too short");
                return FALSE;
            }

            const CommitTextHeader* commitHeader = reinterpret_cast<const CommitTextHeader*>(payload.data());
            response.modeChanged = (commitHeader->flags & COMMIT_FLAG_MODE_CHANGED) != 0;
            response.chineseMode = (commitHeader->flags & COMMIT_FLAG_CHINESE_MODE) != 0;

            // Extract text
            if (commitHeader->textLength > 0)
            {
                size_t textOffset = sizeof(CommitTextHeader);
                if (textOffset + commitHeader->textLength <= payload.size())
                {
                    response.text = _Utf8ToWide(
                        reinterpret_cast<const char*>(payload.data() + textOffset),
                        commitHeader->textLength);
                }
            }

            // Extract new composition text (only if compositionLength > 0)
            if ((commitHeader->flags & COMMIT_FLAG_HAS_NEW_COMPOSITION) && commitHeader->compositionLength > 0)
            {
                size_t compOffset = sizeof(CommitTextHeader) + commitHeader->textLength;
                if (compOffset + commitHeader->compositionLength <= payload.size())
                {
                    response.newComposition = _Utf8ToWide(
                        reinterpret_cast<const char*>(payload.data() + compOffset),
                        commitHeader->compositionLength);
                }
            }
            // restartComposition = flag set, regardless of compositionLength
            // (non-inline preedit sends flag=true + empty composition → placeholder restart)
            response.restartComposition = (commitHeader->flags & COMMIT_FLAG_HAS_NEW_COMPOSITION) != 0;

            _LogDebug(L"Response: CommitText textLen=%zu, modeChanged=%d, restartComp=%d, newCompLen=%zu",
                      response.text.length(), response.modeChanged, response.restartComposition, response.newComposition.length());
        }
        break;

    case CMD_UPDATE_COMPOSITION:
        {
            response.type = ResponseType::UpdateComposition;

            if (payload.size() < sizeof(CompositionHeader))
            {
                _LogError(L"UpdateComposition payload too short");
                return FALSE;
            }

            const CompositionHeader* compHeader = reinterpret_cast<const CompositionHeader*>(payload.data());
            response.caretPos = compHeader->caretPos;

            // Extract composition text
            size_t textLength = payload.size() - sizeof(CompositionHeader);
            if (textLength > 0)
            {
                response.composition = _Utf8ToWide(
                    reinterpret_cast<const char*>(payload.data() + sizeof(CompositionHeader)),
                    textLength);
            }

            _LogDebug(L"Response: UpdateComposition textLen=%zu, caret=%d",
                      response.composition.length(), response.caretPos);
        }
        break;

    case CMD_MODE_CHANGED:
        {
            response.type = ResponseType::ModeChanged;

            if (payload.size() >= 4)
            {
                response.statusFlags = *reinterpret_cast<const uint32_t*>(payload.data());
                response.chineseMode = (response.statusFlags & STATUS_CHINESE_MODE) != 0;
            }

            _LogDebug(L"Response: ModeChanged chineseMode=%d", response.chineseMode);
        }
        break;

    case CMD_STATUS_UPDATE:
        {
            response.type = ResponseType::StatusUpdate;

            if (payload.size() < sizeof(StatusHeader))
            {
                _LogError(L"StatusUpdate payload too short");
                return FALSE;
            }

            const StatusHeader* statusHeader = reinterpret_cast<const StatusHeader*>(payload.data());
            response.statusFlags = statusHeader->flags;
            response.chineseMode = (statusHeader->flags & STATUS_CHINESE_MODE) != 0;

            // Extract hotkeys
            size_t hotkeysOffset = sizeof(StatusHeader);
            uint32_t totalHotkeys = statusHeader->keyDownCount + statusHeader->keyUpCount;

            if (payload.size() >= hotkeysOffset + totalHotkeys * sizeof(uint32_t))
            {
                const uint32_t* hotkeys = reinterpret_cast<const uint32_t*>(payload.data() + hotkeysOffset);

                for (uint32_t i = 0; i < statusHeader->keyDownCount; i++)
                {
                    response.keyDownHotkeys.push_back(hotkeys[i]);
                }

                for (uint32_t i = 0; i < statusHeader->keyUpCount; i++)
                {
                    response.keyUpHotkeys.push_back(hotkeys[statusHeader->keyDownCount + i]);
                }
            }

            // Extract trailing icon label (UTF-8, after StatusHeader + hotkeys)
            size_t structuredSize = hotkeysOffset + totalHotkeys * sizeof(uint32_t);
            if (payload.size() > structuredSize)
            {
                std::string utf8Label(payload.begin() + structuredSize, payload.end());
                int wideLen = MultiByteToWideChar(CP_UTF8, 0, utf8Label.c_str(), (int)utf8Label.size(), NULL, 0);
                if (wideLen > 0)
                {
                    response.iconLabel.resize(wideLen);
                    MultiByteToWideChar(CP_UTF8, 0, utf8Label.c_str(), (int)utf8Label.size(), &response.iconLabel[0], wideLen);
                }
            }

            _LogDebug(L"Response: StatusUpdate mode=%d, width=%d, punct=%d, toolbar=%d, keyDown=%d, keyUp=%d, label=%ls",
                      response.IsChineseMode(), response.IsFullWidth(), response.IsChinesePunct(),
                      response.IsToolbarVisible(), (int)response.keyDownHotkeys.size(), (int)response.keyUpHotkeys.size(),
                      response.iconLabel.empty() ? L"(none)" : response.iconLabel.c_str());
        }
        break;

    case CMD_SYNC_HOTKEYS:
        {
            response.type = ResponseType::StatusUpdate; // Treat as status update

            if (payload.size() < sizeof(StatusHeader))
            {
                _LogError(L"SyncHotkeys payload too short");
                return FALSE;
            }

            const StatusHeader* syncHeader = reinterpret_cast<const StatusHeader*>(payload.data());

            // Extract hotkeys
            size_t hotkeysOffset = sizeof(StatusHeader);
            uint32_t totalHotkeys = syncHeader->keyDownCount + syncHeader->keyUpCount;

            if (payload.size() >= hotkeysOffset + totalHotkeys * sizeof(uint32_t))
            {
                const uint32_t* hotkeys = reinterpret_cast<const uint32_t*>(payload.data() + hotkeysOffset);

                for (uint32_t i = 0; i < syncHeader->keyDownCount; i++)
                {
                    response.keyDownHotkeys.push_back(hotkeys[i]);
                }

                for (uint32_t i = 0; i < syncHeader->keyUpCount; i++)
                {
                    response.keyUpHotkeys.push_back(hotkeys[syncHeader->keyDownCount + i]);
                }
            }

            _LogInfo(L"Response: SyncHotkeys keyDown=%d, keyUp=%d",
                     (int)response.keyDownHotkeys.size(), (int)response.keyUpHotkeys.size());
        }
        break;

    case CMD_STATE_PUSH:
        {
            // State push from Go service (proactive state broadcast)
            // Format is same as StatusUpdate
            response.type = ResponseType::StatusUpdate;

            if (payload.size() < sizeof(StatusHeader))
            {
                _LogError(L"StatePush payload too short");
                return FALSE;
            }

            const StatusHeader* statusHeader = reinterpret_cast<const StatusHeader*>(payload.data());
            response.statusFlags = statusHeader->flags;
            response.chineseMode = (statusHeader->flags & STATUS_CHINESE_MODE) != 0;

            // Extract trailing icon label (UTF-8, after StatusHeader)
            size_t structuredSize = sizeof(StatusHeader) +
                (statusHeader->keyDownCount + statusHeader->keyUpCount) * sizeof(uint32_t);
            if (payload.size() > structuredSize)
            {
                std::string utf8Label(payload.begin() + structuredSize, payload.end());
                int wideLen = MultiByteToWideChar(CP_UTF8, 0, utf8Label.c_str(), (int)utf8Label.size(), NULL, 0);
                if (wideLen > 0)
                {
                    response.iconLabel.resize(wideLen);
                    MultiByteToWideChar(CP_UTF8, 0, utf8Label.c_str(), (int)utf8Label.size(), &response.iconLabel[0], wideLen);
                }
            }

            _LogInfo(L"Response: StatePush mode=%d, width=%d, punct=%d, toolbar=%d, caps=%d, label=%ls",
                     response.IsChineseMode(), response.IsFullWidth(), response.IsChinesePunct(),
                     response.IsToolbarVisible(), response.IsCapsLock(),
                     response.iconLabel.empty() ? L"(none)" : response.iconLabel.c_str());
        }
        break;

    case CMD_COMMIT_RESULT:
        {
            // CommitResult uses same format as CommitText, but includes barrierSeq
            response.type = ResponseType::CommitText; // Treat as CommitText for handling

            if (payload.size() < sizeof(CommitResultPayload))
            {
                _LogError(L"CommitResult payload too short");
                return FALSE;
            }

            const CommitResultPayload* resultPayload = reinterpret_cast<const CommitResultPayload*>(payload.data());
            response.modeChanged = (resultPayload->flags & COMMIT_FLAG_MODE_CHANGED) != 0;
            response.chineseMode = (resultPayload->flags & COMMIT_FLAG_CHINESE_MODE) != 0;

            // Extract text
            if (resultPayload->textLength > 0)
            {
                size_t textOffset = sizeof(CommitResultPayload);
                if (textOffset + resultPayload->textLength <= payload.size())
                {
                    response.text = _Utf8ToWide(
                        reinterpret_cast<const char*>(payload.data() + textOffset),
                        resultPayload->textLength);
                }
            }

            // Extract new composition
            if ((resultPayload->flags & COMMIT_FLAG_HAS_NEW_COMPOSITION) && resultPayload->compositionLength > 0)
            {
                size_t compOffset = sizeof(CommitResultPayload) + resultPayload->textLength;
                if (compOffset + resultPayload->compositionLength <= payload.size())
                {
                    response.newComposition = _Utf8ToWide(
                        reinterpret_cast<const char*>(payload.data() + compOffset),
                        resultPayload->compositionLength);
                }
            }

            _LogDebug(L"Response: CommitResult barrierSeq=%d, textLen=%zu, modeChanged=%d",
                      resultPayload->barrierSeq, response.text.length(), response.modeChanged);
        }
        break;

    case CMD_COMMIT_TEXT_WITH_CURSOR:
        {
            response.type = ResponseType::InsertTextWithCursor;
            if (payload.size() < sizeof(CommitTextWithCursorPayload))
            {
                _LogError(L"CommitTextWithCursor payload too short");
                return FALSE;
            }
            const CommitTextWithCursorPayload* p = reinterpret_cast<const CommitTextWithCursorPayload*>(payload.data());
            response.cursorOffset = (int)p->cursorOffset;
            if (p->textLength > 0)
            {
                size_t textOffset = sizeof(CommitTextWithCursorPayload);
                if (textOffset + p->textLength <= payload.size())
                {
                    response.text = _Utf8ToWide(
                        reinterpret_cast<const char*>(payload.data() + textOffset),
                        p->textLength);
                }
            }
            _LogDebug(L"Response: InsertTextWithCursor textLen=%zu, cursorOffset=%d",
                      response.text.length(), response.cursorOffset);
        }
        break;

    case CMD_MOVE_CURSOR:
        {
            response.type = ResponseType::MoveCursorRight;
            _LogDebug(L"Response: MoveCursorRight");
        }
        break;

    case CMD_DELETE_PAIR:
        {
            response.type = ResponseType::DeletePair;
            _LogDebug(L"Response: DeletePair");
        }
        break;

    case CMD_HOST_RENDER_SETUP:
        {
            response.type = ResponseType::HostRenderSetup;

            if (payload.size() < sizeof(HostRenderSetupHeader))
            {
                _LogError(L"HostRenderSetup payload too short");
                return FALSE;
            }

            const HostRenderSetupHeader* setupHeader = reinterpret_cast<const HostRenderSetupHeader*>(payload.data());
            response.maxBufferSize = setupHeader->maxBufferSize;

            size_t offset = sizeof(HostRenderSetupHeader);
            // Extract shared memory name
            if (setupHeader->shmNameLen > 0 && offset + setupHeader->shmNameLen <= payload.size())
            {
                response.shmName = _Utf8ToWide(
                    reinterpret_cast<const char*>(payload.data() + offset),
                    setupHeader->shmNameLen);
                offset += setupHeader->shmNameLen;
            }
            // Extract event name
            if (setupHeader->eventNameLen > 0 && offset + setupHeader->eventNameLen <= payload.size())
            {
                response.eventName = _Utf8ToWide(
                    reinterpret_cast<const char*>(payload.data() + offset),
                    setupHeader->eventNameLen);
            }

            _LogInfo(L"Response: HostRenderSetup shm=%ls, event=%ls, maxSize=%u",
                     response.shmName.c_str(), response.eventName.c_str(), response.maxBufferSize);
        }
        break;

    default:
        _LogError(L"Unknown response command: 0x%04X", header.command);
        response.type = ResponseType::Error;
        return FALSE;
    }

    return TRUE;
}

// ============================================================================
// Async and Batch support
// ============================================================================

BOOL CIPCClient::SendAsync(uint16_t command, const void* payload, uint32_t size)
{
    if (!_ShouldAttemptOperation())
    {
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
        return FALSE;
    }

    return _SendBinaryMessage(command, payload, size, true /* async */);
}

BOOL CIPCClient::SendSync(uint16_t command, const void* payload, uint32_t size, ServiceResponse& response)
{
    if (!_ShouldAttemptOperation())
    {
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
        return FALSE;
    }

    if (!_SendBinaryMessage(command, payload, size, false /* sync */))
    {
        return FALSE;
    }

    return ReceiveResponse(response);
}

void CIPCClient::BeginBatch()
{
    _batchBuffer.clear();
    _batchNeedResponse.clear();
    _batchCount = 0;

    // Reserve space for BatchHeader (will be filled in SendBatch)
    _batchBuffer.resize(sizeof(BatchHeader));
}

void CIPCClient::AddBatchEvent(uint16_t command, const void* payload, uint32_t size, bool needResponse)
{
    // Build event header
    IpcHeader eventHeader;
    eventHeader.version = needResponse ? PROTOCOL_VERSION : (PROTOCOL_VERSION | ASYNC_FLAG);
    eventHeader.command = command;
    eventHeader.length = size;

    // Append header to buffer
    size_t offset = _batchBuffer.size();
    _batchBuffer.resize(offset + sizeof(IpcHeader) + size);

    memcpy(_batchBuffer.data() + offset, &eventHeader, sizeof(IpcHeader));

    // Append payload if any
    if (size > 0 && payload != nullptr)
    {
        memcpy(_batchBuffer.data() + offset + sizeof(IpcHeader), payload, size);
    }

    _batchNeedResponse.push_back(needResponse);
    _batchCount++;
}

BOOL CIPCClient::SendBatch(std::vector<ServiceResponse>& responses)
{
    if (_batchCount == 0)
    {
        return TRUE;
    }

    if (!_ShouldAttemptOperation())
    {
        return FALSE;
    }

    if (!IsConnected() && !Connect())
    {
        return FALSE;
    }

    // Fill in BatchHeader at the beginning of buffer
    BatchHeader* batchHeader = reinterpret_cast<BatchHeader*>(_batchBuffer.data());
    batchHeader->eventCount = _batchCount;
    batchHeader->reserved = 0;

    _LogDebug(L"Sending batch with %d events, size=%zu", _batchCount, _batchBuffer.size());

    // Send the batch message
    if (!_SendBinaryMessage(CMD_BATCH_EVENTS, _batchBuffer.data(), static_cast<uint32_t>(_batchBuffer.size()), false))
    {
        return FALSE;
    }

    // Count how many sync responses we expect
    int syncCount = 0;
    for (bool needResp : _batchNeedResponse)
    {
        if (needResp)
        {
            syncCount++;
        }
    }

    // Receive batch response if there are sync events
    if (syncCount > 0)
    {
        return ReceiveBatchResponse(responses, syncCount);
    }

    return TRUE;
}

BOOL CIPCClient::ReceiveBatchResponse(std::vector<ServiceResponse>& responses, int expectedCount)
{
    // Read batch response header
    IpcHeader header;
    std::vector<uint8_t> payload;

    if (!_ReceiveBinaryMessage(header, payload))
    {
        return FALSE;
    }

    if (header.command != CMD_BATCH_RESPONSE)
    {
        _LogError(L"Expected batch response, got command 0x%04X", header.command);
        return FALSE;
    }

    if (payload.size() < sizeof(BatchHeader))
    {
        _LogError(L"Batch response payload too short: %zu bytes", payload.size());
        return FALSE;
    }

    // Parse batch response header
    const BatchHeader* batchHeader = reinterpret_cast<const BatchHeader*>(payload.data());
    uint16_t responseCount = batchHeader->eventCount; // eventCount holds response count in batch response

    if (responseCount != expectedCount)
    {
        _LogError(L"Batch response count mismatch: expected %d, got %d", expectedCount, responseCount);
        // Continue processing what we have
    }

    _LogDebug(L"Received batch response with %d responses", responseCount);

    // Parse individual responses
    size_t offset = sizeof(BatchHeader);
    responses.clear();
    responses.reserve(responseCount);

    for (uint16_t i = 0; i < responseCount && offset < payload.size(); i++)
    {
        // Check if we have enough data for a header
        if (offset + sizeof(IpcHeader) > payload.size())
        {
            _LogError(L"Batch response %d: incomplete header at offset %zu", i, offset);
            break;
        }

        // Parse response header
        const IpcHeader* respHeader = reinterpret_cast<const IpcHeader*>(payload.data() + offset);
        offset += sizeof(IpcHeader);

        // Check if we have enough data for the payload
        if (offset + respHeader->length > payload.size())
        {
            _LogError(L"Batch response %d: incomplete payload at offset %zu", i, offset);
            break;
        }

        // Extract payload
        std::vector<uint8_t> respPayload;
        if (respHeader->length > 0)
        {
            respPayload.assign(payload.begin() + offset, payload.begin() + offset + respHeader->length);
            offset += respHeader->length;
        }

        // Parse response
        ServiceResponse response;
        if (_ParseResponse(*respHeader, respPayload, response))
        {
            responses.push_back(std::move(response));
        }
        else
        {
            _LogError(L"Failed to parse batch response %d", i);
            ServiceResponse errorResp;
            errorResp.type = ResponseType::Error;
            responses.push_back(std::move(errorResp));
        }
    }

    return TRUE;
}

// ============================================================================
// Async Reader Thread Implementation
// ============================================================================

void CIPCClient::SetStatePushCallback(StatePushCallback callback)
{
    EnterCriticalSection(&_asyncLock);
    _statePushCallback = callback;
    LeaveCriticalSection(&_asyncLock);
}

void CIPCClient::SetCommitTextCallback(CommitTextCallback callback)
{
    EnterCriticalSection(&_asyncLock);
    _commitTextCallback = callback;
    LeaveCriticalSection(&_asyncLock);
}

void CIPCClient::SetClearCompositionCallback(ClearCompositionCallback callback)
{
    EnterCriticalSection(&_asyncLock);
    _clearCompositionCallback = callback;
    LeaveCriticalSection(&_asyncLock);
}

void CIPCClient::SetUpdateCompositionCallback(UpdateCompositionCallback callback)
{
    EnterCriticalSection(&_asyncLock);
    _updateCompositionCallback = callback;
    LeaveCriticalSection(&_asyncLock);
}

void CIPCClient::SetSyncConfigCallback(SyncConfigCallback callback)
{
    EnterCriticalSection(&_asyncLock);
    _syncConfigCallback = callback;
    LeaveCriticalSection(&_asyncLock);
}

void CIPCClient::SetServiceReadyCallback(ServiceReadyCallback callback)
{
    EnterCriticalSection(&_asyncLock);
    _serviceReadyCallback = callback;
    LeaveCriticalSection(&_asyncLock);
}

BOOL CIPCClient::StartAsyncReader()
{
    if (_asyncReaderRunning)
    {
        _LogDebug(L"Async reader already running");
        return TRUE;
    }

    // Reset stop event
    ResetEvent(_hStopEvent);

    // Connect to push pipe
    _LogInfo(L"Connecting to push pipe...");

    for (int attempt = 0; attempt < 3; attempt++)
    {
        if (!WaitNamedPipeW(PUSH_PIPE_NAME, IPCConfig::CONNECT_TIMEOUT_MS))
        {
            DWORD error = GetLastError();
            if (error == ERROR_FILE_NOT_FOUND)
            {
                _LogDebug(L"Push pipe not found, attempt %d", attempt + 1);
                Sleep(200);
                continue;
            }
            else if (error == ERROR_SEM_TIMEOUT)
            {
                _LogDebug(L"WaitNamedPipe timed out for push pipe, attempt %d", attempt + 1);
                continue;
            }
        }

        _hReadPipe = CreateFileW(
            PUSH_PIPE_NAME,
            GENERIC_READ,
            0,
            nullptr,
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            nullptr);

        if (_hReadPipe != INVALID_HANDLE_VALUE)
        {
            DWORD mode = PIPE_READMODE_MESSAGE;
            SetNamedPipeHandleState(_hReadPipe, &mode, nullptr, nullptr);
            _LogInfo(L"Connected to push pipe");
            break;
        }

        DWORD error = GetLastError();
        _LogDebug(L"Push pipe connection attempt %d failed: error=%d", attempt + 1, error);
        if (error == ERROR_ACCESS_DENIED)
        {
            WindHostProcessInfo currentHost;
            if (WindQueryCurrentProcessInfo(&currentHost))
                WindLogHostProcessInfo(4, L"compat.push_connect_access_denied.current_host", currentHost);
            WindLogForegroundProcessInfo(4, L"compat.push_connect_access_denied.foreground_host");
        }

        if (error == ERROR_PIPE_BUSY)
        {
            Sleep(50);
            continue;
        }
    }

    if (_hReadPipe == INVALID_HANDLE_VALUE)
    {
        _LogError(L"Failed to connect to push pipe");
        return FALSE;
    }

    // Create async reader thread
    _hAsyncThread = CreateThread(
        NULL,
        0,
        _AsyncReaderThread,
        this,
        0,
        NULL);

    if (_hAsyncThread == NULL)
    {
        _LogError(L"Failed to create async reader thread: %d", GetLastError());
        CloseHandle(_hReadPipe);
        _hReadPipe = INVALID_HANDLE_VALUE;
        return FALSE;
    }

    _asyncReaderRunning = TRUE;
    _LogInfo(L"Async reader thread started");
    return TRUE;
}

void CIPCClient::StopAsyncReader()
{
    if (!_asyncReaderRunning)
    {
        return;
    }

    _LogInfo(L"Stopping async reader thread...");

    // Signal thread to stop
    SetEvent(_hStopEvent);

    // Wait for thread to exit (with timeout)
    if (_hAsyncThread != NULL)
    {
        DWORD waitResult = WaitForSingleObject(_hAsyncThread, 2000);
        if (waitResult == WAIT_TIMEOUT)
        {
            _LogError(L"Async reader thread did not exit in time, terminating");
            TerminateThread(_hAsyncThread, 0);
        }
        CloseHandle(_hAsyncThread);
        _hAsyncThread = NULL;
    }

    // Close push pipe
    if (_hReadPipe != INVALID_HANDLE_VALUE)
    {
        CancelIo(_hReadPipe);
        CloseHandle(_hReadPipe);
        _hReadPipe = INVALID_HANDLE_VALUE;
    }

    _asyncReaderRunning = FALSE;
    _LogInfo(L"Async reader thread stopped");
}

BOOL CIPCClient::IsAsyncReaderRunning() const
{
    return _asyncReaderRunning;
}

DWORD WINAPI CIPCClient::_AsyncReaderThread(LPVOID lpParam)
{
    CIPCClient* pThis = static_cast<CIPCClient*>(lpParam);
    pThis->_AsyncReaderLoop();
    return 0;
}

void CIPCClient::_AsyncReaderLoop()
{
    _LogInfo(L"Async reader loop started");

    HANDLE hReadEvent = CreateEventW(NULL, TRUE, FALSE, NULL);
    if (hReadEvent == NULL)
    {
        _LogError(L"Failed to create read event for async reader: %d", GetLastError());
        return;
    }

    std::vector<uint8_t> buffer(IPCConfig::PIPE_BUFFER_SIZE);
    HANDLE waitHandles[2] = { _hStopEvent, hReadEvent };

    while (true)
    {
        // Check if we need to reconnect to push pipe
        if (_hReadPipe == INVALID_HANDLE_VALUE)
        {
            _LogInfo(L"Async reader: attempting to reconnect to push pipe...");

            // Wait a bit before reconnecting
            DWORD waitResult = WaitForSingleObject(_hStopEvent, 1000);
            if (waitResult == WAIT_OBJECT_0)
            {
                _LogInfo(L"Async reader: stop event received during reconnect wait");
                break;
            }

            // Try to reconnect
            for (int attempt = 0; attempt < 3; attempt++)
            {
                if (WaitForSingleObject(_hStopEvent, 0) == WAIT_OBJECT_0)
                {
                    break; // Stop requested
                }

                if (WaitNamedPipeW(PUSH_PIPE_NAME, 500))
                {
                    _hReadPipe = CreateFileW(
                        PUSH_PIPE_NAME,
                        GENERIC_READ,
                        0,
                        nullptr,
                        OPEN_EXISTING,
                        FILE_FLAG_OVERLAPPED,
                        nullptr);

                    if (_hReadPipe != INVALID_HANDLE_VALUE)
                    {
                        DWORD mode = PIPE_READMODE_MESSAGE;
                        SetNamedPipeHandleState(_hReadPipe, &mode, nullptr, nullptr);
                        _LogInfo(L"Async reader: reconnected to push pipe");
                        break;
                    }
                }
                Sleep(200);
            }

            if (_hReadPipe == INVALID_HANDLE_VALUE)
            {
                _LogError(L"Async reader: failed to reconnect to push pipe, will retry...");
                continue;
            }
        }

        // Start overlapped read
        OVERLAPPED overlapped = {};
        overlapped.hEvent = hReadEvent;
        ResetEvent(hReadEvent);

        DWORD bytesRead = 0;
        BOOL result = ReadFile(_hReadPipe, buffer.data(), static_cast<DWORD>(buffer.size()), &bytesRead, &overlapped);

        if (!result)
        {
            DWORD error = GetLastError();
            if (error != ERROR_IO_PENDING)
            {
                _LogError(L"Async reader: ReadFile failed: %d", error);
                // Close handle and try to reconnect
                CloseHandle(_hReadPipe);
                _hReadPipe = INVALID_HANDLE_VALUE;
                continue;
            }

            // Wait for either stop event or read completion
            DWORD waitResult = WaitForMultipleObjects(2, waitHandles, FALSE, INFINITE);

            if (waitResult == WAIT_OBJECT_0)
            {
                // Stop event signaled
                _LogInfo(L"Async reader: stop event received");
                CancelIo(_hReadPipe);
                break;
            }
            else if (waitResult == WAIT_OBJECT_0 + 1)
            {
                // Read completed
                if (!GetOverlappedResult(_hReadPipe, &overlapped, &bytesRead, FALSE))
                {
                    DWORD error = GetLastError();
                    if (error == ERROR_BROKEN_PIPE || error == ERROR_PIPE_NOT_CONNECTED)
                    {
                        _LogInfo(L"Async reader: pipe disconnected, will reconnect...");
                        CloseHandle(_hReadPipe);
                        _hReadPipe = INVALID_HANDLE_VALUE;
                        continue;
                    }
                    _LogError(L"Async reader: GetOverlappedResult failed: %d", error);
                    continue;
                }
            }
            else
            {
                _LogError(L"Async reader: WaitForMultipleObjects failed: %d", GetLastError());
                // Close handle and try to reconnect
                CloseHandle(_hReadPipe);
                _hReadPipe = INVALID_HANDLE_VALUE;
                continue;
            }
        }

        // Process received message
        if (bytesRead >= sizeof(IpcHeader))
        {
            IpcHeader header;
            memcpy(&header, buffer.data(), sizeof(IpcHeader));

            _LogDebug(L"Async reader: received message cmd=0x%04X, len=%d", header.command, header.length);

            if (header.command == CMD_STATE_PUSH)
            {
                // Parse state push
                std::vector<uint8_t> payload;
                if (header.length > 0 && bytesRead >= sizeof(IpcHeader) + header.length)
                {
                    payload.assign(buffer.begin() + sizeof(IpcHeader),
                                   buffer.begin() + sizeof(IpcHeader) + header.length);
                }

                ServiceResponse response;
                if (_ParseResponse(header, payload, response))
                {
                    _LogInfo(L"Async reader: state push received - mode=%d, fullWidth=%d",
                             response.IsChineseMode(), response.IsFullWidth());

                    // Call callback
                    EnterCriticalSection(&_asyncLock);
                    StatePushCallback callback = _statePushCallback;
                    LeaveCriticalSection(&_asyncLock);

                    if (callback)
                    {
                        callback(response);
                    }
                }
            }
            else if (header.command == CMD_SERVICE_READY)
            {
                // Go service connected push pipe — fire callback so TextService
                // posts to LangBarItemButton's message window and calls
                // _DoFullStateSync() on the TSF thread.
                _LogInfo(L"Async reader: CMD_SERVICE_READY received");
                EnterCriticalSection(&_asyncLock);
                ServiceReadyCallback srCallback = _serviceReadyCallback;
                LeaveCriticalSection(&_asyncLock);
                if (srCallback)
                    srCallback();
            }
            else if (header.command == CMD_COMMIT_TEXT)
            {
                // Parse commit text (from Go - mouse click on candidate)
                std::vector<uint8_t> payload;
                if (header.length > 0 && bytesRead >= sizeof(IpcHeader) + header.length)
                {
                    payload.assign(buffer.begin() + sizeof(IpcHeader),
                                   buffer.begin() + sizeof(IpcHeader) + header.length);
                }

                ServiceResponse response;
                if (_ParseResponse(header, payload, response))
                {
                    _LogDebug(L"Async reader: commit text received, textLen=%zu", response.text.length());

                    // Call callback
                    EnterCriticalSection(&_asyncLock);
                    CommitTextCallback callback = _commitTextCallback;
                    LeaveCriticalSection(&_asyncLock);

                    if (callback && !response.text.empty())
                    {
                        callback(response.text);
                    }
                }
            }
            else if (header.command == CMD_CLEAR_COMPOSITION)
            {
                _LogInfo(L"Async reader: clear composition received from Go service");

                // Call callback
                EnterCriticalSection(&_asyncLock);
                ClearCompositionCallback callback = _clearCompositionCallback;
                LeaveCriticalSection(&_asyncLock);

                if (callback)
                {
                    callback();
                }
            }
            else if (header.command == CMD_UPDATE_COMPOSITION)
            {
                // Parse update composition (from Go - mouse click partial confirm)
                std::vector<uint8_t> payload;
                if (header.length > 0 && bytesRead >= sizeof(IpcHeader) + header.length)
                {
                    payload.assign(buffer.begin() + sizeof(IpcHeader),
                                   buffer.begin() + sizeof(IpcHeader) + header.length);
                }

                ServiceResponse response;
                if (_ParseResponse(header, payload, response))
                {
                    _LogDebug(L"Async reader: update composition received, textLen=%zu, caret=%d",
                              response.composition.length(), response.caretPos);

                    // Call callback
                    EnterCriticalSection(&_asyncLock);
                    UpdateCompositionCallback callback = _updateCompositionCallback;
                    LeaveCriticalSection(&_asyncLock);

                    if (callback)
                    {
                        callback(response.composition, response.caretPos);
                    }
                }
            }
            else if (header.command == CMD_SYNC_CONFIG)
            {
                // Parse config sync: keyLen(2, LE) + valueLen(4, LE) + key(UTF-8) + value(bytes)
                std::vector<uint8_t> payload;
                if (header.length > 0 && bytesRead >= sizeof(IpcHeader) + header.length)
                {
                    payload.assign(buffer.begin() + sizeof(IpcHeader),
                                   buffer.begin() + sizeof(IpcHeader) + header.length);
                }

                if (payload.size() >= 6)
                {
                    uint16_t keyLen = *reinterpret_cast<const uint16_t*>(payload.data());
                    uint32_t valueLen = *reinterpret_cast<const uint32_t*>(payload.data() + 2);

                    if (payload.size() >= 6 + keyLen + valueLen)
                    {
                        std::string key(reinterpret_cast<const char*>(payload.data() + 6), keyLen);
                        std::vector<uint8_t> value(payload.data() + 6 + keyLen, payload.data() + 6 + keyLen + valueLen);

                        _LogInfo(L"Async reader: config sync received key=%hs valueLen=%u", key.c_str(), valueLen);

                        EnterCriticalSection(&_asyncLock);
                        SyncConfigCallback configCallback = _syncConfigCallback;
                        LeaveCriticalSection(&_asyncLock);

                        if (configCallback)
                        {
                            configCallback(key, value);
                        }
                    }
                }
            }
        }
    }

    CloseHandle(hReadEvent);
    _LogInfo(L"Async reader loop exited");
}
