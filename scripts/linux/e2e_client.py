#!/usr/bin/env python3
"""wind_linux 端到端测试的「应用」一侧：经 DBus 扮演一个文本框，对真实 fcitx5 打字。

由 scripts/linux/e2e.sh 在 dbus-run-session 里调用（那边已起好 Rust 服务与 fcitx5）。
每条用例输出一行 `PASS <名字>` 或 `FAIL <名字>: <原因>`，有任何 FAIL 则退出码非 0。

协议：org.fcitx.Fcitx5 的 InputMethod1.CreateInputContext 建上下文，InputContext1.
ProcessKeyEvent 送键（返回值 = 输入法是否吃掉），信号 CommitString / UpdateFormattedPreedit
回传上屏与预编辑。
"""
import asyncio
import os
import subprocess
import sys
import time

from dbus_next.aio import MessageBus
from dbus_next import Variant

SERVICE = "org.fcitx.Fcitx5"
IM_PATH = "/org/freedesktop/portal/inputmethod"
IM_IFACE = "org.fcitx.Fcitx.InputMethod1"
IC_IFACE = "org.fcitx.Fcitx.InputContext1"

# fcitx CapabilityFlag：Preedit | FormattedPreedit —— 让预编辑走 UpdateFormattedPreedit 信号。
CAP_PREEDIT = (1 << 1) | (1 << 4)
CAP_PASSWORD = 1 << 3
STATE_SHIFT = 1 << 0
STATE_LOCK = 1 << 1
STATE_CTRL = 1 << 2

# X11 keysym 与 US 布局的 X keycode（evdev + 8）。
KEYSYM = {c: ord(c) for c in "abcdefghijklmnopqrstuvwxyz0123456789 "}
KEYSYM.update({"BackSpace": 0xFF08, "Return": 0xFF0D, "Escape": 0xFF1B, "Shift_L": 0xFFE1,
               "Caps_Lock": 0xFFE5,
               "Left": 0xFF51, "Up": 0xFF52, "Right": 0xFF53, "Down": 0xFF54,
               "equal": 0x3D, "Tab": 0xFF09})
KEYCODE = {
    **dict(zip("qwertyuiop", range(24, 34))),
    **dict(zip("asdfghjkl", range(38, 47))),
    **dict(zip("zxcvbnm", range(52, 59))),
    **dict(zip("1234567890", range(10, 20))),
    " ": 65, "Return": 36, "Escape": 9, "BackSpace": 22, "Shift_L": 50, "Caps_Lock": 66,
    "Left": 113, "Up": 111, "Right": 114, "Down": 116, "equal": 21, "Tab": 23,
}


class Ctx:
    """一个输入上下文（≈ 应用里的一个文本框）及其收到的信号。"""

    def __init__(self, bus, path):
        self.path = path
        self.commits = []
        self.preedits = []  # 每次 UpdateFormattedPreedit 的纯文本
        self.forwarded = []
        self.bus = bus

    async def init(self, caps):
        intro = await self.bus.introspect(SERVICE, self.path)
        obj = self.bus.get_proxy_object(SERVICE, self.path, intro)
        self.ic = obj.get_interface(IC_IFACE)
        self.ic.on_commit_string(lambda s: self.commits.append(s))
        self.ic.on_update_formatted_preedit(
            lambda segs, cursor: self.preedits.append("".join(s for s, _ in segs)))
        self.ic.on_forward_key(lambda sym, state, rel: self.forwarded.append((sym, state, rel)))
        await self.ic.call_set_capability(caps)

    @property
    def preedit(self):
        return self.preedits[-1] if self.preedits else ""

    async def key(self, name, state=0):
        sym = KEYSYM[name]
        code = KEYCODE.get(name, 0)
        eaten = await self.ic.call_process_key_event(sym, code, state, False, 0)
        await self.ic.call_process_key_event(sym, code, state | 0, True, 0)
        await asyncio.sleep(0.03)
        return eaten

    async def type(self, text):
        return [await self.key(c) for c in text]

    async def tap_shift(self, state=0):
        # 单击 Shift：按下时 state 不含 Shift，松开时含（X11 语义：state 是事件前的修饰态）。
        code = KEYCODE["Shift_L"]
        sym = KEYSYM["Shift_L"]
        await self.ic.call_process_key_event(sym, code, state, False, 0)
        await self.ic.call_process_key_event(sym, code, state | STATE_SHIFT, True, 0)
        await asyncio.sleep(0.15)

    async def tap_caps_lock(self, locked_before):
        # 按一下 CapsLock，state 按真实 X11 的样子给（Xvfb + xev 实测）：开 → 按下 0、松开 Lock；
        # 关 → 按下 Lock、松开**仍是** Lock（XKB 在松开时才解锁，state 是事件之前的）。
        code = KEYCODE["Caps_Lock"]
        sym = KEYSYM["Caps_Lock"]
        before = STATE_LOCK if locked_before else 0
        await self.ic.call_process_key_event(sym, code, before, False, 0)
        await self.ic.call_process_key_event(sym, code, STATE_LOCK, True, 0)
        await asyncio.sleep(0.15)

    def take(self):
        """取出并清空累计的上屏文本。"""
        s = "".join(self.commits)
        self.commits.clear()
        return s


results = []


def check(name, ok, why=""):
    results.append(ok)
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f": {why}"), flush=True)


async def new_ctx(bus, im, program, caps=CAP_PREEDIT):
    path, _uuid = await im.call_create_input_context([["program", program], ["display", ""]])
    c = Ctx(bus, path)
    await c.init(caps)
    await c.ic.call_focus_in()
    await asyncio.sleep(0.2)  # activate → 服务端 FocusGained
    return c


# ── 候选窗（X11）辅助 ───────────────────────────────────────────────

def sh(*args):
    return subprocess.run(args, capture_output=True, text=True).stdout


def candidate_window(classname="wind-candidate"):
    """按 WM_CLASS 实例名找窗（候选窗 wind-candidate；浮层 wind-status / wind-toast /
    wind-tooltip；面板 wind-toolbar / wind-softkeyboard），返回 (id, x, y, w, h, 是否可见)；
    没建过返回 None。

    查的途中窗口可能正被销毁重建（合成器起停换 visual、X 重连），xwininfo 查不到的跳过。"""
    for wid in sh("xdotool", "search", "--classname", classname).split():
        info = sh("xwininfo", "-id", wid)
        def field(name):
            for line in info.splitlines():
                if line.strip().startswith(name):
                    return line.split(":", 1)[1].strip()
            return ""
        if not field("Width"):
            continue
        return (wid, int(field("Absolute upper-left X")), int(field("Absolute upper-left Y")),
                int(field("Width")), int(field("Height")), field("Map State") == "IsViewable")
    return None


def xwd_to_png(xwd_bytes, png_path):
    """把 `xwd` 的输出转成 PNG（只支持每像素 3 或 4 字节的 ZPixmap）。
    返回 (宽, 高, 取像素函数)。不引第三方库：e2e 环境里没有 PIL。"""
    import struct
    import zlib
    hdr = struct.unpack(">25I", xwd_bytes[:100])
    header_size, width, height = hdr[0], hdr[4], hdr[5]
    byte_order, bpp, bytes_per_line, ncolors = hdr[7], hdr[11], hdr[12], hdr[19]
    # 有 ARGB 窗口（合成器在）时 xwd 走多视觉路径：头里 bits_per_pixel=24，像素按 3 字节紧排，
    # bytes_per_line 却仍是 宽×4（行尾空着）。按 bits_per_pixel 取，别按行宽推。
    step = bpp // 8
    assert step in (3, 4) and bytes_per_line >= width * step, f"不支持的像素格式，头 {hdr[:20]}"
    off = header_size + ncolors * 12
    data = xwd_bytes[off:off + bytes_per_line * height]

    def px(x, y):
        i = y * bytes_per_line + x * step
        b = data[i:i + step]
        # LSBFirst(0)：内存里依次 B G R (x)；MSBFirst：(x) R G B
        return (b[2], b[1], b[0]) if byte_order == 0 else tuple(b[step - 3:step])

    raw = bytearray()
    for y in range(height):
        raw.append(0)
        for x in range(width):
            raw.extend(px(x, y))
    def chunk(tag, body):
        return struct.pack(">I", len(body)) + tag + body + struct.pack(">I", zlib.crc32(tag + body))
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 6)) + chunk(b"IEND", b"")
    open(png_path, "wb").write(png)
    return width, height, px


def screenshot(name, wid=None):
    """截全屏（wid=None）或只截候选窗本身。后者取的是窗口自己的像素（不经合成器），
    用来确认贴上去的位图没错；前者看它在屏幕上摆在哪。"""
    shots = os.environ.get("WIND_E2E_SHOTS", "")
    target = ["-id", wid] if wid else ["-root"]
    xwd = subprocess.run(["xwd", *target, "-silent"], capture_output=True).stdout
    path = os.path.join(shots, name + ".png")
    _, _, px = xwd_to_png(xwd, path)
    return path, px


