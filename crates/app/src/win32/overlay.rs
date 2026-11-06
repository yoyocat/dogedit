//! The overlay scrollbars as their own layered child windows.
//!
//! Why a window and not the shared [`dg_render::DrawList`]: "see through
//! the bar" is a compositing question. The text under the strip is painted
//! by the editor window, so the blend has to happen where the two surfaces
//! meet — which is DWM, given a `WS_EX_LAYERED` child. The strip is pushed
//! to the glass with `UpdateLayeredWindow` as premultiplied ARGB: pixels
//! outside the thumb pill are fully transparent, and the pill itself
//! carries its own per-pixel alpha, so its anti-aliased edge fades into
//! whatever the editor painted beneath it.
//!
//! What this takes off the shell: the hand-written hit tests, the
//! mouse/wheel/timer routing arms, and the client-to-bar coordinate
//! transforms. The bar receives native messages in the space its model
//! already works in, because the model's rect *is* the window.
//!
//! Every claim here was measured in M1b step 0 (`examples/overlay_spike.rs`,
//! plan §M1b 补记): layered children composite once the manifest declares
//! Win8+; `MA_NOACTIVATE` keeps focus (and with it the caret and the IME
//! window) on the editor; `SetCapture` holds a drag without focus; and a
//! bar that is hidden or `HTTRANSPARENT` hands its column back to the text.

// The same elision the frame's handler uses: this module's message loops
// are `unsafe fn`s whose bodies are one long translation from a native
// message into a model event.
#![allow(unsafe_op_in_unsafe_fn)]

use std::sync::{Once, OnceLock};

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, BI_RGB,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, EndPaint,
    FillRect, GetDC, HBITMAP, HDC, HFONT, HGDIOBJ, InvalidateRect, PAINTSTRUCT, ReleaseDC,
    ScreenToClient, SelectObject,
    ValidateRect,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA,
    GetClientRect, GetWindowLongPtrW, HTCLIENT, HTTRANSPARENT, IDC_ARROW, KillTimer,
    LoadCursorW, MA_NOACTIVATE, RegisterClassW, SW_HIDE, SW_SHOW, SWP_NOACTIVATE,
    SetCursor, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, ULW_ALPHA,
    UpdateLayeredWindow, WM_CAPTURECHANGED, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCHITTEST, WM_PAINT, WM_SETCURSOR, WM_TIMER,
    WNDCLASSW, WS_CHILD, WS_EX_LAYERED,
};
use windows::core::{PCWSTR, w};

use crate::win32::window::{EditorState, high_word, low_word, state_mut, to_colorref};
use dg_render::Color;
use dg_render::Rect;
use dg_render::theme::Theme;
use dg_skin::UiOp;
use dg_ui::event::{Event, MouseButton, Point, mods};
use dg_ui::scrollbar::{FADE_H, FADE_V, Mark, Metrics, Scrollbar, Zone};
use dg_ui::widget::{Action, HostCmd, WidgetCtx};

/// Both bars share one window class; the handler tells them apart by
/// comparing its HWND against the bars in the editor state.
const BAR_CLASS: PCWSTR = w!("dogedit_overlay_bar");

/// How solid a 修改行 mark is. The marks are the one thing on the strip that is
/// never translucent: a change the pointer is nowhere near still has to be
/// findable, and the pill's resting opacity would hide it.
const MARK_ALPHA: u8 = 0xE0;

/// Which bar set a bar window belongs to. The document view's pair and each
/// 输出窗口 pane's pair share this class *and* the same frame parent, so the
/// handle is the only thing that tells them apart.
///
/// `Pane` carries the pane's index into `Panels::outputs`, not just "a pane":
/// the index alone would resolve through `Panels::output_mut`, which is the
/// *active* pane. A message arriving at a background pane's bar would then be
/// answered on the active pane's window — `BeginPaint` would consume the wrong
/// update region and the message window's own region would never be consumed,
/// so its `WM_PAINT` would be re-delivered as fast as the loop runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Owner {
    /// 编辑区的一格：`0` 是主分组那格，`1` 是第二分组（或分割出来）那格。
    /// 每格有自己的竖条和横条，句柄是把它们分开的东西。
    Editor(usize),
    Pane(usize),
}

/// Bar indices: the vertical bar is `V`, the horizontal one `H`.
pub(super) const V: usize = 0;
pub(super) const H: usize = 1;

/// The strip is an overlay on the text, not a scrollbar control: it keeps the
/// default arrow. The editor's class would leave its I-beam behind (the strip
/// would look like more text), and the double arrow a real scrollbar shows
/// would promise a drag handle the thin strip does not offer everywhere.
fn bar_cursor() -> PCWSTR {
    IDC_ARROW
}

/// The strip's pixels as premultiplied BGRA rows: fully transparent everywhere
/// except where something is drawn. The 修改行 marks go down first, in the
/// pill's own column, and the thumb capsule composites over them — so a mark
/// the thumb has reached still reads through the translucent pill instead of
/// disappearing behind it.
///
/// Coverage for the capsule is analytic — each pixel's distance to its core
/// segment against the radius — so the edge anti-aliases without GDI's help,
/// and a pill reads the same shape on both axes.
///
/// This is pure pixel math (no handles, no message state) so the paint's
/// look stays a function of `BarLook` alone. `out` is the caller's buffer,
/// reused across frames: the strip repaints on every tick of its fade, and a
/// fresh `Vec` per frame was an allocation per frame for pixels that are
/// overwritten whole anyway.
fn strip_pixels(
    w: i32,
    h: i32,
    pill: &Rect,
    colour: COLORREF,
    alpha: u8,
    marks: &[(Rect, COLORREF, u8)],
    out: &mut Vec<u8>,
) {
    let w = w.max(0) as usize;
    let h = h.max(0) as usize;
    out.clear();
    out.resize(w * h * 4, 0);
    let buf = out.as_mut_slice();
    for (rect, mark_colour, mark_alpha) in marks {
        let rgb = split_rgb(*mark_colour);
        let y0 = (rect.y.round().max(0.0) as usize).min(h);
        let y1 = (((rect.y + rect.h).round().max(0.0) as usize).min(h)).max(y0);
        let x0 = (rect.x.round().max(0.0) as usize).min(w);
        let x1 = (((rect.x + rect.w).round().max(0.0) as usize).min(w)).max(x0);
        for y in y0..y1 {
            for x in x0..x1 {
                blend_over(buf, w, x, y, rgb, *mark_alpha as f32 / 255.0);
            }
        }
    }
    let x0 = (pill.x.round().max(0.0) as usize).min(w);
    let y0 = (pill.y.round().max(0.0) as usize).min(h);
    let x1 = (((pill.x + pill.w).round().max(0.0) as usize).min(w)).max(x0);
    let y1 = (((pill.y + pill.h).round().max(0.0) as usize).min(h)).max(y0);
    if x1 == x0 || y1 == y0 {
        return;
    }
    // The radius is half the short side, so both ends cap in a semicircle
    // whatever the pill's length.
    let r = (x1 - x0).min(y1 - y0) as f32 / 2.0;
    // The core segment the caps swing around: down the pill if it stands,
    // across it if it lies. A thumb shorter than it is wide leaves the two
    // ends coincident — the capsule degenerates to a disc, which the
    // distance function handles without a branch.
    let (ax, ay, bx, by) = if y1 - y0 >= x1 - x0 {
        let cx = (x0 + x1) as f32 / 2.0;
        (cx, y0 as f32 + r, cx, y1 as f32 - r)
    } else {
        let cy = (y0 + y1) as f32 / 2.0;
        (x0 as f32 + r, cy, x1 as f32 - r, cy)
    };
    let rgb = split_rgb(colour);
    for y in y0..y1 {
        let py = y as f32 + 0.5;
        for x in x0..x1 {
            let px = x as f32 + 0.5;
            let d = seg_dist(px, py, ax, ay, bx, by);
            let cov = (r - d + 0.5).clamp(0.0, 1.0);
            if cov <= 0.0 {
                continue;
            }
            blend_over(buf, w, x, y, rgb, alpha as f32 / 255.0 * cov);
        }
    }
}

