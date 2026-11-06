//! 脚本窗格：一块**皮肤自己画**的停靠窗格（`s:pane{...}`）。
//!
//! 它和别的停靠窗格是同一个东西 —— 挂在某个 `Host` 的页签上，由
//! [`crate::win32::panels`] 的 `pane_rect` 定位、`place` 显示/隐藏，和目录树、
//! 输出窗口一样参与停靠、切换、关闭。区别只有一个：**内容没有内置画法**。
//!
//! 画法走和状态栏同一条路（`host.paint(region)` + `skinpaint::replay`），所以
//! 十二条原语在这里**全都能用** —— 这一块是真的 HWND，有 GDI 的 DC，不是编辑区
//! 那种只能 `fill`/`rect`/`text` 的 DX 合成面。
//!
//! 区域名是 `pane.<id>`，`ctx.part` 也是 `<id>`，和 `s:paint("pane.clock", fn)`
//! 对得上。`ctx` 里其余字段照旧（`rect`/`w`/`h`/`dpi`/`theme`/`doc`/9 个状态位），
//! 其中 `hot` 是「指针停在这块窗格里」—— 只有这一位，因为窗格内部的分格归皮肤
//! 自己算（它有点的坐标吗？`s:on` 的 `ctx` 里没有 x/y，所以 v1 只到整块粒度）。
//!
//! # 滚动
//!
//! 内容比窗格高时能滚，**几何由宿主算、像素由皮肤画**：
//!
//! * 内容高从 `s:paint` 的返回值来 —— `return 1200` 就是「我画了，内容 1200 DIP
//!   高」。数字在 Lua 里本来就算真值，所以这是纯扩展（见 `PaintOut::content`）。
//! * `s:pane{ bar = 10 }` 声明轨道占右边多宽；0 = 不要轨道（滚轮照样能用）。
//! * 宿主算好 `ctx.bar`（轨道）、`ctx.thumb`（滑块）、`ctx.scroll`（偏移）交给
//!   皮肤，**并把 `ctx.rect` 收窄成内容区**（减掉轨道宽）。
//! * 滚轮、拖滑块、点轨道翻页都是宿主的活：那三样都要按同一份几何反推位置，
//!   而皮肤拿不到指针坐标（`ctx` 里没有 x/y）。皮肤只填色。
//!
//! 偏移**不由回放层平移**，是皮肤自己减 `ctx.scroll`。理由和别处一样：`ctx` 说
//! 状态，皮肤说像素。附带好处是固定表头免费 —— 那一行不加 `ctx.scroll` 就是了，
//! 而「整块平移」的写法做不到这件事。

#![allow(unsafe_op_in_unsafe_fn)]

use std::sync::Once;
use std::sync::atomic::{AtomicIsize, Ordering};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush, DeleteDC,
    DeleteObject, EndPaint, FillRect, HBITMAP, HDC, HFONT, HGDIOBJ, InvalidateRect, PAINTSTRUCT,
    SRCCOPY, SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemServices::MK_LBUTTON;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, CS_DBLCLKS, DefWindowProcW, GWLP_USERDATA, GetClientRect,
    GetParent, IDC_ARROW, LoadCursorW, PostMessageW, RegisterClassW, SW_HIDE, SW_SHOW,
    SWP_NOACTIVATE, SetWindowLongPtrW, SetWindowPos, WINDOW_EX_STYLE, WM_CAPTURECHANGED,
    WM_ERASEBKGND, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONDOWN, WM_SETFOCUS, WM_SIZE, WNDCLASSW,
    WS_CHILD,
};

use crate::keys::{MOD_ALT, MOD_CTRL, MOD_SHIFT};
use crate::win32::window::{state_mut, to_colorref};
use dg_render::Rect;
use dg_ui::panel::PanelSide;

const SKIN_PANE_CLASS: windows::core::PCWSTR = windows::core::w!("dogedit_skin_pane");

/// 窗格 id 的控件号起点。停靠页签的 `PaneId::Script(n)` 用 `SCRIPT_BASE + n`，
/// 这里只给窗口一个不撞别人的控件号。
const CTRL_BASE: usize = 3400;

/// 滚轮一格滚多少（DIP）。约三行 16 DIP 的字，和 `dg_ui::scrollbar` 的
/// `WHEEL_LINES` 一个量级 —— 窗格没有行高可乘，直接给 DIP。
const WHEEL_STEP: f32 = 48.0;

/// 滑块最短长度（DIP）。再短就抓不住了。
const MIN_THUMB: f32 = 20.0;

