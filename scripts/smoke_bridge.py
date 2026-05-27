#!/usr/bin/env python3
"""smoke_bridge.py — Go 服务 bridge 通路冒烟测试.

镜像 docs/macos-build.md §6 的最小客户端, 用于在 Swift IMKit 客户端落地前
验证 Go 服务端协议帧能正常收发.

用法:
    scripts/smoke_bridge.py [push_wait_seconds]    # 默认 5 秒

行为:
    1. 连 bridge.sock, 发一帧 KeyEvent (VK 'A' down), 打印响应 cmd/len/hex
    2. 连 bridge_push.sock, 阻塞读 N 秒, 打印每一帧 cmd/len/payload 前缀
"""
import os
import socket
import struct
import sys
import threading
import time

RUNTIME_DIR = os.environ.get(
    "WIND_INPUT_RUNTIME_DIR",
    os.path.expanduser("~/Library/Application Support/WindInput"),
)
BRIDGE_SOCK = f"{RUNTIME_DIR}/bridge.sock"
PUSH_SOCK   = f"{RUNTIME_DIR}/bridge_push.sock"

PROTO_VER     = 0x1001
CMD_KEY_EVENT = 0x0101

PUSH_WAIT = float(sys.argv[1]) if len(sys.argv) >= 2 else 5.0


def hexs(b: bytes, n: int = 48) -> str:
    return b[:n].hex()


def read_frame(sock: socket.socket):
    hdr = b""
    while len(hdr) < 8:
        chunk = sock.recv(8 - len(hdr))
        if not chunk:
            return None
        hdr += chunk
    ver, cmd, length = struct.unpack("<HHI", hdr)
    body = b""
    while len(body) < length:
        chunk = sock.recv(length - len(body))
        if not chunk:
            return None
        body += chunk
    return ver, cmd, body


# 1. KeyEvent roundtrip
print(f"[smoke] runtime  : {RUNTIME_DIR}")
print(f"[smoke] bridge   : {BRIDGE_SOCK}")
print(f"[smoke] push     : {PUSH_SOCK}")
print()
print("[smoke] === KeyEvent roundtrip ===")
try:
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(3.0)
    s.connect(BRIDGE_SOCK)

    payload = struct.pack(
        "<IIIBBHH",
        0x41,  # KeyCode 'A'
        0,     # ScanCode
        0,     # Modifiers
        0,     # EventType down
        0,     # Toggles
        1,     # EventSeq
        0,     # PrevChar
    )
    header = struct.pack("<HHI", PROTO_VER, CMD_KEY_EVENT, len(payload))
    frame = header + payload
    print(f"[smoke] -> KeyEvent  bytes={len(frame)} hex={frame.hex()}")
    s.sendall(frame)

    r = read_frame(s)
    if r is None:
        print("[smoke] !! request channel: EOF")
    else:
        ver, cmd, body = r
        print(f"[smoke] <- cmd=0x{cmd:04x} ver=0x{ver:04x} len={len(body)} body={hexs(body)}")
    s.close()
except Exception as e:
    print(f"[smoke] !! request channel error: {e}")

# 2. push subscribe
print()
print(f"[smoke] === Push channel ({PUSH_WAIT:.0f}s) ===")
try:
    p = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    p.connect(PUSH_SOCK)
    p.settimeout(0.5)

    deadline = time.time() + PUSH_WAIT
    n = 0
    while time.time() < deadline:
        try:
            r = read_frame(p)
        except socket.timeout:
            continue
        except Exception as e:
            print(f"[smoke] !! push error: {e}")
            break
        if r is None:
            print("[smoke] push EOF (server closed)")
            break
        ver, cmd, body = r
        n += 1
        print(f"[smoke] push cmd=0x{cmd:04x} len={len(body)} body={hexs(body)}")
    print(f"[smoke] received {n} push frame(s)")
    p.close()
except Exception as e:
    print(f"[smoke] !! push connect error: {e}")

print()
print("[smoke] done")
