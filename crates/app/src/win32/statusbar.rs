//! The status strip as a native `msctls_statusbar32`. Why the common control and not another
//! pass over the shared [`dg_render::DrawList`]: the strip is a row of *parts*, and a part
//! is something the shell already knows how to make, resize, hit-test and hand a size grip
//! to. Drawing it by hand meant this project kept its own copy of that geometry — twelve
//! rectangles laid out from the model, a `widget_at` branch for the strip, a click
//! dispatcher, a hover flag — while the editor above it had already moved to a window of its
//! own (see [`crate::win32::view`]). On a hit a `WM_COMMAND` to the parent carrying
//! `MAKEWPARAM(2000, pane id)`. The control's own click notification names the *control*,
//! not the pane, which is the entire reason the subclass exists. * the subclass intercepts
//! `WM_ERASEBKGND` and returns 1, so the control never erases with the system face colour.
//! The one deliberate divergence is colour. The control fills its client with
//! `COLOR_BTNFACE` inside `WM_PAINT` — measured as `#F0F0F0` on this strip, after a
//! `WM_ERASEBKGND` pass that it simply paints over — and there is no message that changes
//! it. So the strip's `WM_PAINT` is answered here instead: the theme background, the pane
//! texts and the grip are drawn from `Theme`, while the parts geometry, the click hit-test
//! and the corner's `HTBOTTOMRIGHT` stay the control's. This implementation asks the system
//! for the per-DPI UI font so a Chinese or English desktop gets its own face at the right
//! size, and sets it on the control with `WM_SETFONT` — comctl32 also derives the strip's
//! height and borders from it. The left text inset is the control's 2 px border
//! (`SB_GETBORDERS`'s first value at 96 DPI), not a character advance. Non-pane clicks are
//! forwarded instead of swallowed strip covers the frame's bottom-right corner, and the
//! grip's `HTBOTTOMRIGHT` answer is what keeps that corner resizable.
#![allow(unsafe_op_in_unsafe_fn)]