/// 一块窗格的滚动几何，全是 DIP。
///
/// 和 `dg_ui::scrollbar::Scrollbar` 分开算，不是重复：那一个的 `pos` 是**行号**、
/// 轨道就是它自己的窗口、还带着淡出计时和分层窗口那一套；窗格这里是**任意 DIP
/// 内容**、轨道只是客户区右边的一条、没有淡出。公式一样（滑块长 = 页 / 内容
/// 占比，最短 [`MIN_THUMB`]），量纲和生命周期都不一样，硬合并只会让两边都多出
/// 用不上的字段。这里是个纯函数，好测。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct BarGeom {
    /// 内容区：客户区减掉轨道宽。没有轨道时就是整个客户区。
    pub content: Rect,
    /// 整条轨道。没有轨道时是空矩形。
    pub track: Rect,
    /// 滑块。没有轨道时是空矩形。
    pub thumb: Rect,
    /// 最大滚动量（DIP）= `内容高 - 视口高`，不小于 0。**和有没有轨道无关** ——
    /// 声明 `bar = 0` 只是不要轨道，滚轮照样能滚。
    pub max_scroll: f32,
    /// 夹进 `[0, max_scroll]` 之后的滚动位置。
    pub scroll: f32,
}

impl BarGeom {
    /// `w`/`h` 是客户区（DIP），`bar` 是声明的轨道宽，`content_h` 是上一帧
    /// `s:paint` 报的内容高，`scroll` 是当前的滚动位置。
    pub(super) fn new(w: f32, h: f32, bar: f32, content_h: f32, scroll: f32) -> Self {
        let w = w.max(0.0);
        let h = h.max(0.0);
        let max_scroll = (content_h - h).max(0.0);
        let scroll = scroll.clamp(0.0, max_scroll);
        // 轨道只在「声明了宽度 + 真的滚得动 + 有高度」时存在。`bar` 也夹到客户区
        // 宽：声明 400 DIP 的条不能把内容区挤成负数。
        let bar = bar.max(0.0).min(w);
        let scrollable = bar > 0.0 && max_scroll > 0.0 && h > 0.0;
        if !scrollable {
            return Self {
                content: Rect::new(0.0, 0.0, w, h),
                track: Rect::new(0.0, 0.0, 0.0, 0.0),
                thumb: Rect::new(0.0, 0.0, 0.0, 0.0),
                max_scroll,
                scroll,
            };
        }
        // 滑块长度按「页 / 内容」占比；`max_scroll > 0` 保证 `content_h > h > 0`，
        // 这里的除法不会碰到 0。
        let thumb_h = (h * (h / content_h)).clamp(MIN_THUMB.min(h), h);
        let y = (h - thumb_h) * (scroll / max_scroll);
        Self {
            content: Rect::new(0.0, 0.0, w - bar, h),
            track: Rect::new(w - bar, 0.0, bar, h),
            thumb: Rect::new(w - bar, y, bar, thumb_h),
            max_scroll,
            scroll,
        }
    }

    /// 这个点（DIP）落在轨道上了吗 —— 落上了就归宿主，不给皮肤。
    pub(super) fn on_track(&self, x: f32) -> bool {
        self.track.w > 0.0 && x >= self.track.x
    }
}

static REGISTER_CLASS: Once = Once::new();

/// 拿着键盘焦点的那块脚本窗格的句柄（0 = 编辑区拿着）。
///
/// 只在这条线程上写（窗口消息天然是单线程的），读的时候也无所谓顺序 —— 它
/// 只影响画面上有没有一圈焦点边、以及纯字符键算不算数。
static FOCUSED: AtomicIsize = AtomicIsize::new(0);

fn instance() -> HINSTANCE {
    unsafe {
        GetModuleHandleW(None)
            .map(|m| HINSTANCE(m.0))
            .unwrap_or(HINSTANCE(std::ptr::null_mut()))
    }
}

/// 一块窗格的离屏缓冲。
///
/// 一帧里的所有东西（底色 + 皮肤那几十条 op）必须先在**同一张位图**上落完，
/// 再一次 `BitBlt` 上去。直接往窗口 DC 上画就是两趟：底色一趟、内容一趟 ——
/// 滚轮每动一格、拖滑块每挪一像素都要整块重画，而那时候人眼正盯着那一块，
/// 中间那半帧就是闪。窗格条（`docktabs`）和输出窗格用的是同一条规矩。
///
/// 缓冲按尺寸复用，不每帧重建：一块 1000×200 的兼容位图是 800 KB，滚动时一秒
/// 重建几十次不值当。
struct BackBuffer {
    mem: HDC,
    bmp: HBITMAP,
    /// 位图选进来之前 `mem` 里那只，删位图前要还回去。
    old: HGDIOBJ,
    w: i32,
    h: i32,
}

impl BackBuffer {
    fn empty() -> Self {
        Self {
            mem: HDC::default(),
            bmp: HBITMAP::default(),
            old: HGDIOBJ::default(),
            w: 0,
            h: 0,
        }
    }

    fn alive(&self) -> bool {
        !self.mem.0.is_null() && !self.bmp.0.is_null()
    }