/// A `COLORREF`'s channels as floats, in the order the buffer wants them. The
/// value packs as `0x00BBGGRR`, so the low byte is red.
fn split_rgb(colour: COLORREF) -> (f32, f32, f32) {
    (
        (colour.0 & 0xFF) as f32,
        ((colour.0 >> 8) & 0xFF) as f32,
        ((colour.0 >> 16) & 0xFF) as f32,
    )
}

/// One pixel of source over one pixel of a premultiplied BGRA buffer — the
/// form `UpdateLayeredWindow`'s `AC_SRC_ALPHA` blend wants, so the glass needs
/// no second pass.
#[inline]
fn blend_over(buf: &mut [u8], w: usize, x: usize, y: usize, rgb: (f32, f32, f32), a: f32) {
    if a <= 0.0 {
        return;
    }
    let i = (y * w + x) * 4;
    let sa = (a * 255.0).round().min(255.0);
    let keep = 1.0 - sa / 255.0;
    let da = buf[i + 3] as f32;
    buf[i] = (rgb.2 * sa / 255.0 + buf[i] as f32 * keep).round() as u8;
    buf[i + 1] = (rgb.1 * sa / 255.0 + buf[i + 1] as f32 * keep).round() as u8;
    buf[i + 2] = (rgb.0 * sa / 255.0 + buf[i + 2] as f32 * keep).round() as u8;
    buf[i + 3] = (sa + da * keep).round().min(255.0) as u8;
}

/// 挑一个这次 op 列表没用过的不透明颜色当抠图色。
///
/// 写死一个颜色（比如 `#FF00FF`）会把「皮肤正好填了这个颜色」变成透明洞。列表
/// 里的颜色是有限的，扫一遍就能挑个空位。蓝通道从 `0xFF` 往下退，256 次正好
/// 覆盖全部蓝值 —— 一次滚动条绘制用不满 256 个不同颜色，所以一定有空位。
///
/// 渐变只有两端在列表里，中间那几带可能撞上：撞上就是一条 1 px 的缝，肉眼
/// 看不见，也不会有人靠它读信息。
///
/// 一次分配都没有：外层最多试 256 次，实际第一次就中（皮肤不用那个颜色）。
fn key_colour(ops: &[UiOp]) -> Color {
    for i in 0..=255u8 {
        let c = Color::rgb(0xFF, 0x00, 0xFFu8.wrapping_sub(i));
        if !ops.iter().any(|op| op_uses(*op, c)) {
            return c;
        }
    }
    Color::rgb(0xFF, 0x00, 0xFF)
}

/// 这条 op 会不会画出颜色 `c`。alpha 不比：抠图色比对的是 RGB。
fn op_uses(op: UiOp, c: Color) -> bool {
    let same = |x: Color| x.r == c.r && x.g == c.g && x.b == c.b;
    match op {
        UiOp::Fill { color, .. }
        | UiOp::Rect { color, .. }
        | UiOp::Line { color, .. }
        | UiOp::Poly { color, .. }
        | UiOp::Text { color, .. }
        | UiOp::Shadow { color, .. } => same(color),
        UiOp::Round { fill, stroke, .. } | UiOp::Circle { fill, stroke, .. } => {
            fill.is_some_and(same) || stroke.is_some_and(same)
        }
        UiOp::Gradient { from, to, .. } => same(from) || same(to),
        UiOp::Image { tint, .. } => tint.is_some_and(same),
        UiOp::Spacer | UiOp::Clip { .. } | UiOp::Unclip => false,
    }
}

/// Distance from a point to a segment — the capsule's field function.
fn seg_dist(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let abx = bx - ax;
    let aby = by - ay;
    let apx = px - ax;
    let apy = py - ay;
    let len2 = abx * abx + aby * aby;
    let t = if len2 <= 0.0 {
        0.0
    } else {
        ((apx * abx + apy * aby) / len2).clamp(0.0, 1.0)
    };
    let dx = apx - t * abx;
    let dy = apy - t * aby;
    (dx * dx + dy * dy).sqrt()
}

/// A strip's drawing surface, kept across frames: the DIB section, the memory
/// DC it is selected into, and the CPU-side pixel buffer `strip_pixels` writes.
///
/// The strip repaints on every tick of its fade — twice a frame while both bars
/// animate — and the old paint built all three from scratch each time: a fresh
/// `Vec`, a `CreateDIBSection`, a `CreateCompatibleDC`, and the matching
/// destroys. The bar's size only changes on a relayout, so the surface is
/// rebuilt on a size change and reused otherwise.
struct Surface {
    mem: HDC,
    dib: HBITMAP,
    /// The DIB's own pixels. Valid until `dib` is deleted.
    bits: *mut u8,
    w: i32,
    h: i32,
    /// What `strip_pixels` fills; copied into `bits` whole on every paint.
    buf: Vec<u8>,
}

impl Surface {
    fn empty() -> Self {
        Self {
            mem: HDC::default(),
            dib: HBITMAP::default(),
            bits: std::ptr::null_mut(),
            w: 0,
            h: 0,
            buf: Vec::new(),
        }
    }

    fn alive(&self) -> bool {
        !self.mem.0.is_null() && !self.bits.is_null()
    }

    /// Makes the surface `w` by `h`, rebuilding it only when the size moved.
    /// Returns false when GDI refuses, in which case the paint is skipped — the
    /// strip is decoration, and a failed allocation must not take the window
    /// down.
    unsafe fn ensure(&mut self, w: i32, h: i32) -> bool {
        if self.alive() && self.w == w && self.h == h {
            return true;
        }
        self.release();
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w;
        bmi.bmiHeader.biHeight = -h; // top-down rows
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let Ok(dib) = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            return false;
        };
        if bits.is_null() {
            let _ = DeleteObject(HGDIOBJ(dib.0));
            return false;
        }
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let _ = ReleaseDC(None, screen);
        if mem.0.is_null() {
            let _ = DeleteObject(HGDIOBJ(dib.0));
            return false;
        }
        SelectObject(mem, HGDIOBJ(dib.0));
        self.mem = mem;
        self.dib = dib;
        self.bits = bits as *mut u8;
        self.w = w;
        self.h = h;
        true
    }

    unsafe fn release(&mut self) {
        if !self.mem.0.is_null() {
            let _ = DeleteDC(self.mem);
        }
        if !self.dib.0.is_null() {
            let _ = DeleteObject(HGDIOBJ(self.dib.0));
        }
        self.mem = HDC::default();
        self.dib = HBITMAP::default();
        self.bits = std::ptr::null_mut();
        self.w = 0;
        self.h = 0;
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe { self.release() };
    }
}