use crate::win32::dpi::scaled;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DEFAULT_GUI_FONT, DeleteDC,
    DeleteObject, EndPaint, FillRect, GetDC, GetStockObject, GetTextExtentPoint32W, HDC, HFONT,
    HGDIOBJ, InvalidateRect, PAINTSTRUCT, PtInRect, ReleaseDC, SRCCOPY, SelectObject,
    SetBkMode, SetTextColor, TRANSPARENT, TextOutW,
};
use windows::Win32::UI::Controls::{
    ICC_BAR_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, SB_GETPARTS, SB_GETRECT,
    SB_SETMINHEIGHT, SB_SETPARTS, SB_SETTEXTW, SBT_NOBORDERS, SBT_OWNERDRAW, WM_MOUSELEAVE,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GWLP_WNDPROC, GetClientRect, GetParent, GetSystemMetrics,
    GetWindowRect, HTCLIENT, HMENU, KillTimer, SBS_SIZEGRIP, SM_CXEDGE, SM_CXVSCROLL, SWP_NOACTIVATE,
    SWP_NOZORDER, SendMessageW, SetTimer, SetWindowLongPtrW, SetWindowPos, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_COMMAND, WM_ERASEBKGND, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_MOUSEMOVE,
    WM_PAINT, WM_RBUTTONDOWN, WM_SETFONT, WM_SETCURSOR, WM_TIMER, WNDPROC, WS_CHILD,
    WS_CLIPCHILDREN,
    WS_CLIPSIBLINGS, WS_VISIBLE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::core::{PCWSTR, w};

use crate::win32::view::instance;
use crate::win32::window::{EditorState, high_word, low_word, state_mut, to_colorref};
use dg_render::Rect;
use dg_render::theme::{Color, Theme};
use dg_ui::statusbar::{
    HexStatus, Selection, StatusBar, StatusModel, StatusPane, encoding_label, pane_width,
};

/// The window's own id is different — the frame creates it as `0xE801` — but the click
/// notification deliberately names `2000`.
pub(super) const CTRL_ID: u32 = 2000;

/// 皮肤模式下 part 的 id 从这里往上数。`StatusField::pane_id()` 最大是 14，
/// 高过它就不可能撞上；`WM_COMMAND` 那一侧靠这个位判断「这是脚本段」。
///
/// 为什么还要走 `WM_COMMAND`：那是这条状态栏**已经有的**契约 —— 点一下等于
/// 往 frame 发一条 `MAKEWPARAM(2000, id)`，进程外一条 `PostMessageW` 就能复现。
/// 另开一条「子类里直接跑命令」的路会让两套点击行为慢慢分叉。
pub(super) const SCRIPT_BASE: u32 = 0x8000;

/// 脚本回调注册用的区域名，与 `dg_skin::Region::StatusPane` 同一个词。
const SKIN_REGION: &str = "status.pane";

const WINDOW_ID: u32 = 0xE801;

/// The class name of the common control; no registration needed, comctl32
/// owns it.
const STATUS_CLASS: PCWSTR = w!("msctls_statusbar32");

// 文字左边距（96 DPI 下 2 px，也就是控件自己那条边框）现在是皮肤度量
// `status.pad`，默认值等于这里原来那个常量。
/// How far the divider tick is inset from the top of the strip. Start
/// about 5 px below the top edge at 125%.
const DIVIDER_INSET: i32 = 4;

const BLINK_TIMER_ID: usize = 1;
/// 通知脉冲的一帧。40 ms（25 fps）够顺，一条二十来像素高的带子也画得起：
/// 整段 2 s 一共 50 帧，每帧一次离屏重画加一次 blit。
const BLINK_INTERVAL_MS: u32 = 40;
/// 一口气的时长。两口气连起来 2 s —— 和原来四次反色一样长，只是不再是硬翻。
const BLINK_PULSE_MS: u32 = 1000;
const BLINK_PULSES: u32 = 2;
/// 峰值不透明度（0..=255）。0x6B 约 42%：随包主题（`Selection=#296899`）里
/// 消息格从 `#252526` 挪到 `#274156` —— 看得出变了色，又不像反色那样整格发白。
const BLINK_PEAK_ALPHA: u8 = 0x6B;

/// 皮肤那条里带 `update` 回调的段要多久重算一次文字。
///
/// 500 ms 是「秒表够用、又不至于让人看见跳字」的那个点：一次重算 = 每段一次
/// 文字测量 + 有回调的那几段各进一次 Lua。全模板/全内置的那条根本不装这个
/// 定时器（见 [`StatusWin::row_interval`]）。
const ROW_TIMER_ID: usize = 2;
const ROW_INTERVAL_MS: u32 = 500;

fn to_wndproc(v: isize) -> WNDPROC {
    if v == 0 {
        None
    } else {
        // A window procedure is a code address: the same width as `isize` on
        // every platform this backend builds for.
        Some(unsafe { std::mem::transmute_copy(&v) })
    }
}

/// 脉冲走到 `elapsed_ms` 时的不透明度：每 [`BLINK_PULSE_MS`] 一口气，`sin²` 起落。
///
/// 用 `sin²` 而不是方波或三角波：两端导数为 0，所以亮起来和退回去都是「停一下
/// 再走」，看上去是呼吸。原来的做法是整格 `InvertRect` —— 深色主题里一帧从
/// `#252526` 跳到近白，一秒钟跳两回，这才是刺眼的来源。
fn pulse_alpha(elapsed_ms: u32) -> u8 {
    let phase = (elapsed_ms % BLINK_PULSE_MS) as f32 / BLINK_PULSE_MS as f32;
    let wave = (std::f32::consts::PI * phase).sin();
    (wave * wave * BLINK_PEAK_ALPHA as f32).round() as u8
}

/// 把 `base` 往 `glow` 挪 `alpha`/255，得到脉冲那一刻的底色。
fn pulse_color(base: Color, glow: Color, alpha: u8) -> Color {
    let t = alpha as f32 / 255.0;
    let mix = |from: u8, to: u8| (from as f32 + (to as f32 - from as f32) * t).round() as u8;
    Color::rgb(mix(base.r, glow.r), mix(base.g, glow.g), mix(base.b, glow.b))
}

/// The strip window, plus the two things the control cannot hold for itself.
pub(super) struct StatusWin {
    hwnd: HWND,
    /// Last rect handed to `SetWindowPos`, so a layout that changes nothing
    /// costs no window message.
    frame: Rect,
    /// The control's own window procedure, saved when this module installed
    /// its subclass.
    ///
    /// It cannot live in the strip's `GWLP_USERDATA` the way the editor view's
    /// state pointer does: a common control keeps its internal structure in
    /// that slot, and overwriting it is a crash rather than a trade-off.
    orig: WNDPROC,
    /// The pane table currently in the control, in part order. Both drawing
    /// and hit-testing read ids from here instead of reading them back.
    panes: Vec<StatusPane>,
    /// Last `SB_SETPARTS` array.
    parts: Vec<i32>,
    /// The control's own natural height, measured after creation. The common
    /// control enforces it regardless of the height it is given — 23 px at
    /// 96 DPI, 29 at 120 — and the dock has to reserve exactly this or the
    /// strip covers the bottom of the editor at fractional scales.
    band: f32,
    /// The system UI font for the current DPI, also set on the control with
    /// `WM_SETFONT`. Null until [`StatusWin::ensure_font`] runs.
    font: HFONT,
    /// DPI `font` was built for; a monitor change rebuilds it.
    font_dpi: i32,
    /// 脉冲走到第几毫秒；0 表示没在跑。
    blink_ms: u32,
    blink_armed: bool,
    /// 这一帧垫在消息格底下的不透明度（0 = 不垫）。脉冲走绘制而不是直接改屏 ——
    /// 叠在屏幕上的那几秒里，任何一次重画（行列变化、悬停、皮肤）都会把它
    /// 冲掉，看上去就是状态栏自己在不规则地闪。
    blink_alpha: u8,
    /// 皮肤脚本（M2）：这一格的绘制交给它。`Rc` 共享而不是借用 frame ——
    /// 绘制期 `EditorState` 可能正被 `state_mut` 借走，重入是硬约束。
    script: Option<std::rc::Rc<dg_script::ScriptHost>>,
    /// 指针停在哪一格（`ctx.hot`）。comctl32 的状态栏没有热跟踪，也没有悬停
    /// 消息 —— 这里自己 `WM_MOUSEMOVE` 命中测试 + `TME_LEAVE` 清位，和工具栏、
    /// 停靠页签一条路子。
    hot: Option<usize>,
    /// 已经向系统要过 `WM_MOUSELEAVE`；离开时清掉，下次进来再要一次。
    tracking: bool,
    /// 皮肤要的指针形状（`s:hint("status.pane", {cursor=…})`）。提示不在这里 ——
    /// 状态栏的提示归 comctl32（文字放不下时自己弹），皮肤再挂一条就是两个框。
    hint: crate::win32::skinhint::SkinHint,
    /// 皮肤声明的整条（`s:status{...}`）。`Some` = **这一条由皮肤说了算**：内置
    /// 那十三个字段一个都不画，位置、宽度、文字全按声明来。`None` = 内置那条。
    ///
    /// 存一份而不是每次排版去问 Lua：`apply` 每次光标移动都跑，问一趟就是十几次
    /// 表读加字符串拷贝。皮肤重载时由 [`Self::reload_row`] 重取。
    skin: Option<Vec<dg_skin::Segment>>,
    /// 这一帧的模板展开快照（`%l` / `%f` / `%M`……）。
    facts: dg_skin::statusline::Facts,
    /// 「自己醒一次」的定时器已经装上了没。
    row_armed: bool,
    /// 段表换过、但还没重新铺到控件上。
    ///
    /// `reload_row` 会把 `panes` / `parts` 清空，指望下一次 `refresh` 重新算；
    /// 可 `refresh` 只在「模型变了」或「这条有 `update` 回调」时才推。推一句
    /// 静态文字恰好会把回调摘掉（见 `ui.layout:update`），于是重取之后既没有
    /// 模型变化也没有回调 —— 控件里 part 还在、`panes` 却是空的，画出来是一
    /// 条空白栏。这个位就是「重取过，必须无条件推一次」。
    stale: bool,
}

impl StatusWin {
    pub(super) fn new() -> Self {
        Self {
            hwnd: HWND::default(),
            frame: Rect::new(0.0, 0.0, 0.0, 0.0),
            orig: None,
            panes: Vec::new(),
            parts: Vec::new(),
            band: dg_ui::statusbar::HEIGHT,
            font: HFONT::default(),
            font_dpi: 0,
            blink_ms: 0,
            blink_armed: false,
            blink_alpha: 0,
            script: None,
            hot: None,
            tracking: false,
            hint: crate::win32::skinhint::SkinHint::new(),
            skin: None,
            facts: dg_skin::statusline::Facts::default(),
            row_armed: false,
            stale: false,
        }
    }

    /// 装上皮肤脚本宿主。frame 建完这条状态栏就调一次。
    pub(super) fn set_script(&mut self, host: Option<std::rc::Rc<dg_script::ScriptHost>>) {
        self.script = host;
        self.reload_row();
    }

    /// 重取皮肤声明的那一条。皮肤热重载后调一次 —— 加/删/改一段不用重开进程。
    ///
    /// 返回是否真的有变化，调用方据此决定要不要立刻重排。
    pub(super) fn reload_row(&mut self) -> bool {
        let next = self.script.as_ref().map(|h| h.status()).filter(|s| !s.is_empty());
        if next == self.skin {
            return false;
        }
        self.skin = next;
        // 段表变了，控件里的 part 数也得跟着变：下一次 `apply` 会重发
        // `SB_SETPARTS`，但没人叫它。清掉缓存让 `apply` 认出「几何变了」。
        self.parts.clear();
        self.panes.clear();
        self.stale = true;
        self.arm_row_timer();
        true
    }

    pub(super) fn alive(&self) -> bool {
        !self.hwnd.0.is_null()
    }

    /// The strip's own window, for the full-screen toggle
    /// (the full-screen toggle) to hide and show it.
    pub(super) fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Creates the control and subclasses it. The strip shares no pixel with
    /// the editor or with an overlay bar, so its position in the sibling order
    /// is free.
    pub(super) unsafe fn create(&mut self, parent: HWND) {
        // comctl32 registers the status class, but only once it has been told
        // which classes the app wants; without this the window below silently
        // fails to exist.
        let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            // The structure is versioned by its own size.
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_BAR_CLASSES,
        });
        if self.alive() {
            return;
        }
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            STATUS_CLASS,
            w!("status"),
            // The grip is what answers `HTBOTTOMRIGHT` for the corner this
            // window covers.
            WS_CHILD
                | WS_VISIBLE
                | WS_CLIPSIBLINGS
                | WS_CLIPCHILDREN
                | WINDOW_STYLE(SBS_SIZEGRIP as u32),
            0,
            0,
            0,
            0,
            Some(parent),
            Some(HMENU(WINDOW_ID as usize as *mut core::ffi::c_void)),
            Some(instance()),
            None,
        ) else {
            return;
        };
        self.orig = to_wndproc(SetWindowLongPtrW(
            hwnd,
            GWLP_WNDPROC,
            status_proc as *const () as isize,
        ));
        self.hwnd = hwnd;
        self.reband();
    }

    /// The height the dock has to reserve for this strip, in pixels. The
    /// common control ignores the height it is asked for and keeps its own
    /// natural one, so the value is read back from the control rather than
    /// assumed.
    pub(super) fn band_h(&self) -> f32 {
        self.band
    }

    /// Forces the control to evaluate its natural height at the current DPI
    /// and records it. Called once at creation and again on `WM_DPICHANGED`.
    pub(super) unsafe fn reband(&mut self) -> f32 {
        if !self.alive() {
            return self.band;
        }
        // The font has to be on the control before the height is requested:
        // until `WM_SETFONT`, comctl32 locks the strip to its 24 px fontless
        // minimum and ignores every SetWindowPos height.
        self.ensure_font();
        // Ask for the designed height at this DPI (23 px at 96, 29 at 120).
        // A skin can name one instead (`[metrics] status.height`); 0 means "leave
        // the control alone", and that path stays byte-identical to before —
        // `SB_SETMINHEIGHT` changes the drawing area inside the same window rect,
        // so sending it unconditionally would move the text by a pixel.
        let skin_h = crate::skin::metrics().get(dg_skin::MetricId::StatusHeight);
        let dip = match skin_h {
            h if h > 0.0 => {
                let dip = h.round() as i32;
                let nominal = scaled(dip, self.dpi());
                let _ = SendMessageW(
                    self.hwnd,
                    SB_SETMINHEIGHT,
                    Some(WPARAM(nominal as usize)),
                    Some(LPARAM(0)),
                );
                dip
            }
            _ => dg_ui::statusbar::HEIGHT.round() as i32,
        };
        let nominal = scaled(dip, self.dpi());
        let _ = SetWindowPos(
            self.hwnd,
            None,
            0,
            0,
            100,
            nominal,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
        let mut rc = RECT::default();
        let _ = GetWindowRect(self.hwnd, &mut rc);
        let h = rc.bottom - rc.top;
        if h > 0 {
            self.band = h as f32;
        }
        // `WM_DPICHANGED` calls this before the next layout measures text.
        self.band
    }

    /// Moves the window to `frame` (frame-client coordinates).
    pub(super) unsafe fn place(&mut self, frame: Rect) {
        if !self.alive() || self.frame == frame {
            return;
        }
        self.frame = frame;
        let _ = SetWindowPos(
            self.hwnd,
            None,
            frame.x.round() as i32,
            frame.y.round() as i32,
            frame.w.round() as i32,
            frame.h.round() as i32,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
        // The control may have overridden the requested height; keep the
        // band the dock reserves in step with what it actually occupies.
        let mut rc = RECT::default();
        if GetWindowRect(self.hwnd, &mut rc).is_ok() {
            let h = (rc.bottom - rc.top) as f32;
            if h > 0.0 {
                self.band = h;
            }
        }
    }

    /// Asks the window itself how wide it is, because `place` handed over a
    /// request: rounding and a layout that has not run yet both make the
    /// answer differ, and the parts have to be sized against the pixels that
    /// actually exist.
    pub(super) fn client(&self) -> (i32, i32) {
        if !self.alive() {
            return (0, 0);
        }
        let mut rc = RECT::default();
        let _ = unsafe { GetClientRect(self.hwnd, &mut rc) };
        (rc.right - rc.left, rc.bottom - rc.top)
    }

    fn dpi(&self) -> i32 {
        if self.alive() {
            unsafe { GetDpiForWindow(self.hwnd) }.max(96) as i32
        } else {
            96
        }
    }

    /// 量一格宽度的三个数，按 `[metrics]` 和 DPI 折成像素。
    ///
    /// **`status.pad` 同时是画的内缩和量的预留**：内置画法把文字放在
    /// `rc.left + scaled(pad)`，所以宽度要留两份（左右各一份）。以前这里写死
    /// `extent + 4` 像素 —— 96 DPI 下正好等于 2 DIP × 2，换个 DPI 就偏，皮肤
    /// 画得更宽（示例皮肤内缩 9 DIP）时偏得更多，最后被 `ellipsis` 吃掉半截。
    ///
    /// 另外两个度量（图标位、下限）以前是**声明了但没人读**的死键，这里一并接上。
    fn width_rules(&self, dpi: i32) -> dg_ui::statusbar::WidthRules {
        let m = crate::skin::metrics();
        // 这里**不用** `scaled`：它的 `.max(1)` 是给「画一条至少 1 px 的
        // 线」准备的，宽度不行 —— `status.pad = 0` 是「不要边距」，留 1 px 就
        // 变成量的比画的宽一点，永远差一像素。
        let px = |dip: f32| (dip * dpi as f32 / 96.0).round() as i32;
        dg_ui::statusbar::WidthRules {
            text_pad: px(m.get(dg_skin::MetricId::StatusPad) * 2.0),
            icon_pad: px(m.get(dg_skin::MetricId::StatusIconPad)),
            min_width: px(m.get(dg_skin::MetricId::StatusMinWidth)).max(1),
        }
    }

    /// 字体换了：把「这只字体是哪档 DPI 造的」这个记号抹掉，下一次
    /// [`Self::ensure_font`] 就会重造并 `WM_SETFONT`。
    ///
    /// 不在这里删句柄：删掉而没人重设，控件会退回系统字体，那比旧字体更难查。
    ///
    /// 状态栏多高不是 `[Metrics]` 直接说了算，是 comctl32 在 `WM_SETFONT` 之后
    /// 报的那个数 —— 所以换了字体之后还得跟着走一遍 [`Self::reband`]，光改静态
    /// 表是没用的。
    pub(super) fn drop_font(&mut self) {
        self.font_dpi = 0;
    }

    /// Builds (or rebuilds) the strip's font for the current DPI and hands it
    /// to the control with `WM_SETFONT`. The face/size is the system UI font
    /// (9 pt Segoe UI, Microsoft YaHei UI on a Chinese system). `WM_SETFONT`
    /// is not optional: a fontless comctl32 status bar is locked to its 24 px
    /// minimum and ignores `SetWindowPos` heights, so the 29 px reference
    /// band is unreachable without it. Font handles are not marshalled across
    /// processes. `SPI_GETNONCLIENTMETRICS` reports the metrics but returns
    /// zeroed font fields on this system, so its `lfMessageFont` is only
    /// trusted when it actually carries a height; the icon-title LOGFONT is
    /// the fallback and is the same 9 pt UI face.
    unsafe fn ensure_font(&mut self) {
        if !self.alive() {
            return;
        }
        let dpi = self.dpi();
        if self.font_dpi == dpi && !self.font.0.is_null() {
            return;
        }
        if !self.font.0.is_null() {
            let _ = DeleteObject(self.font.into());
            self.font = HFONT::default();
        }
        self.font = crate::win32::uifont::create_for(dg_skin::FontToken::Status, dpi);
        self.font_dpi = dpi;
        if !self.font.0.is_null() {
            let _ = SendMessageW(
                self.hwnd,
                WM_SETFONT,
                Some(WPARAM(self.font.0 as usize)),
                Some(LPARAM(1)),
            );
        }
    }

    /// The font to select into a DC: the scaled one when it exists, the
    /// stock object otherwise.
    unsafe fn font_obj(&self) -> HGDIOBJ {
        if self.font.0.is_null() {
            GetStockObject(DEFAULT_GUI_FONT)
        } else {
            HGDIOBJ(self.font.0)
        }
    }

    fn grip_width(&self) -> i32 {
        unsafe { GetSystemMetrics(SM_CXEDGE) + GetSystemMetrics(SM_CXVSCROLL) }
    }

    /// Every later pane is the previous edge plus its own width. Clamped at
    /// zero rather than allowed to go negative: `SB_SETPARTS` rejects a
    /// non-increasing array by falling back to one full-width part, which on a
    /// narrow window would hide every pane instead of clipping the last one.
    fn parts_of(widths: &[i32], client_w: i32, grip: i32) -> Vec<i32> {
        let mut parts = vec![0i32; widths.len()];
        if parts.is_empty() {
            return parts;
        }
        let sum: i32 = widths.iter().skip(1).copied().sum();
        parts[0] = (client_w - sum - grip).max(0);
        for (i, width) in widths.iter().enumerate().skip(1) {
            parts[i] = parts[i - 1] + (*width).max(1);
        }
        parts
    }

    /// The status-pane width routine's width pass: measure each pane with the
    /// strip's per-DPI UI font, then apply the padding and minimum from
    /// [`StatusWin::width_rules`] (defaults 4 / 36 / 45 px at 96 DPI). A pane
    /// that changes on every caret move is measured against its base sample as
    /// well (`StatusBar::default_text`), so a number gaining a digit does not
    /// slide its neighbours; a pane with settled text — the syntax, the
    /// encoding, the edit mode — keeps sizing to what it prints.
    unsafe fn measure(&self, table: &StatusBar, panes: &[StatusPane]) -> Vec<i32> {
        let mut widths = vec![0i32; panes.len()];
        if !self.alive() {
            return widths;
        }
        let dc = GetDC(Some(self.hwnd));
        if dc.0.is_null() {
            return widths;
        }
        let rules = self.width_rules(self.dpi());
        let old = SelectObject(dc, self.font_obj());
        for (slot, pane) in widths.iter_mut().zip(panes.iter()) {
            let base = table.default_text(pane.id);
            let mut extent = 0;
            for text in std::iter::once(pane.text.as_str()).chain(base.as_deref()) {
                let utf16: Vec<u16> = text.encode_utf16().collect();
                if utf16.is_empty() {
                    continue;
                }
                let mut measured = utf16;
                measured.push(0);
                let mut size = SIZE::default();
                let _ = GetTextExtentPoint32W(dc, &measured, &mut size);
                extent = extent.max(size.cx);
            }
            *slot = pane_width(extent, pane.id, rules);
        }
        SelectObject(dc, old);
        let _ = ReleaseDC(Some(self.hwnd), dc);
        widths
    }

    /// Hands the model's pane table to the control.
    ///
    /// Returns whether the strip has to be repainted, which is when either the
    /// geometry or a text changed: a caret move repaints the line and column
    /// panes, an idle frame sends nothing.
    pub(super) unsafe fn apply(&mut self, table: &StatusBar, client_w: i32) -> bool {
        if !self.alive() {
            return false;
        }
        self.ensure_font();
        // 皮肤接管了整条：位置、宽度、文字都按声明来。`take` 而不是借用 ——
        // 下面要 `&mut self`，而 `self.skin` 是同一个结构体的字段。
        if let Some(segs) = self.skin.take() {
            let changed = self.apply_script(&segs, table, client_w);
            self.skin = Some(segs);
            // 内置那条在这一支的末尾会 `invalidate`，脚本这条以前只把 `true`
            // 交回去就完事 —— 于是「推了一句新文字」只改了数据，屏幕上还是
            // 上一帧。状态栏是自绘的，没人重画就没人看见。
            if changed {
                self.invalidate();
            }
            return changed;
        }
        let panes = table.panes();
        let panes = panes.as_slice();
        let widths = self.measure(table, panes);
        let parts = Self::parts_of(&widths, client_w, self.grip_width());
        let geometry = parts != self.parts;
        let identity = panes.len() != self.panes.len()
            || panes
                .iter()
                .zip(self.panes.iter())
                .any(|(a, b)| a.id != b.id);
        let text = panes.len() != self.panes.len()
            || panes
                .iter()
                .zip(self.panes.iter())
                .any(|(a, b)| a.text != b.text);
        if geometry {
            let _ = SendMessageW(
                self.hwnd,
                SB_SETPARTS,
                Some(WPARAM(panes.len())),
                Some(LPARAM(parts.as_ptr() as isize)),
            );
            self.parts = parts;
        }
        if identity {
            // The ids travel with the parts. `SBT_OWNERDRAW` is what makes the
            // control treat the string pointer as item data instead of reading
            // it as text — the control's part index is not a pane id.
            // 只有段表本身换了才重推：这一句按格号一问一答，条数不少，而段的
            // id 里不含宽度，单纯把窗体拉宽并不欠它一次。
            for (i, pane) in panes.iter().enumerate() {
                let _ = SendMessageW(
                    self.hwnd,
                    SB_SETTEXTW,
                    Some(WPARAM(
                        ((i as u32) | SBT_OWNERDRAW | SBT_NOBORDERS) as usize,
                    )),
                    Some(LPARAM(pane.id as isize)),
                );
            }
        }
        self.panes = panes.to_vec();
        if geometry || text {
            self.invalidate();
            return true;
        }
        false
    }

    /// 皮肤声明的那一条：按 `width` 规则排版、按 `priority` 丢段、余量给 `fill`。
    ///
    /// 丢掉的段**根本不进 part 数组** —— 不是宽度给 0 留在那儿。少一个 part 的
    /// 右侧邻居会自己左移，而宽度 0 的 part 会留下一条点得到、画不出的缝。
    unsafe fn apply_script(
        &mut self,
        segs: &[dg_skin::Segment],
        table: &StatusBar,
        client_w: i32,
    ) -> bool {
        let dpi = self.dpi();
        let texts: Vec<String> = segs.iter().map(|s| self.segment_text(s, table)).collect();
        let measured = self.measure_segments(segs, &texts, table);
        let avail = (client_w - self.grip_width()).max(0);
        let slots = dg_skin::statusline::layout(segs, &measured, avail, dpi);
        // 段号是 `s:status` 里的位置，不是显示位置 —— 一段被丢掉再回来，它的
        // 绘制缓存还得命中同一条。
        let mut panes: Vec<StatusPane> = Vec::new();
        let mut widths: Vec<i32> = Vec::new();
        for (i, (seg, slot)) in segs.iter().zip(slots.iter()).enumerate() {
            if !slot.shown {
                continue;
            }
            let field = dg_ui::statusbar::PANES
                .iter()
                .copied()
                .find(|f| f.name() == seg.id)
                .unwrap_or(dg_ui::statusbar::StatusField::Message);
            panes.push(StatusPane {
                field,
                id: SCRIPT_BASE + i as u32,
                text: texts[i].clone(),
                name: seg.id.clone(),
                click: seg.click.clone(),
            });
            widths.push(slot.width);
        }
        let parts = Self::parts_of(&widths, client_w, self.grip_width());
        let geometry = parts != self.parts;
        let text = panes.len() != self.panes.len()
            || panes
                .iter()
                .zip(self.panes.iter())
                .any(|(a, b)| a.text != b.text || a.name != b.name);
        if geometry {
            let _ = SendMessageW(
                self.hwnd,
                SB_SETPARTS,
                Some(WPARAM(panes.len())),
                Some(LPARAM(parts.as_ptr() as isize)),
            );
            self.parts = parts;
        }
        if panes.len() != self.panes.len()
            || panes
                .iter()
                .zip(self.panes.iter())
                .any(|(a, b)| a.id != b.id)
        {
            for (i, pane) in panes.iter().enumerate() {
                let _ = SendMessageW(
                    self.hwnd,
                    SB_SETTEXTW,
                    Some(WPARAM(((i as u32) | SBT_OWNERDRAW | SBT_NOBORDERS) as usize)),
                    Some(LPARAM(pane.id as isize)),
                );
            }
        }
        self.panes = panes;
        geometry || text
    }

    /// 一段的文字。三条来源，按优先级：
    ///
    /// 1. `update(ctx)` —— 脚本算的（时钟、计数器、`ctx.doc` 里的任何东西）。
    /// 2. `text` 模板 —— `%l:%c` 这种，宿主展开。
    /// 3. 内置字段 —— 按 id 查 `就绪` / `4.2 KB` / `U+4E2D`。模板表达不了这些
    ///    （要查语言表、要格式化字节数），所以继承来的格走这一条。
    ///
    /// 三条都不成立就是空串 —— 一格空着，比画一个「%q」上去好。
    fn segment_text(&self, seg: &dg_skin::Segment, table: &StatusBar) -> String {
        if seg.dynamic
            && let Some(host) = &self.script
            && let Some(text) = host.status_text(&seg.id)
        {
            return text;
        }
        if !seg.template.is_empty() {
            match dg_skin::statusline::expand(&seg.template, &self.facts) {
                Ok(text) => return text,
                // 写错一个占位符就整条不变样，是那种「看着没问题、其实没生效」
                // 的坑 —— 报出来，然后用内置文字兜住这一格。
                Err(e) => eprintln!("[script] status.{}: {e}", seg.id),
            }
        }
        table.text_of(&seg.id)
    }

    /// 皮肤模式下量每一段。规则和内置那条一致：文字宽度 + 内边距，最小
    /// `min_width`；内置字段还按自己的图标位和基准样本量。
    ///
    /// 三个数都从 `[metrics]` 来（[`StatusWin::width_rules`]），所以皮肤画得
    /// 多宽，宿主就留多宽 —— 内边距是画和量共用的一把尺子。
    unsafe fn measure_segments(
        &self,
        segs: &[dg_skin::Segment],
        texts: &[String],
        table: &StatusBar,
    ) -> Vec<i32> {
        let mut widths = vec![0i32; segs.len()];
        if !self.alive() {
            return widths;
        }
        let dc = GetDC(Some(self.hwnd));
        if dc.0.is_null() {
            return widths;
        }
        let rules = self.width_rules(self.dpi());
        let old = SelectObject(dc, self.font_obj());
        for (i, seg) in segs.iter().enumerate() {
            // 内置字段留图标位（编码/换行/制表符/语法四格），脚本段不留。
            let extra = if StatusBar::field_reserves_icon(&seg.id) {
                rules.icon_pad
            } else {
                rules.text_pad
            };
            let mut extent = Self::extent(dc, texts.get(i).map(String::as_str).unwrap_or(""));
            // 每帧都在变的那几格按基准样本量 —— 行号多一位不该把邻居推走。
            if let Some(sample) = table.field_sample(&seg.id) {
                extent = extent.max(Self::extent(dc, &sample));
            }
            widths[i] = (extent + extra).max(rules.min_width);
        }
        SelectObject(dc, old);
        let _ = ReleaseDC(Some(self.hwnd), dc);
        widths
    }

    /// 一段文字有多宽，用这条状态栏自己的字体量。
    unsafe fn extent(dc: HDC, text: &str) -> i32 {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        if utf16.is_empty() {
            return 0;
        }
        let mut measured = utf16;
        measured.push(0);
        let mut size = SIZE::default();
        let _ = GetTextExtentPoint32W(dc, &measured, &mut size);
        size.cx
    }

    pub(super) unsafe fn invalidate(&self) {
        if self.alive() {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// Places the window and re-pushes the pane table.
    ///
    /// The parts depend on the strip's own width, so a resize is a geometry
    /// change even when no text did.
    pub(super) unsafe fn layout(&mut self, table: &mut StatusBar, frame: Rect) {
        self.place(frame);
        self.push(table);
    }

    /// Refreshes the panes from a model built out of the editor state.
    ///
    /// Called on every paint, which is why the change detection matters: a
    /// caret move sends a couple of `SB_SETTEXT`s and an idle frame sends
    /// nothing at all.
    ///
    /// `facts` 只有皮肤模式用得上（模板展开），内置那条一个字段都不读 —— 但它
    /// 得在这里落下来，因为 `apply` 拿不到 `EditorState`（那会儿编辑器可能正被画）。
    pub(super) unsafe fn refresh(
        &mut self,
        table: &mut StatusBar,
        model: &StatusModel,
        facts: dg_skin::statusline::Facts,
    ) {
        // `%M` 和「编辑模式」那一格是同一个词，词表在 `StatusBar` 里（语言表）。
        let mut facts = facts;
        facts.mode = model
            .edit_mode
            .map(|m| table.mode_word(m))
            .unwrap_or_default();
        self.facts = facts;
        // 皮肤模式下**每次都要推**：文字来自 `update` 回调，而那可能是时钟、
        // 是计数器、是任何宿主看不见的东西 —— `set_model` 只认内置那十三个
        // 字段，它说「没变」的时候脚本那一格可能已经变了。
        //
        // 只对**有回调**的那条路放开：全模板的段由 `facts` 决定，而 `facts`
        // 跟着内置模型走，`set_model` 说没变就真没变。
        let dynamic = self
            .skin
            .as_ref()
            .is_some_and(|segs| segs.iter().any(|s| s.dynamic));
        if self.stale || table.set_model(model) || dynamic {
            self.push(table);
        }
    }

    /// 这条状态栏要多久自己醒一次。`None` = 不用醒。
    ///
    /// 只有带 `update` 回调的段需要：它的文字随时间/外部状态变，而宿主没有
    /// 别的消息会到。全内置或者全模板的那条不该被这个定时器白白叫醒。
    fn row_interval(&self) -> Option<u32> {
        self.skin
            .as_ref()
            .is_some_and(|segs| segs.iter().any(|s| s.dynamic))
            .then_some(ROW_INTERVAL_MS)
    }

    /// 按当前这条段表装/拆那个「自己醒一次」的定时器。
    fn arm_row_timer(&mut self) {
        if !self.alive() {
            return;
        }
        match self.row_interval() {
            Some(ms) if !self.row_armed => {
                if unsafe { SetTimer(Some(self.hwnd), ROW_TIMER_ID, ms, None) } != 0 {
                    self.row_armed = true;
                }
            }
            None if self.row_armed => {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), ROW_TIMER_ID);
                }
                self.row_armed = false;
            }
            _ => {}
        }
    }

    /// Hands the table's panes to the control, sized to this window's own
    /// client width.
    unsafe fn push(&mut self, table: &StatusBar) {
        let (w, _) = self.client();
        if w <= 0 {
            return;
        }
        // 宽度是 0 的这一次什么都没铺，`stale` 得留着 —— 清在这儿而不是
        // `refresh` 里，是因为只有这里能保证「真的铺上去了」。
        self.stale = false;
        self.apply(table, w);
    }

    /// The control's rect for part `i`, in strip-client coordinates. Both the
    /// drawing and the hit-test read the geometry from here, so a pane is
    /// clicked exactly where it was painted.
    unsafe fn part_rect(&self, i: i32) -> Option<RECT> {
        let mut rc = RECT::default();
        let ok = SendMessageW(
            self.hwnd,
            SB_GETRECT,
            Some(WPARAM(i as usize)),
            Some(LPARAM(&mut rc as *mut RECT as isize)),
        );
        if ok.0 == 0 { None } else { Some(rc) }
    }

    /// The number of parts the control holds.
    unsafe fn part_count(&self) -> i32 {
        let count = SendMessageW(self.hwnd, SB_GETPARTS, Some(WPARAM(0)), None);
        // The control answers in the low word.
        ((count.0 as u32) & 0xFFFF) as i32
    }

    unsafe fn pane_at(&self, pt: POINT) -> Option<&StatusPane> {
        self.panes.get(self.pane_index_at(pt)?)
    }

    /// 皮肤模式下 `id` 那一段点下去该干什么：`(click, name)`。
    ///
    /// `click` 是 `s:status` 里写的命令名，空 = 没写；`name` 是段名，窗口那一侧
    /// 用它判断「这段其实是个内置字段」，好退回内置那张表（行号点一下跳到行、
    /// 编码点一下弹菜单）。两样都给，是因为「有段名但不是内置字段也没有 click」
    /// 的段应该什么都不做，而不是被当成消息格。
    pub(super) fn click_of(&self, id: u32) -> Option<(String, String)> {
        let pane = self.panes.iter().find(|p| p.id == id)?;
        Some((pane.click.clone(), pane.name.clone()))
    }

    /// 指针落在第几格。绘制和命中测试读同一份几何，所以「点到哪一格」和
    /// 「画的是哪一格」永远一致。
    unsafe fn pane_index_at(&self, pt: POINT) -> Option<usize> {
        for i in 0..self.part_count().max(0) {
            if let Some(rc) = self.part_rect(i)
                && PtInRect(&rc, pt).as_bool()
            {
                return self.panes.get(i as usize).map(|_| i as usize);
            }
        }
        None
    }

    /// 指针换格了：记下来并重画那条。`true` 表示真的换了。
    unsafe fn set_hot(&mut self, hot: Option<usize>) -> bool {
        if self.hot == hot {
            return false;
        }
        self.hot = hot;
        self.invalidate();
        true
    }

    /// The whole strip, in the theme's colours.
    ///
    /// The pass goes to a memory DC and reaches the window in one `BitBlt`.
    /// Painting straight to the screen showed the face fill of a part before
    /// its text landed, and the strip redraws its whole width whenever one
    /// pane changes — the caret moving was enough to see the flash.
    unsafe fn paint(&self, hdc: HDC, theme: &Theme) {
        let _stats = super::paintstats::begin(super::paintstats::Region::StatusBar);
        let mut rc = RECT::default();
        let _ = GetClientRect(self.hwnd, &mut rc);
        let w = (rc.right - rc.left).max(1);
        let h = (rc.bottom - rc.top).max(1);
        let mem = CreateCompatibleDC(Some(hdc));
        let bmp = if mem.0.is_null() {
            Default::default()
        } else {
            CreateCompatibleBitmap(hdc, w, h)
        };
        let offscreen = !bmp.0.is_null();
        if !offscreen && !mem.0.is_null() {
            let _ = DeleteDC(mem);
        }
        let old = if offscreen {
            Some(SelectObject(mem, HGDIOBJ(bmp.0)))
        } else {
            None
        };
        let target = if offscreen { mem } else { hdc };
        let full = RECT {
            left: 0,
            top: 0,
            right: w,
            bottom: h,
        };
        FillRect(target, &full, super::gdicache::brush(to_colorref(theme.statusbar_bg)));
        let dpi = self.dpi();
        // The common control's 1 px top edge
        // carries in the same colour as its separators. The custom paint
        // replaced the control's border pass, so it has to draw it.
        let edge = super::gdicache::brush(to_colorref(theme.statusbar_divider));
        let top = RECT {
            left: full.left,
            top: full.top,
            right: full.right,
            bottom: full.top + scaled(1, dpi),
        };
        FillRect(target, &top, edge);
        for i in 0..self.part_count().max(0) {
            if let Some(part) = self.part_rect(i) {
                self.paint_item(target, &part, i as usize, theme, dpi);
            }
        }
        draw_grip(target, &full, theme, dpi);
        if let Some(old) = old {
            let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
            let _ = SelectObject(mem, old);
            let _ = DeleteObject(HGDIOBJ(bmp.0));
            let _ = DeleteDC(mem);
        }
    }

    /// 这一格的绘制/悬停上下文。
    ///
    /// 绘制和悬停读**同一份**：两处各拼一遍的话，`@text` 展开的和画在格子里的
    /// 迟早会不一样。
    unsafe fn pane_input(&self, rc: &RECT, index: usize, dpi: i32) -> Option<dg_script::PaintInput> {
        let pane = self.panes.get(index)?;
        let scale = if dpi > 0 { dpi as f32 / 96.0 } else { 1.0 };
        let rect = (
            rc.left as f32 / scale,
            rc.top as f32 / scale,
            (rc.right - rc.left) as f32 / scale,
            (rc.bottom - rc.top) as f32 / scale,
        );
        Some(dg_script::PaintInput {
            rect,
            // 状态格没有「会动的部件」，`ctx.thumb` 与 `ctx.rect` 同值。
            thumb: rect,
            bar: (0.0, 0.0, 0.0, 0.0),
            scroll: 0.0,
            // 状态位：这一格是不是指针停着的那格。comctl32 自己不发悬停消息，
            // 位置是 `status_proc` 在 `WM_MOUSEMOVE` 里算好存下的。
            state: self.pane_state(index).bits(),
            part: pane.name.clone(),
            text: pane.text.clone(),
            label: String::new(),
            value: String::new(),
            dpi,
            // 缓存键的区分位：内置模式用 `StatusField` 的声明顺序（重排不改键），
            // 皮肤模式用段号（段号是声明里的位置，被丢掉再回来还是同一条）。
            index: if self.skin.is_some() {
                pane.id as u16
            } else {
                pane.field as u16
            },
        })
    }

    /// 第 `index` 格的上下文，外加这一格在客户区里的位置（悬停提示的落点）。
    /// 矩形问控件要 —— 和绘制走的是同一条 `SB_GETRECT`。
    unsafe fn pane_input_at(&self, index: usize) -> Option<(dg_script::PaintInput, POINT)> {
        let rc = self.part_rect(index as i32)?;
        let input = self.pane_input(&rc, index, self.dpi())?;
        Some((input, crate::win32::skinhint::under(rc.left, rc.bottom)))
    }

    /// 让脚本画这一格。`true` 表示它画了，内置画法让位。
    ///
    /// 没注册、返回 false、抛错，一律 `false` —— 一个坏皮肤不该让状态栏空一块。
    /// 矩形换算成 DIP 再交出去：`UiOp` 的坐标是 DIP，回放层按 DPI 放大。
    unsafe fn paint_by_script(&self, hdc: HDC, rc: &RECT, index: usize, dpi: i32) -> bool {
        let Some(host) = &self.script else {
            return false;
        };
        let Some(input) = self.pane_input(rc, index, dpi) else {
            return false;
        };
        // 皮肤声明了整条时，每格有自己的区域名 `status.<id>`；没声明时全条共用
        // `status.pane`（`ctx.part` 区分是哪一格）。两条都试一遍，是因为**老皮肤
        // 只写了 `status.pane`**：它一旦同时声明了 `s:status`，按段名找会全部落空，
        // 整条状态栏突然变成内置画法。回退一次比让人去改皮肤便宜。
        let per_segment = format!("status.{}", input.part);
        let region = if self.skin.is_some() && host.has_paint(&per_segment) {
            per_segment.as_str()
        } else {
            SKIN_REGION
        };
        let Some(out) = host.paint(region, &input) else {
            return false;
        };
        if !out.handled {
            return false;
        }
        let ops = out.compacted();
        // 字体表只有第 0 格（这条状态栏自己的字体）；脚本点名别的字体是 M3 的事。
        let fonts = [HFONT(self.font_obj().0)];
        let ctx = super::skinpaint::ReplayCtx {
            fonts: &fonts,
            images: &[],
            strings: &out.strings,
            points: &out.points,
            text: &input.text,
            label: "",
            value: "",
            dpi,
        };
        let _ = super::skinpaint::replay(hdc, ops.as_slice(), &ctx);
        true
    }

    /// Draws one pane into the strip's device context.
    ///
    /// The font is the system's per-DPI UI font, the same handle the control
    /// was given with `WM_SETFONT`. The left inset is the control's 2 px
    /// border, and the text is vertically centred, both matching the common
    /// 这一格的状态位。绘制（配方 `states.*`）和脚本 `ctx.state` 读同一个函数。
    fn pane_state(&self, index: usize) -> dg_skin::State {
        if self.hot == Some(index) {
            dg_skin::State::HOT
        } else {
            dg_skin::State::normal()
        }
    }

    /// control.
    ///
    /// 配方在这里只认两个键，原因写在 [`dg_skin::WIRED_REGIONS`] 旁边：这一格的
    /// 内置画法**从不填自己那一块**（只填整条带），所以 `radius` 没有东西可磨；
    /// 文字是垂直居中的，对称的 `pad_y` 内缩等于什么都没做。
    ///
    /// - `gap`：文字右边距多留这么多（每格各留一次，两格之间就是两倍）。
    /// - `border`：格间那道竖线的宽度。
    unsafe fn paint_item(&self, hdc: HDC, rc: &RECT, index: usize, theme: &Theme, dpi: i32) {
        let Some(pane) = self.panes.get(index) else {
            return;
        };
        // 消息格的通知脉冲：底色往 `selection_bg` 那边挪一点，垫在文字**底下**。
        // 先铺再画，而不是画完盖一层半透明 —— 盖上去会把 `#CCCCCC` 的字压成
        // 灰蓝，看着像整格变暗。按**名字**认而不是按 id：皮肤模式下 id 是段号，
        // `0` 是声明里的第一段，那未必是消息格。
        if self.blink_alpha > 0 && pane.name == "message" {
            let wash = pulse_color(theme.statusbar_bg, theme.selection_bg, self.blink_alpha);
            FillRect(hdc, rc, super::gdicache::brush(to_colorref(wash)));
        }
        // 皮肤接管：脚本说「我画了」就整格交给它，内置画法让位。
        let scripted = self.paint_by_script(hdc, rc, index, dpi);
        if !scripted {
            let scale = if dpi > 0 { dpi as f32 / 96.0 } else { 1.0 };
            let shape = crate::skin::shape(SKIN_REGION, self.pane_state(index));
            let gap = (shape.gap * scale).round() as i32;
            // 竖线至少 1 px：内置就是 1 px，`border = 0` 是「没写」不是「不要线」。
            let border = ((shape.border * scale).round() as i32).max(1);
            let _ = SetBkMode(hdc, TRANSPARENT);
            let _ = SetTextColor(hdc, to_colorref(theme.statusbar_fg));
            let old = SelectObject(hdc, self.font_obj());
            let utf16: Vec<u16> = pane.text.encode_utf16().collect();
            if !utf16.is_empty() {
                let mut size = SIZE::default();
                let _ = GetTextExtentPoint32W(hdc, &utf16, &mut size);
                let pad = crate::skin::metrics().get(dg_skin::MetricId::StatusPad).round() as i32;
                let x = rc.left + scaled(pad, dpi) + gap;
                let y = rc.top + ((rc.bottom - rc.top - size.cy) / 2).max(0);
                let _ = TextOutW(hdc, x, y, &utf16);
            }
            SelectObject(hdc, old);
            // The part boundary, in the colour the theme names. The stretch pane
            // is index 0, so it has nothing to its left.
            if index > 0 {
                let bg = super::gdicache::brush(to_colorref(theme.statusbar_divider));
                let inset = scaled(DIVIDER_INSET, dpi);
                let tick = RECT {
                    left: rc.left - 1,
                    top: rc.top + inset,
                    right: rc.left - 1 + border,
                    bottom: rc.bottom,
                };
                FillRect(hdc, &tick, bg);
            }
        }
    }

    /// Arms the message pane's pulse: one timer on the strip, 40 ms a frame,
    /// two one-second breaths.
    pub(super) unsafe fn start_blink(&mut self) {
        if !self.alive() {
            return;
        }
        self.stop_blink();
        self.blink_ms = 0;
        self.blink_alpha = 0;
        if SetTimer(Some(self.hwnd), BLINK_TIMER_ID, BLINK_INTERVAL_MS, None) != 0 {
            self.blink_armed = true;
        }
    }

    /// Cancels a running pulse. Invalidates because a frame already painted
    /// with the wash would otherwise keep it until the next paint.
    pub(super) unsafe fn stop_blink(&mut self) {
        if !self.blink_armed {
            return;
        }
        let _ = KillTimer(Some(self.hwnd), BLINK_TIMER_ID);
        self.blink_armed = false;
        self.blink_ms = 0;
        self.blink_alpha = 0;
        self.invalidate();
    }

    /// One timer tick: advance the pulse, and stop once both breaths are done.
    pub(super) unsafe fn blink_tick(&mut self) {
        if !self.blink_armed {
            return;
        }
        self.blink_ms = self.blink_ms.saturating_add(BLINK_INTERVAL_MS);
        if self.blink_ms >= BLINK_PULSE_MS * BLINK_PULSES {
            let _ = KillTimer(Some(self.hwnd), BLINK_TIMER_ID);
            self.blink_armed = false;
            self.blink_ms = 0;
            self.blink_alpha = 0;
            self.invalidate();
            return;
        }
        self.blink_alpha = pulse_alpha(self.blink_ms);
        self.invalidate();
    }
}

