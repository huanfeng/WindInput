// GNOME Shell 42–44 版的 windinput-caret（旧扩展格式：imports + init()）。与
// windinput-caret@windinput 导出同一个 D-Bus 接口；坐标换算用 kimpanel 在 mutter 47 之前的做法：
// 窗口 actor（= buffer）左上 + 相对坐标 × shellScale / 客户端 sf。说明见上一级的 README.md。
/* exported init */
const {Gio, St} = imports.gi;

const IFACE = `
<node>
  <interface name="org.windinput.Caret">
    <method name="GetFocusFrame">
      <arg type="i" direction="out" name="x"/>
      <arg type="i" direction="out" name="y"/>
      <arg type="i" direction="out" name="w"/>
      <arg type="i" direction="out" name="h"/>
      <arg type="d" direction="out" name="shell_scale"/>
    </method>
    <method name="TranslateRelativeSpot">
      <arg type="i" direction="in" name="x"/>
      <arg type="i" direction="in" name="y"/>
      <arg type="i" direction="in" name="w"/>
      <arg type="i" direction="in" name="h"/>
      <arg type="d" direction="in" name="scale"/>
      <arg type="i" direction="out" name="sx"/>
      <arg type="i" direction="out" name="sy"/>
      <arg type="i" direction="out" name="sw"/>
      <arg type="i" direction="out" name="sh"/>
      <arg type="d" direction="out" name="shell_scale"/>
    </method>
  </interface>
</node>`;

function shellScale() {
    return St.ThemeContext.get_for_stage(global.stage).scale_factor || 1;
}

class WindInputCaretLegacy {
    enable() {
        this._impl = Gio.DBusExportedObject.wrapJSObject(IFACE, this);
        this._impl.export(Gio.DBus.session, '/org/windinput/Caret');
        this._owner = Gio.DBus.session.own_name(
            'org.windinput.Caret', Gio.BusNameOwnerFlags.NONE, null, null);
    }

    disable() {
        if (this._owner) {
            Gio.DBus.session.unown_name(this._owner);
            this._owner = 0;
        }
        if (this._impl) {
            this._impl.unexport();
            this._impl = null;
        }
    }

    /** 焦点窗口外框（layout / stage）。无焦点时全 0。 */
    GetFocusFrame() {
        const win = global.display.focus_window;
        if (!win) {
            return [0, 0, 0, 0, shellScale()];
        }
        const r = win.get_frame_rect();
        return [r.x, r.y, r.width, r.height, shellScale()];
    }

    /** 窗口相对光标 → stage 绝对坐标。scale：客户端矩形的缩放（≤0 当 1）。 */
    TranslateRelativeSpot(x, y, w, h, scale) {
        const ss = shellScale();
        const win = global.display.focus_window;
        if (!win) {
            return [x, y, w, h, ss];
        }
        const k = ss / (scale > 0.01 ? scale : 1);
        const actor = win.get_compositor_private();
        const ox = actor ? actor.x : 0;
        const oy = actor ? actor.y : 0;
        return [Math.round(ox + x * k), Math.round(oy + y * k),
            Math.round(w * k), Math.round(h * k), ss];
    }
}

function init() {
    return new WindInputCaretLegacy();
}