    /// 保证缓冲是 `w`×`h`；尺寸没变就什么都不做。
    ///
    /// GDI 拒绝时返回 false，这一帧就不画 —— 窗格是装饰，一次分配失败不该把
    /// 窗口带下去（同 `overlay::Surface`）。
    unsafe fn fit(&mut self, screen: HDC, w: i32, h: i32) -> bool {
        if w <= 0 || h <= 0 {
            return false;
        }
        if self.alive() && self.w == w && self.h == h {
            return true;
        }
        self.release();
        let mem = CreateCompatibleDC(Some(screen));
        if mem.0.is_null() {
            return false;
        }
        let bmp = CreateCompatibleBitmap(screen, w, h);
        if bmp.0.is_null() {
            let _ = DeleteDC(mem);
            return false;
        }
        self.old = SelectObject(mem, HGDIOBJ(bmp.0));
        self.mem = mem;
        self.bmp = bmp;
        self.w = w;
        self.h = h;
        true
    }

    unsafe fn release(&mut self) {
        if !self.bmp.0.is_null() {
            if !self.mem.0.is_null() {
                SelectObject(self.mem, self.old);
            }
            let _ = DeleteObject(HGDIOBJ(self.bmp.0));
        }
        if !self.mem.0.is_null() {
            let _ = DeleteDC(self.mem);
        }
        *self = Self::empty();
    }
}

/// 一块脚本窗格。
pub(super) struct SkinPane {
    hwnd: HWND,
    /// `s:pane` 里的那个 id 名，也是区域名 `pane.<id>` 的后半截。
    name: String,
    /// 页签标题。
    title: String,
    /// 停在哪一边。
    side: PanelSide,
    /// 这一侧的厚度（DIP）。0 = 不指定，用停靠栏自己的记忆值。
    size: f32,
    /// 页签图标：工具栏精灵格号，0 = 不给图标。
    icon: i32,
    dpi: i32,
    /// 这一块自己的字体（界面字体）。`c:text` 回放时用它。
    font: HFONT,
    /// 指针停在这块窗格里。
    hot: bool,
    /// 已经发过 `TrackMouseEvent`，等 `WM_MOUSELEAVE` 清位。
    tracking: bool,
    /// 竖直滚动位置（DIP），总在 `[0, 内容高 - 视口高]` 里。
    scroll: f32,
    /// 内容总高（DIP）。上一帧 `s:paint` 返回的数字；0 = 不知道 / 不滚。
    content: f32,
    /// 滚动条轨道宽度（DIP）。0 = 不要轨道（滚轮照样能滚）。
    bar: f32,
    /// 正在拖滑块时，指针到滑块上沿的距离（DIP）。`None` = 没在拖。
    grab: Option<f32>,
    /// 这一帧画在哪儿。见 [`BackBuffer`]：整帧一次落完再贴上去。
    back: BackBuffer,
}