impl Drop for StatusWin {
    fn drop(&mut self) {
        if !self.font.0.is_null() {
            // The window may already be gone; the font object is ours and
            // outlives it until the state is dropped.
            unsafe {
                let _ = DeleteObject(self.font.into());
            }
        }
    }
}

/// The size grip: three rows of dots against the bottom-right corner.
///
/// The control draws this with `DrawFrameControl(DFC_SCROLL, DFCS_SCROLLSIZE)`,
/// which shades with the system button colours and so cannot be read at all on
/// a dark strip. The dot pattern is the same one, in the theme's divider
/// colour. It is decoration only — the corner's `HTBOTTOMRIGHT` is the
/// control's own answer, which a live probe confirmed with the paint taken
/// over.
unsafe fn draw_grip(hdc: HDC, client: &RECT, theme: &Theme, dpi: i32) {
    let right = client.right - 1;
    let bottom = client.bottom - 1;
    // The same dot grows with the DPI: one pixel at 100%, two at 150%+.
    let dot = scaled(1, dpi);
    let pitch = dot * 4;
    let brush = super::gdicache::brush(to_colorref(theme.statusbar_divider));
    for i in 1..=3 {
        for j in 1..=3 {
            if i + j > 4 {
                continue;
            }
            let cell = RECT {
                left: right - i * pitch,
                top: bottom - j * pitch,
                right: right - i * pitch + dot,
                bottom: bottom - j * pitch + dot,
            };
            FillRect(hdc, &cell, brush);
        }
    }
}

