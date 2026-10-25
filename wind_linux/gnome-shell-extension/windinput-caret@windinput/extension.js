// GNOME Shell 扩展：焦点窗口坐标系下的相对光标 → stage 绝对坐标，经 D-Bus 交给 WindInput。
// gi://Mtk 从 Shell 45 起就有；protocol_to_stage_rect 从 mutter 47 起才有。shell-version 因此从 47 起。
// 版本范围与「尚未在真实会话里对过光标」的说明见同目录上一级的 README.md。
import Gio from 'gi://Gio';
import Mtk from 'gi://Mtk';
import St from 'gi://St';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

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

export default class WindInputCaretExtension extends Extension {
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
        const shellScale =
            St.ThemeContext.get_for_stage(global.stage).scale_factor || 1;
        if (!win) {
            return [0, 0, 0, 0, shellScale];
        }
        const r = win.get_frame_rect();
        return [r.x, r.y, r.width, r.height, shellScale];
    }

    /**
     * 窗口相对光标 → stage 绝对坐标。
     * scale：客户端光标矩形所在坐标系相对 shell 的缩放（≤0 当 1）。
     */
    TranslateRelativeSpot(x, y, w, h, scale) {
        const shellScale =
            St.ThemeContext.get_for_stage(global.stage).scale_factor || 1;
        const win = global.display.focus_window;
        if (!win) {
            return [x, y, w, h, shellScale];
        }
        const spotScale = scale > 0.01 ? scale : 1;
        let rect = new Mtk.Rectangle({
            x: Math.round(x * (shellScale / spotScale)),
            y: Math.round(y * (shellScale / spotScale)),
            width: Math.round(w * (shellScale / spotScale)),
            height: Math.round(h * (shellScale / spotScale)),
        });
        rect = win.protocol_to_stage_rect(rect);
        const actor = win.get_compositor_private();
        if (actor) {
            rect.x += actor.x;
            rect.y += actor.y;
        }
        return [rect.x, rect.y, rect.width, rect.height, shellScale];
    }
}