impl SkinPane {
    pub(super) fn new() -> Self {
        Self {
            hwnd: HWND::default(),
            name: String::new(),
            title: String::new(),
            side: PanelSide::Bottom,
            size: 0.0,
            icon: 0,
            dpi: 96,
            font: HFONT::default(),
            hot: false,
            tracking: false,
            scroll: 0.0,
            content: 0.0,
            bar: 0.0,
            grab: None,
            back: BackBuffer::empty(),
        }
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn title(&self) -> &str {
        &self.title
    }

    pub(super) fn side(&self) -> PanelSide {
        self.side
    }

    pub(super) fn set_side(&mut self, side: PanelSide) {
        self.side = side;
    }

    /// 页签上那个图标的精灵格号（0 = 不给图标）。
    pub(super) fn icon_cell(&self) -> i32 {
        self.icon
    }

    /// 皮肤要的厚度（DIP）；0 = 没指定。
    pub(super) fn size(&self) -> f32 {
        self.size
    }

    /// 绘制区域名：`pane.<id>`。
    pub(super) fn region(&self) -> String {
        format!("pane.{}", self.name)
    }

    /// 按皮肤声明把这一块对齐：标题、图标、厚度、边都换成新的；还没有窗口
    /// 就顺手造出来。
    ///
    /// 复用而不是重建是热重载的关键：`skin.lua` 存一次就重跑一遍，如果每次都
    /// 销毁重造，窗格上的焦点、滚动位置、指针热区全都会在用户打字时被抹掉。
    pub(super) unsafe fn describe(
        &mut self,
        parent: HWND,
        ctrl: usize,
        spec: &dg_script::PaneSpec,
        side: PanelSide,
        dpi: i32,
    ) {
        if self.alive() {
            self.title = spec.title.clone();
            self.icon = spec.icon;
            self.size = spec.size;
            // 轨道宽是热重载能改的：改了就得重画（内容区宽跟着变，缓存键里的
            // `rect` 自己会变，但 `bar` 从有到无时不重画就会留一条旧轨道）。
            if (self.bar - spec.bar).abs() > 0.01 {
                self.bar = spec.bar;
                let _ = InvalidateRect(Some(self.hwnd), None, false);
            }
            self.side = side;
            self.set_dpi(dpi);
            return;
        }
        self.create(
            parent, ctrl, &spec.id, &spec.title, side, spec.size, spec.icon, spec.bar, dpi,
        );
    }

    /// 拆掉窗口。皮肤把这一块删了的时候走这里。
    pub(super) unsafe fn destroy(&mut self) {
        if self.alive() {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(self.hwnd);
            // 拆掉的窗口不会再收到 `WM_KILLFOCUS`，焦点得在这里还回去。
            let _ = FOCUSED.compare_exchange(
                self.hwnd.0 as isize,
                0,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
        }
        self.hwnd = HWND::default();
        if !self.font.0.is_null() {
            let _ = DeleteObject(HGDIOBJ(self.font.0));
            self.font = HFONT::default();
        }
        // 离屏位图要自己还：窗口没了，DC 和位图不会跟着走。
        self.back.release();
    }

    /// 造窗口。`ctrl` 是控件号，同一个父窗口里不能重。
    pub(super) unsafe fn create(
        &mut self,
        parent: HWND,
        ctrl: usize,
        name: &str,
        title: &str,
        side: PanelSide,
        size: f32,
        icon: i32,
        bar: f32,
        dpi: i32,
    ) {
        self.name = name.to_string();
        self.title = title.to_string();
        self.side = side;
        self.size = size;
        self.icon = icon;
        self.bar = bar;
        self.set_dpi(dpi);
        REGISTER_CLASS.call_once(|| {
            let _ = RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(pane_proc),
                hInstance: instance(),
                lpszClassName: SKIN_PANE_CLASS,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                // 双击也要给：皮肤窗格里双击是常见的（展开/折叠、放大）。
                style: CS_DBLCLKS,
                ..Default::default()
            });
        });
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            SKIN_PANE_CLASS,
            windows::core::w!("skin pane"),
            // 不是 WS_VISIBLE：`place` 等到停靠栏给它矩形才显示。
            WS_CHILD,
            0,
            0,
            0,
            0,
            Some(parent),
            Some(windows::Win32::UI::WindowsAndMessaging::HMENU(
                (CTRL_BASE + ctrl) as *mut core::ffi::c_void,
            )),
            Some(instance()),
            None,
        ) else {
            return;
        };
        self.hwnd = hwnd;
        // 让窗口过程能直接找到自己那一项，不必每次扫一遍。
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ctrl as isize);
    }

    pub(super) fn alive(&self) -> bool {
        !self.hwnd.0.is_null()
    }

    /// 让这一块重画一帧。脚本窗格是独立的子窗口，父窗口带 `WS_CLIPCHILDREN`，
    /// 作废 frame 不会顺带作废它 —— 定时器心跳和 `App.ui.repaint` 都得点名。
    pub(super) fn invalidate(&self) {
        if self.alive() {
            unsafe {
                let _ = InvalidateRect(Some(self.hwnd), None, false);
            }
        }
    }

    /// 换字体。DPI 变了要重来一次，不然字会按旧 DPI 排。
    pub(super) unsafe fn set_dpi(&mut self, dpi: i32) {
        self.dpi = dpi.max(96);
        if !self.font.0.is_null() {
            let _ = DeleteObject(HGDIOBJ(self.font.0));
        }
        self.font = super::uifont::create_for(dg_skin::FontToken::Pane, self.dpi);
    }

    /// 摆到 `rect`。空矩形 = 藏起来（不销毁，切回来还在）。
    pub(super) unsafe fn place(&mut self, rect: Rect) {
        if !self.alive() {
            return;
        }
        let empty = rect.is_empty();
        let _ = SetWindowPos(
            self.hwnd,
            None,
            rect.x as i32,
            rect.y as i32,
            rect.w.max(0.0) as i32,
            rect.h.max(0.0) as i32,
            SWP_NOACTIVATE,
        );
        let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(
            self.hwnd,
            if empty { SW_HIDE } else { SW_SHOW },
        );
    }

    pub(super) unsafe fn apply_theme(&self, theme: &dg_render::theme::Theme) {
        // 皮肤没接管时用它填底；接了管就全靠脚本。
        let _ = theme;
        if self.alive() {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    pub(super) fn state(&self) -> dg_skin::State {
        let mut s = if self.hot {
            dg_skin::State::HOT
        } else {
            dg_skin::State::normal()
        };
        if self.focused() {
            s = s.with(dg_skin::State::FOCUS);
        }
        s
    }

    /// 这块窗格拿着键盘焦点。
    ///
    /// 焦点**不在** `EditorState` 里，而是这条线程的一个原子量。原因是
    /// `SetFocus` 是同步的：它当场给旧焦点窗口发 `WM_KILLFOCUS`、给新窗口发
    /// `WM_SETFOCUS`，而这两个消息都可能落在外层正拿着 `&mut EditorState`
    /// 的时刻里（`state_mut` 是裸指针 `&'static mut`，同一个对象两份可变借用
    /// 就是 UB）。焦点只有一个句柄，放外面既够用也不会重入。
    pub(super) fn focused(&self) -> bool {
        FOCUSED.load(Ordering::Relaxed) == self.hwnd.0 as isize
    }

    /// 这一块（客户区 `w` × `h`，DIP）的滚动几何。
    pub(super) fn geom(&self, w: f32, h: f32) -> BarGeom {
        BarGeom::new(w, h, self.bar, self.content, self.scroll)
    }
}

/// 这一块窗格的 `ctx`：整块客户区，没有「部件」。
///
/// `width`/`height` 必须是 **DIP**，不是像素。回放那侧把每条 op 的长度乘
/// `dpi/96`（`skinpaint::ReplayCtx::scale`），所以 `ctx.rect` 说 195 宽、
/// 实际画到 244 px —— 和别的区域一致。传像素进去的话，`c:fill(ctx.rect, …)`
/// 靠裁剪切巧是对的，但 `c:rect(ctx.rect, …, 1)` 的右、下两条边会被乘到客户区
/// 外面，整块只剩上边一条线。
///
/// `index` 是它在 `script_panes` 里的下标。缓存键里的部件号只有 8 位（`pane.<id>`
/// 的哈希），两块窗格撞上是 1/256 的事；序号把同区域里的每一块分开，撞了也不会
/// 画成对方的。
pub(super) fn input_for(pane: &SkinPane, width: f32, height: f32, index: u16) -> dg_script::PaintInput {
    let g = pane.geom(width, height);
    dg_script::PaintInput {
        // 内容区，不是客户区：轨道那一竖条不算内容（`ctx.bar` 才是它）。
        rect: (g.content.x, g.content.y, g.content.w, g.content.h),
        // 窗格唯一的「会动的部件」就是滑块。
        thumb: (g.thumb.x, g.thumb.y, g.thumb.w, g.thumb.h),
        bar: (g.track.x, g.track.y, g.track.w, g.track.h),
        scroll: g.scroll,
        state: pane.state().bits(),
        part: pane.name.clone(),
        text: String::new(),
        label: String::new(),
        value: String::new(),
        dpi: pane.dpi,
        index,
    }
}

/// 鼠标坐标（客户区像素）→ DIP。`lparam` 里是两个有符号 16 位，拖到窗格外
/// 会是负数，所以走 `i16` 而不是 `u16`。
fn dip_of(lparam: LPARAM, scale: f32) -> (f32, f32) {
    let lo = (lparam.0 & 0xFFFF) as u16 as i16 as f32;
    let hi = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as f32;
    (lo / scale, hi / scale)
}

/// 客户区尺寸（DIP）。
fn client_dip(hwnd: HWND, dpi: i32) -> (f32, f32, f32) {
    let mut rc = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
    }
    let scale = (dpi.max(96) as f32) / 96.0;
    (
        (rc.right - rc.left) as f32 / scale,
        (rc.bottom - rc.top) as f32 / scale,
        scale,
    )
}