impl Default for StatusWin {
    fn default() -> Self {
        Self::new()
    }
}

/// The syntax pane's display name. This implementation's documents carry the file extension, so this
/// maps the extensions its grammars know. `None` means no syntax is attached shows as 默认 — a
/// `.txt` tab must not print "txt".
fn syntax_display_name(lang: &str) -> Option<String> {
    Some(
        match lang {
            "rs" | "rust" => "Rust",
            "py" | "python" => "Python",
            "cpp" | "cc" | "cxx" => "C++",
            "js" | "mjs" | "javascript" => "JavaScript",
            "html" | "htm" => "HTML",
            "json" => "JSON",
            "java" => "Java",
            "php" => "PHP",
            "md" | "markdown" => "Markdown",
            "css" => "CSS",
            "xml" => "XML",
            _ => return None,
        }
        .to_string(),
    )
}

/// The syntax pane's text for a `syntax_lang` key.
///
/// The parser package's own `manifest.display` comes first: that is exactly
/// what 「切换文件类型」 lists, so picking a type there and reading the strip
/// can never disagree. The built-in table above only covers grammars that
/// have no package (a `register`-ed one, or a bare extension after the
/// fallback table resolved it) — without it those would fall back to 默认.
fn syntax_label(reg: &dg_syntax::LanguageRegistry, lang: &str) -> Option<String> {
    reg.display_name(lang).or_else(|| syntax_display_name(lang))
}