/// Hands the finished ARGB buffer to DWM in one `UpdateLayeredWindow` call.
/// The alpha lives per pixel now, so the colour-key + uniform-alpha pair the
/// strip used to push with (`SetLayeredWindowAttributes`) is gone with it.
unsafe fn blit_layered(hwnd: HWND, surface: &Surface) {
    let pixels = &surface.buf;
    let want = surface.w as usize * surface.h as usize * 4;
    if pixels.len() != want || !surface.alive() {
        return;
    }
    std::ptr::copy_nonoverlapping(pixels.as_ptr(), surface.bits, want);
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let size = SIZE {
        cx: surface.w,
        cy: surface.h,
    };
    let src = POINT { x: 0, y: 0 };
    let screen = GetDC(None);
    let _ = UpdateLayeredWindow(
        hwnd,
        Some(screen),
        None,
        Some(&size),
        Some(surface.mem),
        Some(&src),
        COLORREF(0),
        Some(&blend),
        ULW_ALPHA,
    );
    let _ = ReleaseDC(None, screen);
}

static REGISTER_CLASS: Once = Once::new();

/// Set for the whole of a splitter drag. The drag holds `LockWindowUpdate` on
/// the frame, so every strip under it paints into a DC whose visible region is
/// empty: the work is thrown away before it reaches the screen, and the strips
/// are frozen at exactly the geometry they had when the drag started (the ghost
/// moves, not the hosts). Drawing them is therefore pure waste — a real drag
/// delivers hundreds of `WM_MOUSEMOVE`s, and every repaint the frame or a strip
/// schedules along the way would be a full two-rectangle paint per strip.
///
/// While this is set `sync` stops invalidating and `bar_proc` swallows
/// `WM_PAINT` without drawing. `finish_split_drag` clears it and repaints every
/// strip once, which is the paint that actually lands.
static FROZEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Freezes or thaws the strips for a splitter drag. See [`FROZEN`].
pub(super) fn freeze(on: bool) {
    FROZEN.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub(super) fn frozen() -> bool {
    FROZEN.load(std::sync::atomic::Ordering::Relaxed)
}

/// Closes a `WM_PAINT` without drawing anything. `EndPaint` is what validates
/// the update region, and a region left open is re-delivered as fast as the
/// message loop can run — so a paint that is skipped still has to be closed.
unsafe fn swallow_paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    if hdc.0.is_null() {
        let _ = ValidateRect(Some(hwnd), None);
    } else {
        let _ = EndPaint(hwnd, &ps);
    }
}

/// Kill switch for the whole overlay subsystem: with this off no strip window
/// is created, so `alive()` is false everywhere and every `place`/`sync`/`paint`
/// path short-circuits. The dock keeps reserving the same rects, so the only
/// difference is whether the windows exist — which is what made it a usable
/// bisect while the repaint storm was under investigation.
///
/// The storm's cause was the pane routing in `Owner::Pane` (see its doc), so the
/// bars are back on by default; `DG_BARS=0` still turns them off without a
/// rebuild.
fn bars_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("DG_BARS").map_or(true, |v| v != "0"))
}

fn instance() -> HINSTANCE {
    unsafe {
        GetModuleHandleW(None)
            .map(|m| HINSTANCE(m.0))
            .unwrap_or(HINSTANCE(std::ptr::null_mut()))
    }
}

/// One bar: the model plus the window that shows it.
struct BarWindow {
    hwnd: HWND,
    model: Scrollbar,
    /// The strip in the parent's client coordinates — where the window
    /// sits, as opposed to the model's rect, which is the same area in the
    /// window's own space.
    frame: Rect,
    /// Mirror of the last `ShowWindow` issued, so visibility changes are
    /// detected instead of re-applied every frame.
    window_shown: bool,
    /// The theme inputs the pill's pixels are a function of: the resting
    /// colour, the highlighted one, and the resting pill's opacity. The
    /// highlighted state is the more solid one — a pointer that found the
    /// bar should see it light up, not just shift hue.
    idle: COLORREF,
    hover: COLORREF,
    alpha: u8,
    /// 修改行标记: the two edit states' colours — the same pair the gutter's
    /// strip paints with.
    mark_modified: COLORREF,
    mark_saved: COLORREF,
    /// `WM_MOUSELEAVE` is a one-shot request, not a subscription: one
    /// `TrackMouseEvent` call buys one leave, so keep track of whether a
    /// request is outstanding.
    tracking: bool,
    /// What the pixels currently on the glass were drawn from. A strip is a
    /// function of these values and nothing else, so `sync` — which runs on
    /// every relayout, i.e. on every keystroke — only asks for a paint when
    /// one of them moved. Without this, typing a character repainted four
    /// strips (two here, two in the output pane) to show a thumb that had
    /// not shifted a pixel.
    painted: Option<BarLook>,
    /// Set by anything that changes the strip outside the model — a theme
    /// switch, a thaw after a splitter drag. The next `sync` repaints once
    /// and clears it.
    stale: bool,
    /// The DIB and its buffer, kept across frames. See [`Surface`].
    surface: Surface,
    /// 皮肤：`scroll.v` / `scroll.h` 有回调时整条交给它画。
    script: Option<std::rc::Rc<dg_script::ScriptHost>>,
    /// 皮肤画字用的字体。滚动条平时没有文字，所以它是**按需**建的：没皮肤、
    /// 或者皮肤没画字，这里就一直是空的。
    font: HFONT,
    /// `font` 是按哪个 DPI 建的。换屏（拖到另一块显示器）要重建。
    font_dpi: i32,
}

/// The inputs a strip's paint actually reads. Two paints with equal `BarLook`s
/// are pixel-identical, which is what makes it a valid repaint key.
#[derive(Clone, Copy, PartialEq, Eq)]
struct BarLook {
    pos: i32,
    range: i32,
    page: i32,
    zone: Zone,
    dragging: bool,
    idle: COLORREF,
    hover: COLORREF,
    alpha: u8,
    mark_modified: COLORREF,
    mark_saved: COLORREF,
    /// The marks themselves are a list, so the key reads the model's counter
    /// for them instead: it only moves when `set_marks` swaps the list.
    marks_rev: u64,
}

impl Drop for BarWindow {
    fn drop(&mut self) {
        if !self.font.0.is_null() {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(self.font.0));
            }
            self.font = HFONT::default();
        }
    }
}

impl BarWindow {
    fn new(vertical: bool) -> Self {
        Self {
            hwnd: HWND::default(),
            model: Scrollbar::new(vertical),
            frame: Rect::new(0.0, 0.0, 0.0, 0.0),
            window_shown: false,
            idle: COLORREF(0),
            hover: COLORREF(0),
            alpha: 0,
            mark_modified: COLORREF(0),
            mark_saved: COLORREF(0),
            tracking: false,
            painted: None,
            stale: true,
            surface: Surface::empty(),
            script: None,
            font: HFONT::default(),
            font_dpi: 0,
        }
    }

    /// 换上（或撤掉）皮肤。句柄没变就什么都不做：`paint_bar` 每次绘制都会
    /// 调一次，而重载皮肤是件稀有的事。
    fn set_script(&mut self, host: Option<std::rc::Rc<dg_script::ScriptHost>>) {
        if self.script.as_ref().map(std::rc::Rc::as_ptr) == host.as_ref().map(std::rc::Rc::as_ptr) {
            return;
        }
        self.script = host;
        // 皮肤来去都换像素，但 `look()` 里没有它 —— 不主动标脏，`sync` 就
        // 认不出该重画。
        self.stale = true;
    }

    fn alive(&self) -> bool {
        !self.hwnd.0.is_null()
    }