async def x11_cases(bus, im):
    """P2：候选窗的出现、位置、生命周期、鼠标选词。文字后端就绪前帧里可能没有字，
    这里只断言窗口本身：有像素、摆对地方、该藏时藏、点得动。"""
    c = await new_ctx(bus, im, "e2e-x11")
    # 光标在 (300, 400)，行高 20：候选窗应出现在光标下方附近。
    await c.ic.call_set_cursor_rect(300, 400, 2, 20)
    await c.type("nihao")
    await asyncio.sleep(0.4)
    win = candidate_window()
    check("X11：组字时候选窗出现（已映射、非零尺寸）",
          win is not None and win[5] and win[3] > 0 and win[4] > 0, f"window={win}")
    if not win:
        return
    _, x, y, w, h, _ = win
    print(f"  候选窗几何：{w}x{h} @ ({x},{y})，光标 (300,400) 行高 20", flush=True)
    check("X11：候选窗左上角落在光标下方附近",
          240 <= x <= 330 and 400 <= y <= 470, f"@({x},{y})")
    path, px = screenshot("candidate_nihao")
    colors = {px(xx, yy) for xx in range(x, x + w, max(1, w // 40))
              for yy in range(y, y + h, max(1, h // 20))}
    check("X11：候选窗区域确有绘制（截图里不是单一颜色）", len(colors) > 1,
          f"采样到 {len(colors)} 种颜色，截图 {path}")
    print(f"  截图：{path}", flush=True)
    wpath, _ = screenshot("candidate_window_only", win[0])
    print(f"  候选窗自身像素：{wpath}", flush=True)

    await c.key(" ")
    await asyncio.sleep(0.3)
    win = candidate_window()
    c.take()
    check("X11：上屏后候选窗隐藏", win is not None and not win[5], f"window={win}")

    await c.type("nihao")
    await c.key("Escape")
    await asyncio.sleep(0.3)
    win = candidate_window()
    check("X11：Esc 后候选窗隐藏", win is not None and not win[5], f"window={win}")

    # 滚轮：服务端把它解释成「上下键调整高亮项」——窗口像素要变、不上屏。
    await c.type("nihao")
    await asyncio.sleep(0.3)
    win = candidate_window()
    changed = False
    if win and win[5]:
        before = subprocess.run(["xwd", "-id", win[0], "-silent"], capture_output=True).stdout
        sh("xdotool", "mousemove", str(win[1] + win[3] // 2), str(win[2] + win[4] // 2),
           "click", "5")
        await asyncio.sleep(0.3)
        after = subprocess.run(["xwd", "-id", win[0], "-silent"], capture_output=True).stdout
        changed = before != after
        screenshot("candidate_after_scroll", win[0])
    got = c.take()
    check("X11：滚轮下滚 → 高亮移动（窗口像素变化）且不上屏", changed and got == "",
          f"changed={changed} 上屏={got!r}")
    await c.key("Escape")
    sh("xdotool", "mousemove", "5", "5")
    await asyncio.sleep(0.2)

    # 鼠标选词：候选的命中矩形只有服务端知道，沿窗口中线从左往右逐点点击，直到有上屏。
    await c.type("nihao")
    await asyncio.sleep(0.3)
    win = candidate_window()
    got = ""
    hit = None
    if win and win[5]:
        _, x, y, w, h, _ = win
        for dx in range(4, w, 8):
            sh("xdotool", "mousemove", str(x + dx), str(y + h // 2), "click", "1")
            await asyncio.sleep(0.12)
            got = c.take()
            if got:
                hit = (x + dx, y + h // 2)
                break
    check("X11：鼠标点候选 → 上屏（CMD_CANDIDATE_SELECT → push 回来的 CommitText）",
          got != "", "点遍中线都没有上屏")
    await asyncio.sleep(0.3)
    win = candidate_window()
    check("X11：鼠标选词后候选窗隐藏", win is not None and not win[5], f"window={win}")
    sh("xdotool", "mousemove", "5", "5")

    # 点选在松开时生效（同 macOS）：按下不上屏；按住拖出候选窗再松开 = 放弃，候选窗仍在。
    if hit:
        await c.type("nihao")
        await asyncio.sleep(0.3)
        sh("xdotool", "mousemove", str(hit[0]), str(hit[1]), "mousedown", "1")
        await asyncio.sleep(0.2)
        on_press = c.take()
        sh("xdotool", "mousemove", "5", "5", "mouseup", "1")
        await asyncio.sleep(0.2)
        on_release = c.take()
        win = candidate_window()
        check("X11：按下候选不上屏，拖出候选窗松开 = 放弃（候选窗仍在）",
              on_press == "" and on_release == "" and win is not None and win[5],
              f"按下={on_press!r} 松开={on_release!r} window={win}")
        await c.key("Escape")
        c.take()

    # 屏幕底边：光标在 (300, 780)，下方放不下 → 翻到光标上方，且不出屏。
    await c.ic.call_set_cursor_rect(300, 780, 2, 16)
    await c.type("nihao")
    await asyncio.sleep(0.4)
    win = candidate_window()
    ok = win is not None and win[5] and win[2] + win[4] <= 800 and win[2] < 780
    check("X11：屏幕底边时翻到光标上方、不出屏", ok, f"window={win}")
    if win:
        screenshot("candidate_flipped")
    await c.key("Escape")
    await c.ic.call_focus_out()
    await asyncio.sleep(0.2)


async def wait_window(classname, visible, timeout=3.0):
    """等某个窗口进入指定可见态，返回最后一次查到的窗口信息（超时也返回，由调用方断言）。"""
    deadline = asyncio.get_running_loop().time() + timeout
    while True:
        win = candidate_window(classname)
        if (win is not None and win[5]) == visible:
            return win
        if asyncio.get_running_loop().time() >= deadline:
            return win
        await asyncio.sleep(0.05)


def window_has_content(wid, name):
    """截窗口自身像素存 PNG，返回 (路径, 颜色种数)。"""
    path, px = screenshot(name, wid)
    info = sh("xwininfo", "-id", wid)
    w = h = 0
    for line in info.splitlines():
        if line.strip().startswith("Width"):
            w = int(line.split(":", 1)[1])
        if line.strip().startswith("Height"):
            h = int(line.split(":", 1)[1])
    colors = {px(x, y) for x in range(0, w, max(1, w // 60)) for y in range(0, h, max(1, h // 20))}
    return path, len(colors)


async def overlay_cases(bus, im):
    """光栅浮层：状态气泡 / Toast / 悬停提示。服务光栅化 → 各层 SHM → CMD_OVERLAY_FRAME →
    addon 各自一个 override-redirect 窗口。断言窗口出现、位置合理、有像素、到点消失。
    字形对不对要看截图（路径打印在输出里）。"""
    c = await new_ctx(bus, im, "e2e-overlay")
    await c.ic.call_set_cursor_rect(300, 400, 2, 20)

    # a) Shift 单击切中英：光标下方弹状态气泡，几秒后自己消失（计时在 addon）。
    await c.tap_shift()
    win = await wait_window("wind-status", True)
    check("浮层：Shift 切中英后状态气泡出现", win is not None and win[5], f"window={win}")
    if win and win[5]:
        _, x, y, w, h, _ = win
        print(f"  状态气泡：{w}x{h} @ ({x},{y})，光标 (300,400) 行高 20", flush=True)
        check("浮层：状态气泡落在光标下方附近", 280 <= x <= 330 and 410 <= y <= 450,
              f"@({x},{y})")
        path, n = window_has_content(win[0], "status_toggle_en")
        check("浮层：状态气泡有绘制（不是单色）", n > 1, f"{n} 种颜色，截图 {path}")
        print(f"  状态气泡截图：{path}", flush=True)
        win = await wait_window("wind-status", False, timeout=8.0)
        check("浮层：状态气泡到点自动消失", win is not None and not win[5], f"window={win}")
    await c.tap_shift()  # 切回中文
    await wait_window("wind-status", True)

    # b) Ctrl+Shift+E 切方案：气泡文字换成方案名（字形看截图）。
    await c.ic.call_process_key_event(0x45, 26, STATE_CTRL | STATE_SHIFT, False, 0)
    await c.ic.call_process_key_event(0x45, 26, STATE_CTRL | STATE_SHIFT, True, 0)
    await asyncio.sleep(0.3)
    win = await wait_window("wind-status", True)
    ok = win is not None and win[5]
    check("浮层：Ctrl+Shift+E 切方案后状态气泡出现", ok, f"window={win}")
    if ok:
        path, n = window_has_content(win[0], "status_switch_schema")
        check("浮层：切方案气泡有绘制", n > 1, f"{n} 种颜色，截图 {path}")
        print(f"  切方案气泡截图：{path}", flush=True)
        full, _ = screenshot("status_switch_schema_screen")
        print(f"  全屏截图：{full}", flush=True)
    await c.ic.call_process_key_event(0x45, 26, STATE_CTRL | STATE_SHIFT, False, 0)
    await c.ic.call_process_key_event(0x45, 26, STATE_CTRL | STATE_SHIFT, True, 0)
    await asyncio.sleep(0.3)

    # c) Toast：`wind_input ui toast` 经 RPC 让在线服务弹一条（右上角，1.5 秒）。
    svc = os.path.join(os.environ.get("W", ""), "svc", "wind_input")
    r = await asyncio.to_thread(subprocess.run,
                                [svc, "ui", "toast", "输入法已就绪", "--pos", "top_right",
                                 "--ms", "1500"], capture_output=True, text=True)
    win = await wait_window("wind-toast", True)
    ok = win is not None and win[5]
    check("浮层：ui toast 后 Toast 出现", ok,
          f"window={win} cli rc={r.returncode} {r.stderr.strip()[:200]}")
    if ok:
        _, x, y, w, h, _ = win
        print(f"  Toast：{w}x{h} @ ({x},{y})，屏幕 1280x800", flush=True)
        check("浮层：Toast 在右上角（离边留白）", x + w >= 1280 - 40 and y <= 40, f"@({x},{y})")
        path, n = window_has_content(win[0], "toast_top_right")
        check("浮层：Toast 有绘制", n > 1, f"{n} 种颜色，截图 {path}")
        print(f"  Toast 截图：{path}", flush=True)
        win = await wait_window("wind-toast", False, timeout=6.0)
        check("浮层：Toast 到点自动消失", win is not None and not win[5], f"window={win}")

    # d) 悬停提示：鼠标停在候选上 → 候选窗旁出现 tooltip；Esc 收掉候选窗时一起藏。
    await c.type("nihao")
    await asyncio.sleep(0.4)
    cand = candidate_window()
    tip = None
    if cand and cand[5]:
        _, x, y, w, h, _ = cand
        for dx in range(6, w, 10):
            sh("xdotool", "mousemove", str(x + dx), str(y + h // 2))
            tip = await wait_window("wind-tooltip", True, timeout=0.6)
            if tip and tip[5]:
                break
    ok = tip is not None and tip[5]
    check("浮层：鼠标悬停候选后 tooltip 出现", ok, f"tooltip={tip} candidate={cand}")
    if ok:
        _, tx, ty, tw, th, _ = tip
        print(f"  tooltip：{tw}x{th} @ ({tx},{ty})，候选窗 {cand[3]}x{cand[4]} @ ({cand[1]},{cand[2]})",
              flush=True)
        check("浮层：tooltip 在候选窗下方", ty >= cand[2] + cand[4] // 2, f"tooltip@({tx},{ty})")
        path, n = window_has_content(tip[0], "tooltip_hover")
        check("浮层：tooltip 有绘制", n > 1, f"{n} 种颜色，截图 {path}")
        print(f"  tooltip 截图：{path}", flush=True)
        full, _ = screenshot("tooltip_hover_screen")
        print(f"  全屏截图：{full}", flush=True)
    await c.key("Escape")
    tip = await wait_window("wind-tooltip", False, timeout=2.0)
    check("浮层：候选窗收起时 tooltip 一起隐藏", tip is None or not tip[5], f"tooltip={tip}")
    sh("xdotool", "mousemove", "5", "5")
    await c.ic.call_focus_out()
    await asyncio.sleep(0.2)


# ── 自绘菜单（X11）─────────────────────────────────────────────────────
# 菜单由服务光栅化（复用 Windows 的 popup_menu），每级一个窗口 wind-menu-<级>；addon 在菜单
# 打开期间抓住指针把原始事件报回服务。行高与内边距取默认主题：条目 27px、上下留白约 4px，
# 分隔线 7px——下面按它估算行位置，点错了效果断言会红，不会静默通过。
MENU_ROW = 27


def menu_win(level=0):
    return candidate_window(f"wind-menu-{level}")


def xwd_raw(wid):
    return subprocess.run(["xwd", "-id", wid, "-silent"], capture_output=True).stdout


def menus_gone():
    return all(w is None or not w[5] for w in (menu_win(k) for k in range(6)))


async def wait_menus_gone(timeout=3.0):
    return await wait_until(menus_gone, timeout)


async def typing_works(c, label):
    """关菜单后立刻打字：nihao+空格 → 你好（菜单态没卡住方向键 / 回车 / 字母）。"""
    await c.type("nihao")
    await c.key(" ")
    got = c.take()
    check(f"菜单：{label}后立即能正常打字", got == "你好", f"上屏={got!r}")


async def probe_row(win, from_bottom=False):
    """找菜单窗口里第一个（或最后一个）可选行的纵坐标：从边缘往里逐步移动鼠标，窗口像素一变（高亮
    亮起）就是碰到了那一行。软投影扩边的宽度随主题而定，不能按固定偏移猜。返回行内偏里一点的 y。"""
    wid, x, y, w, h, _ = win
    base = xwd_raw(wid)
    # 走满整个窗口：第一行可能是禁用项（候选菜单首候选的「置顶」），禁用项不高亮。
    steps = range(h - 2, 1, -3) if from_bottom else range(2, h - 1, 3)
    for dy in steps:
        sh("xdotool", "mousemove", str(x + w // 3), str(y + dy))
        await asyncio.sleep(0.08)
        if xwd_raw(wid) != base:
            return y + dy + (-8 if from_bottom else 8)
    return None


async def hover_candidate_point(c):
    """候选的命中矩形只有服务端知道：沿候选窗中线移动鼠标，悬停提示出现处就在某个候选上。"""
    cand = candidate_window()
    if not (cand and cand[5]):
        return None
    _, x, y, w, h, _ = cand
    for dx in range(6, w, 10):
        sh("xdotool", "mousemove", str(x + dx), str(y + h // 2))
        tip = await wait_window("wind-tooltip", True, timeout=0.6)
        if tip and tip[5]:
            return (x + dx, y + h // 2)
    return None


async def open_candidate_menu(c):
    """组字 → 右键某个候选 → 候选菜单。返回 (菜单窗口, 右键点)。"""
    await c.type("nihao")
    await asyncio.sleep(0.4)
    pt = await hover_candidate_point(c)
    if not pt:
        return None, None
    sh("xdotool", "mousemove", str(pt[0]), str(pt[1]), "click", "3")
    return await wait_window("wind-menu-0", True), pt


_IMPANEL = None


async def impanel(bus):
    """同一条总线上只能有一个 kimpanel 替身（名字与对象路径都只能占一次）。"""
    global _IMPANEL
    if _IMPANEL is None:
        _IMPANEL = Impanel(bus)
        await _IMPANEL.start()
    return _IMPANEL


async def open_main_menu(c, tries=1, at=(300, 200)):
    """功能主菜单的鼠标入口：Fcitx5 状态区动作「清风输入法菜单」（托盘菜单 / kimpanel 面板里
    那一项），主菜单弹在指针处（先把指针挪到 at）。候选菜单里不再有「更多…」（见 linux-port.md
    §5c）。返回主菜单窗口或 None。

    tries > 1：服务刚重启时 socket 先于整个服务就绪，menu.open 可能被当「未就绪」丢掉，重试。"""
    panel = await impanel(c.bus)
    for _ in range(tries):
        sh("xdotool", "mousemove", str(at[0]), str(at[1]))
        panel.trigger("/Fcitx/windinput-menu")
        main = await wait_window("wind-menu-0", True)
        if main and main[5]:
            return main
        await c.key("Escape")
        await wait_menus_gone()
    return None


async def end_composition(c):
    """收掉仍挂着的组字（没有组字时这一下 Esc 交还宿主，无副作用）。"""
    await c.key("Escape")
    c.take()
    c.preedits.clear()


def tray_icon(tag, label):
    """服务端运行时渲染的托盘图标名（与 wind_ui::tray_icon::icon_name / addon 的
    dynamicIconName 同一规则）：windinput-lbl-<zh|en|caps>-<主字 UTF-8 十六进制>。"""
    return f"windinput-lbl-{tag}-{label.encode().hex()}"


ICON_PIN = tray_icon("zh", "拼")
ICON_WU = tray_icon("zh", "五")
ICON_EN = tray_icon("en", "英")
ICON_CAPS = tray_icon("caps", "A")


def tray_icon_files_ok(name):
    """这个图标名在用户图标目录（$XDG_DATA_HOME/icons/hicolor）里各尺寸都在，且是尺寸对得上的 PNG。"""
    root = os.path.join(os.environ["XDG_DATA_HOME"], "icons", "hicolor")
    for n in (16, 22, 24, 32, 48, 64):
        path = os.path.join(root, f"{n}x{n}", "apps", name + ".png")
        try:
            with open(path, "rb") as f:
                head = f.read(24)
        except OSError:
            return False, f"缺 {path}"
        if head[:8] != b"\x89PNG\r\n\x1a\n" or head[12:16] != b"IHDR":
            return False, f"不是 PNG：{path}"
        w, h = int.from_bytes(head[16:20], "big"), int.from_bytes(head[20:24], "big")
        if (w, h) != (n, n):
            return False, f"{path} 尺寸 {w}x{h}"
    return True, ""


class KimpanelWatch:
    """听 Fcitx5 kimpanel 模块发给面板的属性：`/Fcitx/im:<名>:<图标>:<子模式>:menu,label=<标签>`
    是当前输入法的托盘图标（取自引擎的 subModeIcon，notificationitem 的 SNI 图标取的是同一个
    值）；`RegisterProperties` 里是状态区动作。"""

    def __init__(self):
        self.props = []
        self.registered = []

    async def start(self, bus):
        name, path = "org.kde.kimpanel.inputmethod", "/kimpanel"
        intro = await bus.introspect(name, path)
        iface = bus.get_proxy_object(name, path, intro).get_interface(name)
        iface.on_update_property(lambda p: self.props.append(p))
        iface.on_register_properties(lambda ps: self.registered.append(list(ps)))
        # Fcitx5 输入面板里的预编辑 / 辅助文本：classicui 会把它们画成另一个小窗。
        self.panel_texts = []
        iface.on_update_preedit_text(lambda t, _a: self.panel_texts.append(t))
        iface.on_update_aux(lambda t, _a: self.panel_texts.append(t))

    def im(self):
        for p in reversed(self.props):
            if p.startswith("/Fcitx/im:"):
                return p
        return ""

    def icon(self):
        parts = self.im().split(":")
        return parts[2] if len(parts) > 2 else ""


_KIMPANEL_WATCH = None


async def kimpanel_watch(bus):
    global _KIMPANEL_WATCH
    if _KIMPANEL_WATCH is None:
        await impanel(bus)  # kimpanel 模块看见面板在场才导出 org.kde.kimpanel.inputmethod
        w = KimpanelWatch()
        await w.start(bus)
        _KIMPANEL_WATCH = w
    return _KIMPANEL_WATCH


class Impanel:
    """扮演 KDE / GNOME 的 kimpanel 面板：Fcitx5 kimpanel 模块据此把状态区动作挂出来，
    面板点动作时发 `TriggerProperty("/Fcitx/<动作名>")`——就是用户点「清风输入法设置」。"""

    def __init__(self, bus):
        from dbus_next.service import ServiceInterface, signal

        class Iface(ServiceInterface):
            def __init__(self):
                super().__init__("org.kde.impanel")

            @signal()
            def TriggerProperty(self, key) -> "s":  # noqa: N802
                return key

        self.iface = Iface()
        self.bus = bus

    async def start(self):
        self.bus.export("/org/kde/impanel", self.iface)
        await self.bus.request_name("org.kde.impanel")
        await asyncio.sleep(0.5)  # kimpanel 模块看见名字出现后才接信号

    def trigger(self, key):
        self.iface.TriggerProperty(key)


async def menu_cases(bus, im):
    """自绘菜单：入口、渲染、悬停 / 键盘 / 点选、子菜单、动作生效，以及每一条关闭路径——
    每条关闭后立刻打字，验证 menu_open 没有卡住输入。"""
    watch = await kimpanel_watch(bus)
    c = await new_ctx(bus, im, "e2e-menu")
    await c.ic.call_set_cursor_rect(300, 400, 2, 20)

    # a) 右键候选 → 候选菜单，出现在右键处。
    menu, pt = await open_candidate_menu(c)
    ok = menu is not None and menu[5]
    check("菜单：右键候选弹出候选菜单", ok, f"menu={menu} 右键点={pt}")
    if not ok:
        await c.key("Escape")
        return
    _, mx, my, mw, mh, _ = menu
    print(f"  候选菜单：{mw}x{mh} @ ({mx},{my})，右键点 {pt}", flush=True)
    # 窗口 = 内容 − 软投影扩边：内容左上才是右键点，窗口左上在它左上方几个到十几个像素。
    check("菜单：候选菜单出现在右键处（内容左上 = 右键点，窗口含投影扩边）",
          0 <= pt[0] - mx <= 30 and 0 <= pt[1] - my <= 30, f"@({mx},{my}) 右键点 {pt}")
    path, n = window_has_content(menu[0], "menu_candidate")
    check("菜单：候选菜单有绘制", n > 1, f"{n} 种颜色，截图 {path}")
    print(f"  候选菜单截图：{path}", flush=True)
    full, _ = screenshot("menu_candidate_screen")
    print(f"  全屏截图：{full}", flush=True)

    # b) 悬停高亮：鼠标移到第一项上，窗口像素要变。
    row0 = await probe_row(menu)
    check("菜单：悬停可选项 → 高亮（像素变化）", row0 is not None, "从上往下移遍窗口，像素一直没变")
    path, _ = window_has_content(menu[0], "menu_candidate_hover")
    print(f"  悬停高亮截图：{path}", flush=True)

    # c) 键盘：菜单开着时按键被吃；↓ 移动高亮；普通字母关菜单且不外泄（不组字、不上屏）。
    before = xwd_raw(menu[0])
    eaten = await c.key("Down")
    await asyncio.sleep(0.3)
    after = xwd_raw(menu[0])
    check("菜单：↓ 被吃掉、高亮移动", eaten is True and before != after,
          f"eaten={eaten} 像素变化={before != after}")
    c.preedits.clear()
    eaten = await c.key("a")
    gone = await wait_menus_gone()
    check("菜单：普通字母关菜单、被吃掉、不外泄", eaten is True and gone and c.take() == "",
          f"eaten={eaten} 菜单消失={gone}")
    await c.key("Escape")  # 收掉组字
    c.take()
    await typing_works(c, "字母关闭")

    # d) 候选菜单里没有通往主菜单的项（末行是「复制」，协调器单测钉内容）；主菜单走状态区入口。
    #    主菜单比候选菜单高，截图看字形 / 勾选 / 分隔线 / 子菜单箭头。
    menu, pt = await open_candidate_menu(c)
    cand_h = menu[4] if menu else 0
    await c.key("Escape")
    await wait_menus_gone()
    await end_composition(c)
    main = await open_main_menu(c)
    ok = main is not None and main[5] and main[4] > cand_h
    check("菜单：状态区「清风输入法菜单」→ 功能主菜单（比候选菜单高）", ok,
          f"候选菜单={menu} 主菜单={main}")
    if ok:
        _, mx, my, mw, mh, _ = main
        path, n = window_has_content(main[0], "menu_main")
        check("菜单：主菜单有绘制", n > 1, f"{n} 种颜色，截图 {path}")
        print(f"  主菜单：{mw}x{mh} @ ({mx},{my})，截图 {path}", flush=True)
        # e) 子菜单：悬停第 0 行「输入方案 ▸」→ 第 1 级出现在右侧。
        await probe_row(main)
        sub = await wait_window("wind-menu-1", True)
        ok = sub is not None and sub[5] and sub[1] >= mx + mw - 10
        check("菜单：悬停「输入方案」→ 子菜单在右侧展开", ok, f"sub={sub} main=({mx},{my},{mw})")
        if ok:
            path, _ = window_has_content(sub[0], "menu_submenu")
            full, _ = screenshot("menu_submenu_screen")
            print(f"  子菜单截图：{path}；全屏：{full}", flush=True)
            # f) 点子菜单第 0 行「英文」→ 切到英文：字母交还宿主。
            r0 = await probe_row(sub)
            if r0 is not None:
                sh("xdotool", "mousemove", str(sub[1] + sub[3] // 3), str(r0), "click", "1")
            gone = await wait_menus_gone()
            eaten = await c.type("abc")
            check("菜单：点子菜单「英文」→ 菜单收起、切到英文（字母直通）",
                  gone and eaten == [False] * 3 and c.take() == "",
                  f"菜单消失={gone} eaten={eaten}")
            # 不经按键的切换：服务端经 push 通道推 CMD_STATE_PUSH，托盘图标要跟上。
            en = await wait_until(lambda: watch.icon() == ICON_EN, 2)
            check("托盘图标：菜单点「英文」→ 「英」（push 通道的状态推送）", en,
                  f"im={watch.im()!r}")
    await c.key("Escape")
    c.take()
    # 切回中文（顺带验按键这条来源的托盘图标）：下面点子菜单「全拼」要从中文态验起。
    await c.tap_shift()
    zh = await wait_until(lambda: watch.icon() == ICON_PIN, 2)
    check("托盘图标：Shift 切回中文 → 「拼」（全拼）", zh, f"im={watch.im()!r}")

    # g) 主菜单的子菜单点选：输入方案 ▸ 全拼（英文、分隔线之后那一行）→ 仍是中文全拼。
    #    关掉菜单后组字还挂着，先收掉再验打字。
    main = await open_main_menu(c)
    ok = main is not None
    check("菜单：状态区入口再开主菜单", ok, f"menu={main}")
    if ok:
        await probe_row(main)
        sub = await wait_window("wind-menu-1", True)
        r0 = await probe_row(sub) if sub and sub[5] else None
        if r0 is not None:
            # 「英文」之后隔一条分隔线（7px）才是「全拼」。
            sh("xdotool", "mousemove", str(sub[1] + sub[3] // 3), str(r0 + MENU_ROW + 7),
               "click", "1")
        gone = await wait_menus_gone()
        check("菜单：点子菜单「全拼」后菜单收起", gone, f"sub={sub}")
    await end_composition(c)
    await typing_works(c, "子菜单点选「全拼」")

    # h) 键盘驱动主菜单：↓↓ 到「全角」、回车 → 全角生效（空格上屏全角空格）。
    await open_main_menu(c)
    await c.key("Down")
    await c.key("Down")
    await c.key("Return")
    gone = await wait_menus_gone()
    await end_composition(c)
    await c.key(" ")
    got = c.take()
    check("菜单：键盘 ↓↓回车 选「全角」→ 菜单收起、空格上屏全角空格",
          gone and got == "　", f"菜单消失={gone} 上屏={got!r}")
    # 复原：Shift+空格（全角开关热键）。
    await c.ic.call_process_key_event(0x20, 65, STATE_SHIFT, False, 0)
    await c.ic.call_process_key_event(0x20, 65, STATE_SHIFT, True, 0)
    await asyncio.sleep(0.1)
    await typing_works(c, "键盘点选")

    # i) 键盘 → 展开子菜单、← 收回。
    await open_main_menu(c)
    await c.key("Down")
    await c.key("Right")
    sub = await wait_window("wind-menu-1", True)
    await c.key("Left")
    back = await wait_window("wind-menu-1", False)
    check("菜单：→ 展开子菜单、← 收回", sub is not None and sub[5] and back is not None
          and not back[5], f"→ {sub} ← {back}")

    # ── 关闭路径（每条后立刻打字）──
    # 1) Esc
    await c.key("Escape")
    check("菜单关闭：Esc", await wait_menus_gone(), "菜单还在")
    await end_composition(c)
    await typing_works(c, "Esc 关闭")

    # 2) 点菜单外
    await open_main_menu(c)
    sh("xdotool", "mousemove", "1200", "60", "click", "1")
    check("菜单关闭：点菜单外", await wait_menus_gone(), "菜单还在")
    await end_composition(c)
    await typing_works(c, "点菜单外关闭")

    # 3) 右键（任何位置都只关菜单，同 Windows）
    main = await open_main_menu(c)
    if main:
        sh("xdotool", "mousemove", str(main[1] + 20), str(main[2] + 20), "click", "3")
    check("菜单关闭：右键", await wait_menus_gone(), "菜单还在")
    await end_composition(c)
    await typing_works(c, "右键关闭")

    # 4) 失焦
    await open_main_menu(c)
    await asyncio.sleep(0.3)  # 过服务端 250ms 的「刚打开」焦点守卫之外，addon 那条是无条件的
    await c.ic.call_focus_out()
    check("菜单关闭：失焦", await wait_menus_gone(), "菜单还在")
    await c.ic.call_focus_in()
    await asyncio.sleep(0.2)
    await end_composition(c)
    await typing_works(c, "失焦关闭")

    # 5) 切换输入上下文（焦点换到另一个文本框）
    await open_main_menu(c)
    d = await new_ctx(bus, im, "e2e-menu-other")
    check("菜单关闭：焦点切到另一个文本框", await wait_menus_gone(), "菜单还在")
    await typing_works(d, "换输入上下文")
    await d.ic.call_focus_out()
    await c.ic.call_focus_in()
    await asyncio.sleep(0.2)
    await end_composition(c)

    # 6) 候选被清空 / 组合结束（宿主 Reset：点了别处、挪了光标）→ 候选菜单随之关
    menu, _ = await open_candidate_menu(c)
    ok = menu is not None and menu[5]
    await c.ic.call_reset()
    gone = await wait_menus_gone()
    check("菜单关闭：组合被宿主终止（Reset）时候选菜单一并关", ok and gone,
          f"开={ok} 关={gone}")
    c.preedits.clear()
    await typing_works(c, "组合终止")

    # 7) 空闲超时（e2e 把 addon 的超时调到 WIND_MENU_IDLE_TIMEOUT_MS）
    idle_ms = int(os.environ.get("WIND_MENU_IDLE_TIMEOUT_MS", "0") or 0)
    if idle_ms:
        await open_main_menu(c)
        gone = await wait_until(menus_gone, idle_ms / 1000 + 3)
        check(f"菜单关闭：空闲 {idle_ms}ms 自动收起", gone, "菜单还在")
        await end_composition(c)
        await typing_works(c, "空闲超时")

    sh("xdotool", "mousemove", "5", "5")
    await c.ic.call_focus_out()
    await asyncio.sleep(0.2)
    return c


def pointer_grab_free():
    """另起一个 X 客户端试着抓指针：抓得到 = 此刻没有别的客户端抓着（抓到即放）。
    菜单开着时这里必须是 False——判据先验可观测，再拿它断言「放开了」。"""
    import ctypes
    x = ctypes.cdll.LoadLibrary("libX11.so.6")
    x.XOpenDisplay.restype = ctypes.c_void_p
    x.XOpenDisplay.argtypes = [ctypes.c_char_p]
    x.XDefaultRootWindow.restype = ctypes.c_ulong
    x.XDefaultRootWindow.argtypes = [ctypes.c_void_p]
    x.XGrabPointer.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_uint,
                               ctypes.c_int, ctypes.c_int, ctypes.c_ulong, ctypes.c_ulong,
                               ctypes.c_ulong]
    x.XUngrabPointer.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
    x.XSync.argtypes = [ctypes.c_void_p, ctypes.c_int]
    x.XCloseDisplay.argtypes = [ctypes.c_void_p]
    d = x.XOpenDisplay(None)
    if not d:
        return False
    # ButtonPressMask = 4，GrabModeAsync = 1；返回 GrabSuccess = 0 / AlreadyGrabbed = 1。
    r = x.XGrabPointer(d, x.XDefaultRootWindow(d), 0, 4, 1, 1, 0, 0, 0)
    if r == 0:
        x.XUngrabPointer(d, 0)
        x.XSync(d, 0)
    x.XCloseDisplay(d)
    return r == 0


async def menu_stall_cases(bus, im):
    """服务卡死（SIGSTOP）时菜单开着：push 仍在线（不会走断线收菜单那条路），指针却被抓着——
    不设防的话整个桌面的鼠标失灵，且每次移动都续空闲计时、永远收不掉。要求：动一下鼠标，
    上报失败（一次超时）后本端收菜单、放开指针；恢复后照常打字。"""
    c = await new_ctx(bus, im, "e2e-menu-stall")
    await c.ic.call_set_cursor_rect(300, 400, 2, 20)
    menu = await open_main_menu(c, tries=3)
    grabbed = menu is not None and not pointer_grab_free()
    pid = service_pid()
    subprocess.run(["kill", "-STOP", pid])
    try:
        t0 = time.monotonic()
        sh("xdotool", "mousemove", "1100", "700")
        sh("xdotool", "mousemove", "1110", "710")
        freed = await wait_until(pointer_grab_free, 8.0)
        dt = time.monotonic() - t0
        gone = menus_gone()
    finally:
        subprocess.run(["kill", "-CONT", pid])
    check("菜单开着时指针确被抓住（「放开」判据可观测）", grabbed, f"菜单={menu}")
    print(f"  卡死后动鼠标到放开指针用时 {dt:.2f}s", flush=True)
    # 上界取 4 秒：一次超时（2 秒）量级，且明显短于空闲超时（e2e 里 5 秒）——靠空闲计时兜底
    # 收起的不算数。
    check("服务卡死时动一下鼠标：本端收起菜单、放开指针（一次超时量级）",
          freed and gone and dt < 4.0, f"放开={freed} 菜单收={gone} 用时 {dt:.1f}s")
    await asyncio.sleep(3.5)  # 熔断期过去
    c.preedits.clear()
    await c.type("nihao")
    await c.key(" ")
    got = c.take()
    check("菜单 + 服务卡死恢复后照常上屏「你好」", got == "你好", f"上屏={got!r}")
    sh("xdotool", "mousemove", "5", "5")
    await c.ic.call_focus_out()
    await asyncio.sleep(0.2)


async def menu_idle_ignores_motion_cases(bus, im):
    """指针移动不续空闲计时：在菜单外来回动（服务端没有新菜单帧），到点照样收起。曾经每批移动
    都续计时，服务卡死时只要鼠标在动，菜单就永远收不掉、指针一直被抓着。"""
    idle_ms = int(os.environ.get("WIND_MENU_IDLE_TIMEOUT_MS", "60000"))
    c = await new_ctx(bus, im, "e2e-menu-motion")
    await c.ic.call_set_cursor_rect(300, 400, 2, 20)
    menu = await open_main_menu(c, tries=3)
    t0 = time.monotonic()
    closed_at = None
    i = 0
    while time.monotonic() - t0 < idle_ms / 1000 + 3:
        if menus_gone():
            closed_at = time.monotonic() - t0
            break
        i += 1
        sh("xdotool", "mousemove", str(1100 + i % 2 * 7), str(700 + i % 2 * 5))
        await asyncio.sleep(0.3)
    check(f"菜单外一直动鼠标：空闲 {idle_ms}ms 照样收起（移动不续计时）",
          menu is not None and closed_at is not None, f"菜单={menu} 收起于 {closed_at}")
    await end_composition(c)
    await typing_works(c, "动鼠标不续空闲计时")
    sh("xdotool", "mousemove", "5", "5")
    await c.ic.call_focus_out()
    await asyncio.sleep(0.2)


async def menu_max_grab_cases(bus, im):
    """抓指针的绝对上限：一直有键盘活动（空闲计时一直被续）也不能无限抓着。e2e 把上限调到
    WIND_MENU_MAX_GRAB_MS（出厂 120 秒）。"""
    max_ms = int(os.environ.get("WIND_MENU_MAX_GRAB_MS", "0") or 0)
    idle_ms = int(os.environ.get("WIND_MENU_IDLE_TIMEOUT_MS", "60000"))
    c = await new_ctx(bus, im, "e2e-menu-maxgrab")
    await c.ic.call_set_cursor_rect(300, 400, 2, 20)
    menu = await open_main_menu(c, tries=3)
    t0 = time.monotonic()
    closed_at = None
    while time.monotonic() - t0 < max_ms / 1000 + 4:
        if menus_gone():
            closed_at = time.monotonic() - t0
            break
        await c.key("Down")  # 菜单键：续空闲计时
        await asyncio.sleep(min(1.0, idle_ms / 4000))
    freed = pointer_grab_free()
    check(f"菜单一直有活动：开满 {max_ms}ms 本端收起、放开指针",
          menu is not None and max_ms > idle_ms and closed_at is not None
          and closed_at > idle_ms / 1000 and freed,
          f"菜单={menu} 收起于 {closed_at} 放开={freed}")
    await end_composition(c)
    await typing_works(c, "抓指针上限收起")
    await c.ic.call_focus_out()
    await asyncio.sleep(0.2)


async def menu_compositor_toggle_cases(bus, im):
    """菜单开着时合成器起停：第 0 级菜单按新的合成器状态换 visual 重建，旧窗口连同指针抓取一起
    没了（X 在抓取窗口不可见时自动放掉抓取）。本端得重新抓上，否则点菜单外再也看不见。"""
    c = await new_ctx(bus, im, "e2e-menu-comp")
    await c.ic.call_set_cursor_rect(300, 400, 2, 20)
    menu = await open_main_menu(c, tries=3)
    grabbed = menu is not None and not pointer_grab_free()
    comp_pid = os.environ.get("WIND_E2E_COMP_PID", "")
    ours = None
    if comp_pid:
        subprocess.run(["kill", comp_pid])
    else:
        ours = subprocess.Popen(["xcompmgr"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    await asyncio.sleep(0.8)
    await c.key("Down")  # 菜单重绘 → 新的一帧 → 按新的合成器状态重建窗口
    await asyncio.sleep(0.6)
    after = menu_win(0)
    recreated = menu is not None and after is not None and after[0] != menu[0] and after[5]
    regrabbed = not pointer_grab_free()
    # 合成器状态还原（被我们杀掉的那个换一个新的接着跑，随 Xvfb 退出）。
    if comp_pid:
        new = subprocess.Popen(["xcompmgr"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        os.environ["WIND_E2E_COMP_PID"] = str(new.pid)
    else:
        ours.terminate()
        ours.wait()
    await asyncio.sleep(0.8)
    check("合成器起停：第 0 级菜单重建后重新抓住指针",
          grabbed and recreated and regrabbed,
          f"起初抓住={grabbed} 重建={recreated}（{menu} → {after}） 重抓={regrabbed}")
    await c.key("Escape")
    await wait_menus_gone()
    await end_composition(c)
    await typing_works(c, "合成器起停")
    await c.ic.call_focus_out()
    await asyncio.sleep(0.2)


async def x_conn_lost_cases(bus, im):
    """X 连接进入错误态（xkill、Xwayland 重启）时菜单开着：菜单空闲计时器到点收菜单、按指针
    真实位置重定悬停。重定曾经会顺手重连 X，重连销毁了正在执行的那个计时器（释放后使用，
    fcitx5 随之段错误）。现在查指针只读现有连接，重连只在下一次贴帧时做。"""
    c = await new_ctx(bus, im, "e2e-xkill")
    await c.ic.call_set_cursor_rect(300, 400, 2, 20)
    menu = await open_main_menu(c, tries=3)
    killed = False
    if menu:
        r = subprocess.run(["xkill", "-id", menu[0]], capture_output=True, text=True)
        killed = r.returncode == 0
        print(f"  xkill -id {menu[0]} rc={r.returncode} {r.stderr.strip()}", flush=True)
    idle_ms = int(os.environ.get("WIND_MENU_IDLE_TIMEOUT_MS", "60000"))
    await asyncio.sleep(idle_ms / 1000 + 1.5)  # 等空闲计时器到点（它在断掉的连接上收菜单）
    await end_composition(c)
    await c.type("nihao")
    cand = await wait_window("wind-candidate", True)
    await c.key(" ")
    got = c.take()
    check("X 连接被断（xkill）且菜单开着：空闲超时收菜单后 fcitx5 存活、照常上屏",
          menu is not None and killed and got == "你好", f"菜单={menu} xkill={killed} 上屏={got!r}")
    check("X 连接被断后下一帧重连，候选窗重新出现", cand is not None and cand[5], f"{cand}")
    await c.ic.call_focus_out()
    await asyncio.sleep(0.2)


async def menu_restart_cases(bus, im, restart):
    """服务一侧没了（被杀 / 重启）：菜单是它画的，addon 必须自己收掉、放开指针；新服务起来后
    照常打字。仅在 e2e 自己管服务时跑（自动拉起模式下服务不归脚本管）。"""
    c = await new_ctx(bus, im, "e2e-menu-restart")
    await c.ic.call_set_cursor_rect(300, 400, 2, 20)
    w = os.environ["W"]
    # a) 服务被杀：push 断线 → addon 收菜单。
    menu = await open_main_menu(c, tries=5)
    opened = menu is not None
    pid = open(os.path.join(w, "svc.pid")).read().strip()
    kr = subprocess.run(["kill", "-9", pid], capture_output=True, text=True)
    print(f"  菜单={menu} kill -9 {pid} rc={kr.returncode} {kr.stderr.strip()}", flush=True)
    gone = await wait_menus_gone(5)
    check("菜单关闭：服务进程被杀（push 断线）", opened and gone, f"开={opened} 关={gone}")
    rc = await asyncio.to_thread(subprocess.run, [restart])
    await c.ic.call_focus_in()
    ready = await wait_ready(c)
    check("菜单：服务被杀后重启，按键恢复", rc.returncode == 0 and ready, f"rc={rc.returncode}")
    await typing_works(c, "服务被杀并重启")

    # b) 服务正常重启（SERVICE_READY）：菜单开着时重启。
    menu = await open_main_menu(c, tries=5)
    opened = menu is not None
    rc = await asyncio.to_thread(subprocess.run, [restart])
    gone = await wait_menus_gone(5)
    check("菜单关闭：服务重启（SERVICE_READY）", opened and gone, f"开={opened} 关={gone}")
    await c.ic.call_focus_in()
    await wait_ready(c)
    await typing_works(c, "服务重启")
    return c

# ── 浮层交互（X11）：状态气泡 / 悬停提示 / Toast 的鼠标交互与右键菜单 ─────────────────
# 规则对位 Windows（wind_linux/include/OverlayInput.h）：悬停 / 拖动 / 菜单开着时不自动消失；
# 提示在指针移进它时保持、右键弹提示菜单；气泡可拖（松手报 pos.status_tip，落不落盘看定位
# 方式）、右键弹气泡菜单；Toast 点一下就关。e2e.sh 把气泡时长调成 2 秒、放了假 xclip。

def read_text(path):
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            return f.read()
    except FileNotFoundError:
        return ""


def user_config():
    return read_text(os.path.join(os.environ["XDG_CONFIG_HOME"], "WindInput", "config.toml"))


def config_int(text, key):
    import re
    m = re.search(rf"^{key}\s*=\s*(-?\d+)", text, re.M)
    return int(m.group(1)) if m else None


def config_str(text, key):
    import re
    m = re.search(rf'^{key}\s*=\s*"([^"]*)"', text, re.M)
    return m.group(1) if m else None


def fcitx_log_lines():
    return read_lines(os.path.join(os.environ["W"], "fcitx5.log"))


def drag_reports():
    """addon 上报 pos.status_tip 的次数（WIND_DEBUG「状态气泡拖动松手」）。"""
    return sum("状态气泡拖动松手" in l for l in fcitx_log_lines())


def clipboard_log():
    return read_text(os.path.join(os.environ["W"], "clipboard.log"))


def center(win):
    return win[1] + win[3] // 2, win[2] + win[4] // 2


async def pop_status(c):
    """中英各切一次（输入状态不变），等状态气泡出现。"""
    await c.tap_shift()
    await c.tap_shift()
    return await wait_window("wind-status", True)


async def drag(x0, y0, dx, dy, steps=6):
    """左键按在 (x0, y0) 拖 (dx, dy) 后松开（XTEST，分步移动让 addon 收到连续的移动事件）。"""
    sh("xdotool", "mousemove", str(x0), str(y0))
    await asyncio.sleep(0.05)
    sh("xdotool", "mousedown", "1")
    for i in range(1, steps + 1):
        sh("xdotool", "mousemove", str(x0 + dx * i // steps), str(y0 + dy * i // steps))
        await asyncio.sleep(0.03)
    sh("xdotool", "mouseup", "1")
    await asyncio.sleep(0.3)


async def open_tooltip_menu(c):
    """组字 → 悬停候选出提示 → 指针挪进提示 → 右键。返回 (菜单窗口, 提示窗口)。"""
    await c.type("nihao")
    await asyncio.sleep(0.4)
    if not await hover_candidate_point(c):
        return None, None
    tip = candidate_window("wind-tooltip")
    if not (tip and tip[5]):
        return None, tip
    tx, ty = center(tip)
    sh("xdotool", "mousemove", str(tx), str(ty))
    await asyncio.sleep(0.4)
    sh("xdotool", "click", "3")
    return await wait_window("wind-menu-0", True), tip


async def overlay_interaction_cases(bus, im):
    c = await new_ctx(bus, im, "e2e-overlay-ui")
    await c.ic.call_set_cursor_rect(300, 400, 2, 20)
    sh("xdotool", "mousemove", "5", "5")

    # ── 状态气泡 ──
    # a) 悬停保持：指针停在气泡上，过了时长（2 秒）仍在；离开后按时长消失。
    st = await pop_status(c)
    ok = st is not None and st[5]
    check("气泡交互：Shift 切换后状态气泡出现", ok, f"window={st}")
    if ok:
        cx, cy = center(st)
        sh("xdotool", "mousemove", str(cx), str(cy))
        await asyncio.sleep(3.0)
        held = candidate_window("wind-status")
        check("气泡交互：指针悬停其上时超过时长也不消失", held is not None and held[5],
              f"window={held}")
        # b) 拖动（出厂跟随光标）：窗口跟手平移；松手上报 pos.status_tip；这一模式不落盘。
        before = drag_reports()
        await drag(cx, cy, 120, 60)
        moved = candidate_window("wind-status")
        ok = moved is not None and moved[5] and (moved[1] - st[1], moved[2] - st[2]) == (120, 60)
        check("气泡交互：左键拖动跟手（窗口平移 = 指针位移）", ok, f"拖前={st} 拖后={moved}")
        check("气泡交互：松手上报 pos.status_tip", drag_reports() == before + 1,
              f"上报 {drag_reports() - before} 次")
        await asyncio.sleep(0.3)
        cfg = user_config()
        check("气泡交互：跟随光标模式下拖动不落盘", config_int(cfg, "custom_x") is None,
              f"config.toml 里出现了 custom_x：{cfg!r}")
        full, _ = screenshot("status_dragged_screen")
        print(f"  拖动后全屏截图：{full}", flush=True)
        # c) 离开后照常消失（悬停结束重新给满一份时长）。
        sh("xdotool", "mousemove", "5", "5")
        gone = await wait_window("wind-status", False, timeout=5.0)
        check("气泡交互：指针离开后按时长自动消失", gone is not None and not gone[5],
              f"window={gone}")

    # d) 右键气泡 → 气泡菜单（常驻显示 / 焦点切换时显示 / 固定位置 / 恢复默认位置 / 截图）；
    #    菜单开着时气泡不消失；点「固定位置」→ 以气泡当前位置落盘（经 pos.status_tip.query）。
    st = await pop_status(c)
    menu = None
    if st and st[5]:
        cx, cy = center(st)
        sh("xdotool", "mousemove", str(cx), str(cy), "click", "3")
        menu = await wait_window("wind-menu-0", True)
    ok = menu is not None and menu[5]
    check("气泡菜单：右键状态气泡弹出菜单", ok, f"menu={menu} 气泡={st}")
    if ok:
        path, n = window_has_content(menu[0], "menu_status")
        check("气泡菜单：菜单有绘制", n > 1, f"{n} 种颜色，截图 {path}")
        print(f"  气泡菜单截图：{path}", flush=True)
        full, _ = screenshot("menu_status_screen")
        print(f"  全屏截图：{full}", flush=True)
        await asyncio.sleep(2.6)
        still = candidate_window("wind-status")
        check("气泡菜单：菜单开着时气泡超过时长也不消失", still is not None and still[5],
              f"window={still}")
        row0 = await probe_row(menu)
        if row0 is not None:
            sh("xdotool", "mousemove", str(menu[1] + menu[3] // 3), str(row0 + 2 * MENU_ROW),
               "click", "1")
        gone = await wait_menus_gone()
        pinned = await wait_until(
            lambda: config_str(user_config(), "position_mode") == "fixed"
            and config_int(user_config(), "custom_x") is not None, 3)
        cfg = user_config()
        fx, fy = config_int(cfg, "custom_x"), config_int(cfg, "custom_y")
        # 落盘的是内容左上，窗口左上再往左上偏一个软阴影扩边（0～30 像素）。
        near = (fx is not None and fy is not None and 0 <= fx - still[1] <= 30
                and 0 <= fy - still[2] <= 30)
        check("气泡菜单：点「固定位置」→ 以气泡当前位置落盘", gone and pinned and near,
              f"菜单消失={gone} custom=({fx},{fy}) 气泡窗口=({still[1]},{still[2]})")

        # e) 固定位置模式下拖动：落点写回 custom_x/y（位移与拖动一致）。
        st = await pop_status(c)
        ok = st is not None and st[5]
        if ok:
            check("气泡交互：固定位置模式下气泡出现在落盘位置",
                  0 <= fx - st[1] <= 30 and 0 <= fy - st[2] <= 30, f"气泡={st} custom=({fx},{fy})")
            cx, cy = center(st)
            await drag(cx, cy, -100, -50)
            ok = await wait_until(lambda: (config_int(user_config(), "custom_x"),
                                           config_int(user_config(), "custom_y"))
                                  == (fx - 100, fy - 50), 3)
            cfg = user_config()
            check("气泡交互：固定位置模式下拖动落盘（custom_x/y 随落点更新）", ok,
                  f"拖前 ({fx},{fy}) 拖后 ({config_int(cfg, 'custom_x')},{config_int(cfg, 'custom_y')})")

        # f) 恢复默认位置：回到跟随光标。
        st = await pop_status(c)
        if st and st[5]:
            cx, cy = center(st)
            sh("xdotool", "mousemove", str(cx), str(cy), "click", "3")
            menu = await wait_window("wind-menu-0", True)
            row0 = await probe_row(menu) if menu and menu[5] else None
            if row0 is not None:
                sh("xdotool", "mousemove", str(menu[1] + menu[3] // 3),
                   str(row0 + 3 * MENU_ROW), "click", "1")
        await wait_menus_gone()
        # 用户配置只写非默认值：回到出厂的跟随光标后，这几个键从 config.toml 里消失（或写成默认值）。
        back = await wait_until(
            lambda: config_str(user_config(), "position_mode") in (None, "follow_caret")
            and config_int(user_config(), "custom_x") in (None, 0), 3)
        cfg = user_config()
        check("气泡菜单：点「恢复默认位置」→ 回到跟随光标", back,
              f"position_mode={config_str(cfg, 'position_mode')} custom_x={config_int(cfg, 'custom_x')}")
    sh("xdotool", "mousemove", "5", "5")
    await wait_window("wind-status", False, timeout=5.0)
    await typing_works(c, "气泡菜单用完")

    # ── 悬停提示 ──
    # g) 指针从候选挪进提示：提示保持（Windows：光标进入气泡即撤掉候选悬停的变化）。
    await c.type("nihao")
    await asyncio.sleep(0.4)
    pt = await hover_candidate_point(c)
    tip = candidate_window("wind-tooltip")
    kept = None
    if pt and tip and tip[5]:
        tx, ty = center(tip)
        sh("xdotool", "mousemove", str(tx), str(ty))
        await asyncio.sleep(1.2)
        kept = candidate_window("wind-tooltip")
    check("提示交互：指针从候选挪进提示后提示保持显示", kept is not None and kept[5],
          f"提示={tip} 1.2 秒后={kept}")
    # h) 离开提示到别处：宽限后收起。
    sh("xdotool", "mousemove", "5", "5")
    gone = await wait_window("wind-tooltip", False, timeout=2.0)
    check("提示交互：指针离开提示后提示收起", gone is not None and not gone[5], f"提示={gone}")
    await c.key("Escape")
    c.take()

    # i) 右键提示 → 提示菜单（复制「段」/ 上屏「段」/ … / 复制全部 / 截图此窗口）；菜单开着时
    #    提示不消失；点第一项（复制类）→ 剪贴板里是提示内容，并弹「提示内容已复制」。
    menu, tip = await open_tooltip_menu(c)
    ok = menu is not None and menu[5]
    check("提示菜单：右键悬停提示弹出菜单", ok, f"menu={menu} 提示={tip}")
    if ok:
        path, n = window_has_content(menu[0], "menu_tooltip")
        check("提示菜单：菜单有绘制", n > 1, f"{n} 种颜色，截图 {path}")
        print(f"  提示菜单截图：{path}", flush=True)
        full, _ = screenshot("menu_tooltip_screen")
        print(f"  全屏截图：{full}", flush=True)
        await asyncio.sleep(0.6)
        t2 = candidate_window("wind-tooltip")
        check("提示菜单：菜单开着时提示不消失", t2 is not None and t2[5], f"提示={t2}")
        clip0 = len(clipboard_log())
        row0 = await probe_row(menu)
        if row0 is not None:
            sh("xdotool", "mousemove", str(menu[1] + menu[3] // 3), str(row0), "click", "1")
        gone = await wait_menus_gone()
        copied = await wait_until(lambda: len(clipboard_log()) > clip0, 3)
        got = clipboard_log()[clip0:]
        text = "\n".join(l for l in got.splitlines() if not l.startswith("== ")).strip()
        check("提示菜单：点「复制…」→ 剪贴板里是提示内容", gone and copied and text != "",
              f"菜单消失={gone} 剪贴板新增={got!r}")
        print(f"  复制到剪贴板：{text!r}", flush=True)
        toast = await wait_window("wind-toast", True, timeout=2.0)
        check("提示菜单：复制后弹 Toast 反馈", toast is not None and toast[5], f"toast={toast}")
        if toast and toast[5]:
            path, _ = window_has_content(toast[0], "toast_tooltip_copied")
            print(f"  复制反馈 Toast 截图：{path}", flush=True)
    await c.key("Escape")
    c.take()
    sh("xdotool", "mousemove", "5", "5")
    await typing_works(c, "提示菜单用完")

    # ── Toast ──
    svc = os.path.join(os.environ.get("W", ""), "svc", "wind_input")

    async def toast(text, ms):
        await asyncio.to_thread(subprocess.run, [svc, "ui", "toast", text, "--pos", "top_right",
                                                 "--ms", str(ms)], capture_output=True)
        return await wait_window("wind-toast", True)

    await wait_window("wind-toast", False, timeout=5.0)
    # j) 悬停暂停：指针停在 Toast 上时过了时长仍在；离开后照常消失。
    t = await toast("悬停不消失", 1500)
    if t and t[5]:
        tx, ty = center(t)
        sh("xdotool", "mousemove", str(tx), str(ty))
        await asyncio.sleep(2.5)
        held = candidate_window("wind-toast")
        check("Toast：指针悬停其上时超过时长也不消失", held is not None and held[5], f"toast={held}")
        sh("xdotool", "mousemove", "5", "5")
        gone = await wait_window("wind-toast", False, timeout=4.0)
        check("Toast：指针离开后照常消失", gone is not None and not gone[5], f"toast={gone}")
    else:
        check("Toast：ui toast 后出现", False, f"toast={t}")
    # k) 点击提前关闭。
    t = await toast("点我关闭", 10000)
    if t and t[5]:
        tx, ty = center(t)
        sh("xdotool", "mousemove", str(tx), str(ty), "click", "1")
        gone = await wait_window("wind-toast", False, timeout=1.0)
        check("Toast：点击后立即关闭（不等 10 秒）", gone is not None and not gone[5],
              f"toast={gone}")
    else:
        check("Toast：ui toast 后出现", False, f"toast={t}")
    sh("xdotool", "mousemove", "5", "5")

    # ── 复位路径：浮层交互进行中被打断，之后立刻能正常打字 ──
    # 1) 提示菜单开着 → Esc
    menu, _ = await open_tooltip_menu(c)
    ok = menu is not None and menu[5]
    await c.key("Escape")
    check("提示菜单关闭：Esc", ok and await wait_menus_gone(), f"开={ok}")
    await c.key("Escape")
    c.take()
    await typing_works(c, "提示菜单 Esc 关闭")
    # 2) 提示菜单开着 → 组合被宿主终止（Reset）：提示依附于候选，菜单随之收
    menu, _ = await open_tooltip_menu(c)
    ok = menu is not None and menu[5]
    await c.ic.call_reset()
    check("提示菜单关闭：组合被宿主终止时一并收起", ok and await wait_menus_gone(), f"开={ok}")
    c.preedits.clear()
    await typing_works(c, "提示菜单随组合终止")
    # 3) 提示菜单开着 → 失焦
    menu, _ = await open_tooltip_menu(c)
    ok = menu is not None and menu[5]
    await asyncio.sleep(0.3)
    await c.ic.call_focus_out()
    check("提示菜单关闭：失焦", ok and await wait_menus_gone(), f"开={ok}")
    await c.ic.call_focus_in()
    await asyncio.sleep(0.2)
    await typing_works(c, "提示菜单失焦关闭")
    # 4) 气泡菜单开着 → 失焦：菜单收起，气泡的「菜单开着」保持随之解除（随后按时长消失）
    sh("xdotool", "mousemove", "5", "5")
    st = await pop_status(c)
    menu = None
    if st and st[5]:
        cx, cy = center(st)
        sh("xdotool", "mousemove", str(cx), str(cy), "click", "3")
        menu = await wait_window("wind-menu-0", True)
    ok = menu is not None and menu[5]
    await asyncio.sleep(0.3)
    await c.ic.call_focus_out()
    closed = await wait_menus_gone()
    sh("xdotool", "mousemove", "5", "5")
    gone = await wait_window("wind-status", False, timeout=5.0)
    check("气泡菜单关闭：失焦（菜单收起、气泡随后按时长消失）",
          ok and closed and gone is not None and not gone[5], f"开={ok} 菜单关={closed} 气泡={gone}")
    await c.ic.call_focus_in()
    await asyncio.sleep(0.2)
    await typing_works(c, "气泡菜单失焦关闭")
    # 5) 拖动中失焦：服务端失焦即收状态提示（同 Windows），气泡被摘 → 拖动随之作废、不上报
    #    （用户没松手，谈不上摆到了哪）；松手不卡，之后照常打字、还能再拖。
    st = await pop_status(c)
    if st and st[5]:
        cx, cy = center(st)
        before = drag_reports()
        sh("xdotool", "mousemove", str(cx), str(cy))
        await asyncio.sleep(0.05)
        sh("xdotool", "mousedown", "1")
        sh("xdotool", "mousemove", str(cx + 40), str(cy + 20))
        await c.ic.call_focus_out()
        gone = await wait_window("wind-status", False, timeout=2.0)
        sh("xdotool", "mousemove", str(cx + 80), str(cy + 40))
        sh("xdotool", "mouseup", "1")
        await asyncio.sleep(0.3)
        check("气泡拖动中失焦：气泡随失焦收起、拖动作废不上报",
              gone is not None and not gone[5] and drag_reports() == before,
              f"气泡={gone} 上报 {drag_reports() - before} 次")
        await c.ic.call_focus_in()
        await asyncio.sleep(0.2)
        st = await pop_status(c)
        if st and st[5]:
            before = drag_reports()
            await drag(*center(st), 30, 10)
            check("气泡拖动中失焦：之后还能再拖（上报一次）", drag_reports() == before + 1,
                  f"上报 {drag_reports() - before} 次")
    else:
        check("气泡拖动中失焦：气泡出现", False, f"window={st}")
    sh("xdotool", "mousemove", "5", "5")
    await typing_works(c, "拖动中失焦")
    await wait_window("wind-status", False, timeout=5.0)
    await c.ic.call_focus_out()
    await asyncio.sleep(0.2)


async def wait_push(c, timeout=10.0):
    """服务（重）启后请求通道先通（按键被吃），push 通道每秒重连一次、要晚一点——在那之前推下来
    的帧（候选窗、浮层）全丢。打一个字等候选窗出现、Esc 等它消失：两步都靠 push，才算通了
    （服务被杀时候选窗可能还挂着旧帧，只看「出现」会被它骗过）。"""
    deadline = asyncio.get_running_loop().time() + timeout
    while True:
        await c.key("a")
        shown = await wait_window("wind-candidate", True, timeout=1.0)
        await c.key("Escape")
        hidden = await wait_window("wind-candidate", False, timeout=1.0)
        c.take()
        c.preedits.clear()
        if shown and shown[5] and hidden is not None and not hidden[5]:
            return True
        if asyncio.get_running_loop().time() >= deadline:
            return False


async def overlay_restart_cases(bus, im, restart):
    """浮层交互进行中服务没了（被杀 / 重启）：拖动态、菜单、气泡的保持都得归位，之后照常打字、
    还能再拖。仅在 e2e 自己管服务时跑。"""
    c = await new_ctx(bus, im, "e2e-overlay-restart")
    await c.ic.call_set_cursor_rect(300, 400, 2, 20)
    sh("xdotool", "mousemove", "5", "5")
    w = os.environ["W"]
    # 上一组用例刚重启过服务：先等请求与 push 两条通道都通。
    await wait_ready(c)
    await wait_push(c)

    # a) 拖动中服务重启（SERVICE_READY → addon 收掉全部浮层）：拖动态随窗口归位、这次不上报。
    st = await pop_status(c)
    ok = st is not None and st[5]
    before = drag_reports()
    if ok:
        cx, cy = center(st)
        sh("xdotool", "mousemove", str(cx), str(cy))
        sh("xdotool", "mousedown", "1")
        sh("xdotool", "mousemove", str(cx + 30), str(cy + 15))
        rc = await asyncio.to_thread(subprocess.run, [restart])
        gone = await wait_window("wind-status", False, timeout=5.0)
        sh("xdotool", "mousemove", str(cx + 60), str(cy + 30))
        sh("xdotool", "mouseup", "1")
        await asyncio.sleep(0.3)
        check("气泡拖动中服务重启：气泡收起、拖动作废不上报",
              rc.returncode == 0 and gone is not None and not gone[5] and drag_reports() == before,
              f"rc={rc.returncode} 气泡={gone} 上报 {drag_reports() - before} 次")
    else:
        check("气泡拖动中服务重启：气泡出现", False, f"window={st}")
    await c.ic.call_focus_in()
    await wait_ready(c)
    await wait_push(c)
    await typing_works(c, "拖动中服务重启")
    st = await pop_status(c)
    if st and st[5]:
        cx, cy = center(st)
        before = drag_reports()
        await drag(cx, cy, 50, 20)
        moved = candidate_window("wind-status")
        check("气泡拖动中服务重启：之后还能再拖（上报一次、跟手）",
              drag_reports() == before + 1 and moved and (moved[1] - st[1], moved[2] - st[2]) == (50, 20),
              f"上报 {drag_reports() - before} 次 拖前={st} 拖后={moved}")
    sh("xdotool", "mousemove", "5", "5")

    # b) 提示菜单开着时服务重启：菜单收起，之后照常打字。
    menu, _ = await open_tooltip_menu(c)
    opened = menu is not None and menu[5]
    rc = await asyncio.to_thread(subprocess.run, [restart])
    gone = await wait_menus_gone(5)
    check("提示菜单关闭：服务重启", opened and gone, f"开={opened} 关={gone}")
    await c.ic.call_focus_in()
    await wait_ready(c)
    await wait_push(c)
    await typing_works(c, "提示菜单开着时服务重启")

    # c) 拖动中服务被杀：push 断线，没人再推隐藏帧——松手不卡，气泡随后按时长自己消失。
    st = await pop_status(c)
    if st and st[5]:
        cx, cy = center(st)
        sh("xdotool", "mousemove", str(cx), str(cy))
        sh("xdotool", "mousedown", "1")
        sh("xdotool", "mousemove", str(cx + 30), str(cy + 15))
        pid = open(os.path.join(w, "svc.pid")).read().strip()
        subprocess.run(["kill", "-9", pid], capture_output=True)
        await asyncio.sleep(0.5)
        sh("xdotool", "mousemove", str(cx + 60), str(cy + 30))
        sh("xdotool", "mouseup", "1")
        sh("xdotool", "mousemove", "5", "5")
        gone = await wait_window("wind-status", False, timeout=5.0)
        check("气泡拖动中服务被杀：松手不卡、气泡按时长消失", gone is not None and not gone[5],
              f"气泡={gone}")
    else:
        check("气泡拖动中服务被杀：气泡出现", False, f"window={st}")
    rc = await asyncio.to_thread(subprocess.run, [restart])
    await c.ic.call_focus_in()
    ready = await wait_ready(c)
    check("气泡拖动中服务被杀：重启后按键恢复", rc.returncode == 0 and ready, f"rc={rc.returncode}")
    await typing_works(c, "拖动中服务被杀并重启")
    return c


# ── 工具栏 / 软键盘（X11）────────────────────────────────────────────────
# 两块常驻面板由服务光栅化，addon 各开一个 override-redirect、不取焦点的窗口（wind-toolbar /
# wind-softkeyboard），拖动区外的点击经 CMD_PANEL_POINTER 转给服务端。格子与键帽的命中矩形只在
# 服务端：几何按默认主题算（wind-ui 的 toolbar.rs / soft_keyboard.rs，Xvfb 下缩放为 1），算错了
# 效果断言会红，不会静默通过。工具栏显隐有迟滞（toolbar_gate），等待一律留足。
TB_GRIP = 12  # 工具栏拖动柄宽
TB_CELL = 30  # 每格宽
TB_CELLS = 5  # 出厂排布显示的格：中英 / 标点 / 全半角 / 软键盘 / 设置
SK_UNIT, SK_GAP, SK_PAD = 42, 5, 12  # 软键盘键位单元 / 键间距 / 面板内边距


def toolbar_cell(win, i):
    """工具栏第 i 格的中心（横条：拖动柄在左，其后每格等宽）。"""
    return win[1] + TB_GRIP + TB_CELL * i + TB_CELL // 2, win[2] + win[4] // 2


def softkey_digit_row(win, col):
    """软键盘数字行第 col 个键帽的中心。行从底往上数：底行（Ctrl / 空格 ┄ ◀ ▶ Esc）贴底边内边距，
    其上依次 ZXCV / ASDF / QWERTY / 数字行，每行一个单元高、隔一个键间距；数字行行首没有功能键。"""
    _, x, y, w, h, _ = win
    top = y + h - SK_PAD - SK_UNIT - 4 * (SK_UNIT + SK_GAP)
    return x + SK_PAD + col * (SK_UNIT + SK_GAP) + SK_UNIT // 2, top + SK_UNIT // 2


def softkey_page_next(win):
    """底行「▶」（下一面）的中心：右组 ◀ ▶ Esc 贴右缘，各 1.5 个单元宽。"""
    _, x, y, w, h, _ = win
    key_w = SK_UNIT * 3 // 2 + SK_GAP // 2
    return x + w - SK_PAD - key_w - SK_GAP - key_w // 2, y + h - SK_PAD - SK_UNIT // 2


async def chord(c, sym, code, state):
    eaten = await c.ic.call_process_key_event(sym, code, state, False, 0)
    await c.ic.call_process_key_event(sym, code, state, True, 0)
    await asyncio.sleep(0.05)
    return eaten


async def click(x, y, settle=0.4):
    sh("xdotool", "mousemove", str(x), str(y), "click", "1")
    await asyncio.sleep(settle)


PANEL_KEYS = ("ui.toolbar.auto_hide_delay", "ui.toolbar.auto_hide_hover_reveal")


async def restore_panel_state(c, saved):
    """面板用例改过的服务状态，失败路径上也还原，免得一条失败带红后面一串：关掉自动隐藏、
    延时与悬停唤回改回原值；中英 / 标点按实测状态拨回中文、中文标点（点工具栏格子）。"""
    sh("xdotool", "mousemove", "5", "5")
    await set_config("ui.toolbar.auto_hide", "false")
    for key, value in saved.items():
        if value:
            await set_config(key, value)
    await c.ic.call_focus_in()
    tb = await wait_window("wind-toolbar", True)
    if not (tb and tb[5]):
        return
    await end_composition(c)
    if await c.key("a") is False:  # 英文态：字母直通
        await click(*toolbar_cell(tb, 0))
    await end_composition(c)
    await chord(c, 0x2C, 59, 0)
    if c.take() == ",":  # 英文标点
        await click(*toolbar_cell(tb, 1))
    sh("xdotool", "mousemove", "5", "5")
    await end_composition(c)


async def panel_cases(bus, im):
    """工具栏随焦点显隐、点工具栏不夺焦（组字原样、上下文不换）、指针停在工具栏上时被隐藏，
    悬停唤回，以及软键盘点键帽上屏。"""
    c = await new_ctx(bus, im, "e2e-panels")
    saved = {k: await get_config(k) for k in PANEL_KEYS}
    try:
        await panel_toolbar_cases(c)
        await panel_hover_reveal_cases(c)
    finally:
        await restore_panel_state(c, saved)
    await panel_softkeyboard_cases(c)


async def panel_hover_reveal_cases(c):
    """悬停唤回（X11 + 合成器）：淡出完毕工具栏以 alpha≈1 留在原位（假隐藏），指针移回去要能
    唤回。服务端把整窗透明度乘进了像素，面板若按 alpha 裁输入区，假隐藏那帧的输入区是空的，
    指针穿透下去、服务端收不到移动，永远唤不回来。按 alpha 裁输入区只在 ARGB 那条路上做，
    故临时起合成器（e2e.sh 已起的话沿用）。"""
    ours = None
    if not os.environ.get("WIND_E2E_COMP_PID"):
        ours = subprocess.Popen(["xcompmgr"], stdout=subprocess.DEVNULL,
                                stderr=subprocess.DEVNULL)
        await asyncio.sleep(0.8)
    try:
        # 重新获焦推一帧：窗口按合成器状态重建成 ARGB 的。
        await c.ic.call_focus_out()
        await wait_window("wind-toolbar", False)
        await c.ic.call_focus_in()
        tb = await wait_window("wind-toolbar", True)
        await asyncio.sleep(0.4)
        if not (tb and tb[5]):
            check("工具栏：悬停唤回（合成器下工具栏出现）", False, f"window={tb}")
            return
        shown = xwd_raw(tb[0])
        rs = [await set_config("ui.toolbar.auto_hide_delay", "1"),
              await set_config("ui.toolbar.auto_hide_hover_reveal", "true"),
              await set_config("ui.toolbar.auto_hide", "true")]
        # 延时 1 秒 + 淡出 1 秒，之后内容停在假隐藏那帧、窗口仍映射。
        await asyncio.sleep(3.0)
        ghost_win = candidate_window("wind-toolbar")
        ghost = xwd_raw(tb[0])
        sh("xdotool", "mousemove", *map(str, toolbar_cell(tb, 2)))
        revealed = await wait_until(lambda: xwd_raw(tb[0]) != ghost, 2.0)
        sh("xdotool", "mousemove", "5", "5")
        check("工具栏：假隐藏后指针移回原位，工具栏被唤回",
              all(r.returncode == 0 for r in rs) and ghost != shown
              and ghost_win is not None and ghost_win[5] and revealed,
              f"config set rc={[r.returncode for r in rs]} 淡出={ghost != shown} "
              f"假隐藏仍映射={ghost_win} 唤回={revealed}")
    finally:
        if ours:
            ours.terminate()
            ours.wait()
            await asyncio.sleep(0.5)


async def panel_toolbar_cases(c):
    await c.ic.call_set_cursor_rect(300, 400, 2, 20)
    sh("xdotool", "mousemove", "5", "5")

    # ── toolbar_follows_focus：焦点进入 → 映射，离开 → 取消映射 ──
    await c.ic.call_focus_out()
    hidden = await wait_window("wind-toolbar", False)
    await c.ic.call_focus_in()
    tb = await wait_window("wind-toolbar", True)
    check("工具栏：焦点进入后出现（wind-toolbar 已映射）",
          (hidden is None or not hidden[5]) and tb is not None and tb[5],
          f"进入前={hidden} 进入后={tb}")
    await c.ic.call_focus_out()
    gone = await wait_window("wind-toolbar", False)
    check("工具栏：焦点离开后隐藏（取消映射）", gone is not None and not gone[5], f"window={gone}")
    await c.ic.call_focus_in()
    tb = await wait_window("wind-toolbar", True)
    check("工具栏：焦点回来再出现", tb is not None and tb[5], f"window={tb}")
    if not (tb and tb[5]):
        return
    check("工具栏：几何与默认主题一致（下面按它算格子坐标）",
          (tb[3], tb[4]) == (TB_GRIP + TB_CELL * TB_CELLS, TB_CELL), f"window={tb}")
    await asyncio.sleep(0.3)
    baseline = xwd_raw(tb[0])

    # ── click_panel_keeps_focus：点工具栏不夺焦 ──
    # a) 中英格：点一下切英文（字母直通），再点切回中文（组字）——点击生效，上下文还是这一个。
    await click(*toolbar_cell(tb, 0))
    en_eaten = await c.key("a")
    await click(*toolbar_cell(tb, 0))
    sh("xdotool", "mousemove", "5", "5")
    await c.type("nihao")
    await c.key(" ")
    got = c.take()
    check("工具栏：点中英格切到英文（字母直通）、再点切回中文照常上屏",
          en_eaten is False and got == "你好", f"英文态 a 被吃={en_eaten} 切回后上屏={got!r}")

    # b) 组字中点标点格：状态切了，组字原样挂着（不上屏、预编辑不清、候选窗还在），接着打字照常。
    await c.type("nihao")
    await asyncio.sleep(0.3)
    before = c.preedit
    n = len(c.preedits)
    await click(*toolbar_cell(tb, 1))
    sh("xdotool", "mousemove", "5", "5")
    await asyncio.sleep(0.2)
    later = c.preedits[n:]
    cand = candidate_window()
    committed = c.take()
    check("工具栏：组字中点标点格，组字不受影响（不上屏、预编辑不清、候选窗仍在）",
          before != "" and committed == "" and "" not in later and c.preedit == before
          and cand is not None and cand[5],
          f"点前预编辑={before!r} 之后={later} 上屏={committed!r} 候选窗={cand}")
    await c.key(" ")
    got = c.take()
    check("工具栏：点击后接着按空格，「你好」上屏到同一个文本框", got == "你好", f"上屏={got!r}")
    # 标点格真的切了：无组字时逗号由中文「，」变成英文「,」，再点一下切回。
    await chord(c, 0x2C, 59, 0)
    after_toggle = c.take()
    await click(*toolbar_cell(tb, 1))
    sh("xdotool", "mousemove", "5", "5")
    await chord(c, 0x2C, 59, 0)
    restored = c.take()
    check("工具栏：标点格点击生效（逗号出英文「,」），再点恢复中文「，」",
          after_toggle == "," and restored == "，",
          f"切后={after_toggle!r} 恢复后={restored!r}")

    # ── 指针停在工具栏上时服务端推隐藏：X 不会再发离开，addon 摘窗时补报 LEAVE ──
    # LEAVE 丢了的可见后果：服务端认为指针一直在工具栏里，自动隐藏永远不触发。故开着自动隐藏
    # （1 秒）验：悬停 → 失焦隐藏 → 指针挪走 → 焦点回来，工具栏要按时自己隐藏；全程不卡。
    tb = candidate_window("wind-toolbar")
    hx, hy = toolbar_cell(tb, 2)
    sh("xdotool", "mousemove", str(hx), str(hy))
    await asyncio.sleep(0.4)
    hovered = xwd_raw(tb[0]) != baseline
    r1 = await set_config("ui.toolbar.auto_hide_delay", "1")
    r2 = await set_config("ui.toolbar.auto_hide", "true")
    await c.ic.call_focus_out()
    gone = await wait_window("wind-toolbar", False)
    sh("xdotool", "mousemove", "5", "5")
    await c.ic.call_focus_in()
    back = await wait_window("wind-toolbar", True)
    t0 = time.monotonic()
    auto = await wait_window("wind-toolbar", False, timeout=6.0)
    waited = time.monotonic() - t0
    await set_config("ui.toolbar.auto_hide", "false")
    check("工具栏：指针悬停其上时失焦隐藏（补报 LEAVE），回来后自动隐藏照常触发",
          r1.returncode == 0 and r2.returncode == 0 and hovered
          and gone is not None and not gone[5] and back is not None and back[5]
          and auto is not None and not auto[5],
          f"config set rc={r1.returncode},{r2.returncode} {r1.stderr.strip()}{r2.stderr.strip()} "
          f"悬停高亮={hovered} 隐藏={gone} 回来={back} 自动隐藏={auto}（等了 {waited:.1f}s）")
    await c.ic.call_focus_out()
    await wait_window("wind-toolbar", False)
    await c.ic.call_focus_in()
    back = await wait_window("wind-toolbar", True)
    await asyncio.sleep(0.4)
    clean = back is not None and back[5] and xwd_raw(back[0]) == baseline
    check("工具栏：关掉自动隐藏后焦点进入照常出现，且不残留悬停高亮", clean, f"window={back}")
    await c.type("nihao")
    await c.key(" ")
    got = c.take()
    check("工具栏：悬停中被隐藏后照常打字", got == "你好", f"上屏={got!r}")


async def panel_softkeyboard_cases(c):
    # ── softkeyboard_click_commits：Ctrl+Shift+K 开软键盘，点键帽上屏 ──
    # Shift 按住时 k 的 keysym 是 K（0x4B），keycode 45。
    eaten = await chord(c, 0x4B, 45, STATE_CTRL | STATE_SHIFT)
    sk = await wait_window("wind-softkeyboard", True)
    check("软键盘：Ctrl+Shift+K 被吃、面板出现", eaten is True and sk is not None and sk[5],
          f"eaten={eaten} window={sk}")
    if sk and sk[5]:
        path, _ = screenshot("softkeyboard", sk[0])
        print(f"  软键盘：{sk[3]}x{sk[4]} @ ({sk[1]},{sk[2]})，截图 {path}", flush=True)
        # 出厂第一面是 PC 键盘（送按键、不直接上屏），点 ▶ 翻到第二面「标点符号」。
        face = xwd_raw(sk[0])
        await click(*softkey_page_next(sk))
        sh("xdotool", "mousemove", "5", "5")
        await asyncio.sleep(0.2)
        flipped = xwd_raw(sk[0]) != face
        screenshot("softkeyboard_punct", sk[0])
        c.take()
        await click(*softkey_digit_row(sk, 1))  # 「1」键位：标点面上是「！」
        sh("xdotool", "mousemove", "5", "5")
        got = c.take()
        check("软键盘：点 ▶ 翻到标点面，点「1」键帽上屏「！」",
              flipped and got == "！", f"翻面={flipped} 上屏={got!r}")
    eaten = await chord(c, 0x4B, 45, STATE_CTRL | STATE_SHIFT)
    gone = await wait_window("wind-softkeyboard", False)
    check("软键盘：再按 Ctrl+Shift+K 关闭", eaten is True and gone is not None and not gone[5],
          f"eaten={eaten} window={gone}")
    await c.type("nihao")
    await c.key(" ")
    got = c.take()
    check("软键盘：关闭后照常打字", got == "你好", f"上屏={got!r}")
    await c.ic.call_focus_out()
    await asyncio.sleep(0.2)


def settings_windows():
    """设置程序的顶层窗口（WM_CLASS 实例名 = 可执行文件名 wind_setting.bin，见 windui
    `x11.rs::wm_class`；e2e 里经包装脚本启动，真程序是那个软链）。"""
    ids = sh("xdotool", "search", "--classname", "wind_setting").split()
    return [i for i in ids if "IsViewable" in sh("xwininfo", "-id", i)]


def read_lines(path):
    try:
        with open(path, encoding="utf-8") as f:
            return f.read().splitlines()
    except FileNotFoundError:
        return []


async def wait_until(pred, timeout):
    deadline = asyncio.get_running_loop().time() + timeout
    while not pred():
        if asyncio.get_running_loop().time() >= deadline:
            return False
        await asyncio.sleep(0.1)
    return True


async def settings_cases(bus, im):
    """「从输入法打开设置」整条链：Ctrl+Shift+]（keys.open_settings 出厂值）→ addon 按键转发
    → 服务热键分派 open_settings → 下行扩展信封 settings.open → addon 拉起 WIND_INPUT_SETTING
    （e2e.sh 的包装脚本记下 argv 再换成真设置程序）→ 设置程序经控制 socket 连上服务。"""
    w = os.environ["W"]
    argv_log = os.path.join(w, "setting.argv")
    setting_log = os.path.join(os.environ["XDG_DATA_HOME"], "WindInput", "logs",
                               "wind_setting.1.log")
    c = await new_ctx(bus, im, "e2e-settings")

    async def open_settings_hotkey():
        # Shift 按住时 X 客户端送的是 braceright（0x7d），keycode 35 = ]。
        down = await c.ic.call_process_key_event(0x7D, 35, STATE_CTRL | STATE_SHIFT, False, 0)
        await c.ic.call_process_key_event(0x7D, 35, STATE_CTRL | STATE_SHIFT, True, 0)
        return down

    eaten = await open_settings_hotkey()
    check("设置：Ctrl+Shift+] 被输入法吃掉（热键命中 open_settings）", eaten is True,
          f"eaten={eaten}")
    launched = await wait_until(lambda: len(read_lines(argv_log)) >= 1, 10)
    check("设置：addon 收到 settings.open 后拉起设置程序", launched,
          f"{argv_log} 无记录（fcitx5 日志里找「启动设置程序」/「settings.open」）")
    args = read_lines(argv_log)
    check("设置：打开设置不带深链参数（默认页）", args[:1] == [""], f"argv={args}")
    mapped = await wait_until(lambda: len(settings_windows()) == 1, 20)
    wins = settings_windows()
    check("设置：设置窗口已映射", mapped, f"windows={wins}")
    if wins:
        # 映射早于首帧（首帧要等方案列表等数据回来）：轮询到有内容为止。
        shot = {}

        def painted():
            shot["path"], shot["n"] = window_has_content(wins[0], "settings_main")
            return shot["n"] > 8
        await wait_until(painted, 10)
        check("设置：窗口有渲染内容", shot["n"] > 8, f"颜色数={shot['n']} 截图={shot['path']}")
        print(f"      截图 {shot['path']}", flush=True)
    connected = await wait_until(
        lambda: any("apply_loaded 完成, connected=true" in l for l in read_lines(setting_log)), 15)
    check("设置：设置程序经控制 socket 连上服务", connected,
          f"{setting_log} 里没有 connected=true")

    # 已开着时再按一次：新进程只把 argv 转给首实例（windui 单实例），窗口不重复。
    await open_settings_hotkey()
    again = await wait_until(lambda: len(read_lines(argv_log)) >= 2, 10)
    forwarded = await wait_until(
        lambda: any("二次实例 argv" in l for l in read_lines(setting_log)), 10)
    await asyncio.sleep(0.5)
    check("设置：再按一次转交给已开的设置程序、不开第二个窗口",
          again and forwarded and len(settings_windows()) == 1,
          f"再次启动={again} 转交={forwarded} windows={settings_windows()}")

    subprocess.run(["pkill", "-f", "--", "^" + os.path.join(w, "svc", "wind_setting")])
    closed = await wait_until(lambda: not settings_windows(), 5)
    check("设置：设置程序退出后窗口消失", closed, f"windows={settings_windows()}")

    async def launched_again(label, before):
        """从 before 行起又拉起一次设置程序：argv 记录多一行（无深链参数）、窗口出现；随后关掉。"""
        ok = await wait_until(lambda: len(read_lines(argv_log)) > before, 10)
        args = read_lines(argv_log)[before:]
        mapped = await wait_until(lambda: len(settings_windows()) == 1, 20)
        check(f"设置：{label} → 拉起设置程序（默认页）", ok and args[:1] == [""] and mapped,
              f"拉起={ok} argv={args} 窗口={settings_windows()}")
        subprocess.run(["pkill", "-f", "--", "^" + os.path.join(w, "svc", "wind_setting")])
        await wait_until(lambda: not settings_windows(), 5)

    # 空闲入口：Fcitx5 状态区动作「清风输入法设置」（托盘菜单 / kimpanel 面板里那一项）。
    panel = await impanel(bus)
    watch = await kimpanel_watch(bus)
    await c.ic.call_focus_out()
    await c.ic.call_focus_in()  # 聚焦时 kimpanel 重报一遍状态区动作
    await asyncio.sleep(0.3)
    regs = [p for ps in watch.registered for p in ps]
    listed = any(p.startswith("/Fcitx/windinput-settings:清风输入法设置:") for p in regs)
    listed_menu = any(p.startswith("/Fcitx/windinput-menu:清风输入法菜单:") for p in regs)
    check("设置：状态区同时列出「清风输入法设置」与「清风输入法菜单」",
          listed and listed_menu, f"属性={regs[-6:]}")
    before = len(read_lines(argv_log))
    panel.trigger("/Fcitx/windinput-settings")
    await launched_again("状态区点「清风输入法设置」", before)

    # 状态区「清风输入法菜单」：主菜单弹在指针处（刚点托盘的地方），不在文本光标处。
    # 内容左上 = 指针，位图带阴影扩边，故按窗口左上留 30px 余量。
    await c.ic.call_set_cursor_rect(900, 600, 2, 20)  # 光标放远处：菜单若跟光标走就会被看出来
    sh("xdotool", "mousemove", "300", "120")
    panel.trigger("/Fcitx/windinput-menu")
    main = await wait_window("wind-menu-0", True)
    ok = main is not None and main[5]
    check("菜单：状态区「清风输入法菜单」弹出主菜单", ok, f"menu={main}")
    if ok:
        _, mx, my, _, _, _ = main
        check("菜单：状态区入口的主菜单在指针处（不跟文本光标）",
              0 <= 300 - mx <= 30 and 0 <= 120 - my <= 30, f"@({mx},{my})")
    await c.key("Escape")
    await wait_menus_gone()
    # 指针在屏幕右下角（托盘的典型位置）：菜单翻到指针左上方、整个留在屏内（1280x800）
    sh("xdotool", "mousemove", "1270", "790")
    panel.trigger("/Fcitx/windinput-menu")
    main = await wait_window("wind-menu-0", True)
    ok = (main is not None and main[5] and main[1] >= 0 and main[2] >= 0
          and main[1] + main[3] <= 1280 + 30 and main[2] + main[4] <= 800 + 30)
    check("菜单：指针在屏幕右下角时主菜单不出屏", ok, f"menu={main}")
    await c.key("Escape")
    await wait_menus_gone()
    sh("xdotool", "mousemove", "5", "5")

    # 系统输入法配置里点清风的「配置」：配置工具经 Controller1.GetConfig 取描述。整页只有一个
    # External 项时 fcitx5-configtool（5.1.6+）直接启动它，而不是弹一张空白配置页。
    intro = await bus.introspect(SERVICE, "/controller")
    ctl = bus.get_proxy_object(SERVICE, "/controller", intro).get_interface(
        "org.fcitx.Fcitx.Controller1")
    external = ""
    for uri in ("fcitx://config/inputmethod/windinput", "fcitx://config/addon/windinput"):
        try:
            _, desc = await ctl.call_get_config(uri)
        except Exception as e:  # noqa: BLE001 — 失败原因原样进断言信息
            desc, err = [], repr(e)
        else:
            err = ""
        opts = desc[0][1] if len(desc) == 1 else []
        only = opts[0] if len(opts) == 1 else None
        external = only[4]["External"].value if only and "External" in only[4] else ""
        check(f"配置：{uri} 是唯一一项 External、指向设置程序",
              only is not None and only[1] == "External"
              and external == os.environ.get("WIND_INPUT_SETTING"),
              f"desc={desc} {err}")
    # 照配置工具的做法把 External 当命令启动（QProcess::startDetached），确认它真能拉起来。
    if external:
        before = len(read_lines(argv_log))
        subprocess.Popen([external], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                         start_new_session=True)
        await launched_again("配置工具启动 External 命令", before)

    await c.ic.call_focus_out()
    await asyncio.sleep(0.2)


async def mode_icon_cases(bus, im):
    """托盘 / 面板图标随模式切换：引擎的 subModeIcon 经 kimpanel 模块报给面板。按键切换
    （Shift 单击 → STATUS_UPDATE 响应）与焦点进入（FocusGained → MODE_PUSH）两条来源；
    菜单切换（push 通道 STATE_PUSH）在 menu_cases 里验。对齐 Windows 语言栏：中文态按方案
    标签（e2e 用全拼，「拼」）、英文态「英」、大写锁定无论中英都是「A」。"""
    watch = await kimpanel_watch(bus)
    c = await new_ctx(bus, im, "e2e-mode-icon")
    zh = await wait_until(lambda: watch.icon() == ICON_PIN, 2)
    check("托盘图标：中文态按方案标签运行时渲染，全拼为「拼」、标签「拼」",
          zh and watch.im().endswith("label=拼"), f"im={watch.im()!r}")
    # 服务端每次构建状态都把中文 / 英文 / 大写三组一起备好（addon 本端判定大写时不必等帧）。
    for name in (ICON_PIN, ICON_EN, ICON_CAPS):
        files, why = tray_icon_files_ok(name)
        check(f"托盘图标：{name} 各尺寸 PNG 已写进用户图标目录", files, why)
    await c.tap_shift()
    en = await wait_until(lambda: watch.icon() == ICON_EN, 2)
    check("托盘图标：Shift 切英文 → 「英」、标签「英」",
          en and watch.im().endswith("label=英"), f"im={watch.im()!r}")
    # 换个文本框再回来：焦点进入时服务端回的 MODE_PUSH 仍是英文，图标不被焦点事件打回中文。
    d = await new_ctx(bus, im, "e2e-mode-icon-2")
    await d.ic.call_focus_out()
    await c.ic.call_focus_in()
    await asyncio.sleep(0.3)
    check("托盘图标：换焦点后仍是「英」", watch.icon() == ICON_EN,
          f"im={watch.im()!r}")

    # 大写锁定（英文态）：按下 + 松开 → 「A」；服务端同步镜像并回 STATUS_CAPS_LOCK。
    await c.tap_caps_lock(False)
    caps = await wait_until(lambda: watch.icon() == ICON_CAPS, 2)
    check("托盘图标：英文态按 CapsLock → 「A」图标、标签「A」",
          caps and watch.im().endswith("label=A"), f"im={watch.im()!r}")
    # 大写锁定下打字母：服务端认得大写（英文态本就直通，这里只验不被吞、图标不动）。
    eaten = await c.key("a", STATE_LOCK)
    check("大写锁定：英文态字母直通、图标仍是「A」",
          eaten is False and watch.icon() == ICON_CAPS, f"eaten={eaten} im={watch.im()!r}")
    # 换焦点：MODE_PUSH 不带大写锁定位，不能把「A」打回「英」。
    await c.ic.call_focus_out()
    await d.ic.call_focus_in()
    await asyncio.sleep(0.3)
    check("托盘图标：大写锁定时换焦点不回退", watch.icon() == ICON_CAPS,
          f"im={watch.im()!r}")
    await d.ic.call_focus_out()
    await c.ic.call_focus_in()
    await asyncio.sleep(0.3)
    # Shift 单击（带着 Lock 位）：切到中文，但大写锁定仍开着 → 仍是「A」。修的是一个真坑：
    # 修饰键单击那一帧曾发 toggles=0，服务端据此把大写锁定镜像校准成「关」。
    await c.tap_shift(STATE_LOCK)
    await asyncio.sleep(0.3)
    check("托盘图标：大写锁定时 Shift 切中文，仍是「A」（中文态也显示「A」）",
          watch.icon() == ICON_CAPS, f"im={watch.im()!r}")
    # 中文态 + 大写锁定：字母按大写直出，不进组字。
    eaten = await c.key("a", STATE_LOCK)
    await asyncio.sleep(0.2)
    check("大写锁定：中文态字母不进组字", c.preedit == "", f"eaten={eaten} preedit={c.preedit!r}")
    c.take()
    # 再按一次 CapsLock：回到当前模式的图标（中文 → 「拼」）。
    await c.tap_caps_lock(True)
    back = await wait_until(lambda: watch.icon() == ICON_PIN, 2)
    check("托盘图标：再按 CapsLock → 回到「拼」",
          back and watch.im().endswith("label=拼"), f"im={watch.im()!r}")
    typed = await c.type("ni")
    check("大写锁定关掉后中文组字恢复", typed == [True, True] and c.preedit != "",
          f"eaten={typed} preedit={c.preedit!r}")
    await c.key("Escape")
    c.take()
    # 在别处开了大写锁定（本引擎没收到 Caps_Lock）：第一个带 Lock 位的键就校准回来。
    await c.key("Left", STATE_LOCK)
    cal = await wait_until(lambda: watch.icon() == ICON_CAPS, 2)
    check("托盘图标：别处开的大写锁定，第一个带 Lock 位的键就校准成「A」", cal,
          f"im={watch.im()!r}")
    await c.key("Left")
    uncal = await wait_until(lambda: watch.icon() == ICON_PIN, 2)
    check("托盘图标：Lock 位消失即回到「拼」", uncal, f"im={watch.im()!r}")

    await c.tap_shift()
    en2 = await wait_until(lambda: watch.icon() == ICON_EN, 2)
    await c.tap_shift()
    back = await wait_until(lambda: watch.icon() == ICON_PIN, 2)
    check("托盘图标：再按 Shift 切英 / 切回 → 「英」/「拼」", en2 and back,
          f"im={watch.im()!r}")
    # 切方案（Ctrl+Shift+E，全拼 ⇄ 五笔）：中英不变、方案标签变，图标跟着换。
    for want, label in ((ICON_WU, "五"), (ICON_PIN, "拼")):
        await c.ic.call_process_key_event(0x45, 26, STATE_CTRL | STATE_SHIFT, False, 0)
        await c.ic.call_process_key_event(0x45, 26, STATE_CTRL | STATE_SHIFT, True, 0)
        ok = await wait_until(lambda: watch.icon() == want, 2)
        files, why = tray_icon_files_ok(want)
        check(f"托盘图标：Ctrl+Shift+E 切方案 → 「{label}」的运行时图标（{want}，各尺寸 PNG 在盘上）",
              ok and files and watch.im().endswith(f"label={label}"),
              f"im={watch.im()!r} {why}")
    check("托盘图标：切方案前后图标名不同", ICON_WU != ICON_PIN)
    await c.ic.call_focus_out()
    await asyncio.sleep(0.2)

# ── 异形皮肤（设计 theme-skin-animation.md §17）──────────────────────

def png_bytes(w, h, rgba, frames=None):
    """程序生成 PNG（RGBA8）；`frames` = [(rgba, 毫秒), …] 时生成 APNG（循环播放）。"""
    import struct
    import zlib

    def chunk(tag, body):
        return struct.pack(">I", len(body)) + tag + body + struct.pack(">I", zlib.crc32(tag + body))

    def idat(px):
        raw = bytearray()
        for y in range(h):
            raw.append(0)
            raw.extend(px * w)
        return zlib.compress(bytes(raw), 6)

    out = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
    if not frames:
        return out + chunk(b"IDAT", idat(rgba)) + chunk(b"IEND", b"")
    out += chunk(b"acTL", struct.pack(">II", len(frames), 0))
    seq = 0
    for i, (px, ms) in enumerate(frames):
        out += chunk(b"fcTL", struct.pack(">IIIIIHHBB", seq, w, h, 0, 0, ms, 1000, 0, 0))
        seq += 1
        if i == 0:
            out += chunk(b"IDAT", idat(px))
        else:
            out += chunk(b"fdAT", struct.pack(">I", seq) + idat(px))
            seq += 1
    return out + chunk(b"IEND", b"")


BLOCK_IDLE = bytes([0, 0, 0, 255])
BLOCK_HIT = bytes([255, 0, 255, 255])
SKIN_COLORS = [bytes([255, 0, 0, 255]), bytes([0, 255, 0, 255]), bytes([0, 0, 255, 255]),
               bytes([255, 255, 0, 255])]


def write_skin_theme(name):
    """60×40 白窗体；右上外侧 24×24 穿透装饰（四色 APNG，每帧 100ms，z = 1）；左上外侧 24×24
    block 装饰（两帧：常驻黑，点中切品红）。主题放用户层 themes/<name>/。"""
    d = os.path.join(os.environ["W"], "xdg", "config", "WindInput", "themes", name)
    os.makedirs(d, exist_ok=True)
    open(os.path.join(d, "frame.png"), "wb").write(png_bytes(60, 40, bytes([255, 255, 255, 255])))
    open(os.path.join(d, "block.png"), "wb").write(
        png_bytes(24, 24, None, [(BLOCK_IDLE, 100), (BLOCK_HIT, 100)]))
    open(os.path.join(d, "pass.png"), "wb").write(
        png_bytes(24, 24, None, [(c, 100) for c in SKIN_COLORS]))
    open(os.path.join(d, "theme.toml"), "w").write(f"""[meta]
name = "{name}"
[skin]
density = 1
[skin.horizontal.frame]
image = "frame.png"
stretch_x = [[20, 40]]
stretch_y = [[15, 25]]
fill = "stretch"
[skin.horizontal.content]
left = 6
top = 6
right = 6
bottom = 6
[[skin.horizontal.decor]]
image = "pass.png"
anchor = "frame.top_right"
pivot = "bottom_left"
z = 1
hit = "pass"
[[skin.horizontal.decor]]
image = "block.png"
anchor = "frame.top_left"
pivot = "bottom_right"
hit = "block"
[skin.horizontal.decor.states]
idle = {{ frames = "0-0" }}
hit = {{ frames = "1-1" }}
[skin.horizontal.decor.on]
click = {{ state = "hit" }}
""")


def pointer_window(x, y):
    """指针挪到 (x, y)，返回指针下的顶层窗口 id（十进制串；根窗口为空串）。输入区（XShape
    SK_INPUT）外的点会落到下面的窗口。"""
    out = sh("xdotool", "mousemove", str(x), str(y), "getmouselocation", "--shell")
    for line in out.splitlines():
        if line.startswith("WINDOW="):
            return line.split("=", 1)[1]
    return ""


async def skin_cases(bus, im):
    """异形皮肤：穿透装饰不接鼠标、block 装饰与窗体接；透明外扩区不接；动图装饰按帧推。"""
    write_skin_theme("e2e-skin")
    await set_config("ui.theme.name", "e2e-skin")
    c = await new_ctx(bus, im, "e2e-skin")
    await c.ic.call_set_cursor_rect(300, 300, 2, 20)
    await c.type("nihao")
    win = await wait_window("wind-candidate", True)
    check("皮肤：候选窗出现", win is not None and win[5], f"window={win}")
    if not win or not win[5]:
        return
    wid, x, y, w, h, _ = win
    print(f"  皮肤候选窗：{w}x{h} @ ({x},{y})", flush=True)
    await asyncio.sleep(0.3)  # 等首帧贴完、输入区设好（CMD_CANDIDATE_SKIN 先于 CMD_HOST_RENDER_FRAME）
    # 两块装饰都伸出窗体左上 / 右上 24px：窗口上缘 24px 是外扩区。
    on_pass = pointer_window(x + w - 12, y + 12)
    on_block = pointer_window(x + 12, y + 12)
    on_gap = pointer_window(x + w // 2, y + 6)
    on_frame = pointer_window(x + w // 2, y + 24 + (h - 24) // 2)
    check("皮肤：穿透装饰上指针落到下面（不是候选窗）", on_pass != wid, f"pass={on_pass} cand={wid}")
    check("皮肤：block 装饰接鼠标", on_block == wid, f"block={on_block} cand={wid}")
    check("皮肤：外扩区的透明处不接鼠标", on_gap != wid, f"gap={on_gap} cand={wid}")
    check("皮肤：窗体接鼠标", on_frame == wid, f"frame={on_frame} cand={wid}")
    # 动图装饰：显示期间按帧换色（服务端推 FLAG_ANIM_FRAME 帧）。
    seen = set()
    for _ in range(12):
        _, px = screenshot("skin_anim", wid)
        seen.add(px(w - 12, 12))
        await asyncio.sleep(0.1)
    screenshot("skin_candidate", wid)
    check("皮肤：穿透装饰画在候选窗里且按帧换色", len(seen) >= 2, f"采到 {seen}")
    # 点 block 装饰：宿主报 skin.decor_click，服务端触发它的 click 事件（切到「hit」状态，品红）；
    # 不选词、候选窗不收。
    block_before = px(12, 12)
    sh("xdotool", "mousemove", str(x + 12), str(y + 12), "click", "1")
    await asyncio.sleep(0.4)
    _, px2 = screenshot("skin_decor_click", wid)
    check("皮肤：点 block 装饰触发它的 click 事件", px2(12, 12) != block_before,
          f"点前 {block_before} 点后 {px2(12, 12)}")
    shown = candidate_window()
    check("皮肤：点装饰不上屏、候选窗不收", c.take() == "" and bool(shown and shown[5]),
          f"window={shown}")
    # 打字续帧时窗口不动、穿透照旧（输入区随新帧重设）。
    await c.type("n")
    await asyncio.sleep(0.4)
    win2 = candidate_window()
    on_pass2 = pointer_window(win2[1] + win2[3] - 12, win2[2] + 12) if win2 else ""
    check("皮肤：换内容后穿透装饰仍不接鼠标", win2 is not None and on_pass2 != win2[0],
          f"window={win2} pass={on_pass2}")
    sh("xdotool", "mousemove", "5", "5")
    await c.key("Escape")
    c.take()
    # 换回普通主题：输入区复位（整窗接鼠标，窗口右上角那一点又是候选窗）。
    await set_config("ui.theme.name", "default")
    await c.type("nihao")
    win3 = await wait_window("wind-candidate", True)
    await asyncio.sleep(0.3)
    if win3:
        inside = pointer_window(win3[1] + win3[3] // 2, win3[2] + win3[4] // 2)
        check("皮肤：换回普通主题后候选窗照常接鼠标", inside == win3[0],
              f"inside={inside} cand={win3[0]}")
    sh("xdotool", "mousemove", "5", "5")
    await c.key("Escape")
    c.take()
    await c.ic.call_focus_out()
    await asyncio.sleep(0.2)


def write_drag_skin_theme(name):
    """60×40 白窗体；左上外侧 24×24 block 装饰（黑），右上外侧 24×24 drag 装饰（绿）。"""
    d = os.path.join(os.environ["W"], "xdg", "config", "WindInput", "themes", name)
    os.makedirs(d, exist_ok=True)
    open(os.path.join(d, "frame.png"), "wb").write(png_bytes(60, 40, bytes([255, 255, 255, 255])))
    open(os.path.join(d, "block.png"), "wb").write(png_bytes(24, 24, BLOCK_IDLE))
    open(os.path.join(d, "drag.png"), "wb").write(png_bytes(24, 24, bytes([0, 255, 0, 255])))
    open(os.path.join(d, "theme.toml"), "w").write(f"""[meta]
name = "{name}"
[skin]
density = 1
[skin.horizontal.frame]
image = "frame.png"
stretch_x = [[20, 40]]
stretch_y = [[15, 25]]
fill = "stretch"
[skin.horizontal.content]
left = 6
top = 6
right = 6
bottom = 6
[[skin.horizontal.decor]]
image = "block.png"
anchor = "frame.top_left"
pivot = "bottom_right"
hit = "block"
[[skin.horizontal.decor]]
image = "drag.png"
anchor = "frame.top_right"
pivot = "bottom_left"
hit = "drag"
""")


def candidate_config_int(key):
    """用户 config.toml 里 `[ui.candidate]` 段的整数键（状态气泡段有同名键，不能按行首直接搜）。"""
    import re
    m = re.search(r"^\[ui\.candidate\]\s*$(.*?)(?=^\[|\Z)", user_config(), re.M | re.S)
    return config_int(m.group(1), key) if m else None


def candidate_drag_reports():
    return sum("候选窗拖动松手" in l for l in fcitx_log_lines())


async def candidate_drag_cases(bus, im):
    """候选窗本端拖动（X11，设计 §17.7）：空白处与 drag 装饰起拖、block 装饰不拖；本次组合内续帧
    按拖动锚摆，收起后回到光标旁；固定位置模式下 pos.candidate.query 应答与松手上报都是内容左上，
    重显不漂。"""
    write_drag_skin_theme("e2e-skin-drag")
    await set_config("ui.theme.name", "e2e-skin-drag")
    c = await new_ctx(bus, im, "e2e-cand-drag")
    await c.ic.call_set_cursor_rect(300, 300, 2, 20)
    sh("xdotool", "mousemove", "5", "5")
    await c.type("nihao")
    w0 = await wait_window("wind-candidate", True)
    check("拖动：候选窗出现", w0 is not None and w0[5], f"window={w0}")
    if not w0 or not w0[5]:
        return
    await asyncio.sleep(0.3)
    wid, x0, y0, _, _, _ = w0
    # 窗体左上内边距里（两块装饰都伸出窗体上方 24px、左侧 24px）：空白处。
    bx, by = 24 + 2, 24 + 2

    async def moved_by(px, py, dx, dy):
        before = candidate_window()
        await drag(before[1] + px, before[2] + py, dx, dy)
        after = candidate_window()
        return before, after, (after[1] - before[1], after[2] - before[2])

    n0 = candidate_drag_reports()
    _, w1, d = await moved_by(bx, by, 80, 50)
    check("拖动：按住空白处拖动，窗口跟手", d == (80, 50), f"位移={d}")
    check("拖动：松手上报 pos.candidate", candidate_drag_reports() == n0 + 1,
          f"上报 {candidate_drag_reports() - n0} 次")
    check("拖动：没选词", c.take() == "")
    await c.type("n")
    await asyncio.sleep(0.4)
    w2 = candidate_window()
    check("拖动：组合内续帧按拖动锚摆（不弹回光标旁）", (w2[1], w2[2]) == (w1[1], w1[2]),
          f"拖后={w1} 续帧={w2}")
    _, _, d = await moved_by(12, 12, 60, 40)
    check("拖动：block 装饰上按住拖不动", d == (0, 0), f"位移={d}")
    _, _, d = await moved_by(w2[3] - 12, 12, -40, 30)
    check("拖动：drag 装饰上能拖", d == (-40, 30), f"位移={d}")
    # 没过阈值的松开是点击，不是拖动：窗口不动、不报。
    n1 = candidate_drag_reports()
    _, _, d = await moved_by(bx, by, 3, 3)
    check("拖动：阈值内松开不算拖", d == (0, 0) and candidate_drag_reports() == n1,
          f"位移={d} 上报 {candidate_drag_reports() - n1} 次")
    cfg = candidate_config_int("custom_x")
    check("拖动：跟随光标模式下不落盘", cfg in (None, 0), f"custom_x={cfg}")
    await c.key("Escape")
    c.take()
    await c.type("nihao")
    await asyncio.sleep(0.4)
    w3 = await wait_window("wind-candidate", True)
    check("拖动：收起后再输入回到光标旁", w3 is not None and (w3[1], w3[2]) == (x0, y0),
          f"首显={w0} 再显={w3}")

    # 固定位置：显示中切过去 → 服务端问 pos.candidate.query → 以当前位置落盘（内容左上）。
    await set_config("ui.candidate.position_mode", "fixed")
    pinned = await wait_until(lambda: candidate_config_int("custom_x") is not None, 3)
    fx, fy = candidate_config_int("custom_x"), candidate_config_int("custom_y")
    off = (fx - w3[1], fy - w3[2]) if pinned else None
    check("拖动：切固定位置时应答 pos.candidate.query，落盘内容左上", pinned and off[0] >= 24
          and off[1] >= 24, f"custom=({fx},{fy}) 窗口=({w3[1]},{w3[2]})")
    if not pinned:
        return
    _, w4, d = await moved_by(bx, by, -70, 40)
    ok = await wait_until(lambda: (candidate_config_int("custom_x"),
                                   candidate_config_int("custom_y")) == (fx - 70, fy + 40), 3)
    check("拖动：固定位置模式下松手落盘（custom_x/y 随落点更新）", d == (-70, 40) and ok,
          f"位移={d} custom=({candidate_config_int('custom_x')},{candidate_config_int('custom_y')})")
    await c.key("Escape")
    c.take()
    await c.type("nihao")
    await asyncio.sleep(0.4)
    w5 = await wait_window("wind-candidate", True)
    check("拖动：固定位置重显停在拖到的地方（不漂一截装饰外扩）",
          w5 is not None and (w5[1], w5[2]) == (w4[1], w4[2]), f"拖后={w4} 重显={w5}")
    sh("xdotool", "mousemove", "5", "5")
    await c.key("Escape")
    c.take()
    await set_config("ui.candidate.position_mode", "follow_caret")
    await set_config("ui.theme.name", "default")
    await c.ic.call_focus_out()
    await asyncio.sleep(0.2)


NS_GNOME = "org.gnome.desktop.interface"
NS_APPEARANCE = "org.freedesktop.appearance"
NS_KDE = "org.kde.kdeglobals.KDE"


class FakePortal:
    """扮演设置门户（`org.freedesktop.portal.Settings`）。fcitx5 早于它起来：addon 首读落空，之后靠
    门户上线时的重读、SettingChanged 信号或服务重启时的重读拿到值。

    几处故意不规矩，验 addon 的容错（`DesktopMotion`）：
    - `enable-animations`：第一次 `Read` 不回（addon 等到 NoReply 再重试），之后每次晚 3 秒才回（读超时
      若还是 2 秒，重试也全落空）；
    - KDE 的 `AnimationDurationFactor`：回包是裸 `d`，不是约定的 `v`（addon 不能去 peek，否则 fcitx5 abort）；
    - 其余键答 NotFound。"""

    def __init__(self, bus):
        from dbus_next.service import ServiceInterface, signal

        class Iface(ServiceInterface):
            def __init__(self):
                super().__init__("org.freedesktop.portal.Settings")

            @signal()
            def SettingChanged(self, namespace, key, value) -> "ssv":  # noqa: N802,F821
                return [namespace, key, value]

        self.animations = True
        self.reads = 0  # 收到的 enable-animations 读取次数
        self.iface = Iface()
        self.bus = bus
        bus.add_message_handler(self._on_message)

    def _on_message(self, msg):
        from dbus_next import Message, MessageType

        if (msg.message_type != MessageType.METHOD_CALL or msg.member != "Read"
                or msg.interface != "org.freedesktop.portal.Settings"):
            return None
        ns, key = msg.body
        if (ns, key) == (NS_GNOME, "enable-animations"):
            self.reads += 1
            if self.reads == 1:
                return True  # 不回：addon 那头等到 NoReply
            # 旧接口 Read 的回包套两层 variant（ReadOne 才是一层）；值取回包那一刻的
            asyncio.get_running_loop().call_later(3.0, lambda: self.bus.send(Message.new_method_return(
                msg, "v", [Variant("v", Variant("b", self.animations))])))
            return True
        if (ns, key) == (NS_KDE, "AnimationDurationFactor"):
            return Message.new_method_return(msg, "d", [0.0])
        return Message.new_error(msg, "org.freedesktop.portal.Error.NotFound", "没有这个键")

    async def start(self):
        """接过 `org.freedesktop.portal.Desktop`。私有会话总线照样按本机的 .service 文件激活了真门户
        （addon 首读时；它在这条总线上没有后端，一律答 UnknownMethod）：杀掉这个私有总线的子进程，
        排队的名字随之落到本端。返回是否拿到。"""
        from dbus_next.constants import RequestNameReply

        name = "org.freedesktop.portal.Desktop"
        self.bus.export("/org/freedesktop/portal/desktop", self.iface)
        if await self.bus.request_name(name) == RequestNameReply.PRIMARY_OWNER:
            return True
        daemon = await self._daemon()
        owner = await daemon.call_get_name_owner(name)
        pid = await daemon.call_get_connection_unix_process_id(owner)
        print(f"  私有总线上已有门户（pid {pid}），结束它", flush=True)
        os.kill(pid, 15)
        for _ in range(50):
            await asyncio.sleep(0.1)
            if await daemon.call_get_name_owner(name) == self.bus.unique_name:
                return True
        return False

    async def _daemon(self):
        intro = await self.bus.introspect("org.freedesktop.DBus", "/org/freedesktop/DBus")
        return self.bus.get_proxy_object("org.freedesktop.DBus", "/org/freedesktop/DBus",
                                         intro).get_interface("org.freedesktop.DBus")

    async def fcitx_alive(self):
        return await (await self._daemon()).call_name_has_owner("org.fcitx.Fcitx5")

    def changed(self, ns, key, value):
        self.iface.SettingChanged(ns, key, value)

    def changed_raw(self, ns, key, sig, value):
        """发一条签名不合约定的 SettingChanged（第三个参数不是 variant）。"""
        from dbus_next import Message, MessageType

        self.bus.send(Message(message_type=MessageType.SIGNAL, path="/org/freedesktop/portal/desktop",
                              interface="org.freedesktop.portal.Settings", member="SettingChanged",
                              signature="ss" + sig, body=[ns, key, value]))

    def set(self, animations):
        self.animations = animations
        self.changed(NS_GNOME, "enable-animations", Variant("b", animations))


async def skin_anim_colors(c):
    """打字显示皮肤候选窗，1 秒内采右上穿透装饰（四色动图）的像素，返回采到的颜色集合，再收起。"""
    await c.type("nihao")
    win = await wait_window("wind-candidate", True)
    seen = set()
    if win and win[5]:
        await asyncio.sleep(0.3)
        for _ in range(10):
            _, px = screenshot("skin_motion", win[0])
            seen.add(px(win[3] - 12, 12))
            await asyncio.sleep(0.1)
    await c.key("Escape")
    c.take()
    await asyncio.sleep(0.2)
    return seen


async def skin_motion_settled(c, frozen, tries=8):
    """多显示几次，直到动图装饰冻结（`frozen`）或还在动；返回最后一次采到的颜色集合。"""
    seen = set()
    for _ in range(tries):
        seen = await skin_anim_colors(c)
        if (len(seen) == 1) == frozen:
            break
    return seen


async def skin_motion_cases(bus, im):
    """系统「减少动态效果」（§17.1）：addon 读设置门户报 host.reduce_motion，服务端冻结皮肤动画。"""
    await set_config("ui.theme.name", "e2e-skin")
    portal = FakePortal(bus)
    portal.animations = False
    check("减少动态效果：扮演设置门户", await portal.start())
    c = await new_ctx(bus, im, "e2e-skin-motion")
    await c.ic.call_set_cursor_rect(300, 300, 2, 20)
    # 门户上线 → addon 重读一轮：KDE 键回裸 d（不能 abort）；签名不对的信号也不能 abort。
    portal.changed_raw(NS_KDE, "AnimationDurationFactor", "d", 0.0)
    await asyncio.sleep(1.0)
    check("减少动态效果：门户回包 / 信号不是 variant 时 fcitx5 不退出", await portal.fcitx_alive())
    # enable-animations 首读不回、重试晚 3 秒才回：5 秒超时 + 重试才拿得到「关动画」。
    frozen = await skin_motion_settled(c, True, tries=10)
    check("减少动态效果：门户上线后重读，首读没回音时重试拿到值", len(frozen) == 1
          and portal.reads >= 2, f"采到 {frozen}，读了 {portal.reads} 次")
    portal.set(True)
    await asyncio.sleep(0.3)
    moving = await skin_anim_colors(c)
    check("减少动态效果：门户报开动画后皮肤动画恢复", len(moving) >= 2, f"采到 {moving}")
    portal.set(False)
    await asyncio.sleep(0.3)
    frozen = await skin_anim_colors(c)
    check("减少动态效果：门户报 GNOME 关动画后皮肤动画冻结", len(frozen) == 1, f"采到 {frozen}")
    # reduced-motion 优先于 GNOME 的键：u = 0 → 不减少（压过上面的关动画），s "1" → 减少。
    portal.changed(NS_APPEARANCE, "reduced-motion", Variant("u", 0))
    await asyncio.sleep(0.3)
    moving = await skin_anim_colors(c)
    check("减少动态效果：reduced-motion（u）= 0 压过 GNOME 关动画", len(moving) >= 2, f"采到 {moving}")
    portal.changed(NS_APPEARANCE, "reduced-motion", Variant("s", "1"))
    await asyncio.sleep(0.3)
    frozen = await skin_anim_colors(c)
    check("减少动态效果：reduced-motion（s）= \"1\" 冻结", len(frozen) == 1, f"采到 {frozen}")
    portal.changed(NS_APPEARANCE, "reduced-motion", Variant("s", "0"))
    await asyncio.sleep(0.3)
    restart = os.environ.get("WIND_E2E_RESTART")
    if restart:
        # 不发信号：新服务没有这个值，只能靠 addon 在 SERVICE_READY 时向门户重读（Read）再报。门户不认
        # reduced-motion（NotFound）：addon 得丢掉上面信号给的 0，否则它压过 GNOME 的关动画、仍在动。
        portal.animations = False
        rc = await asyncio.to_thread(subprocess.run, [restart])
        await c.ic.call_focus_in()
        ready = await wait_ready(c)
        # 新服务的 SERVICE_READY 晚于首次按键连上（约 1 秒），重读的回包晚 3 秒；值在下一次显示时生效。
        again = await skin_motion_settled(c, True)
        check("减少动态效果：服务重启后 addon 重读门户再报，读失败的键不留旧值", rc.returncode == 0
              and ready and len(again) == 1, f"rc={rc.returncode} ready={ready} 采到 {again}")
        portal.set(True)
    await set_config("ui.theme.name", "default")
    await c.ic.call_focus_out()
    await asyncio.sleep(0.2)


async def get_config(key):
    """经 `wind_input config get` 读在线服务的配置值（读不到返回空串）。"""
    svc = os.path.join(os.environ["W"], "svc", "wind_input")
    r = await asyncio.to_thread(subprocess.run, [svc, "config", "get", key],
                                capture_output=True, text=True)
    return r.stdout.strip() if r.returncode == 0 else ""


async def set_config(key, value):
    """经 `wind_input config set` 改在线服务的配置（RPC 热重载，不重启服务）。"""
    svc = os.path.join(os.environ["W"], "svc", "wind_input")
    r = await asyncio.to_thread(subprocess.run, [svc, "config", "set", key, value],
                                capture_output=True, text=True)
    await asyncio.sleep(0.3)
    return r


async def grid_cases(bus, im):
    """候选展开（卷轴）：开关打开后按 `=` 原地展开成多行网格；鼠标点第 3 行的格子，上屏的
    与键盘走到同一格（↓↓ + 空格）的是同一个词。

    展开态的命中下标是网格平铺下标，宿主原样回传、由服务端换算——这条把这个约定钉在真实
    Fcitx5 宿主上（Windows / macOS 宿主同样不解释下标）。"""
    r = await set_config("ui.candidate.grid_expand", "true")
    check("展开：config set grid_expand=true", r.returncode == 0,
          f"rc={r.returncode} {r.stderr.strip()[:200]}")
    # 下面拿键盘上屏的词当鼠标那一下的对照：调频开着的话，第一次上屏就把那个词提前了，
    # 第二次同一格已经换了人。
    await set_config("schema.pinyin.frequency.enabled", "false")
    c = await new_ctx(bus, im, "e2e-grid")
    await c.ic.call_set_cursor_rect(300, 200, 2, 20)

    # 键盘：展开 → ↓↓ 到第 3 行 → 空格，记下上屏的词。
    await c.type("yi")
    await asyncio.sleep(0.3)
    one = candidate_window()
    await c.key("equal")
    await asyncio.sleep(0.4)
    grid = candidate_window()
    tall = bool(one and grid and one[5] and grid[5] and grid[4] >= one[4] * 2.5)
    check("展开：按 = 后候选窗原地变高（多行网格）", tall, f"单行={one} 展开={grid}")
    if grid and grid[5]:
        screenshot("candidate_grid", grid[0])
    await c.key("Down")
    await c.key("Down")
    c.take()
    await c.key(" ")
    await asyncio.sleep(0.3)
    by_key = c.take()
    check("展开：↓↓ + 空格上屏第 3 行的词", by_key != "", "没有上屏")

    # 鼠标：同一个位置点下去。行高按「展开后比单行多出的高度 / 5」估——多出来的是 4 行候选
    # 加独立成行的翻页栏，与候选行等高。
    await c.type("yi")
    await asyncio.sleep(0.3)
    await c.key("equal")
    await asyncio.sleep(0.4)
    win = candidate_window()
    by_mouse = ""
    if one and win and win[5]:
        _, x, y, _w, h, _ = win
        row_h = (h - one[4]) / 5
        pad_t = (one[4] - row_h) / 2
        cy = int(y + pad_t + 2.5 * row_h)
        sh("xdotool", "mousemove", str(x + 30), str(cy), "click", "1")
        await asyncio.sleep(0.4)
        by_mouse = c.take()
    check("展开：鼠标点第 3 行首格 = 键盘走到的同一格", by_mouse != "" and by_mouse == by_key,
          f"鼠标={by_mouse!r} 键盘={by_key!r}")
    sh("xdotool", "mousemove", "5", "5")

    # 悬停提示：展开态停靠在整个候选窗外侧的顶端，不盖住任何格子；悬停第 3 行也不跟着下移。
    await c.type("yi")
    await asyncio.sleep(0.3)
    await c.key("equal")
    await asyncio.sleep(0.4)
    win = candidate_window()
    tip = None
    if one and win and win[5]:
        _, x, y, _w, h, _ = win
        row_h = (h - one[4]) / 5
        cy = int(y + (one[4] - row_h) / 2 + 2.5 * row_h)
        for dx in (30, 60, 90):
            sh("xdotool", "mousemove", str(x + dx), str(cy))
            tip = await wait_window("wind-tooltip", True, timeout=0.8)
            if tip and tip[5]:
                break
    shown = bool(tip and tip[5])
    check("展开：悬停格子后 tooltip 出现", shown, f"tooltip={tip} candidate={win}")
    if shown:
        _, tx, ty, tw, _th, _ = tip
        outside = tx >= win[1] + win[3] or tx + tw <= win[1]
        check("展开：tooltip 在候选窗外侧、不盖格子", outside,
              f"tooltip x={tx}..{tx + tw} 候选窗 x={win[1]}..{win[1] + win[3]}")
        check("展开：tooltip 停靠在候选窗顶端、不随悬停行下移", abs(ty - win[2]) <= row_h,
              f"tooltip y={ty} 候选窗 y={win[2]} 行高≈{row_h:.0f}")
        screenshot("grid_tooltip_screen")
    sh("xdotool", "mousemove", "5", "5")
    await c.key("Escape")
    await c.key("Escape")
    await asyncio.sleep(0.3)
    c.take()

    # 分类标签：Tab 切到「单字」，窗口高度不变（标签栏恒在），数字键在筛过的列表里选。
    # 用 yi：单字一选即上屏（shijian 里选单字只消耗 shi，按分步转换留在组合区）。
    await c.type("yi")
    await asyncio.sleep(0.3)
    await c.key("equal")
    await asyncio.sleep(0.4)
    all_win = candidate_window()
    await c.key("Tab")
    await asyncio.sleep(0.4)
    single_win = candidate_window()
    if single_win and single_win[5]:
        screenshot("candidate_grid_single", single_win[0])
    check("展开：Tab 切分类后候选窗高度不变",
          bool(all_win and single_win and single_win[5] and single_win[4] == all_win[4]),
          f"全部={all_win} 单字={single_win}")
    c.take()
    await c.key("1")
    await asyncio.sleep(0.3)
    got = c.take()
    check("展开：「单字」里按 1 上屏一个字", len(got) == 1, f"上屏={got!r}")
    await c.key("Escape")
    await c.key("Escape")
    await asyncio.sleep(0.2)
    c.take()

    # Esc 两段式：先收起（候选窗回到单行高度），再按才取消。
    await c.type("yi")
    await asyncio.sleep(0.3)
    await c.key("equal")
    await asyncio.sleep(0.3)
    await c.key("Escape")
    await asyncio.sleep(0.3)
    back = candidate_window()
    check("展开：Esc 先收起、候选窗仍在且回到单行高度",
          bool(one and back and back[5] and back[4] == one[4]), f"单行={one} 收起后={back}")
    await c.key("Escape")
    await asyncio.sleep(0.3)
    gone = candidate_window()
    check("展开：再按 Esc 取消输入", gone is not None and not gone[5], f"window={gone}")
    c.take()

    await set_config("schema.pinyin.frequency.enabled", "true")
    r = await set_config("ui.candidate.grid_expand", "false")
    check("展开：config set grid_expand=false（复原）", r.returncode == 0,
          f"rc={r.returncode} {r.stderr.strip()[:200]}")


async def preedit_display_cases(bus, im):
    """编码显示在候选窗里（非嵌入，`preedit_display = candidate_top`）：服务端照旧给宿主一段单
    空格的占位组合（Windows 靠它取光标坐标），addon 不把它写进应用——应用里既看不到编码、也
    不会多出一格空白把光标推走；上屏后文本里没有多余空格。嵌入模式（出厂）预编辑照常。"""
    r = await set_config("ui.candidate.preedit_display", "candidate_top")
    check("非嵌入：config set preedit_display=candidate_top", r.returncode == 0,
          f"rc={r.returncode} {r.stderr.strip()[:200]}")
    c = await new_ctx(bus, im, "e2e-preedit")
    await c.ic.call_set_cursor_rect(300, 400, 2, 20)
    c.preedits.clear()
    eaten = await c.type("nihao")
    await asyncio.sleep(0.3)
    seen = list(c.preedits)
    check("非嵌入：组字时应用收到的预编辑恒为空（无占位空格）",
          all(eaten) and all(p == "" for p in seen), f"eaten={eaten} 预编辑={seen}")
    if os.environ.get("DISPLAY"):
        win = candidate_window()
        ok = win is not None and win[5]
        check("非嵌入：候选窗出现、左上角落在光标下方附近",
              ok and 240 <= win[1] <= 330 and 400 <= win[2] <= 470, f"window={win}")
        if ok:
            path, _ = screenshot("candidate_preedit_top", win[0])
            print(f"  非嵌入候选窗截图：{path}", flush=True)
    await c.key(" ")
    got = c.take()
    check("非嵌入：上屏「你好」、没有多余空格", got == "你好", f"上屏={got!r}")
    # 首键即上屏的路径（数字选词）同样不带占位。
    await c.type("shi")
    await c.key("1")
    got = c.take()
    check("非嵌入：数字选词上屏不带空格", got != "" and " " not in got, f"上屏={got!r}")

    r = await set_config("ui.candidate.preedit_display", "app_inline")
    c.preedits.clear()
    await c.type("nihao")
    shown = c.preedit
    await c.key(" ")
    got = c.take()
    check("嵌入（复原 app_inline）：预编辑是编码本身、上屏正常",
          r.returncode == 0 and shown.replace("'", "") == "nihao" and got == "你好",
          f"rc={r.returncode} 预编辑={shown!r} 上屏={got!r}")
    await c.ic.call_focus_out()
    await asyncio.sleep(0.2)

    # 客户端不声明 Preedit 能力位（WPS / QQ 一类走 XIM 的应用）：编码不能交给 Fcitx5 输入面板
    # ——classicui 会另起小窗画它，被我们的候选窗盖住——而是由服务端画进候选窗顶部栏。
    # kimpanel 收到的就是 classicui 会画的那份面板文本，拿它作观测点。
    kw = await kimpanel_watch(bus)
    c = await new_ctx(bus, im, "e2e-no-preedit", caps=0)
    await c.ic.call_set_cursor_rect(300, 400, 2, 20)
    kw.panel_texts.clear()
    eaten = await c.type("nihao")
    await asyncio.sleep(0.3)
    leaked = [t for t in kw.panel_texts if t.strip()]
    check("无 Preedit 能力位：编码不交给 Fcitx5 输入面板", all(eaten) and not leaked,
          f"eaten={eaten} 面板文本={leaked}")
    if os.environ.get("DISPLAY"):
        win = candidate_window()
        ok = win is not None and win[5]
        check("无 Preedit 能力位：候选窗照常出现", ok, f"window={win}")
        if ok:
            path, _ = screenshot("candidate_no_preedit_cap", win[0])
            print(f"  无 Preedit 能力位候选窗截图（编码应在顶部栏）：{path}", flush=True)
    await c.key(" ")
    got = c.take()
    check("无 Preedit 能力位：上屏「你好」、没有多余空格", got == "你好", f"上屏={got!r}")
    await c.ic.call_focus_out()
    await asyncio.sleep(0.2)

# ── 命令直通车按键合成（CMD_KEY_TAP / SEQ / HOLD / RELEASE → forwardKey）────────────

XK_HOME, XK_END, XK_BACKSPACE = 0xFF50, 0xFF57, 0xFF08
XK_SHIFT_L, XK_CONTROL_L = 0xFFE1, 0xFFE3
DOWN, UP = False, True


async def run_phrase(c, code, want_events, timeout=3.0):
    """打短语码 + 空格选首选（e2e.sh 追加的 $CC 短语，weight 2000 排第一），等 addon 经
    forwardKey 交出 `want_events` 个事件（服务端推帧是异步的）。返回 (事件, 上屏)。
    选词之前清空 `c.preedits`：之后收到的预编辑都是选词及其后的。"""
    c.forwarded.clear()
    c.take()
    await c.type(code)
    c.preedits.clear()
    await c.key(" ")
    await wait_until(lambda: len(c.forwarded) >= want_events, timeout)
    await asyncio.sleep(0.2)  # 多出来的也收进来，好断言「恰好这些」
    return list(c.forwarded), c.take()


async def keysynth_cases(bus, im):
    """按键合成走 Fcitx5 `InputContext::forwardKey`：DBus 前端把它发成 ForwardKey(sym, state,
    isRelease) 信号，这里逐个断言按下 / 抬起的顺序与 state 位（X 语义：事件之前的修饰态）。"""
    c = await new_ctx(bus, im, "e2e-keys")

    # a) codl（Linux 出厂删行短语）：Home、Shift+End、Backspace 逐个 tap。
    ev, got = await run_phrase(c, "codl", 8)
    want = [(XK_HOME, 0, DOWN), (XK_HOME, 0, UP),
            (XK_SHIFT_L, 0, DOWN), (XK_END, STATE_SHIFT, DOWN),
            (XK_END, STATE_SHIFT, UP), (XK_SHIFT_L, STATE_SHIFT, UP),
            (XK_BACKSPACE, 0, DOWN), (XK_BACKSPACE, 0, UP)]
    check("按键合成：codl → Home / Shift+End / Backspace 的按下抬起序列与 state", ev == want,
          f"收到 {ev}")
    check("按键合成：codl 选中后不上屏候选文字", got == "", f"上屏={got!r}")

    # b) key.tap("Ctrl+C")：Ctrl 按下 → c 按下 → c 抬起 → Ctrl 抬起。
    ev, _ = await run_phrase(c, "zkcc", 4)
    want = [(XK_CONTROL_L, 0, DOWN), (ord("c"), STATE_CTRL, DOWN),
            (ord("c"), STATE_CTRL, UP), (XK_CONTROL_L, STATE_CTRL, UP)]
    check("按键合成：key.tap(Ctrl+C) 修饰键先按后抬、主键居中", ev == want, f"收到 {ev}")

    # d) 合成的键不回流进输入法：key.tap("a") 之后应用侧没有预编辑、服务端没有组合
    #    （接着按空格交还宿主 = 服务端没有在组字），随后照常打字。
    ev, got = await run_phrase(c, "zkta", 2)
    seen = [p for p in c.preedits if p]
    space_eaten = await c.key(" ")
    check("按键合成：key.tap(a) 只经 forwardKey 交给应用",
          ev == [(ord("a"), 0, DOWN), (ord("a"), 0, UP)] and got == "", f"收到 {ev} 上屏={got!r}")
    check("按键合成：合成的 a 不回流成组字（无预编辑、空格交还宿主）",
          not seen and space_eaten is False, f"预编辑={seen} 空格被吃={space_eaten}")
    await c.type("nihao")
    await c.key(" ")
    got = c.take()
    check("按键合成：之后照常打字", got == "你好", f"上屏={got!r}")

    # 不认识的组合（Hyper+C）整条丢弃；超长 seq（65 个组合）整条丢弃。
    ev, _ = await run_phrase(c, "zkbad", 1, timeout=1.0)
    check("按键合成：不认识的修饰名整条丢弃（不退化成裸 c）", ev == [], f"收到 {ev}")
    ev, _ = await run_phrase(c, "zklong", 1, timeout=1.0)
    check("按键合成：超过 64 个组合的 key.seq 整条丢弃", ev == [], f"收到 {ev}")

    # c) key.hold / key.release 成对；按住 Shift 时 tap End 带 Shift 位。
    ev, _ = await run_phrase(c, "zkhs", 1)
    check("按键合成：key.hold(Shift) 只按下", ev == [(XK_SHIFT_L, 0, DOWN)], f"收到 {ev}")
    ev, _ = await run_phrase(c, "zkte", 2)
    check("按键合成：按住 Shift 时 tap End 带 Shift 位",
          ev == [(XK_END, STATE_SHIFT, DOWN), (XK_END, STATE_SHIFT, UP)], f"收到 {ev}")
    ev, _ = await run_phrase(c, "zkrs", 1)
    check("按键合成：key.release(Shift) 抬起", ev == [(XK_SHIFT_L, STATE_SHIFT, UP)], f"收到 {ev}")
    ev, _ = await run_phrase(c, "zkrs", 1, timeout=1.0)
    check("按键合成：没按住时 key.release 不凭空造抬起", ev == [], f"收到 {ev}")

    release = [(XK_SHIFT_L, STATE_SHIFT, UP)]

    # 卡键保护：失焦。
    await run_phrase(c, "zkhs", 1)
    c.forwarded.clear()
    await c.ic.call_focus_out()
    await wait_until(lambda: c.forwarded, 2.0)
    check("卡键保护：按住 Shift 后失焦 → 补发抬起", c.forwarded == release, f"收到 {c.forwarded}")
    await c.ic.call_focus_in()
    await asyncio.sleep(0.2)

    # 卡键保护：换 IC（焦点重叠——B 先 FocusIn、A 才 FocusOut：抬起发回 A，不发给 B）。
    await run_phrase(c, "zkhs", 1)
    c.forwarded.clear()
    b = await new_ctx(bus, im, "e2e-keys-b")
    await wait_until(lambda: c.forwarded, 2.0)
    check("卡键保护：按住 Shift 后焦点换到另一个 IC → 抬起发回原 IC、新 IC 收不到",
          c.forwarded == release and b.forwarded == [], f"A 收到 {c.forwarded} B 收到 {b.forwarded}")
    await c.ic.call_focus_out()
    await b.ic.call_focus_out()
    await c.ic.call_focus_in()
    await asyncio.sleep(0.2)

    # 卡键保护：宿主 reset（鼠标点击挪了光标等）。
    await run_phrase(c, "zkhs", 1)
    c.forwarded.clear()
    await c.ic.call_reset()
    await wait_until(lambda: c.forwarded, 2.0)
    check("卡键保护：按住 Shift 后宿主 reset → 补发抬起", c.forwarded == release,
          f"收到 {c.forwarded}")

    # 卡键保护：最长保持时间（e2e 调成 2 秒，出厂 10 秒）到点自动抬起。
    await run_phrase(c, "zkhs", 1)
    c.forwarded.clear()
    t0 = asyncio.get_running_loop().time()
    await asyncio.sleep(1.0)
    early = list(c.forwarded)
    await wait_until(lambda: c.forwarded, 3.0)
    dt = asyncio.get_running_loop().time() - t0
    check("卡键保护：按住超过最长保持时间自动抬起（未到点前不抬）",
          early == [] and c.forwarded == release and 1.5 <= dt <= 3.5,
          f"1 秒时={early} 最终={c.forwarded} 用时 {dt:.1f}s")

    # 收尾后打字照常。
    await c.type("nihao")
    await c.key(" ")
    got = c.take()
    check("按键合成用例之后照常打字", got == "你好", f"上屏={got!r}")
    await c.ic.call_focus_out()
    await asyncio.sleep(0.2)
    return c


async def keysynth_restart_cases(bus, im, restart):
    """卡键保护：服务重启（push 断线 + 新服务的 SERVICE_READY）补发抬起。"""
    c = await new_ctx(bus, im, "e2e-keys-restart")
    # 上一次重启后 push 通道每秒重连一次，通之前推下来的帧全丢：按到收到为止。
    ev = []
    for _ in range(10):
        ev, _ = await run_phrase(c, "zkhs", 1, timeout=1.0)
        if ev:
            break
    c.forwarded.clear()
    rc = await asyncio.to_thread(subprocess.run, [restart])
    await wait_until(lambda: c.forwarded, 5.0)
    check("卡键保护：按住 Shift 后服务重启 → 补发抬起",
          ev == [(XK_SHIFT_L, 0, DOWN)] and rc.returncode == 0
          and c.forwarded == [(XK_SHIFT_L, STATE_SHIFT, UP)],
          f"按下={ev} rc={rc.returncode} 之后收到 {c.forwarded}")
    ready = await wait_ready(c)
    check("服务重启（按键合成用例）后 addon 自愈重连", ready, "一直透传")
    await c.ic.call_focus_out()
    await asyncio.sleep(0.2)


def service_pid():
    return open(os.path.join(os.environ["W"], "svc.pid")).read().strip()


async def timed_key(c, name):
    t0 = time.monotonic()
    eaten = await c.key(name)
    return eaten, time.monotonic() - t0


async def stall_cases(bus, im):
    """服务卡死（进程在、不响应，用 SIGSTOP 模拟）：addon 跑在 fcitx5 主进程里，每个键都同步
    等服务——不设防的话每键卡 4~6 秒、最坏永久挂住。要求：至多付一次超时（2 秒量级），之后
    熔断期内按键直接透传、焦点切换也不卡；恢复（SIGCONT）后自愈，且服务在恢复时处理掉的
    积压帧不会把旧码拼进新输入。"""
    c = await new_ctx(bus, im, "e2e-stall")
    await wait_ready(c)
    await c.type("ni")
    composing = c.preedit != ""
    pid = service_pid()
    subprocess.run(["kill", "-STOP", pid])
    try:
        first_eaten, first = await timed_key(c, "h")
        rest = [await timed_key(c, k) for k in "aoxy"]
        t0 = time.monotonic()
        await c.ic.call_focus_out()
        await c.ic.call_focus_in()
        refocus = time.monotonic() - t0
        print(f"  卡死期间：首键 {first:.2f}s 其余 {[round(d, 2) for _, d in rest]} "
              f"焦点切换 {refocus:.2f}s", flush=True)
        check("服务卡死：首键至多付一次超时（< 3 秒）", composing and first < 3.0,
              f"组字={composing} 首键 {first:.2f}s")
        check("服务卡死：熔断期内后续按键不再等（每键 < 0.5 秒）、直接透传",
              all(d < 0.5 for _, d in rest) and not any(e for e, _ in rest) and not first_eaten,
              f"{rest} 首键吃={first_eaten}")
        check("服务卡死：焦点切换不卡（< 0.5 秒）", refocus < 0.5, f"{refocus:.2f}s")
        check("服务卡死：本端收起组字（应用里不挂着旧预编辑）", c.preedit == "",
              f"preedit={c.preedit!r}")
    finally:
        subprocess.run(["kill", "-CONT", pid])
    c.take()
    c.preedits.clear()
    await asyncio.sleep(3.5)  # 熔断期（3 秒）过去
    # 不用 wait_ready 探活：它按 a 再按 Esc，Esc 会顺手清掉服务端积压出来的码，掩盖对齐缺失。
    # 服务一直是就绪的（只是停过），熔断到期后的第一个键就该被吃。
    eaten = await c.type("nihao")
    await c.key(" ")
    got = c.take()
    check("服务恢复（SIGCONT）后 addon 自愈：熔断到期后的按键重新被吃", all(eaten), f"{eaten}")
    check("服务恢复后照常上屏「你好」（卡死时积压在旧连接里的码没拼进来）", got == "你好",
          f"上屏={got!r}")
    await c.ic.call_focus_out()
    await asyncio.sleep(0.2)


async def extension_cases(a):
    """扩展程序通道：假扩展（e2e.sh 装进 extensions/echo）应收到按键与上屏。

    握手完成前的事件按设计丢弃，所以先等扩展握手，再自己打一遍字。"""
    import json

    out = os.environ.get("WIND_E2E_EXTENSION_OUT", "")
    if not out:
        check("扩展用例：e2e.sh 应设置 WIND_E2E_EXTENSION_OUT", False)
        return

    def msgs():
        return [json.loads(l)["msg"] for l in read_lines(out) if l.startswith("{")]

    def got_commit():
        return any(m.get("t") == "commit" and m.get("text") == "你好" for m in msgs())

    def welcomed():
        return any(m.get("t") == "welcome" for m in msgs())

    await wait_until(welcomed, 10)
    await a.type("nihao")
    await a.key(" ")
    a.take()
    await wait_until(got_commit, 5)
    ms = msgs()
    welcome = next((m for m in ms if m.get("t") == "welcome"), None)
    check("扩展握手：授予 key.detail / commit / mode",
          welcome is not None
          and {"key.detail", "commit", "mode"} <= set(welcome.get("granted", [])),
          f"welcome={welcome}")
    check("扩展收到模式事件", any(m.get("t") == "mode" for m in ms), f"共 {len(ms)} 条")
    commits = [m for m in ms if m.get("t") == "commit" and m.get("text") == "你好"]
    check("扩展收到上屏「你好」", bool(commits), f"commits={[m for m in ms if m.get('t') == 'commit']}")
    if commits:
        sid = commits[-1].get("sid")
        keys = [m.get("key") for m in ms if m.get("t") == "key" and m.get("sid") == sid]
        check("同一组合会话的按键依次是 n i h a o space（带具体键名）",
              sid and keys == ["n", "i", "h", "a", "o", "space"], f"sid={sid} keys={keys}")


async def wait_ready(c):
    """服务的 socket 起得比引擎早：词库（首次还要建 .wdat 缓存）加载完成前，按键一律透传。
    用真实通路探活——按 a 直到被吃（= 引擎开始组字），再 Esc 撤掉。"""
    for _ in range(600):
        if await c.key("a"):
            await c.key("Escape")
            c.take()
            c.preedits.clear()
            return True
        await asyncio.sleep(0.2)
    return False


async def main():
    bus = await MessageBus().connect()
    intro = await bus.introspect(SERVICE, IM_PATH)
    im = bus.get_proxy_object(SERVICE, IM_PATH, intro).get_interface(IM_IFACE)
    a = await new_ctx(bus, im, "e2e-editor")

    if not await wait_ready(a):
        check("服务引擎就绪（120s 内开始组字）", False, "一直透传")
        return 1

    if os.environ.get("WIND_E2E_ONLY") == "skin":
        await a.ic.call_focus_out()
        await skin_cases(bus, im)
        await candidate_drag_cases(bus, im)
        await skin_motion_cases(bus, im)
        ok = all(results)
        print(f"{'PASS' if ok else 'FAIL'} 总计 {sum(results)}/{len(results)}", flush=True)
        return 0 if ok else 1

    # 1. 全拼 nihao + 空格 → 你好；组字期间有预编辑、上屏后预编辑清空
    eaten = await a.type("nihao")
    had_preedit = a.preedit != ""
    eaten.append(await a.key(" "))
    got = a.take()
    check("全拼 nihao+空格 上屏「你好」", got == "你好", f"上屏={got!r}")
    check("组字期间按键全部被吃、有预编辑", all(eaten) and had_preedit,
          f"eaten={eaten} preedit_seen={had_preedit}")
    check("上屏后预编辑清空", a.preedit == "", f"preedit={a.preedit!r}")
    await extension_cases(a)

    # 2. 退格：nihaoo ← → nihao
    await a.type("nihaoo")
    await a.key("BackSpace")
    await a.key(" ")
    got = a.take()
    check("退格删一码后上屏「你好」", got == "你好", f"上屏={got!r}")

    # 3. Esc 取消：不上屏、预编辑清空；随后空格交还宿主
    await a.type("nihao")
    await a.key("Escape")
    got = a.take()
    check("Esc 取消组字：不上屏", got == "", f"上屏={got!r}")
    check("Esc 后预编辑清空", a.preedit == "", f"preedit={a.preedit!r}")
    space_eaten = await a.key(" ")
    check("Esc 后空格交还宿主（不被吃）", space_eaten is False, f"eaten={space_eaten}")
    a.take()

    # 4. 数字选词：1 选首选；2 选第二候选（与首选不同）
    await a.type("nihao")
    await a.key("1")
    got = a.take()
    check("数字 1 选首选「你好」", got == "你好", f"上屏={got!r}")
    await a.type("shi")
    await a.key(" ")
    first = a.take()
    await a.type("shi")
    await a.key("2")
    second = a.take()
    check("数字 2 选第二候选（非空且不同于首选）",
          first != "" and second != "" and first != second, f"首选={first!r} 第二={second!r}")

    # 5. Shift 单击切英文：字母交还宿主、不上屏；再切回中文恢复
    await a.tap_shift()
    eaten = await a.type("abc")
    got = a.take()
    check("Shift 切英文后字母直通（不被吃、不上屏）", eaten == [False] * 3 and got == "",
          f"eaten={eaten} 上屏={got!r}")
    await a.tap_shift()
    await a.type("nihao")
    await a.key(" ")
    got = a.take()
    check("再按 Shift 切回中文", got == "你好", f"上屏={got!r}")

    # 5b. 带修饰键的功能热键（服务的默认键位）：到得了服务、且真的生效
    async def chord(sym, code, state):
        eaten = await a.ic.call_process_key_event(sym, code, state, False, 0)
        await a.ic.call_process_key_event(sym, code, state, True, 0)
        await asyncio.sleep(0.05)
        return eaten

    # Shift+空格：全角开关。开着时无组字的空格上屏全角空格 U+3000。
    e1 = await chord(0x20, 65, STATE_SHIFT)
    await a.key(" ")
    got = a.take()
    await chord(0x20, 65, STATE_SHIFT)
    await a.key(" ")
    back = a.take()
    check("Shift+空格 被服务吃掉（全角开关）", e1 is True, f"eaten={e1}")
    check("Shift+空格 后空格上屏全角空格", got == "\u3000", f"上屏={got!r}")
    check("再按 Shift+空格 恢复半角", back == "", f"上屏={back!r}（半角空格应交还宿主）")

    # Ctrl+Shift+E：切换引擎（方案）。切走后 nihao+空格 不再是「你好」，再切回来恢复。
    e2 = await chord(0x45, 26, STATE_CTRL | STATE_SHIFT)  # Shift 下 e 的 keysym 是 'E'
    await a.type("nihao")
    await a.key(" ")
    other = a.take()
    await a.key("Escape")
    await chord(0x45, 26, STATE_CTRL | STATE_SHIFT)
    await a.type("nihao")
    await a.key(" ")
    again = a.take()
    check("Ctrl+Shift+E 被服务吃掉（切引擎）", e2 is True, f"eaten={e2}")
    check("Ctrl+Shift+E 切走后不再出「你好」", other != "你好", f"上屏={other!r}")
    check("再按 Ctrl+Shift+E 切回全拼", again == "你好", f"上屏={again!r}")

    # 6. 焦点切换：组字中失焦 → 宿主预编辑被清；回来后是全新一轮
    await a.type("nihao")
    check("失焦前确有预编辑（下一条的前提）", a.preedit != "", "组字没起来")
    await a.ic.call_focus_out()
    await asyncio.sleep(0.2)
    check("失焦时宿主预编辑被清", a.preedit == "", f"preedit={a.preedit!r}")
    await a.ic.call_focus_in()
    await asyncio.sleep(0.2)
    space_eaten = await a.key(" ")
    got = a.take()
    check("回到焦点后旧编码已丢弃（空格直通、不上屏）",
          space_eaten is False and got == "", f"eaten={space_eaten} 上屏={got!r}")

    # 7. 两个文本框之间切换：A 组字中，焦点到 B → A 的预编辑清掉，B 正常输入
    await a.type("nihao")
    await a.ic.call_focus_out()
    b = await new_ctx(bus, im, "e2e-other")
    check("切到另一个文本框时 A 的预编辑被清", a.preedit == "", f"A preedit={a.preedit!r}")
    await b.type("nihao")
    await b.key(" ")
    got_b = b.take()
    got_a = a.take()
    check("B 框输入上屏到 B、不串到 A", got_b == "你好" and got_a == "",
          f"B={got_b!r} A={got_a!r}")

    # 7b. 焦点重叠：A 组字中，B 先 FocusIn、A 才 FocusOut（无焦点组的前端允许这种顺序）。
    #     A 迟到的失焦会被服务端当陈旧事件丢掉——addon 必须在 B 激活时先替 A 收尾。
    await b.ic.call_focus_out()
    await a.ic.call_focus_in()
    await asyncio.sleep(0.2)
    await a.type("nihao")
    await b.ic.call_focus_in()
    await asyncio.sleep(0.2)
    await a.ic.call_focus_out()
    await asyncio.sleep(0.2)
    check("焦点重叠时 A 的预编辑同样被清", a.preedit == "", f"A preedit={a.preedit!r}")
    await b.type("nihao")
    await b.key(" ")
    got_b = b.take()
    got_a = a.take()
    check("焦点重叠时旧编码不拼进 B", got_b == "你好" and got_a == "",
          f"B={got_b!r} A={got_a!r}")

    # 8. 密码框：声明 Password 能力位 → 服务端强制英文直通
    p = await new_ctx(bus, im, "e2e-login", caps=CAP_PREEDIT | CAP_PASSWORD)
    eaten = await p.type("nihao")
    got = p.take()
    check("密码框里字母直通（不组字、不上屏）", eaten == [False] * 5 and got == "",
          f"eaten={eaten} 上屏={got!r}")

    # 候选窗（X11）：只在 e2e.sh 起了 Xvfb 时跑
    await p.ic.call_focus_out()
    await preedit_display_cases(bus, im)
    await keysynth_cases(bus, im)
    if os.environ.get("DISPLAY"):
        await x11_cases(bus, im)
        await grid_cases(bus, im)
        await overlay_cases(bus, im)
        await menu_cases(bus, im)
        await menu_idle_ignores_motion_cases(bus, im)
        await menu_max_grab_cases(bus, im)
        await menu_compositor_toggle_cases(bus, im)
        await x_conn_lost_cases(bus, im)
        await overlay_interaction_cases(bus, im)
        await mode_icon_cases(bus, im)
        await settings_cases(bus, im)
        await panel_cases(bus, im)
        await a.ic.call_focus_in()
        await asyncio.sleep(0.2)

    # 9. 服务重启：addon 的请求连接变成死连接，必须自愈（重连 + 重试当前键），无需重启 fcitx5
    restart = os.environ.get("WIND_E2E_RESTART")
    if restart:
        rc = await asyncio.to_thread(subprocess.run, [restart])
        check("重启服务", rc.returncode == 0, f"restart 退出码 {rc.returncode}")
        await a.ic.call_focus_in()
        ready = await wait_ready(a)
        check("服务重启后 addon 自愈重连（按键重新被吃）", ready, "一直透传")
        await a.type("nihao")
        await a.key(" ")
        got = a.take()
        check("服务重启后照常上屏「你好」", got == "你好", f"上屏={got!r}")
        await a.ic.call_focus_out()
        await stall_cases(bus, im)
        await keysynth_restart_cases(bus, im, restart)
        await a.ic.call_focus_in()
        await asyncio.sleep(0.2)
        if os.environ.get("DISPLAY"):
            await a.ic.call_focus_out()
            await menu_stall_cases(bus, im)
            await menu_restart_cases(bus, im, restart)
            await overlay_restart_cases(bus, im, restart)

    ok = all(results)
    print(f"{'PASS' if ok else 'FAIL'} 总计 {sum(results)}/{len(results)}", flush=True)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