/// The editor state as the strip reads it. The line is 1-based and the column
/// 0-based, which is how the 行 and 列 panes print them. The selection pane is
/// computed the way the selection updater does: the inclusive character span
/// and the number of lines it touches.
///
/// 只给测试用：真正喂给状态栏的是 [`strip_of`]，它一次算完模型和模板快照。
#[cfg(test)]
pub(super) fn model_of(st: &EditorState) -> StatusModel {
    strip_of(st).0
}

/// 状态栏这一帧要的两份东西，**一次算完**：内置字段的模型，和模板展开的快照。
///
/// 合成一个函数而不是两个，是因为两趟都要读光标、读选区、数行数、格式化编码 ——
/// 分两次就是两倍，而它们由同一个 `EditorState` 决定，永远不会各说各话。
///
/// `facts.mode` 留空：那个词在语言表里，而语言表属于 `StatusBar`，由
/// [`StatusWin::refresh`] 补上。这里硬编一份中文会和「编辑模式」那格分叉。
pub(super) fn strip_of(
    st: &EditorState,
) -> (StatusModel, dg_skin::statusline::Facts) {
    let editor = &st.docs.act();
    let (line, col) = editor.caret;
    // One character, from the store's shared decode: the strip is rebuilt on
    // every frame, and copying a long line to read one glyph from it would
    // make the readout cost the line.
    let char_value = editor
        .doc
        .line_chars(line)
        .and_then(|chars| chars.get(col).copied())
        .map(|c| c as u32);
    let (sel_start, sel_end) = st.selection_pos_range();
    let sel_chars = st.selection_char_count();
    let sel_lines = if sel_chars > 0 {
        sel_end.line.saturating_sub(sel_start.line) + 1
    } else {
        0
    };
    let selection = (sel_chars > 0).then(|| Selection {
        chars: sel_chars,
        lines: sel_lines,
    });
    let line_count = editor.doc.line_count();
    let encoding = match editor.doc.codepage() {
        Some(cp) => {
            let plus = if editor.doc.has_bom() { "+" } else { "" };
            format!("CP {cp}{plus}")
        }
        None => encoding_label(editor.doc.encoding().name(), editor.doc.has_bom()),
    };
    let eol = match editor.eol {
        dg_core::Eol::Crlf => "CRLF",
        dg_core::Eol::Lf => "LF",
        dg_core::Eol::Cr => "CR",
    };
    let large = editor.doc.is_large();
    // 模板里的 `%y` 要的是**语法名**，不是大文件那个徽标 —— 徽标有自己的
    // `%a`。所以这里不走模型那条 `syntax` 分支。
    let syntax = editor
        .syntax_lang
        .as_deref()
        .and_then(|k| syntax_label(&st.syntax_registry, k))
        .unwrap_or_default();
    let model = StatusModel {
        message: st.notify.clone(),
        line: Some(line + 1),
        col: Some(col),
        char_value,
        line_count: Some(line_count),
        encoding: Some(encoding.clone()),
        // The edit-mode readout has four states, with 2 for column mode and 3 for
        // multiple live selections; this implementation keeps those latches separately.
        edit_mode: Some(if editor.multi_caret() {
            dg_ui::statusbar::EditMode::Multi
        } else if editor.column_mode {
            dg_ui::statusbar::EditMode::Column
        } else {
            st.edit_mode
        }),
        eol: Some(editor.eol),
        tab_size: Some(editor.tab_size),
        soft_tab: editor.view_flags.get(dg_render::ViewFlags::SOFT_TAB),
        // 大文件模式徽标（plan §4.3）：语法窗格让位给模式提示，只读/
        // 可编辑随 read_only（M6 overlay 编辑放开后自动切到"大文件"）。
        syntax: if large {
            Some(if editor.read_only {
                "大文件·只读".to_string()
            } else {
                "大文件".to_string()
            })
        } else {
            editor
                .syntax_lang
                .as_deref()
                .and_then(|k| syntax_label(&st.syntax_registry, k))
        },
        selection,
        // This implementation has no second synchronized view, so the strip
        // leaves the 同步滚动 pane empty whenever the view lookup finds no target.
        sync: false,
        file_size: editor
            .path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok().map(|m| m.len())),
        // The on-screen view lives in the hex window; a tab that is not on
        // screen keeps its state in the document.
        hex: if st.hex_parked.is_some() && st.hex_win.alive() {
            st.hex_win.with_view(|v| hex_status(v))
        } else {
            editor.hex.as_ref().map(hex_status)
        },
    };
    // 未保存的文档没有文件名：`%f` / `%F` 是空串，不是「未命名」—— 那是个词，
    // 词得走语言表，而这张快照不认识语言表。
    let path = editor.path.clone().unwrap_or_default();
    let name = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let pct = if line_count > 1 {
        ((line + 1) * 100 / line_count).min(100)
    } else {
        100
    };
    let facts = dg_skin::statusline::Facts {
        line: line + 1,
        col,
        lines: line_count,
        pct,
        name,
        path,
        modified: editor.is_modified(),
        read_only: editor.read_only,
        syntax,
        eol: eol.to_string(),
        encoding,
        mode: String::new(),
        sel_chars,
        sel_lines,
        large,
    };
    (model, facts)
}