    /// Everything the next paint would read, in one comparable value.
    fn look(&self) -> BarLook {
        BarLook {
            pos: self.model.pos(),
            range: self.model.range(),
            page: self.model.page(),
            zone: self.model.zone(),
            dragging: self.model.dragging(),
            idle: self.idle,
            hover: self.hover,
            alpha: self.alpha,
            mark_modified: self.mark_modified,
            mark_saved: self.mark_saved,
            marks_rev: self.model.marks_rev(),
        }
    }

    /// Whether a paint now would put different pixels on the glass than the last
    /// one. `sync` runs on every relayout and after every mouse message over the
    /// strip, so this is what keeps a still thumb still.
    fn needs_paint(&self) -> bool {
        self.stale || self.painted != Some(self.look())
    }

    /// Records that the strip is about to be painted from its current look.
    fn stamp_paint(&mut self) {
        self.painted = Some(self.look());
        self.stale = false;
    }

    /// Pushes the strip to the glass as premultiplied ARGB: a fully
    /// transparent buffer with the pill's capsule rastered in, handed to
    /// `UpdateLayeredWindow`. DWM blends it over whatever the editor painted
    /// beneath, so the pill reads as a floating capsule and its anti-aliased
    /// edge fades into the text.
    ///
    /// `hwnd` is the window the `WM_PAINT` arrived at, and it is deliberately
    /// not read from `self`: the update region that has to be consumed belongs
    /// to the window the message named. If routing ever hands this bar another
    /// window's message, validating through the message's handle still leaves
    /// that window clean, so a misroute cannot turn into a repaint loop. The
    /// two only differ on a bug — `Owner::Pane` carries the pane index so they
    /// do not.
    ///
    /// There is no `ps.rcPaint` test here on purpose. An empty `rcPaint` means
    /// the update region fell outside the window's visible region — a locked
    /// window, a strip under a sibling, a strip with no area — and it is still
    /// a paint that has to be closed, and a look the glass should still show.
    unsafe fn paint(&mut self, hwnd: HWND) {
        let _stats = super::paintstats::begin(super::paintstats::Region::Overlay);
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        if hdc.0.is_null() {
            // Nothing here will consume the update region, and a WM_PAINT that
            // is never consumed is re-delivered as fast as the loop can run.
            let _ = ValidateRect(Some(hwnd), None);
            return;
        }
        let _ = EndPaint(hwnd, &ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let w = (rc.right - rc.left).max(0);
        let h = (rc.bottom - rc.top).max(0);
        if w == 0 || h == 0 {
            return;
        }
        let pill = self.model.thumb_rect();
        if !self.surface.ensure(w, h) {
            return;
        }
        // 皮肤接管：整条交给它画，内置胶囊让位。它说没画（没注册回调、返回
        // false、抛错）就照旧走内置画法 —— 一个坏皮肤不该让滚动条消失。
        // 颜色和修改行标记只有内置画法要，放在它后面算：皮肤接管时那一趟
        // `marks` 的分配和逐条换算就全省了。
        if !self.paint_by_script(w, h, &pill) {
            // The highlighted state (pointed at or dragged) is the lighter colour
            // and the more solid alpha; resting is the dark translucent pill.
            let (colour, alpha) = if self.model.dragging() || self.model.zone() != Zone::None {
                (self.hover, self.alpha.max(0xD0))
            } else {
                (self.idle, self.alpha)
            };
            // 修改行标记: only the vertical bar has an axis that document rows map
            // onto. The marks take the pill's own column and the pill composites
            // over them, so a change the thumb has reached still reads through it.
            // They are indicators, not controls — a click on one still belongs to
            // the text beneath (see `Scrollbar::interactive_at`).
            let mut marks: Vec<(Rect, COLORREF, u8)> = Vec::new();
            if self.model.vertical {
                for m in self.model.marks() {
                    let (y, len) = self.model.mark_span(m);
                    let colour = if m.state == 2 {
                        self.mark_saved
                    } else {
                        self.mark_modified
                    };
                    marks.push((Rect::new(pill.x, y, pill.w, len), colour, MARK_ALPHA));
                }
            }
            strip_pixels(w, h, &pill, colour, alpha, &marks, &mut self.surface.buf);
        }
        blit_layered(hwnd, &self.surface);
    }

    /// 皮肤画这一条。`true` 表示它画了，`surface.buf` 里已经是成品。
    ///
    /// 这条通道和别的区域不一样，因为滚动条是**分层窗口**：像素带着逐点 alpha
    /// 交给 DWM，底下的正文要从透明处透出来。而 GDI 只写 RGB、不碰 alpha ——
    /// 「哪几像素画过」得自己算。做法是先用一个**抠图色**铺满 DIB，回放 op
    /// 列表，再逐点比对：等于抠图色的算透明，其余一律不透明。抠图色不是写死
    /// 的，按这次列表里用到的颜色挑一个空位（见 [`key_colour`]），所以皮肤
    /// 填什么颜色都不会被误判成透明洞。
    ///
    /// 代价是**没有半透明**：皮肤画的像素要么全透、要么全实。真正的半透明要
    /// 一张 32bpp 中间表面做 alpha 合成，那是回放层明确「画不出来就跳过」的
    /// 那一类（`shadow`、带着色的图）。
    ///
    /// `ctx.thumb` 给的是内置画法算好的滑块矩形 —— 映射、最短长度、居中全在
    /// `Scrollbar` 里，脚本不该再算一遍。
    unsafe fn paint_by_script(&mut self, w: i32, h: i32, pill: &Rect) -> bool {
        // 先克隆再动 `self`：`host.paint` 借的是这一份，`surface` 要可变借。
        let Some(host) = self.script.clone() else {
            return false;
        };
        let region = if self.model.vertical {
            "scroll.v"
        } else {
            "scroll.h"
        };
        if !host.has_paint(region) {
            return false;
        }
        let dpi = GetDpiForWindow(self.hwnd).max(96) as i32;
        let scale = dpi as f32 / 96.0;
        let mut state = 0u16;
        if self.model.dragging() {
            state |= dg_skin::State::PRESSED.bits();
        }
        // 指针在这条上就算 hot —— 这正是状态栏一直缺的那个位（那边没有逐格
        // 跟踪鼠标），滚动条有，因为窗口本身就是一格。
        if self.model.dragging() || self.model.zone() != Zone::None {
            state |= dg_skin::State::HOT.bits();
        }
        let input = dg_script::PaintInput {
            rect: (0.0, 0.0, w as f32 / scale, h as f32 / scale),
            thumb: (
                pill.x / scale,
                pill.y / scale,
                pill.w / scale,
                pill.h / scale,
            ),
            // 编辑区的滚动条是自绘的覆盖层，走的是 `thumb` 这条路；`bar` 是脚本
            // 窗格专用的一套（轨道 + 滑块都在一块窗格客户区里），这里不填。
            bar: (0.0, 0.0, 0.0, 0.0),
            scroll: 0.0,
            state,
            part: if self.model.vertical { "v" } else { "h" }.to_string(),
            text: String::new(),
            label: String::new(),
            // 滚动条没有文字，`@value` 就报当前行/列 —— 脚本要画「12」这类
            // 位置提示不必再自己记状态。
            value: self.model.pos().to_string(),
            dpi,
            // 一条滚动条只有一块，不需要序号区分。
            index: 0,
        };
        let Some(out) = host.paint(region, &input) else {
            return false;
        };
        if !out.handled {
            return false;
        }
        let ops = out.compacted();
        let key = key_colour(ops.as_slice());
        let rc = RECT {
            left: 0,
            top: 0,
            right: w,
            bottom: h,
        };
        let _ = FillRect(
            self.surface.mem,
            &rc,
            super::gdicache::brush(to_colorref(key)),
        );
        let fonts = [self.font_for(dpi)];
        let ctx = super::skinpaint::ReplayCtx {
            fonts: &fonts,
            images: &[],
            strings: &out.strings,
            points: &out.points,
            // 占位符和 `ctx.text/label/value` 是同一份：脚本写 `"@value"` 时
            // 回放层得拿到同一个字符串，不能一边有值一边空。
            text: &input.text,
            label: &input.label,
            value: &input.value,
            dpi,
        };
        let _ = super::skinpaint::replay(self.surface.mem, ops.as_slice(), &ctx);
        // 抠图色 → 透明，其余 → 不透明。一次滚动条绘制是几千像素的量级。
        let len = (w as usize) * (h as usize) * 4;
        self.surface.buf.clear();
        self.surface.buf.resize(len, 0);
        let bits = std::slice::from_raw_parts(self.surface.bits, len);
        for (dst, src) in self
            .surface
            .buf
            .chunks_exact_mut(4)
            .zip(bits.chunks_exact(4))
        {
            // 缓冲区是 BGRA，`Color` 是 RGB。
            if src[0] == key.b && src[1] == key.g && src[2] == key.r {
                continue;
            }
            dst.copy_from_slice(&[src[0], src[1], src[2], 255]);
        }
        true
    }

    /// 皮肤画字用的字体，按 DPI 建一次。滚动条平时没有文字，所以这是懒建的。
    unsafe fn font_for(&mut self, dpi: i32) -> HFONT {
        if !self.font.0.is_null() && self.font_dpi == dpi {
            return self.font;
        }
        if !self.font.0.is_null() {
            let _ = DeleteObject(HGDIOBJ(self.font.0));
        }
        self.font = super::uifont::create(dpi);
        self.font_dpi = dpi;
        self.font
    }

    /// Arm the leave request once per visit: re-arming while already
    /// tracked would queue up extra leaves.
    unsafe fn arm_leave(&mut self) {
        if self.tracking || !self.alive() {
            return;
        }
        self.tracking = true;
        let mut tme = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: self.hwnd,
            dwHoverTime: 0,
        };
        let _ = TrackMouseEvent(&mut tme);
    }
}