unsafe extern "system" fn pane_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // 焦点这两条**不碰 `EditorState`**，所以放在借它之前 —— 见
    // `SkinPane::focused` 的说明。
    match msg {
        WM_SETFOCUS => {
            FOCUSED.store(hwnd.0 as isize, Ordering::Relaxed);
            let _ = InvalidateRect(Some(hwnd), None, false);
            return LRESULT(0);
        }
        WM_KILLFOCUS => {
            // 只清自己那一份：焦点从 A 直接转到 B 时，A 的 `WM_KILLFOCUS`
            // 可能晚于 B 的 `WM_SETFOCUS` 到达。
            let _ = FOCUSED.compare_exchange(
                hwnd.0 as isize,
                0,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
            let _ = InvalidateRect(Some(hwnd), None, false);
            return LRESULT(0);
        }
        // 拖滑块结束时 `ReleaseCapture` 会**同步**发回这一条，而那时外面这份
        // `&mut EditorState` 还活着 —— 所以它在这里就返回，绝不往下走。
        // 拖拽状态由 `WM_LBUTTONUP` 和 `WM_MOUSEMOVE` 里那道「左键还按着吗」
        // 自愈，不靠这条消息（见 `SkinPane::grab`）。
        WM_CAPTURECHANGED => return LRESULT(0),
        _ => {}
    }
    let frame = match GetParent(hwnd) {
        Ok(f) if !f.0.is_null() => f,
        _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
    };
    // 点击要焦点。**必须在借 `EditorState` 之前**：`SetFocus` 同步给旧焦点
    // 窗口（编辑区）发 `WM_KILLFOCUS`，那条路会自己再借一次。
    //
    // 滚轮也算「点进来」：`WM_MOUSEWHEEL` 是发给**焦点窗口**的，指向一块没被点过
    // 的窗格转轮子，消息会落到编辑区去滚正文。在这里拿焦点是让滚轮认准它底下
    // 那块窗格 —— 和目录树、大纲那几块窗格同一条规矩（`dirtree` 的
    // `WM_MOUSEWHEEL` 注释）。
    if msg == WM_LBUTTONDOWN || msg == WM_LBUTTONDBLCLK || msg == WM_MOUSEWHEEL {
        let _ = SetFocus(Some(hwnd));
    }
    let Some(st) = state_mut(frame) else {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    };
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            paint(st, hwnd, hdc);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_SIZE => {
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let Some(idx) = index_of(st, hwnd) else {
                return LRESULT(0);
            };
            // 拖滑块先办：拖拽期间指针可能已经出了窗格（有 `SetCapture`，消息
            // 还到得了），这时「hot」那套没意义。
            if st.panels.script_panes[idx].grab.is_some() {
                // 左键已经松了却还留着 `grab`（别的窗口抢走了 capture，那条
                // `WM_CAPTURECHANGED` 我们不处理）：就地自愈。
                if wparam.0 & MK_LBUTTON.0 as usize == 0 {
                    st.panels.script_panes[idx].grab = None;
                    return LRESULT(0);
                }
                let dpi = st.panels.script_panes[idx].dpi;
                let (w, h, scale) = client_dip(hwnd, dpi);
                let (_, y) = dip_of(lparam, scale);
                let pane = &mut st.panels.script_panes[idx];
                let grab = pane.grab.unwrap_or(0.0);
                let g = pane.geom(w, h);
                // 滑块上沿跟着指针走：轨道剩下的长度按比例换成滚动量。
                let span = h - g.thumb.h;
                if span > 0.0 && g.max_scroll > 0.0 {
                    pane.scroll = ((y - grab) / span * g.max_scroll).clamp(0.0, g.max_scroll);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                return LRESULT(0);
            }
            if !st.panels.script_panes[idx].hot {
                st.panels.script_panes[idx].hot = true;
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            if !st.panels.script_panes[idx].tracking {
                st.panels.script_panes[idx].tracking = true;
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut track);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            if let Some(idx) = index_of(st, hwnd) {
                st.panels.script_panes[idx].tracking = false;
                if st.panels.script_panes[idx].hot {
                    st.panels.script_panes[idx].hot = false;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN => {
            let Some(idx) = index_of(st, hwnd) else {
                return LRESULT(0);
            };
            let dpi = st.panels.script_panes[idx].dpi;
            let (w, h, scale) = client_dip(hwnd, dpi);
            let (x, y) = dip_of(lparam, scale);
            // 点在轨道上归宿主：拖滑块、点空白翻一屏。**不给皮肤** —— 那两样都
            // 要按轨道几何反推位置，而 `s:on` 的 ctx 里没有指针坐标。
            if msg != WM_RBUTTONDOWN {
                let pane = &mut st.panels.script_panes[idx];
                let g = pane.geom(w, h);
                if g.on_track(x) {
                    let on_thumb = y >= g.thumb.y && y < g.thumb.y + g.thumb.h;
                    if on_thumb {
                        pane.grab = Some(y - g.thumb.y);
                        let _ = SetCapture(hwnd);
                    } else {
                        // 空白处翻一屏（九成，留一点上下文），和
                        // `dg_ui::scrollbar` 的 TrackStart/TrackEnd 同规矩。
                        let dir = if y < g.thumb.y { -1.0 } else { 1.0 };
                        pane.scroll = (g.scroll + dir * h * 0.9).clamp(0.0, g.max_scroll);
                    }
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    return LRESULT(0);
                }
            }
            let input = input_for(&st.panels.script_panes[idx], w, h, idx as u16);
            let region = st.panels.script_panes[idx].region();
            let event = if msg == WM_RBUTTONDOWN { "rclick" } else { "press" };
            // 点击是「干活」那一档：有 bridge，handler 能跑命令。
            // 返回 true 表示脚本接管，这里没有内置动作要让位，所以返回值不用。
            let _ = crate::win32::luascript::press(st, frame, &region, event, &input);
            // 点一下之后状态可能变了（皮肤常把 hot 和选中一起画），重画一块。
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let Some(idx) = index_of(st, hwnd) else {
                return LRESULT(0);
            };
            // 先清状态再放手：`ReleaseCapture` 会同步发回 `WM_CAPTURECHANGED`，
            // 那条路不碰 `EditorState`（见上面那个早退分支）。
            if st.panels.script_panes[idx].grab.take().is_some() {
                let _ = ReleaseCapture();
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let Some(idx) = index_of(st, hwnd) else {
                return LRESULT(0);
            };
            // 高字是有符号的 120 的倍数；低字是屏幕坐标，滚动用不上。
            let delta = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16 as f32;
            let dpi = st.panels.script_panes[idx].dpi;
            let (w, h, _) = client_dip(hwnd, dpi);
            let pane = &mut st.panels.script_panes[idx];
            let g = pane.geom(w, h);
            // 往上推（delta > 0）内容往下走，所以是减。
            let next = (g.scroll - delta / 120.0 * WHEEL_STEP).clamp(0.0, g.max_scroll);
            if (next - g.scroll).abs() > 0.01 {
                pane.scroll = next;
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        // 窗格有焦点时键盘归它：**纯字符键也认**。这正是窗格要焦点的理由 ——
        // 编辑区那边只认 `Ctrl`/`Alt` 组合（`window.rs` 的
        // `editor_key_allowed`），因为正文里的裸字母是要落进文档的字；窗格没有
        // 输入法要喂，一个字母就是一个字母。
        WM_KEYDOWN => {
            let mods = crate::win32::window::modifier_state();
            let vk = wparam.0 as u32;
            if let Some(name) = crate::win32::luascript::lookup_key(
                st,
                mods & MOD_CTRL != 0,
                mods & MOD_SHIFT != 0,
                mods & MOD_ALT != 0,
                vk,
            ) {
                if crate::win32::luascript::command(st, frame, &name) {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                return LRESULT(0);
            }
            // 没接的键照旧交给主窗口：点了窗格之后 Ctrl+S 不能失灵。
            // **Post 不 Send**：Send 会在外面这份 `&mut EditorState` 还活着的
            // 时候重入主窗口过程。
            let _ = PostMessageW(Some(frame), WM_KEYDOWN, wparam, lparam);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn index_of(st: &crate::win32::window::EditorState, hwnd: HWND) -> Option<usize> {
    st.panels.script_panes.iter().position(|p| p.hwnd == hwnd)
}

/// 画一块窗格：底色 + 脚本的 op。
unsafe fn paint(st: &mut crate::win32::window::EditorState, hwnd: HWND, hdc: HDC) {
    let Some(idx) = index_of(st, hwnd) else {
        return;
    };
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let (pw, ph) = ((rc.right - rc.left), (rc.bottom - rc.top));
    if pw <= 0 || ph <= 0 {
        return;
    }
    // 计时只算我们自己画的那一段：`BeginPaint`/`EndPaint` 和 `BitBlt` 不算（和
    // 别的区域同一条尺子）。
    let stats = super::paintstats::begin(super::paintstats::Region::Pane);
    // 像素 → DIP：`ctx` 里的一切都是 DIP，回放时才乘回像素。
    let scale = (st.panels.script_panes[idx].dpi.max(96) as f32) / 96.0;
    let (w, h) = (pw as f32 / scale, ph as f32 / scale);

    // 整帧先画在这张位图上，最后一次贴上去 —— 分两趟落到窗口上会闪（见
    // `BackBuffer`）。拿不到缓冲就这一帧不画：窗格是装饰，不该把窗口带下去。
    let mem = {
        let pane = &mut st.panels.script_panes[idx];
        if !pane.back.fit(hdc, pw, ph) {
            return;
        }
        pane.back.mem
    };
    let bg = st.docs.act().theme.panel_bg;
    let brush = CreateSolidBrush(to_colorref(bg));
    let _ = FillRect(mem, &rc, brush);
    let _ = DeleteObject(HGDIOBJ(brush.0));

    let (input, region, font) = {
        let pane = &mut st.panels.script_panes[idx];
        // 先按上一帧记下的内容高夹一次：这一帧 `ctx.scroll` 用的就是夹过的值
        // （内容变矮之后，旧偏移不能把整块画到窗格外面去）。
        let g = pane.geom(w, h);
        pane.scroll = g.scroll;
        (input_for(pane, w, h, idx as u16), pane.region(), pane.font)
    };
    let out = st.lua.clone().and_then(|host| host.paint(&region, &input));
    if let Some(out) = &out {
        // 这一帧报的内容高：下一帧的夹取、滑块长度、滚轮量都按它算。没报（返回
        // true/布尔）就是 0 —— 「声明内容」和「能不能滚」是同一件事，不许留旧值。
        let content = out.content.unwrap_or(0.0);
        let mut again = false;
        {
            let pane = &mut st.panels.script_panes[idx];
            if (pane.content - content).abs() > 0.01 {
                pane.content = content;
                // 内容高一变，轨道/滑块的长度和内容区宽度都跟着变，而这一帧是按
                // **旧**值画的（第一帧甚至完全没有轨道）—— 所以只要变了就得再要
                // 一帧。收敛是必然的：下一次算出来的还是这个内容高。
                again = true;
                let g = pane.geom(w, h);
                if (g.scroll - pane.scroll).abs() > 0.01 {
                    pane.scroll = g.scroll;
                }
            }
        }
        if again {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
    // 没画法、或回调说「交回内置」：只剩底色那一层，也照样贴上去 —— 不能留着
    // 上一帧的像素，那才叫花屏。
    if let Some(out) = out.as_ref().filter(|o| o.handled) {
        let ops = out.compacted();
        let fonts = [font];
        let ctx = super::skinpaint::ReplayCtx {
            fonts: &fonts,
            images: &[],
            strings: &out.strings,
            points: &out.points,
            text: &input.text,
            label: "",
            value: "",
            dpi: input.dpi,
        };
        let _ = super::skinpaint::replay(mem, ops.as_slice(), &ctx);
    }
    // 计时到此为止：`BitBlt` 不算（和别的区域同一条尺子 —— 那一次贴图是系统的事）。
    drop(stats);
    let _ = BitBlt(hdc, 0, 0, pw, ph, Some(mem), 0, 0, SRCCOPY);
}

#[cfg(test)]
mod bar_tests {
    use super::{BarGeom, MIN_THUMB};

    /// 视口 200×200 DIP 的一格，内容 1000 DIP，轨道 10 DIP。
    fn tall() -> BarGeom {
        BarGeom::new(200.0, 200.0, 10.0, 1000.0, 0.0)
    }

    #[test]
    fn the_content_rect_gives_up_the_track() {
        let g = tall();
        assert_eq!(g.content.w, 190.0);
        assert_eq!(g.track, dg_render::Rect::new(190.0, 0.0, 10.0, 200.0));
        assert_eq!(g.max_scroll, 800.0);
    }

    #[test]
    fn the_thumb_is_the_page_share_and_walks_the_track() {
        // 页 200 / 内容 400 = 一半：滑块 100 DIP，走完轨道剩 100 DIP。
        let top = BarGeom::new(200.0, 200.0, 10.0, 400.0, 0.0);
        assert_eq!(top.thumb.h, 100.0);
        assert_eq!(top.thumb.y, 0.0);
        let mid = BarGeom::new(200.0, 200.0, 10.0, 400.0, 100.0);
        assert_eq!(mid.thumb.y, 50.0);
        let end = BarGeom::new(200.0, 200.0, 10.0, 400.0, 200.0);
        assert_eq!(end.thumb.y + end.thumb.h, 200.0, "滑块底边贴住轨道底边");
    }

    #[test]
    fn a_tiny_thumb_still_has_a_grabbable_length() {
        let g = BarGeom::new(200.0, 200.0, 10.0, 100_000.0, 0.0);
        assert_eq!(g.thumb.h, MIN_THUMB);
    }

    #[test]
    fn a_bar_of_zero_still_scrolls() {
        // 声明 `bar = 0` 只是不要轨道：滚轮、拖拽之外的滚动照样有量。
        let g = BarGeom::new(200.0, 200.0, 0.0, 1000.0, 300.0);
        assert_eq!(g.track.w, 0.0);
        assert_eq!(g.thumb.w, 0.0);
        assert_eq!(g.content.w, 200.0, "没有轨道，内容区就是整个客户区");
        assert_eq!(g.max_scroll, 800.0);
        assert_eq!(g.scroll, 300.0);
    }

    #[test]
    fn content_that_fits_has_no_scroll_at_all() {
        let g = BarGeom::new(200.0, 200.0, 10.0, 120.0, 50.0);
        assert_eq!(g.max_scroll, 0.0);
        assert_eq!(g.scroll, 0.0, "装得下就把偏移归零");
        assert_eq!(g.track.w, 0.0, "不滚就不占轨道那一竖条");
        assert_eq!(g.content.w, 200.0);
    }

    #[test]
    fn the_offset_never_runs_past_either_end() {
        let far = BarGeom::new(200.0, 200.0, 10.0, 1000.0, 9_999.0);
        assert_eq!(far.scroll, 800.0);
        let behind = BarGeom::new(200.0, 200.0, 10.0, 1000.0, -20.0);
        assert_eq!(behind.scroll, 0.0);
    }

    #[test]
    fn a_track_wider_than_the_pane_cannot_eat_the_content() {
        // 声明 400 DIP 的条在 200 DIP 宽的窗格里：夹到 200，内容区宽 0 而不是负数。
        let g = BarGeom::new(200.0, 200.0, 400.0, 1000.0, 0.0);
        assert_eq!(g.track.w, 200.0);
        assert_eq!(g.content.w, 0.0);
    }
}