/// The strip's hex numbers: selection length, caret byte, byte count and the
/// edit mode the typing path checks.
fn hex_status(v: &dg_hex::HexView) -> HexStatus {
    HexStatus {
        selection: v.anchor_a.abs_diff(v.anchor_b),
        offset: v.caret,
        size: v.snap.len(),
        overwrite: v.edit_mode == dg_hex::view::MODE_OVERWRITE,
    }
}

/// The subclass. Four messages
/// subclasses this control at all; everything else goes to the control
/// unchanged.
unsafe extern "system" fn status_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // The strip's `GWLP_USERDATA` belongs to comctl32, so the state is reached
    // through its parent — which is the frame window.
    let frame = match GetParent(hwnd) {
        Ok(f) if !f.0.is_null() => f,
        _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
    };
    let Some(st) = state_mut(frame) else {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    };
    // Copied out before any re-entrant dispatch: `SB_GETRECT` below re-enters
    // this procedure while the borrow it made is still live.
    let orig = st.panels.status_win.orig;
    match msg {
        WM_ERASEBKGND => {
            // `WM_PAINT` fills the strip itself.
            LRESULT(1)
        }
        WM_PAINT => {
            // Taken over because the control's own paint fills the client with
            // `COLOR_BTNFACE` no matter what the erase pass did, and a theme
            // colour has to survive an edit of the `.ini`. Geometry, hit-testing
            // and the resize corner stay the control's.
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            st.panels.status_win.paint(hdc, &st.docs.act().theme);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == BLINK_TIMER_ID => {
            st.panels.status_win.blink_tick();
            LRESULT(0)
        }
        // 皮肤那条里有 `update` 回调：没有别的消息会到，所以自己醒一次，
        // 重算文字再排一遍版。`apply` 自己比对文字，没变就不重画。
        WM_TIMER if wparam.0 == ROW_TIMER_ID => {
            st.refresh_status_bar();
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let pt = POINT {
                x: low_word(lparam),
                y: high_word(lparam),
            };
            let hot = st.panels.status_win.pane_index_at(pt);
            // 换格了才问皮肤：同一格里每秒几十条移动什么都不做。
            if st.panels.status_win.set_hot(hot) {
                match hot.and_then(|i| st.panels.status_win.pane_input_at(i)) {
                    Some((input, anchor)) => {
                        let host = st.lua.clone();
                        st.panels.status_win.hint.retarget(
                            hwnd,
                            frame,
                            host.as_deref(),
                            SKIN_REGION,
                            &input,
                            anchor,
                            &mut st.tip_host,
                        );
                    }
                    None => st.panels.status_win.hint.leave(&mut st.tip_host),
                }
            }
            // 系统只在被问过一次之后才发 `WM_MOUSELEAVE`。
            if !st.panels.status_win.tracking {
                st.panels.status_win.tracking = true;
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut track);
            }
            // **还要转给控件**：状态栏的「文字放不下就弹提示」是 comctl32 自己
            // 靠鼠标消息做的，吞掉这条就没有提示了。
            call_orig(orig, hwnd, msg, wparam, lparam)
        }
        WM_MOUSELEAVE => {
            st.panels.status_win.tracking = false;
            st.panels.status_win.set_hot(None);
            st.panels.status_win.hint.leave(&mut st.tip_host);
            call_orig(orig, hwnd, msg, wparam, lparam)
        }
        // 皮肤声明了 `cursor` 就由它说了算：返回 1 让 `DefWindowProc` 别再按类
        // 光标把箭头塞回来。没声明时照旧交给控件。
        WM_SETCURSOR if low_word(lparam) == HTCLIENT as i32 => {
            if st.panels.status_win.hint.apply() {
                LRESULT(1)
            } else {
                call_orig(orig, hwnd, msg, wparam, lparam)
            }
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN => {
            let pt = POINT {
                x: low_word(lparam),
                y: high_word(lparam),
            };
            match st.panels.status_win.pane_at(pt) {
                Some(pane) => {
                    let id = pane.id;
                    // Sent rather than called: this is the frame's own command
                    // contract, so a pane click is reproducible from outside
                    // the process with one `PostMessageW`.
                    let _ = SendMessageW(
                        frame,
                        WM_COMMAND,
                        Some(WPARAM(((id as usize) << 16) | CTRL_ID as usize)),
                        Some(LPARAM(hwnd.0 as isize)),
                    );
                    LRESULT(0)
                }
                // The grip, or bare strip: the control answers for those,
                // including the resize corner.
                None => call_orig(orig, hwnd, msg, wparam, lparam),
            }
        }
        _ => call_orig(orig, hwnd, msg, wparam, lparam),
    }
}