/// The two overlay bars.
pub(super) struct OverlayBars {
    bars: [BarWindow; 2],
}

impl OverlayBars {
    pub(super) fn new() -> Self {
        Self {
            bars: [BarWindow::new(true), BarWindow::new(false)],
        }
    }

    /// 把皮肤交给两条（或撤掉）。窗口不存在时是空操作 —— `bar` 自己会忽略。
    pub(super) fn set_script(&mut self, host: Option<std::rc::Rc<dg_script::ScriptHost>>) {
        for bar in &mut self.bars {
            bar.set_script(host.clone());
        }
    }

    /// Destroys both strip windows. They are children of the frame, not of the
    /// pane, so nothing else would take them down when a pane is closed.
    pub(super) unsafe fn destroy(&mut self) {
        for bar in &mut self.bars {
            if !bar.hwnd.0.is_null() {
                let _ = DestroyWindow(bar.hwnd);
                bar.hwnd = HWND::default();
                bar.window_shown = false;
            }
        }
    }

    /// Strip thickness: the strip window the pill floats in, which the dock
    /// reserves and the paint never reshapes.
    pub(super) fn strip(theme: &Theme) -> f32 {
        theme.scrollbar_strip_width.max(theme.scrollbar_thumb_width)
    }

    /// Creates both children. They start without `WS_VISIBLE`: a bar with
    /// nothing to scroll must not sit over the text, and it is the hidden
    /// window that hands the last text column back to the editor.
    pub(super) unsafe fn create(&mut self, parent: HWND) {
        // See `bars_enabled`: `DG_BARS=0` runs the whole app without the
        // strip windows, which is how the storm was bisected.
        if !bars_enabled() {
            return;
        }
        REGISTER_CLASS.call_once(|| unsafe {
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(bar_proc),
                hInstance: instance(),
                lpszClassName: BAR_CLASS,
                // No background brush: the paint fills every pixel, and a
                // brush would flash opaque black over the text on each
                // resize (a layered window's glass starts as transparency,
                // not as a brush colour).
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);
        });
        for bar in &mut self.bars {
            if bar.alive() {
                continue;
            }
            if let Ok(hwnd) = CreateWindowExW(
                WS_EX_LAYERED,
                BAR_CLASS,
                w!("overlay scrollbar"),
                WS_CHILD,
                0,
                0,
                0,
                0,
                Some(parent),
                None,
                Some(instance()),
                None,
            ) {
                // The handler reaches the editor state through the frame
                // window, the same way the frame's own handler does.
                let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, parent.0 as isize);
                bar.hwnd = hwnd;
            }
        }
    }

    /// Places both strips (`V` on the editor's right edge, `H` on its
    /// bottom edge) and hands each model the same area in its own
    /// window-local space.
    pub(super) unsafe fn place(&mut self, v: Rect, h: Rect) {
        for (bar, frame) in self.bars.iter_mut().zip([v, h]) {
            if !bar.alive() {
                continue;
            }
            let resized = bar.frame != frame;
            bar.frame = frame;
            bar.model.layout(Rect::new(0.0, 0.0, frame.w, frame.h));
            // A strip placed with no area is hidden rather than left visible and
            // zero-sized: it has no pixels to draw and no reason to be in the
            // z-order. `sync` is the authority on visibility and shows it again
            // as soon as the host gives it area.
            if frame.w <= 0.0 || frame.h <= 0.0 {
                if bar.window_shown {
                    bar.window_shown = false;
                    let _ = ShowWindow(bar.hwnd, SW_HIDE);
                }
            }
            if resized {
                let _ = SetWindowPos(
                    bar.hwnd,
                    None,
                    frame.x.round() as i32,
                    frame.y.round() as i32,
                    frame.w.round() as i32,
                    frame.h.round() as i32,
                    SWP_NOACTIVATE,
                );
            }
        }
    }

    /// Applies the theme: the fixed geometry and the pill's colours. This is
    /// also how a theme switch reaches the bars — no separate hook.
    pub(super) unsafe fn apply_theme(&mut self, theme: &Theme) {
        let idle = to_colorref(theme.scrollbar_thumb);
        let hover = to_colorref(theme.scrollbar_thumb_hover);
        let alpha = theme.scrollbar_alpha;
        let mark_modified = to_colorref(theme.line_state_modified);
        let mark_saved = to_colorref(theme.line_state_saved);
        for bar in &mut self.bars {
            if !bar.alive() {
                continue;
            }
            bar.model.set_metrics(Metrics {
                width: theme.scrollbar_thumb_width,
                strip: theme.scrollbar_strip_width,
                fade_ms: theme.scrollbar_fade_ms,
            });
            bar.idle = idle;
            bar.hover = hover;
            bar.alpha = alpha;
            bar.mark_modified = mark_modified;
            bar.mark_saved = mark_saved;
            // Thumb width, the colours and the fade timing all live outside
            // the model, and a theme switch can change any of them without
            // moving `pos`. `apply_theme` only runs from a layout or a theme
            // change, so one repaint here costs nothing — and it is what
            // makes the `painted` cache safe to trust everywhere else.
            bar.stale = true;
        }
    }

    /// Writes one bar's scroll model. Returns `true` when the *document*
    /// moved it, which is what pops the bar back up.
    pub(super) fn set_model(&mut self, index: usize, pos: i32, range: i32, page: i32) -> bool {
        self.bars[index].model.set_model(pos, range, page)
    }

    /// Hands one bar its 修改行标记. The next [`Self::sync`] repaints it if the
    /// list actually changed; the marks land in the same row space the thumb's
    /// own range/page pair is written in.
    pub(super) fn set_marks(&mut self, index: usize, marks: Vec<Mark>) {
        self.bars[index].model.set_marks(marks);
    }

    /// Pushes both models' presence onto their windows. `reveal` says which
    /// axis the document moved on; a bar's own drag reports `false`, so it
    /// does not re-arm the countdown it is already sitting under.
    pub(super) unsafe fn sync(&mut self, reveal: (bool, bool)) {
        let (rv, rh) = reveal;
        for (bar, reveal) in self.bars.iter_mut().zip([rv, rh]) {
            if !bar.alive() {
                continue;
            }
            if reveal {
                bar.model.reveal(&mut BarCtx { bar: bar.hwnd });
            }
            // A bar exists whenever there is something to scroll: it is the
            // thin strip when idle, never a hidden window. Hiding it was what
            // made the scrollbar undiscoverable, and a hidden window also
            // hands its column back to the text — which the thin strip still
            // does, by claiming only the pixels it paints (see
            // `Scrollbar::interactive_at`).
            //
            // A strip with no area is the one case that must stay hidden: it
            // has nothing to paint and would sit in the z-order over pixels its
            // host needs. A pane dragged down to its floor produces exactly
            // that (`place` hands the vertical strip `h = 0` once the caption
            // and tab row eat the pane).
            let want = bar.model.visible() && bar.frame.w > 0.0 && bar.frame.h > 0.0;
            if want != bar.window_shown {
                bar.window_shown = want;
                let _ = ShowWindow(bar.hwnd, if want { SW_SHOW } else { SW_HIDE });
            }
            // A strip is a pure function of `BarLook`, so this is the only test
            // needed to know whether a paint would change a pixel. `sync` runs
            // from every relayout — every keystroke, every scroll, every pane
            // refresh — and an unconditional invalidation here meant four strip
            // paints per character typed, for a thumb that had not moved.
            if want && !frozen() && bar.needs_paint() {
                bar.stamp_paint();
                // While a splitter drag owns the frame the strips cannot reach
                // the screen (see `FROZEN`); asking for a paint nobody can see
                // is the one thing worse than painting it. `finish_split_drag`
                // repaints every strip when the drag ends.
                let _ = InvalidateRect(Some(bar.hwnd), None, true);
            }
        }
    }

    /// Routes one event into a bar's model, keeping the leave request armed
    /// for exactly as long as the pointer is over the bar.
    pub(super) fn route(&mut self, index: usize, ev: Event) -> Action {
        let bar = &mut self.bars[index];
        let was_over = bar.model.zone() != Zone::None;
        let a = bar.model.on_event(ev, &mut BarCtx { bar: bar.hwnd });
        match ev {
            Event::MouseMove { .. } if !was_over && bar.model.zone() != Zone::None => {
                unsafe { bar.arm_leave() };
            }
            Event::MouseLeave => bar.tracking = false,
            _ => {}
        }
        a
    }

    /// Which bar owns this window; `None` for a window that is not ours.
    pub(super) fn index_of(&self, hwnd: HWND) -> Option<usize> {
        self.bars.iter().position(|b| b.hwnd == hwnd)
    }

    pub(super) fn pos(&self, index: usize) -> i32 {
        self.bars[index].model.pos()
    }

    /// Whether this bar claims the pointer at `pos` (window-local). The host's
    /// `WM_NCHITTEST` is the only caller: everything the strip does not paint
    /// is transparent, and those pixels belong to the text under them.
    pub(super) fn interactive_at(&self, index: usize, pos: Point) -> bool {
        self.bars[index].model.interactive_at(pos)
    }

    pub(super) fn dragging(&self, index: usize) -> bool {
        self.bars[index].model.dragging()
    }

    /// 第 `index` 条滚动条在皮肤词表里的区域名。绘制和 `s:hint` 用的是同一个词。
    pub(super) fn region(&self, index: usize) -> &'static str {
        match self.bars.get(index) {
            Some(bar) if !bar.model.vertical => "scroll.h",
            _ => "scroll.v",
        }
    }

    pub(super) unsafe fn paint(&mut self, index: usize, hwnd: HWND) {
        self.bars[index].paint(hwnd);
    }

    /// Asks every strip that is on screen to repaint once. This is the thaw
    /// after a splitter drag: the strips drew nothing while frozen, so this is
    /// the paint that puts them back on the glass. The cache is stamped with
    /// what the paint is about to draw, so the `sync` that follows the drag does
    /// not ask for the same paint a second time.
    pub(super) unsafe fn repaint(&mut self) {
        for bar in &mut self.bars {
            if bar.alive() && bar.window_shown {
                bar.stamp_paint();
                let _ = InvalidateRect(Some(bar.hwnd), None, true);
            }
        }
    }
}

/// Widget context for a bar: its timers belong to the *bar* window, so the
/// fade tick arrives at `bar_proc` and no shell code has to route it.
struct BarCtx {
    bar: HWND,
}

impl WidgetCtx for BarCtx {
    fn set_timer(&mut self, id: u64, ms: u32) {
        unsafe {
            let _ = SetTimer(Some(self.bar), id as usize, ms, None);
        }
    }

    fn kill_timer(&mut self, id: u64) {
        unsafe {
            let _ = KillTimer(Some(self.bar), id as usize);
        }
    }

    fn host_command(&mut self, _cmd: HostCmd) -> Action {
        // A scrollbar never asks the host for anything: it moves a scroll
        // position, and the handler reads that back off the model.
        Action::NONE
    }
}

/// The bar's message loop: translate native input into [`Event`]s, hand
/// them to the model, let the editor state apply the result. No shell
/// routing code is involved — that is the point of the window.
unsafe extern "system" fn bar_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let Some(st) = state_of(hwnd) else {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    };
    let Some((owner, index)) = st.bar_owner(hwnd) else {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    };
    let local = Point::new(low_word(lparam) as f32, high_word(lparam) as f32);
    match msg {
        WM_PAINT => {
            if frozen() {
                // Frozen for a splitter drag: consume the update region so the
                // message is not re-delivered, but draw nothing. `finish_split_drag`
                // repaints every strip once the drag ends, and that is the paint
                // that lands.
                swallow_paint(hwnd);
            } else {
                st.paint_bar(owner, index, hwnd);
            }
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => {
            // The caret and the IME window belong to the focused window; a
            // bar that took focus would blink them out.
            LRESULT(MA_NOACTIVATE as isize)
        }
        WM_NCHITTEST => {
            // Up: the bar owns the pixels it paints. Down: they are
            // transparent, so they are the text's — which is what keeps the
            // last column of a long line, and the bottom row under the
            // horizontal strip, clickable. `HTTRANSPARENT` is how the hit
            // test continues into the window below this one.
            let mut pt = POINT {
                x: low_word(lparam) as i16 as i32,
                y: high_word(lparam) as i16 as i32,
            };
            let _ = ScreenToClient(hwnd, &mut pt);
            if st.bar_interactive(owner, index, Point::new(pt.x as f32, pt.y as f32)) {
                LRESULT(HTCLIENT as isize)
            } else {
                LRESULT(HTTRANSPARENT as isize)
            }
        }
        WM_SETCURSOR => {
            // The class has no cursor, so DefWindowProc would leave whatever
            // the editor's I-beam put on screen — the strip would look like
            // more text. It is an overlay, not a scrollbar control: answer
            // with the default arrow.
            //
            // 皮肤声明了 `cursor` 就听它的（滚动条本身只会是箭头，所以不冲突）。
            if low_word(lparam) as u32 == HTCLIENT {
                let skinned = st
                    .lua
                    .as_ref()
                    .and_then(|h| h.hint(st.panels.bars.region(index)))
                    .and_then(|s| s.cursor)
                    .and_then(crate::win32::skinhint::system_cursor);
                if let Some(cursor) = skinned.or_else(|| LoadCursorW(None, bar_cursor()).ok()) {
                    let _ = SetCursor(Some(cursor));
                }
                LRESULT(1)
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        WM_MOUSEMOVE => {
            st.bar_event(owner, index, Event::MouseMove { pos: local });
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            st.bar_event(owner, index, Event::MouseLeave);
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            // Capture without focus: the spike measured that a drag holds
            // here, which is what keeps the caret and the IME undisturbed.
            let _ = SetCapture(hwnd);
            st.bar_event(
                owner,
                index,
                Event::MouseDown {
                    pos: local,
                    button: MouseButton::Left,
                    mods: mods::NONE,
                },
            );
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            st.bar_event(
                owner,
                index,
                Event::MouseUp {
                    pos: local,
                    button: MouseButton::Left,
                    mods: mods::NONE,
                },
            );
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            // Capture taken away from under us (system dialog, alt-tab):
            // end the drag, or the thumb stays glued to a pointer we no
            // longer receive.
            if st.bar_dragging(owner, index) {
                st.bar_event(
                    owner,
                    index,
                    Event::MouseUp {
                        pos: Point::new(0.0, 0.0),
                        button: MouseButton::Left,
                        mods: mods::NONE,
                    },
                );
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            // Hovering the bar scrolls the bar, which is what hovering the
            // text does; the model ignores the off-axis wheel.
            let delta = ((wparam.0 as u32) >> 16) as i16 as i32;
            st.bar_event(
                owner,
                index,
                Event::Wheel {
                    delta,
                    horiz: false,
                    pos: local,
                    mods: mods::NONE,
                },
            );
            LRESULT(0)
        }
        WM_TIMER => {
            let id = wparam.0 as u64;
            if id == FADE_V || id == FADE_H {
                st.bar_event(owner, index, Event::Timer { id });
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// The editor state of the frame window this bar was created on.
unsafe fn state_of(hwnd: HWND) -> Option<&'static mut EditorState> {
    let root = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    if root == 0 {
        return None;
    }
    state_mut(HWND(root as *mut core::ffi::c_void))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 抠图色避开列表里用到的颜色 —— 皮肤填 `#FF00FF` 也不会变成透明洞。
    #[test]
    fn the_key_colour_avoids_what_the_skin_painted() {
        // 空列表：第一个候选就是它。
        assert_eq!(key_colour(&[]), Color::rgb(0xFF, 0x00, 0xFF));

        let magenta = UiOp::Fill {
            rect: Rect::new(0.0, 0.0, 10.0, 10.0),
            color: Color::rgb(0xFF, 0x00, 0xFF),
        };
        let key = key_colour(&[magenta]);
        assert_ne!(key, Color::rgb(0xFF, 0x00, 0xFF), "the used colour is skipped");
        assert_eq!(key.r, 0xFF);
        assert_eq!(key.g, 0x00);
        assert_eq!(key.b, 0xFE, "the blue channel steps down by one");

        // 连着的三个蓝值都被用了，就退到第四个。
        let used: Vec<UiOp> = (0..=2u8)
            .map(|i| UiOp::Fill {
                rect: Rect::new(0.0, 0.0, 1.0, 1.0),
                color: Color::rgb(0xFF, 0x00, 0xFFu8.wrapping_sub(i)),
            })
            .collect();
        assert_eq!(key_colour(&used).b, 0xFC);

        // 描边和渐变的端点也算「用到了」。
        let round = UiOp::Round {
            rect: Rect::new(0.0, 0.0, 4.0, 4.0),
            radius: 2.0,
            fill: None,
            stroke: Some(Color::rgb(0xFF, 0x00, 0xFF)),
            stroke_w: 1.0,
        };
        assert_ne!(key_colour(&[round]).b, 0xFF, "a stroke colour counts");
        let grad = UiOp::Gradient {
            rect: Rect::new(0.0, 0.0, 4.0, 4.0),
            from: Color::rgb(0, 0, 0),
            to: Color::rgb(0xFF, 0x00, 0xFF),
            dir: dg_skin::Dir::V,
        };
        assert_ne!(key_colour(&[grad]).b, 0xFF, "a gradient end counts");
    }

    /// The strip is the window the pill floats in, so the dock reserves the
    /// same number the paint works in.
    #[test]
    fn strip_is_the_theme_strip_width() {
        let mut theme = Theme::default();
        assert_eq!(OverlayBars::strip(&theme), 12.0);
        theme.scrollbar_thumb_width = 14.0;
        theme.scrollbar_strip_width = 10.0;
        assert_eq!(OverlayBars::strip(&theme), 14.0);
    }

    /// The pill rasteriser: transparent margins around a capsule of the
    /// theme colour, with the alpha premultiplied into every channel.
    #[test]
    fn strip_pixels_raster_a_translucent_capsule() {
        // A 12 x 40 strip, pill 6 x 20 at y=10: margins left/right and
        // above/below stay fully transparent.
        let mut buf = Vec::new();
        strip_pixels(
            12,
            40,
            &Rect::new(3.0, 10.0, 6.0, 20.0),
            COLORREF(0x00505050),
            0x80,
            &[],
            &mut buf,
        );
        let px = |x: usize, y: usize| {
            let i = (y * 12 + x) * 4;
            (buf[i], buf[i + 1], buf[i + 2], buf[i + 3])
        };
        assert_eq!(px(0, 20), (0, 0, 0, 0), "left margin is the text's");
        assert_eq!(px(11, 20), (0, 0, 0, 0), "right margin is the text's");
        assert_eq!(px(6, 5), (0, 0, 0, 0), "above the pill too");
        // Core of the pill: the colour at full pill alpha, premultiplied.
        let (b, g, r, a) = px(6, 20);
        assert_eq!(a, 0x80);
        assert_eq!((b, g, r), (0x28, 0x28, 0x28), "0x50 * 0x80 / 0xFF");
        // The cap rounds away: the top pixel's centre sits outside the
        // semicircle (transparent), the one below it is on the cap's skirt.
        assert_eq!(px(3, 10), (0, 0, 0, 0), "the cap's corner is empty");
        let (_, _, _, a_skirt) = px(3, 11);
        let (_, _, _, a_core) = px(6, 15);
        assert!(a_skirt < a_core, "the cap's edge is softer than the core");
        assert!(a_skirt > 0, "but the cap is not empty");
    }

    /// 修改行标记: a mark fills the pill's column at its own alpha, and the
    /// translucent pill composites *over* it — so a change the thumb has reached
    /// still reads instead of being wiped out by the capsule.
    #[test]
    fn strip_pixels_lays_the_marks_under_the_pill() {
        let mark = COLORREF(0x00102030);
        let marks = [
            (Rect::new(3.0, 4.0, 6.0, 3.0), mark, 0xE0u8),
            (Rect::new(3.0, 19.0, 6.0, 3.0), mark, 0xE0u8),
        ];
        let mut buf = Vec::new();
        strip_pixels(
            12,
            40,
            &Rect::new(3.0, 10.0, 6.0, 20.0),
            COLORREF(0x00505050),
            0x80,
            &marks,
            &mut buf,
        );
        let px = |x: usize, y: usize| {
            let i = (y * 12 + x) * 4;
            (buf[i], buf[i + 1], buf[i + 2], buf[i + 3])
        };
        // Clear of the pill: the mark alone, premultiplied at its own alpha.
        // `COLORREF(0x00102030)` is R=0x30, G=0x20, B=0x10, and the buffer is BGRA.
        assert_eq!(px(6, 5), (0x0E, 0x1C, 0x2A, 0xE0));
        // Beside it: still the text's pixels.
        assert_eq!(px(0, 5), (0, 0, 0, 0));
        // Under the pill: the same mark plus the capsule over it, which is more
        // blue than the pill on its own (0x28) and more opaque than either.
        let (b_over, _, _, a_over) = px(6, 20);
        assert!(
            b_over > 0x28 && a_over > 0xE0,
            "the pill has to let the mark through: {b_over:#x} {a_over:#x}"
        );
    }

    /// A strip that has been painted is not painted again until something it
    /// draws with changes. `sync` runs after *every* mouse message over the
    /// strip and from every relayout — every keystroke, every scroll — so
    /// without this a pointer moving along a visible bar repainted it (and its
    /// sibling) once per `WM_MOUSEMOVE`, and typing repainted four strips per
    /// character to show a thumb that had not moved a pixel.
    #[test]
    fn a_still_strip_does_not_ask_for_a_repaint() {
        let mut bar = BarWindow::new(true);
        bar.model.layout(Rect::new(0.0, 0.0, 12.0, 400.0));
        bar.model.set_model(40, 200, 20);
        bar.model.reveal(&mut BarCtx {
            bar: HWND::default(),
        });
        assert!(bar.needs_paint(), "nothing painted yet");
        bar.stamp_paint();
        assert!(!bar.needs_paint(), "same look, same pixels");
        assert!(!bar.needs_paint(), "and it stays quiet");
    }

    /// Every input the paint reads has to be part of the key, or a change would
    /// be swallowed and the strip would show a stale thumb.
    #[test]
    fn every_input_the_paint_reads_can_wake_the_strip() {
        let fresh = || {
            let mut bar = BarWindow::new(true);
            bar.model.layout(Rect::new(0.0, 0.0, 12.0, 400.0));
            bar.model.set_model(40, 200, 20);
            bar.model.reveal(&mut BarCtx {
                bar: HWND::default(),
            });
            bar.stamp_paint();
            bar
        };
        // pos: the document scrolled
        let mut bar = fresh();
        bar.model.set_model(41, 200, 20);
        assert!(bar.needs_paint(), "pos");
        // range: the document grew
        let mut bar = fresh();
        bar.model.set_model(40, 260, 20);
        assert!(bar.needs_paint(), "range");
        // page: the view got taller, so the thumb did too
        let mut bar = fresh();
        bar.model.set_model(40, 200, 30);
        assert!(bar.needs_paint(), "page");
        // hover: the pill recolours (the geometry does not move)
        let mut bar = fresh();
        bar.model.on_event(
            Event::MouseMove {
                pos: Point::new(6.0, 40.0),
            },
            &mut BarCtx {
                bar: HWND::default(),
            },
        );
        assert!(bar.needs_paint(), "hover zone");
        // theme: new colours or a new resting alpha re-read the pixels
        let mut bar = fresh();
        bar.idle = COLORREF(0x00123456);
        assert!(bar.needs_paint(), "idle colour");
        let mut bar = fresh();
        bar.alpha = bar.alpha.wrapping_add(1);
        assert!(bar.needs_paint(), "alpha");
        let mut bar = fresh();
        bar.mark_saved = COLORREF(0x0000FF00);
        assert!(bar.needs_paint(), "mark colour");
        // 修改行标记: a document whose changed lines moved has to redraw too
        let mut bar = fresh();
        bar.model.set_marks(vec![Mark {
            start: 10,
            end: 12,
            state: 1,
        }]);
        assert!(bar.needs_paint(), "marks");
        // and handing over the same list again must not
        let mut bar = fresh();
        bar.model.set_marks(vec![Mark {
            start: 10,
            end: 12,
            state: 1,
        }]);
        bar.stamp_paint();
        bar.model.set_marks(vec![Mark {
            start: 10,
            end: 12,
            state: 1,
        }]);
        assert!(!bar.needs_paint(), "the same marks are the same pixels");
        // and anything that bypasses the model outright
        let mut bar = fresh();
        bar.stale = true;
        assert!(bar.needs_paint(), "stale");
    }

    /// A drag freezes the strips: while the frame is locked nothing they draw
    /// can reach the screen, so `sync` stops asking and `WM_PAINT` is closed
    /// without drawing. A change that happens during the drag must still be
    /// owed after it, or the thaw would leave a stale thumb on the glass.
    #[test]
    fn a_splitter_drag_freezes_the_strips_until_the_thaw() {
        let mut bar = BarWindow::new(true);
        bar.model.layout(Rect::new(0.0, 0.0, 18.0, 400.0));
        bar.model.set_model(40, 200, 20);
        bar.stamp_paint();
        assert!(!bar.needs_paint());

        freeze(true);
        assert!(frozen());
        // The document scrolls under the frozen frame.
        bar.model.set_model(90, 200, 20);
        assert!(bar.needs_paint(), "the thaw still owes the paint");

        freeze(false);
        assert!(!frozen());
        assert!(bar.needs_paint(), "and it is still owed once thawed");
        bar.stamp_paint();
        assert!(!bar.needs_paint());
    }

    /// The strip is an overlay on the text, not a scrollbar control: it takes
    /// the default arrow, so pointing at it looks exactly like pointing at the
    /// document. `WM_SETCURSOR` itself is claimed by the bar's message loop
    /// (the live probe reads that answer back); this is the shape it applies.
    #[test]
    fn a_strip_keeps_the_default_arrow() {
        use windows::Win32::UI::WindowsAndMessaging::{GetCursor, IDC_ARROW, SetCursor};
        unsafe {
            assert_eq!(bar_cursor().0, IDC_ARROW.0);
            let cursor = LoadCursorW(None, bar_cursor()).expect("system cursor");
            let _ = SetCursor(Some(cursor));
            assert_eq!(GetCursor(), cursor);
        }
    }

    /// 每帧重建表面 vs 复用。只算 DIB/DC 的建与毁（`UpdateLayeredWindow` 要真
    /// 窗口，不算在内），数字用 `--nocapture` 才看得到，不写进断言。
    #[test]
    fn a_reused_surface_costs_less_than_a_rebuilt_one() {
        const N: u32 = 400;
        const W: i32 = 12;
        const H: i32 = 800;
        unsafe {
            let mut surface = Surface::empty();
            assert!(surface.ensure(W, H));
            let t0 = std::time::Instant::now();
            for _ in 0..N {
                surface.release();
                assert!(surface.ensure(W, H));
            }
            let rebuild = t0.elapsed().as_nanos() as f64 / N as f64;
            let t1 = std::time::Instant::now();
            for _ in 0..N {
                assert!(surface.ensure(W, H));
            }
            let reuse = t1.elapsed().as_nanos() as f64 / N as f64;
            eprintln!("[overlay] rebuild {rebuild:.0} ns, reuse {reuse:.0} ns");
            assert!(reuse < rebuild, "reuse has to be cheaper than a rebuild");
        }
    }
}