/// Hands a message to the control's own procedure. `orig` is empty only if the
/// subclass was never installed, in which case the default procedure is the
/// closest thing to it.
unsafe fn call_orig(
    orig: WNDPROC,
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match orig {
        Some(p) => p(hwnd, msg, wparam, lparam),
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        pulse_alpha, pulse_color, syntax_display_name, syntax_label, BLINK_INTERVAL_MS,
        BLINK_PEAK_ALPHA, BLINK_PULSE_MS,
    };
    use dg_render::theme::Color;
    use dg_syntax::LanguageRegistry;

    /// The syntax pane shows the syntax name, and nothing when no grammar is attached.
    #[test]
    fn syntax_names_match_the_attached_grammars() {
        assert_eq!(syntax_display_name("rs").as_deref(), Some("Rust"));
        assert_eq!(syntax_display_name("py").as_deref(), Some("Python"));
        assert_eq!(syntax_display_name("cc").as_deref(), Some("C++"));
        assert_eq!(syntax_display_name("mjs").as_deref(), Some("JavaScript"));
        assert_eq!(syntax_display_name("md").as_deref(), Some("Markdown"));
        assert_eq!(syntax_display_name("txt"), None);
    }

    /// 内置段的 id 必须就是 `StatusField::name()`，顺序必须就是 `DEFAULT_ORDER`。
    ///
    /// 皮肤用 `s:status_defaults()` 拼出来的那条要和内置那条**逐格对齐** ——
    /// 对不齐的话，「在后面加一格时钟」会顺手把整条的次序改掉，而且没有任何
    /// 报错，只是看起来不对。
    #[test]
    fn the_default_segments_match_the_builtin_order() {
        let segs = dg_skin::statusline::default_segments();
        let want: Vec<&str> = dg_ui::statusbar::DEFAULT_ORDER
            .iter()
            .filter_map(|id| dg_ui::statusbar::StatusField::from_pane_id(*id))
            .map(|f| f.name())
            .collect();
        let got: Vec<&str> = segs.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(got, want);
        // 消息格吃掉余量，其余按文字量 —— 和内置那条的几何一致。
        assert_eq!(segs[0].width, dg_skin::Width::Fill);
        assert!(segs[1..].iter().all(|s| s.width == dg_skin::Width::Auto));
        // 继承来的格没有模板也没有回调：文字按 id 从内置模型取。
        assert!(segs.iter().all(|s| s.template.is_empty() && !s.dynamic));
    }

    /// 没有解析器包时（发行版默认状态）仍要认得出内置表里那几门语言 —
    /// 退回内置表，而不是把窗格打回「默认」。
    #[test]
    fn the_pane_falls_back_to_the_builtin_table() {
        let reg = LanguageRegistry::new();
        assert_eq!(syntax_label(&reg, "python").as_deref(), Some("Python"));
        assert_eq!(syntax_label(&reg, "rs").as_deref(), Some("Rust"));
        assert_eq!(syntax_label(&reg, "astro"), None, "没包没表 = 默认");
    }

    /// 脉冲是「呼吸」不是「闪」：两端归零、中间到峰值，而且一帧挪不了几级。
    ///
    /// 原来那版是一帧 255 级的整格反色 —— 深色主题里从 `#252526` 直接跳到近白。
    #[test]
    fn the_pulse_rises_and_falls_without_a_hard_edge() {
        assert_eq!(pulse_alpha(0), 0);
        assert_eq!(pulse_alpha(BLINK_PULSE_MS), 0, "第二口气从 0 起");
        assert_eq!(pulse_alpha(BLINK_PULSE_MS / 2), BLINK_PEAK_ALPHA);
        let peak = (0..=BLINK_PULSE_MS).map(pulse_alpha).max().unwrap();
        assert_eq!(peak, BLINK_PEAK_ALPHA, "峰值不越界");
        // 一帧（40 ms）里最多挪十几级，原来的反色是一帧 255 级。
        let mut prev = pulse_alpha(0) as i32;
        let mut step = 0;
        let mut at = BLINK_INTERVAL_MS;
        while at <= BLINK_PULSE_MS {
            let now = pulse_alpha(at) as i32;
            step = step.max((now - prev).abs());
            prev = now;
            at += BLINK_INTERVAL_MS;
        }
        assert!(step <= 16, "一帧挪了 {step} 级");
    }

    /// WCAG 的对比度：两色的相对亮度之比，1..21。
    fn contrast(a: Color, b: Color) -> f32 {
        fn channel(v: u8) -> f32 {
            let v = v as f32 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        }
        fn luminance(c: Color) -> f32 {
            0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b)
        }
        let (x, y) = (luminance(a), luminance(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// 底色只往高亮色挪一部分：两端正好是两端，峰值落在中间，字还看得清。
    #[test]
    fn the_wash_moves_the_background_toward_the_highlight() {
        let base = Color::rgb(0x25, 0x25, 0x26);
        // 随包主题的 `Selection`，屏幕上量到的峰值 `#274156` 就是它混出来的。
        let glow = Color::rgb(0x29, 0x68, 0x99);
        assert_eq!(pulse_color(base, glow, 0), base);
        assert_eq!(pulse_color(base, glow, 255), glow);
        let mid = pulse_color(base, glow, BLINK_PEAK_ALPHA);
        assert_eq!(mid, Color::rgb(0x27, 0x41, 0x56));
        // 峰值那一刻仍是深底：`#CCCCCC` 的字压在上面还有 6:1（AA 要 4.5）。
        let ratio = contrast(mid, Color::rgb(0xCC, 0xCC, 0xCC));
        assert!(ratio >= 6.0, "峰值对比度只有 {ratio:.1}:1");
    }
}
