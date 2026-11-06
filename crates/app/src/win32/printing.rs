//! 打印与打印预览。
//!
//! 「文件」菜单的 打印 (57607) 与 打印预览 (57609) 都从这一条管线走：
//! `[Print]` 配置读进来，默认打印机开口建 DC，文档按当前折行切页，每页 =
//! 页眉带 + 正文行 + 页脚带；打印路径把每页画进假脱机 DC，预览路径把
//! 同一页画进画布窗口（缩放只改世界变换，不重排）。页眉/页脚是
//! `<left>/<center>/<right>` 三段模板，宏共八种：`$(FilePath)`
//! `$(FileName)` `$(FileDir)` `$(DateTime)` `$(Date)` `$(Time)`
//! `$(CurPage)` `$(TotalPage)`。
//!
//! 正文行直接复用编辑器的绘制清单（行断点、行号栏都是同一份），所以
//! 预览里看到的分页就是打出来的分页：自动换行与行号两只按钮改的是
//! 当前文档自己的视图设置，彩色一只是预览框自己的开关。

use crate::win32::gdi::to_colorref;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    COLORREF, GlobalFree, HGLOBAL, HWND, LPARAM, LRESULT, RECT, SYSTEMTIME, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateDCW, CreateFontW, CreatePen, CreateSolidBrush, DeleteDC, DeleteObject,
    EndPaint, ExtTextOutW, FillRect, GetDC, GetDeviceCaps, GetTextExtentPoint32W,
    GetTextMetricsW, InvalidateRect, LineTo, MoveToEx, ReleaseDC, RestoreDC, SaveDC,
    SelectObject, SetBkMode, SetGraphicsMode, SetTextColor, SetWorldTransform, ValidateRect,
    ANTIALIASED_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEVMODEW, DM_OUT_BUFFER,
    ETO_OPTIONS, GM_ADVANCED, HBRUSH, HDC, HFONT, HGDIOBJ, LOGPIXELSX, LOGPIXELSY,
    OUT_DEFAULT_PRECIS, PHYSICALHEIGHT, PHYSICALWIDTH, PS_SOLID, TEXTMETRICW, TRANSPARENT, XFORM,
};
use windows::Win32::Graphics::Printing::{
    ClosePrinter, DocumentPropertiesW, OpenPrinterW, PRINTER_HANDLE,
};
use windows::Win32::Storage::Xps::{AbortDoc, EndDoc, EndPage, StartDocW, StartPage, DOCINFOW};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GHND};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::WindowsProgramming::GetProfileStringW;
use windows::Win32::UI::Controls::{SetScrollInfo, NMTTDISPINFOW, TBBUTTON, TTN_NEEDTEXTW};
use windows::Win32::UI::Controls::Dialogs::{
    PrintDlgExW, PD_NOCURRENTPAGE, PD_RESULT_APPLY, PD_RESULT_PRINT,
    PD_USEDEVMODECOPIESANDCOLLATE, PRINTDLGEXW, START_PAGE_GENERAL,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetAsyncKeyState, SetFocus, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetScrollInfo,
    GetSystemMetrics, GetWindowLongPtrW, GetWindowLongW, GetWindowRect, LoadCursorW, MessageBoxW,
    MoveWindow, PostMessageW, RegisterClassW, SendMessageW, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowLongW, SetWindowPos, ShowWindow, GWL_STYLE, GWLP_USERDATA,
    ICON_BIG, IDC_ARROW, MB_ICONERROR, MB_OK, SB_HORZ, SB_LINELEFT, SB_LINERIGHT, SB_PAGELEFT,
    SB_PAGERIGHT, SB_THUMBPOSITION, SB_THUMBTRACK, SB_VERT, SCROLLINFO, SIF_ALL, SIF_PAGE,
    SIF_POS, SIF_RANGE, SM_CXICON, SM_CYICON, SM_CXSCREEN, SM_CYSCREEN, SW_MAXIMIZE, SW_SHOW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_HSCROLL, WM_NOTIFY,
    WM_PAINT, WM_SETICON, WM_SIZE, WM_VSCROLL, WNDCLASSW, WS_BORDER, WS_CHILD, WS_HSCROLL,
    WS_OVERLAPPEDWINDOW, WS_SYSMENU, WS_VSCROLL, WS_VISIBLE,
};

use super::gdi::{base_points, pixel_height, EditorFace};
use super::lang;
use crate::app::WrapMode;
use crate::win32::window::EditorState;

// ---------------------------------------------------------------------------
// [Print] 配置
// ---------------------------------------------------------------------------

/// `[Print]` 段在打印那一刻读到的值。`margin` 的四个数依次是 左、上、下、右，
/// 单位毫米 —— 与「打印」页的四个输入框写盘的顺序一致。
pub(super) struct PrintConfig {
    pub margin: [i32; 4],
    pub mirror_font: bool,
    pub header: String,
    pub footer: String,
}

impl PrintConfig {
    fn load() -> Self {
        let get = |key: &str| crate::settings::section("Print", key);
        PrintConfig {
            margin: read_margin(get("Margin").as_deref()),
            mirror_font: get("MirrorFont")
                .map(|v| v.eq_ignore_ascii_case("y") || v == "1")
                .unwrap_or(false),
            header: get("Header").unwrap_or_default(),
            footer: get("Footer").unwrap_or_default(),
        }
    }
}

/// `[Print]` `Margin` 的解析：连续十六进制按小端双字读（「打印」页写盘的
/// 形态），逗号分节按十进制读，都认不得就落回每边 16 毫米。
/// 「打印」页的读取端（`print_load`）也走这里。
pub(super) fn read_margin(raw: Option<&str>) -> [i32; 4] {
    let mut out = [16i32; 4];
    let Some(raw) = raw else {
        return out;
    };
    let raw = raw.trim();
    let digits: Vec<u8> = raw
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .map(|c| c.to_digit(16).unwrap_or(0) as u8)
        .collect();
    if digits.len() >= 32 {
        for (i, slot) in out.iter_mut().enumerate() {
            let mut word = [0u8; 4];
            for j in 0..4 {
                word[j] = digits[i * 8 + j * 2] << 4 | digits[i * 8 + j * 2 + 1];
            }
            *slot = i32::from_le_bytes(word);
        }
        return out;
    }
    let parts: Vec<i32> = raw
        .split(',')
        .filter_map(|p| p.trim().parse::<i32>().ok())
        .collect();
    if parts.len() == 4 {
        out.copy_from_slice(&parts);
    }
    out
}

/// [`read_margin`] 的写盘形态：四个小端双字连成一条十六进制串。
pub(super) fn margin_hex(margin: [i32; 4]) -> String {
    let mut out = String::with_capacity(32);
    for value in margin {
        for byte in value.to_le_bytes() {
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 打印机
// ---------------------------------------------------------------------------

/// 一台打开的打印机：名字、缓冲好的 `DEVMODE`。名字来自 `windows` 配置节的
/// `device` 值（默认打印机），缓冲由 `DocumentProperties` 两次调用填出。
struct Printer {
    handle: PRINTER_HANDLE,
    name: Vec<u16>,
    devmode: Vec<u8>,
}

impl Printer {
    /// 默认打印机：`GetProfileStringW("windows", "device", …)` 给
    /// 「名,驱动,端口」，名字在第一个逗号前。没有配置节或没名字就没有打印机。
    fn open_default() -> Option<Printer> {
        let mut buf = [0u16; 512];
        let copied = unsafe {
            GetProfileStringW(w!("windows"), w!("device"), w!(","), Some(&mut buf))
        };
        let line = &buf[..copied.min(buf.len() as u32) as usize];
        let name_end = line
            .iter()
            .position(|&c| c == u16::from(b','))
            .unwrap_or(line.len());
        if name_end == 0 {
            return None;
        }
        let namez: Vec<u16> = line[..name_end].iter().copied().chain([0]).collect();
        let mut handle = PRINTER_HANDLE::default();
        if unsafe { OpenPrinterW(PCWSTR::from_raw(namez.as_ptr()), &mut handle, None) }.is_err() {
            return None;
        }
        // 两次 DocumentProperties：先问缓冲多大，再要一份 DEVMODE。
        let need = unsafe {
            DocumentPropertiesW(
                None,
                handle,
                PCWSTR::from_raw(namez.as_ptr()),
                None,
                None,
                DM_OUT_BUFFER.0,
            )
        };
        if need <= 0 {
            return None;
        }
        let mut devmode = vec![0u8; need as usize];
        unsafe {
            DocumentPropertiesW(
                None,
                handle,
                PCWSTR::from_raw(namez.as_ptr()),
                Some(devmode.as_mut_ptr() as *mut DEVMODEW),
                None,
                DM_OUT_BUFFER.0,
            )
        };
        Some(Printer {
            handle,
            name: namez,
            devmode,
        })
    }

    fn devmode(&self) -> &DEVMODEW {
        unsafe { &*(self.devmode.as_ptr() as *const DEVMODEW) }
    }

    /// 对话框改完之后把缓冲里的 DEVMODE 重读一遍：写回去再由驱动展开，
    /// 私有段才是对话框填的那份。
    fn refresh(&mut self) {
        unsafe {
            DocumentPropertiesW(
                None,
                self.handle,
                PCWSTR::from_raw(self.name.as_ptr()),
                Some(self.devmode.as_mut_ptr() as *mut DEVMODEW),
                None,
                DM_OUT_BUFFER.0,
            )
        };
    }

    /// 打印机 DC：CreateDC 的第四参喂进 DEVMODE，纸张和方向就是对话框选的。
    fn create_dc(&self, devmode: Option<&DEVMODEW>) -> Option<HDC> {
        let dc = unsafe {
            CreateDCW(
                PCWSTR::null(),
                PCWSTR::from_raw(self.name.as_ptr()),
                PCWSTR::null(),
                devmode.map(|m| m as *const DEVMODEW),
            )
        };
        if dc.is_invalid() {
            None
        } else {
            Some(dc)
        }
    }
}

impl Drop for Printer {
    fn drop(&mut self) {
        unsafe {
            let _ = ClosePrinter(self.handle);
        }
    }
}

// ---------------------------------------------------------------------------
// 分页布局
// ---------------------------------------------------------------------------

/// 一页的几何：纸、边距、页眉页脚带与正文带，全部是**打印机像素**；正文
/// 行高折算回屏幕像素后切行，所以折行断点与编辑器视图一致。
struct PageLayout {
    /// 纸的宽高（打印机像素）。
    page_w: i32,
    page_h: i32,
    /// 屏幕与打印机的 DPI 比，DrawList 回放的世界变换缩放。
    scale: f32,
    /// 内容区（纸减边距，打印机像素）。
    content: RECT,
    /// 页眉带高 / 页脚带高（打印机像素）—— 各一行正文的高。
    band_h: i32,
    /// 一页的正文行数（屏幕像素空间里除出来的）。
    rows_per_page: usize,
}

impl PageLayout {
    fn build(printer: &Printer, config: &PrintConfig, line_h_screen: f32, screen_dpi: u32) -> PageLayout {
        let dc = printer.create_dc(None);
        let (page_w, page_h, dpi_x, dpi_y) = match &dc {
            Some(hdc) => unsafe {
                (
                    GetDeviceCaps(Some(*hdc), PHYSICALWIDTH),
                    GetDeviceCaps(Some(*hdc), PHYSICALHEIGHT),
                    GetDeviceCaps(Some(*hdc), LOGPIXELSX),
                    GetDeviceCaps(Some(*hdc), LOGPIXELSY),
                )
            },
            None => (0, 0, 0, 0),
        };
        if let Some(hdc) = dc {
            unsafe {
                let _ = DeleteDC(hdc);
            }
        }
        // 纸或 DPI 拿不到（没有打印机）时按 A4@600 落底，布局仍成立，
        // 调用方在此之前就该拦住这一路。
        let page_w = if page_w > 0 { page_w } else { 4960 };
        let page_h = if page_h > 0 { page_h } else { 7016 };
        let dpi_x = if dpi_x > 0 { dpi_x } else { 600 };
        let dpi_y = if dpi_y > 0 { dpi_y } else { 600 };
        let mm_to_px = |mm: i32, dpi: i32| (mm as f64 * dpi as f64 / 25.4).round() as i32;
        let left = mm_to_px(config.margin[0], dpi_x);
        let top = mm_to_px(config.margin[1], dpi_y);
        let right = mm_to_px(config.margin[3], dpi_x);
        let bottom = mm_to_px(config.margin[2], dpi_y);
        let content = RECT {
            left,
            top,
            right: (page_w - right).max(left),
            bottom: (page_h - bottom).max(top),
        };
        let scale = dpi_x as f32 / screen_dpi.max(1) as f32;
        let band_h = (line_h_screen * scale).ceil() as i32;
        // 正文带：页眉页脚各让一行的高，剩下的除以屏幕行高。行高按屏幕
        // 像素折算 —— 分页发生在屏幕空间，断点与视图一致。
        let text_h_printer = (content.bottom - content.top - 2 * band_h).max(band_h);
        let text_h_screen = text_h_printer as f32 / scale;
        let rows_per_page = ((text_h_screen / line_h_screen.max(1.0)).floor() as usize).max(1);
        PageLayout {
            page_w,
            page_h,
            scale,
            content,
            band_h,
            rows_per_page,
        }
    }

    /// 一篇文档的页数：`ceil(总行数 / 每页行数)`，至少一页。
    fn page_count(&self, total_rows: usize) -> usize {
        (total_rows + self.rows_per_page - 1) / self.rows_per_page.max(1)
    }

    /// 第 `page` 页（0 基）的第一行。
    fn first_row(&self, page: usize) -> usize {
        page * self.rows_per_page
    }

    /// 内容区在屏幕像素下的宽高（画布摆放页面用）。
    fn content_screen(&self) -> (f32, f32) {
        (
            (self.content.right - self.content.left) as f32 / self.scale,
            (self.content.bottom - self.content.top) as f32 / self.scale,
        )
    }

    fn page_screen(&self) -> (f32, f32) {
        (self.page_w as f32 / self.scale, self.page_h as f32 / self.scale)
    }
}

// ---------------------------------------------------------------------------
// 页眉 / 页脚
// ---------------------------------------------------------------------------

/// 一条页眉/页脚模板：原文与 `<left>/<center>/<right>` 三段的内容。
struct Band {
    raw: String,
    left: String,
    center: String,
    right: String,
}

impl Band {
    /// `<left>…</left>` `<center>…</center>` `<right>…</right>` 逐段抓出来；
    /// 没写包装标签的模板整段按中间段处理。
    fn parse(raw: &str) -> Band {
        let grab = |open: &str, close: &str| -> Option<String> {
            let s = raw.find(open)? + open.len();
            let e = raw[s..].find(close)? + s;
            Some(raw[s..e].to_string())
        };
        let left = grab("<left>", "</left>").unwrap_or_default();
        let center = grab("<center>", "</center>").unwrap_or_default();
        let right = grab("<right>", "</right>").unwrap_or_default();
        let any = !left.is_empty() || !center.is_empty() || !right.is_empty();
        let (left, center, right) = if any {
            (left, center, right)
        } else {
            (String::new(), raw.to_string(), String::new())
        };
        Band {
            raw: raw.to_string(),
            left,
            center,
            right,
        }
    }

    fn is_empty(&self) -> bool {
        self.raw.trim().is_empty()
    }
}

/// 宏替换在画的这一刻做：页号是逐页变的，日期时间逐页取当刻。
fn expand_macros(text: &str, ctx: &MacroContext, out: &mut String) {
    const NAMES: [&str; 8] = [
        "FilePath", "FileName", "FileDir", "DateTime", "Date", "Time", "CurPage", "TotalPage",
    ];
    let mut rest = text;
    out.clear();
    while let Some(start) = rest.find("$(") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find(')') else {
            out.push_str(&rest[start..]);
            return;
        };
        let name = &after[..end];
        match NAMES.iter().position(|n| *n == name) {
            Some(0) => out.push_str(&ctx.file_path),
            Some(1) => out.push_str(&ctx.file_name),
            Some(2) => out.push_str(&ctx.file_dir),
            Some(3) => out.push_str(&now_text("%Y-%m-%d %H:%M:%S")),
            Some(4) => out.push_str(&now_text("%Y-%m-%d")),
            Some(5) => out.push_str(&now_text("%H:%M:%S")),
            Some(6) => out.push_str(&(ctx.cur_page + 1).to_string()),
            Some(7) => out.push_str(&ctx.total_pages.to_string()),
            _ => out.push_str(&rest[start..start + 2 + end + 1]),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
}

/// 宏展开的上下文：文档名三态与页号。
struct MacroContext {
    file_path: String,
    file_name: String,
    file_dir: String,
    cur_page: usize,
    total_pages: usize,
}

impl MacroContext {
    /// 无标题文档的路径三态：路径空、名字取 `15021`（无标题）、目录空。
    fn for_doc(path: Option<&str>, cur_page: usize, total_pages: usize) -> MacroContext {
        match path {
            Some(p) => {
                let name = std::path::Path::new(p)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.to_string());
                let dir = std::path::Path::new(p)
                    .parent()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                MacroContext {
                    file_path: p.to_string(),
                    file_name: name,
                    file_dir: dir,
                    cur_page,
                    total_pages,
                }
            }
            None => MacroContext {
                file_path: String::new(),
                file_name: lang::string(15021).unwrap_or("无标题").to_string(),
                file_dir: String::new(),
                cur_page,
                total_pages,
            },
        }
    }
}

/// 本地墙钟的 strftime 风格格式串：只认打印模板用到的三种写法，其余原样落。
/// 时间取系统本地时间，不在这里碰时区。
fn now_text(fmt: &str) -> String {
    let st: SYSTEMTIME = unsafe { GetLocalTime() };
    let mut out = String::with_capacity(fmt.len() + 8);
    let mut rest = fmt;
    while let Some(pos) = rest.find('%') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos + 1..];
        let mut chars = tail.chars();
        let spec = chars.next().unwrap_or('%');
        match spec {
            'Y' => out.push_str(&format!("{:04}", st.wYear)),
            'm' => out.push_str(&format!("{:02}", st.wMonth)),
            'd' => out.push_str(&format!("{:02}", st.wDay)),
            'H' => out.push_str(&format!("{:02}", st.wHour)),
            'M' => out.push_str(&format!("{:02}", st.wMinute)),
            'S' => out.push_str(&format!("{:02}", st.wSecond)),
            '%' => out.push('%'),
            other => {
                out.push('%');
                out.push(other);
            }
        }
        rest = &tail[spec.len_utf8()..];
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------
// DrawList → GDI 回放
// ---------------------------------------------------------------------------

/// 回放用的字体组：常规与三张带字形位的字面，尺寸都是屏幕像素 —— 缩放
/// 交给世界变换，预览与打印机两路共用同一组。
struct ReplayFonts {
    faces: [HFONT; 4],
    ascent: i32,
}

impl ReplayFonts {
    fn build(face: &EditorFace, dpi: u32) -> ReplayFonts {
        let px = pixel_height(dpi, face.points).max(1);
        let combos = [
            (i32::from(face.weight), face.italic),
            (700, face.italic),
            (i32::from(face.weight), true),
            (700, true),
        ];
        let mut faces = [HFONT::default(); 4];
        for (i, (weight, italic)) in combos.iter().enumerate() {
            let name: Vec<u16> = face.name.encode_utf16().chain([0]).collect();
            faces[i] = unsafe {
                CreateFontW(
                    -px,
                    0,
                    0,
                    0,
                    *weight,
                    u32::from(*italic),
                    0,
                    0,
                    DEFAULT_CHARSET,
                    OUT_DEFAULT_PRECIS,
                    CLIP_DEFAULT_PRECIS,
                    ANTIALIASED_QUALITY,
                    0,
                    PCWSTR::from_raw(name.as_ptr()),
                )
            };
        }
        // 基线取常规那张字面的上升段：DrawList 的 y 是行顶。
        let mut ascent = 0;
        if let Some(hdc) = screen_dc() {
            unsafe {
                let old = SelectObject(hdc, HGDIOBJ(faces[0].0));
                let mut tm = TEXTMETRICW::default();
                let _ = GetTextMetricsW(hdc, &mut tm);
                ascent = tm.tmAscent;
                SelectObject(hdc, old);
            }
            unsafe {
                let _ = ReleaseDC(None, hdc);
            }
        }
        ReplayFonts { faces, ascent }
    }

    fn face(&self, style: u8) -> HFONT {
        let bold = style & 1 != 0;
        let italic = style & 2 != 0;
        self.faces[(bold as usize) | ((italic as usize) << 1)]
    }
}

impl Drop for ReplayFonts {
    fn drop(&mut self) {
        for f in &self.faces {
            if !f.is_invalid() {
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(f.0));
                }
            }
        }
    }
}

fn screen_dc() -> Option<HDC> {
    let dc = unsafe { GetDC(None) };
    if dc.is_invalid() {
        None
    } else {
        Some(dc)
    }
}

/// 一次回放：把绘制清单按 `zoom` 缩放画进 `hdc`，原点是 `origin`（内容区
/// 左上角，已含平移）。`colors` 关掉时文字全部落黑、填充全部跳过 ——
/// 打出来的就是白纸黑字。`theme_bg` 是主题背景色：等于它的整幅填充跳过，
/// 页面底色留给纸面。
unsafe fn replay(
    hdc: HDC,
    cmds: &[dg_render::DrawCmd],
    fonts: &ReplayFonts,
    metrics: &dg_render::pipeline::LineMetrics<'_>,
    zoom: f32,
    origin: (f32, f32),
    colors: bool,
    theme_bg: dg_render::theme::Color,
) {
        unsafe {

    if zoom <= 0.0 {
        return;
    }
    let saved = SaveDC(hdc);
    let _ = SetGraphicsMode(hdc, GM_ADVANCED);
    let _ = SetWorldTransform(
        hdc,
        &XFORM {
            eM11: zoom,
            eM12: 0.0,
            eM21: 0.0,
            eM22: zoom,
            eDx: origin.0,
            eDy: origin.1,
        },
    );
    let old_font = SelectObject(hdc, HGDIOBJ(fonts.faces[0].0));
    let mut brush_cache: Option<(u32, HBRUSH)> = None;
    for cmd in cmds {
        match cmd {
            dg_render::DrawCmd::FillRect { rect, color } => {
                if !colors || *color == theme_bg {
                    // 页面底色那层留给纸面；彩色关的时候所有填充都不进纸。
                    continue;
                }
                let colorref = to_colorref(*color).0;
                let brush = match brush_cache {
                    Some((c, b)) if c == colorref => b,
                    _ => {
                        if let Some((_, b)) = brush_cache {
                            let _ = DeleteObject(HGDIOBJ(b.0));
                        }
                        let b = CreateSolidBrush(COLORREF(colorref));
                        brush_cache = Some((colorref, b));
                        b
                    }
                };
                let r = RECT {
                    left: rect.x.round() as i32,
                    top: rect.y.round() as i32,
                    right: (rect.x + rect.w).round() as i32,
                    bottom: (rect.y + rect.h).round() as i32,
                };
                let _ = FillRect(hdc, &r, brush);
            }
            dg_render::DrawCmd::AlphaBlend { .. } => {
                // 半透明带在纸上没有对应物，跳过。
            }
            dg_render::DrawCmd::TextRun { x, y, text, color, style } => {
                let ink = if colors {
                    *color
                } else {
                    dg_render::theme::Color::rgb(0, 0, 0)
                };
                let _ = SetTextColor(hdc, to_colorref(ink));
                let font = fonts.face(*style);
                SelectObject(hdc, HGDIOBJ(font.0));
                // 每一格的推进量按布局网格走：制表符的位置相关推进也在内。
                // 代理对两个单元共享一格 —— 首单元拿推进量，尾单元 0。
                let mut utf16: Vec<u16> = Vec::with_capacity(text.len());
                let mut dx: Vec<i32> = Vec::with_capacity(text.len());
                let mut px = x.round() as i32;
                for ch in text.chars() {
                    let adv = if ch == '\t' {
                        metrics.tab_from(px)
                    } else {
                        metrics.advance(ch)
                    };
                    let mut units = [0u16; 2];
                    let encoded = ch.encode_utf16(&mut units);
                    let count = encoded.len();
                    utf16.extend_from_slice(encoded);
                    dx.push(adv);
                    for _ in 1..count {
                        dx.push(0);
                    }
                    px += adv;
                }
                if utf16.is_empty() {
                    continue;
                }
                let baseline = *y + fonts.ascent as f32;
                let _ = ExtTextOutW(
                    hdc,
                    x.round() as i32,
                    baseline.round() as i32,
                    ETO_OPTIONS(0),
                    None,
                    PCWSTR::from_raw(utf16.as_ptr()),
                    utf16.len() as u32,
                    Some(dx.as_ptr()),
                );
            }
        }
    }
    if let Some((_, b)) = brush_cache {
        let _ = DeleteObject(HGDIOBJ(b.0));
    }
    SelectObject(hdc, old_font);
    if saved != 0 {
        let _ = RestoreDC(hdc, saved);
    }
        }
    }

/// 一条带（页眉/页脚）的绘制：三段文字落进带矩形，分隔线横贯内容区。
/// 坐标在内容空间里（世界变换之内），`y0` 是带的顶，文字的笔位要再让出
/// 字面的上升段。
unsafe fn draw_band(
    hdc: HDC,
    band: &Band,
    fallback: BandFallback,
    ctx: &MacroContext,
    rect: RECT,
    rule_y: i32,
    ascent: i32,
) {
        unsafe {

    let buf = &mut String::new();
    let _ = SetBkMode(hdc, TRANSPARENT);
    let _ = SetTextColor(hdc, COLORREF(0));
    let y = rect.top + ascent;
    let width = rect.right - rect.left;
    if band.is_empty() {
        match fallback {
            BandFallback::NameDate => {
                draw_left(hdc, &ctx.file_name, rect.left, y);
                let date = now_text("%Y-%m-%d");
                draw_right(hdc, &date, rect.right, y);
            }
            BandFallback::PageNums => {
                let text = format!("({}/{})", ctx.cur_page + 1, ctx.total_pages);
                draw_center(hdc, &text, rect.left, width, y);
            }
        }
    } else {
        if !band.left.is_empty() {
            expand_macros(&band.left, ctx, buf);
            draw_left(hdc, buf, rect.left, y);
        }
        if !band.center.is_empty() {
            expand_macros(&band.center, ctx, buf);
            draw_center(hdc, buf, rect.left, width, y);
        }
        if !band.right.is_empty() {
            expand_macros(&band.right, ctx, buf);
            draw_right(hdc, buf, rect.right, y);
        }
    }
    // 分隔线：页眉在带底，页脚在带顶。
    let pen = CreatePen(PS_SOLID, 1, COLORREF(0));
    let old = SelectObject(hdc, HGDIOBJ(pen.0));
    let _ = MoveToEx(hdc, rect.left, rule_y, None);
    let _ = LineTo(hdc, rect.right, rule_y);
    SelectObject(hdc, old);
    let _ = DeleteObject(HGDIOBJ(pen.0));
        }
    }

enum BandFallback {
    /// 页眉空：文件名靠左、日期靠右。
    NameDate,
    /// 页脚空：`(当前页/总页)` 居中。
    PageNums,
}

unsafe fn draw_left(hdc: HDC, text: &str, x: i32, y: i32) {
        unsafe {

    let utf16: Vec<u16> = text.encode_utf16().collect();
    if !utf16.is_empty() {
        let _ = ExtTextOutW(
            hdc,
            x,
            y,
            ETO_OPTIONS(0),
            None,
            PCWSTR::from_raw(utf16.as_ptr()),
            utf16.len() as u32,
            None,
        );
    }
        }
    }

unsafe fn draw_right(hdc: HDC, text: &str, right: i32, y: i32) {
        unsafe {

    let utf16: Vec<u16> = text.encode_utf16().collect();
    if !utf16.is_empty() {
        let w = text_width(hdc, &utf16);
        let _ = ExtTextOutW(
            hdc,
            right - w,
            y,
            ETO_OPTIONS(0),
            None,
            PCWSTR::from_raw(utf16.as_ptr()),
            utf16.len() as u32,
            None,
        );
    }
        }
    }

unsafe fn draw_center(hdc: HDC, text: &str, left: i32, width: i32, y: i32) {
        unsafe {

    let utf16: Vec<u16> = text.encode_utf16().collect();
    if !utf16.is_empty() {
        let w = text_width(hdc, &utf16);
        let _ = ExtTextOutW(
            hdc,
            left + ((width - w).max(0) / 2),
            y,
            ETO_OPTIONS(0),
            None,
            PCWSTR::from_raw(utf16.as_ptr()),
            utf16.len() as u32,
            None,
        );
    }
        }
    }

unsafe fn text_width(hdc: HDC, utf16: &[u16]) -> i32 {
        unsafe {

    let mut size = windows::Win32::Foundation::SIZE::default();
    let _ = GetTextExtentPoint32W(hdc, utf16, &mut size);
    size.cx
        }
    }

/// 页边四角记号：内容区四只角的浅灰短线，标出页边距的走线。坐标是目标
/// DC 的设备像素（世界变换之外）。
unsafe fn draw_margin_marks(hdc: HDC, content: RECT, len: i32) {
        unsafe {

    let pen = CreatePen(PS_SOLID, 1, COLORREF(0xC0C0C0));
    let old = SelectObject(hdc, HGDIOBJ(pen.0));
    let (l, t, r, b) = (content.left, content.top, content.right, content.bottom);
    let _ = MoveToEx(hdc, l, t, None);
    let _ = LineTo(hdc, l + len, t);
    let _ = MoveToEx(hdc, l, t, None);
    let _ = LineTo(hdc, l, t + len);
    let _ = MoveToEx(hdc, r - len, t, None);
    let _ = LineTo(hdc, r, t);
    let _ = MoveToEx(hdc, r, t, None);
    let _ = LineTo(hdc, r, t + len);
    let _ = MoveToEx(hdc, l, b - len, None);
    let _ = LineTo(hdc, l, b);
    let _ = MoveToEx(hdc, l, b, None);
    let _ = LineTo(hdc, l + len, b);
    let _ = MoveToEx(hdc, r - len, b, None);
    let _ = LineTo(hdc, r, b);
    let _ = MoveToEx(hdc, r, b - len, None);
    let _ = LineTo(hdc, r, b);
    SelectObject(hdc, old);
    let _ = DeleteObject(HGDIOBJ(pen.0));
        }
    }

// ---------------------------------------------------------------------------
// 一页的完整绘制
// ---------------------------------------------------------------------------

/// 把一页画进 `hdc`：正文（DrawList 回放）+ 页眉 + 页脚 + 页边记号。
/// `content_space` 是内容区原点在目标 DC 里的位置，`zoom` 是缩放。
/// 打印路径 `zoom` = DPI 比、原点 = 内容区在纸上的位置；预览路径原点 =
/// 页面在画布上的位置加内容区偏移。
unsafe fn render_page(
    hdc: HDC,
    st: &mut EditorState,
    fonts: &ReplayFonts,
    layout: &PageLayout,
    bands: (&Band, &Band),
    doc_path: Option<String>,
    page: usize,
    total: usize,
    colors: bool,
    content_space: (f32, f32),
    zoom: f32,
) {
        unsafe {

    let mac = MacroContext::for_doc(doc_path.as_deref(), page, total);
    // 正文：这一页的行带。
    let first = layout.first_row(page);
    let rows = layout.rows_per_page;
    let (content_w, content_h) = layout.content_screen();
    let mut draw = dg_render::DrawList::default();
    st.build_print_page(first, rows, content_w, &mut draw);
    // 网格：与编辑器行进同一套（tab 的位置相关推进在这里重算）。
    let tab = st.docs.act().tab_size as usize;
    let metrics = dg_render::pipeline::LineMetrics::new(&st.docs.act().theme, tab);
    // 回放正文（世界变换之内）。
    replay(
        hdc,
        &draw.cmds,
        fonts,
        &metrics,
        zoom,
        content_space,
        colors,
        st.docs.act().theme.background,
    );
    // 页眉/页脚的矩形在内容空间（屏幕像素坐标），同一只变换之内画。
    let saved = SaveDC(hdc);
    let _ = SetGraphicsMode(hdc, GM_ADVANCED);
    let _ = SetWorldTransform(
        hdc,
        &XFORM {
            eM11: zoom,
            eM12: 0.0,
            eM21: 0.0,
            eM22: zoom,
            eDx: content_space.0,
            eDy: content_space.1,
        },
    );
    let band_h = (layout.band_h as f32 / layout.scale).round() as i32;
    let header_rect = RECT {
        left: 0,
        top: 0,
        right: content_w.round() as i32,
        bottom: band_h,
    };
    draw_band(
        hdc,
        bands.0,
        BandFallback::NameDate,
        &mac,
        header_rect,
        header_rect.bottom,
        fonts.ascent,
    );
    // 页脚：带底在内容区底。
    let footer_rect = RECT {
        left: 0,
        top: content_h.round() as i32 - band_h,
        right: content_w.round() as i32,
        bottom: content_h.round() as i32,
    };
    draw_band(
        hdc,
        bands.1,
        BandFallback::PageNums,
        &mac,
        footer_rect,
        footer_rect.top,
        fonts.ascent,
    );
    if saved != 0 {
        let _ = RestoreDC(hdc, saved);
    }
    // 页边记号按设备像素画：不用变换，直接落在内容区四角。
    let len = (8.0 * zoom).round() as i32;
    let content_px = RECT {
        left: content_space.0.round() as i32,
        top: content_space.1.round() as i32,
        right: (content_space.0 + content_w * zoom).round() as i32,
        bottom: (content_space.1 + content_h * zoom).round() as i32,
    };
    draw_margin_marks(hdc, content_px, len);
        }
    }

// ---------------------------------------------------------------------------
// 打印对话框与作业
// ---------------------------------------------------------------------------

/// 打印：PrintDlgEx 选好打印机，认可后整篇进假脱机。返回是否真的出了作业。
pub(super) fn run_print(st: &mut EditorState, hwnd: HWND) -> bool {
    let Some(mut printer) = Printer::open_default() else {
        no_printer_message(hwnd);
        return false;
    };
    let config = PrintConfig::load();
    let doc_path = st.docs.act().path.clone();
    let total_rows = st.docs.act().total_rows;
    let dpi = st.editor_dpi();
    let line_h_screen = st.docs.act().theme.line_height.max(1.0);
    let layout = PageLayout::build(&printer, &config, line_h_screen, dpi);
    let total_pages = layout.page_count(total_rows);

    // 对话框：无页面范围（整篇），副本与逐份交给驱动。
    let mut pd = PRINTDLGEXW {
        lStructSize: std::mem::size_of::<PRINTDLGEXW>() as u32,
        hwndOwner: hwnd,
        Flags: PD_NOCURRENTPAGE | PD_USEDEVMODECOPIESANDCOLLATE,
        nStartPage: START_PAGE_GENERAL,
        ..Default::default()
    };
    pd.hDevMode = devmode_to_hglobal(printer.devmode());
    let mut result = PD_RESULT_PRINT;
    unsafe {
        if PrintDlgExW(&mut pd).is_ok() {
            result = pd.dwResultAction;
            if result == PD_RESULT_PRINT || result == PD_RESULT_APPLY {
                if !pd.hDevMode.is_invalid() {
                    let ptr = GlobalLock(pd.hDevMode);
                    if !ptr.is_null() {
                        let bytes = std::slice::from_raw_parts(
                            ptr as *const u8,
                            printer.devmode.len(),
                        );
                        printer.devmode.copy_from_slice(bytes);
                        let _ = GlobalUnlock(pd.hDevMode);
                    }
                    printer.refresh();
                }
            }
        }
    }
    if !pd.hDevMode.is_invalid() {
        unsafe {
            let _ = GlobalFree(Some(pd.hDevMode));
        }
    }
    if result != PD_RESULT_PRINT {
        return false;
    }
    spool_document(st, hwnd, &mut printer, &config, &layout, total_pages, doc_path)
}

fn devmode_to_hglobal(devmode: &DEVMODEW) -> HGLOBAL {
    let bytes = unsafe {
        std::slice::from_raw_parts(devmode as *const DEVMODEW as *const u8, devmode.dmSize as usize)
    };
    match unsafe { GlobalAlloc(GHND, bytes.len() + devmode.dmDriverExtra as usize) } {
        Ok(hg) => {
            let ptr = unsafe { GlobalLock(hg) };
            if !ptr.is_null() {
                unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len()) };
                let _ = unsafe { GlobalUnlock(hg) };
            }
            hg
        }
        Err(_) => HGLOBAL::default(),
    }
}

/// 假脱机作业：StartDoc → 每页 StartPage/画/EndPage → EndDoc。ESC 按下
/// 的那一刻停（AbortDoc 收尾），别让整篇锁死在一条同步作业里。
fn spool_document(
    st: &mut EditorState,
    hwnd: HWND,
    printer: &mut Printer,
    config: &PrintConfig,
    layout: &PageLayout,
    total_pages: usize,
    doc_path: Option<String>,
) -> bool {
    let Some(hdc) = printer.create_dc(Some(printer.devmode())) else {
        no_printer_message(hwnd);
        return false;
    };
    let doc_name = doc_path
        .as_deref()
        .and_then(|p| std::path::Path::new(p).file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .or_else(|| doc_path.clone())
        .unwrap_or_else(|| lang::string(15021).unwrap_or("无标题").to_string());
    let namez: Vec<u16> = doc_name.encode_utf16().chain([0]).collect();
    let docinfo = DOCINFOW {
        cbSize: std::mem::size_of::<DOCINFOW>() as i32,
        lpszDocName: PCWSTR::from_raw(namez.as_ptr()),
        lpszOutput: PCWSTR::null(),
        lpszDatatype: PCWSTR::null(),
        fwType: 0,
    };
    let face = print_face(st, config);
    let fonts = ReplayFonts::build(&face, st.editor_dpi());
    let header = Band::parse(&config.header);
    let footer = Band::parse(&config.footer);
    // 直接打印一路不带颜色：白纸黑字。
    let colors = false;
    let job = unsafe { StartDocW(hdc, &docinfo) };
    let mut aborted = job <= 0;
    if !aborted {
        let content_origin = (layout.content.left as f32, layout.content.top as f32);
        for page in 0..total_pages {
            if unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) } as u16 & 0x8000 != 0 {
                aborted = true;
                break;
            }
            if unsafe { StartPage(hdc) } <= 0 {
                aborted = true;
                break;
            }
            unsafe {
                render_page(
                    hdc,
                    st,
                    &fonts,
                    layout,
                    (&header, &footer),
                    doc_path.clone(),
                    page,
                    total_pages,
                    colors,
                    content_origin,
                    layout.scale,
                )
            };
            if unsafe { EndPage(hdc) } <= 0 {
                aborted = true;
                break;
            }
        }
        unsafe {
            if aborted {
                AbortDoc(hdc);
            } else {
                let _ = EndDoc(hdc);
            }
        }
    }
    unsafe {
        let _ = DeleteDC(hdc);
    }
    !aborted
}

/// 打印字体：勾了「直接使用当前编辑器视图的字体」就取编辑器那只（它按
/// 主题 `[Fonts] mono` > 〔设置字体〕槽 > 内置的优先级）；没勾就用
/// 〔设置字体〕的打印槽，再没有才落回 Courier New 10pt。
fn print_face(st: &EditorState, config: &PrintConfig) -> EditorFace {
    if config.mirror_font {
        let points = st.docs.act().font_points.unwrap_or_else(base_points);
        EditorFace::current(points)
    } else if let Some(spec) = super::fontcfg::print_spec() {
        // 〔设置字体〕的第 9 个槽（1438 打印字体）：写了就用它，没写仍按出厂
        // 的 Courier New。主题没有打印字体 token，镜像分支走的是当前视图字体，
        // 不读这个槽。
        let points = spec.points as f32;
        spec.into_face(points)
    } else {
        EditorFace {
            name: "Courier New".to_string(),
            points: 10.0,
            weight: 400,
            italic: false,
        }
    }
}

fn no_printer_message(hwnd: HWND) {
    let text = lang::string(15108)
        .unwrap_or("无法初始化打印程序,请确认您是否安装了一个打印机!")
        .to_string();
    let tz: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let title: Vec<u16> = "DogEdit".encode_utf16().chain([0]).collect();
    unsafe {
        MessageBoxW(
            Some(hwnd),
            PCWSTR::from_raw(tz.as_ptr()),
            PCWSTR::from_raw(title.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

// ---------------------------------------------------------------------------
// 打印预览窗口
// ---------------------------------------------------------------------------

/// 预览工具栏的按钮 id：打印沿用命令 id，其余从 2200 起，与语言表的
/// 字符串 id 同号（工具栏文案与提示语都从那里取）。
const TB_PRINT: u32 = 57607;
const TB_NEXT: u32 = 2200;
const TB_PREV: u32 = 2201;
const TB_ZOOM_IN: u32 = 2202;
const TB_ZOOM_OUT: u32 = 2203;
const TB_CLOSE: u32 = 2204;
const TB_COLOR: u32 = 2205;
const TB_WRAP: u32 = 2206;
const TB_LINES: u32 = 2207;
const TB_CONFIG: u32 = 2209;

const TB_BUTTONSTRUCTSIZE: u32 = 0x041E;
const TB_ADDBUTTONSW: u32 = 0x0444;
const TB_ADDSTRINGW: u32 = 0x044D;
const TB_ENABLEBUTTON: u32 = 0x0401;
const TB_CHECKBUTTON: u32 = 0x0402;
const TB_GETBUTTONSIZE: u32 = 0x043A;
const TB_AUTOSIZE: u32 = 0x0421;
const TB_ISBUTTONCHECKED: u32 = 0x040E;
const TBSTYLE_LIST: u32 = 0x1000;
const TBSTYLE_FLAT: u32 = 0x0800;
const TBSTYLE_TOOLTIPS: u32 = 0x0100;
const BTNS_BUTTON: u8 = 0x00;
const BTNS_SEP: u8 = 0x01;
const BTNS_CHECK: u8 = 0x02;
const BTNS_AUTOSIZE: u8 = 0x10;
const TBSTATE_ENABLED: u8 = 0x04;
const I_IMAGENONE: i32 = -2;

const SB_SETTEXTW: u32 = 0x040B;

const PREVIEW_CLASS: PCWSTR = w!("DogEditPrintPreview");
const CANVAS_CLASS: PCWSTR = w!("DogEditPrintPreviewPage");

/// 预览框自己的状态，挂在预览窗口的 `GWLP_USERDATA` 上。
struct PreviewState {
    /// 主框架窗口：编辑器状态、换行/行号开关、选项对话框都从它来。
    frame: HWND,
    canvas: HWND,
    toolbar: HWND,
    status: HWND,
    layout: PageLayout,
    total_pages: usize,
    cur_page: usize,
    zoom: f32,
    color: bool,
    fonts: ReplayFonts,
    header: Band,
    footer: Band,
    doc_path: Option<String>,
    /// 页面画布的滚动原点（页面留白 32px 之外再平移的量）。
    hscroll: i32,
    vscroll: i32,
}

/// 预览窗口句柄的壳：主框架拿它判活、销毁、聚焦。
#[derive(Default)]
pub(super) struct PreviewWindow {
    hwnd: HWND,
}

impl PreviewWindow {
    pub(super) fn alive(&self) -> bool {
        !self.hwnd.is_invalid()
    }

    pub(super) fn focus(&self) {
        if self.alive() {
            unsafe {
                let _ = SetForegroundWindow(self.hwnd);
            }
        }
    }

    pub(super) fn close(&mut self) {
        if self.alive() {
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        }
        self.hwnd = HWND::default();
    }
}

/// 打印预览 (57609)：一台预览框。已经开着就把它带到前台，不再开第二台。
pub(super) fn open_preview(st: &mut EditorState, frame: HWND) {
    if st.print_preview.alive() {
        st.print_preview.focus();
        return;
    }
    let Some(printer) = Printer::open_default() else {
        no_printer_message(frame);
        return;
    };
    let config = PrintConfig::load();
    let dpi = st.editor_dpi();
    let line_h_screen = st.docs.act().theme.line_height.max(1.0);
    let layout = PageLayout::build(&printer, &config, line_h_screen, dpi);
    let total_pages = layout.page_count(st.docs.act().total_rows);
    let face = print_face(st, &config);
    let fonts = ReplayFonts::build(&face, dpi);
    let header = Band::parse(&config.header);
    let footer = Band::parse(&config.footer);
    let doc_path = st.docs.act().path.clone();
    let doc_title = st.docs.act().display_name();
    let wrap_on = st.print_wrap_on();
    let lines_on = st.print_lines_on();

    register_classes();
    // 预览框：标题 149（打印预览）带文档名，标题栏图标取主框架那只。
    let instance = hinstance();
    let title = format!(
        "{} - {}",
        lang::string(149).unwrap_or("打印预览"),
        doc_title
    );
    let titlez: Vec<u16> = title.encode_utf16().chain([0]).collect();
    let icon = super::window::frame_icon(instance, SM_CXICON, SM_CYICON);
    let created = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PREVIEW_CLASS,
            PCWSTR::from_raw(titlez.as_ptr()),
            WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0 | WS_SYSMENU.0),
            16,
            16,
            (GetSystemMetrics(SM_CXSCREEN) * 3 / 4).max(640),
            (GetSystemMetrics(SM_CYSCREEN) * 3 / 4).max(480),
            None,
            None,
            Some(instance),
            None,
        )
    };
    let Ok(hwnd) = created else {
        return;
    };
    if !icon.is_invalid() {
        unsafe {
            SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_BIG as usize)),
                Some(LPARAM(icon.0 as isize)),
            );
        }
    }
    let state = Box::new(PreviewState {
        frame,
        canvas: HWND::default(),
        toolbar: HWND::default(),
        status: HWND::default(),
        layout,
        total_pages,
        cur_page: 0,
        zoom: 1.0,
        color: false,
        fonts,
        header,
        footer,
        doc_path,
        hscroll: 0,
        vscroll: 0,
    });
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
        create_children(hwnd);
        // 状态条先填一次，两只勾的初值跟当前文档走。
        update_page_status(hwnd);
        if let Some(pv) = state_of(hwnd) {
            if !pv.toolbar.is_invalid() {
                let _ = SendMessageW(
                    pv.toolbar,
                    TB_CHECKBUTTON,
                    Some(WPARAM(TB_WRAP as usize)),
                    Some(LPARAM(wrap_on as isize)),
                );
                let _ = SendMessageW(
                    pv.toolbar,
                    TB_CHECKBUTTON,
                    Some(WPARAM(TB_LINES as usize)),
                    Some(LPARAM(lines_on as isize)),
                );
            }
        }
        // 主编辑区禁用：预览开着时不接受键入（关闭时恢复）。
        let view = st.active_view_hwnd();
        if !view.is_invalid() {
            let _ = EnableWindow(view, false);
        }
        let _ = ShowWindow(hwnd, SW_MAXIMIZE);
        let _ = ShowWindow(hwnd, SW_SHOW);
    }
    st.print_preview.hwnd = hwnd;
}

fn hinstance() -> windows::Win32::Foundation::HINSTANCE {
    unsafe { windows::Win32::Foundation::HINSTANCE(GetModuleHandleW(None).unwrap_or_default().0) }
}

fn register_classes() {
    static DONE: std::sync::Once = std::sync::Once::new();
    DONE.call_once(|| unsafe {
        let instance = hinstance();
        let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(preview_proc),
            hInstance: instance,
            lpszClassName: PREVIEW_CLASS,
            hCursor: cursor,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(canvas_proc),
            hInstance: instance,
            lpszClassName: CANVAS_CLASS,
            hCursor: cursor,
            ..Default::default()
        };
        RegisterClassW(&wc);
    });
}

unsafe fn create_children(hwnd: HWND) {
        unsafe {

    let Some(state) = state_of(hwnd) else { return };
    let instance = hinstance();
    // 工具栏：文字按钮（BTNS_AUTOSIZE），提示语与文案都来自语言表。
    let toolbar = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("ToolbarWindow32"),
        w!(""),
        WINDOW_STYLE((WS_CHILD | WS_VISIBLE).0),
        0,
        0,
        0,
        0,
        Some(hwnd),
        None,
        Some(instance),
        None,
    )
    .unwrap_or_default();
    state.toolbar = toolbar;
    if !toolbar.is_invalid() {
        SendMessageW(
            toolbar,
            TB_BUTTONSTRUCTSIZE,
            Some(WPARAM(std::mem::size_of::<TBBUTTON>())),
            None,
        );
        let style = GetWindowLongW(toolbar, GWL_STYLE);
        SetWindowLongW(
            toolbar,
            GWL_STYLE,
            style | (TBSTYLE_LIST | TBSTYLE_FLAT | TBSTYLE_TOOLTIPS) as i32,
        );
        add_toolbar_buttons(toolbar);
        SendMessageW(toolbar, TB_AUTOSIZE, None, None);
    }
    // 状态栏：一段，页号。
    let status = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("msctls_statusbar32"),
        w!(""),
        WINDOW_STYLE((WS_CHILD | WS_VISIBLE).0),
        0,
        0,
        0,
        0,
        Some(hwnd),
        None,
        Some(instance),
        None,
    )
    .unwrap_or_default();
    state.status = status;
    // 页面画布。
    let canvas = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        CANVAS_CLASS,
        w!(""),
        WINDOW_STYLE(
            (WS_CHILD | WS_VISIBLE | WS_HSCROLL | WS_VSCROLL | WS_BORDER).0,
        ),
        0,
        0,
        0,
        0,
        Some(hwnd),
        None,
        Some(instance),
        None,
    )
    .unwrap_or_default();
    state.canvas = canvas;
    if !canvas.is_invalid() {
        SetWindowLongPtrW(canvas, GWLP_USERDATA, hwnd.0 as isize);
    }
        }
    }

unsafe fn add_toolbar_buttons(toolbar: HWND) {
        unsafe {

    let rows: &[(u32, u8)] = &[
        (TB_PRINT, BTNS_BUTTON),
        (0, BTNS_SEP),
        (TB_NEXT, BTNS_BUTTON),
        (TB_PREV, BTNS_BUTTON),
        (0, BTNS_SEP),
        (TB_COLOR, BTNS_CHECK),
        (TB_WRAP, BTNS_CHECK),
        (TB_LINES, BTNS_CHECK),
        (0, BTNS_SEP),
        (TB_ZOOM_IN, BTNS_BUTTON),
        (TB_ZOOM_OUT, BTNS_BUTTON),
        (0, BTNS_SEP),
        (TB_CONFIG, BTNS_BUTTON),
        (TB_CLOSE, BTNS_BUTTON),
    ];
    // 文案池：每只按钮一条宽字符串，TB_ADDSTRINGW 一次全入。
    let mut pool: Vec<u16> = Vec::new();
    for &(id, style) in rows {
        if style == BTNS_SEP {
            continue;
        }
        let text = button_text(id);
        pool.extend(text.encode_utf16());
        pool.push(0);
    }
    pool.push(0);
    let first = SendMessageW(
        toolbar,
        TB_ADDSTRINGW,
        None,
        Some(LPARAM(pool.as_ptr() as isize)),
    )
    .0 as i32;
    let mut buttons: Vec<TBBUTTON> = Vec::new();
    let mut index = 0i32;
    for &(id, style) in rows {
        if style == BTNS_SEP {
            buttons.push(TBBUTTON {
                iBitmap: 8,
                idCommand: 0,
                fsState: TBSTATE_ENABLED,
                fsStyle: BTNS_SEP,
                bReserved: [0; 6],
                dwData: 0,
                iString: 0,
            });
            continue;
        }
        buttons.push(TBBUTTON {
            iBitmap: I_IMAGENONE,
            idCommand: id as i32,
            fsState: TBSTATE_ENABLED,
            fsStyle: style | BTNS_AUTOSIZE,
            bReserved: [0; 6],
            dwData: 0,
            iString: (first + index) as isize,
        });
        index += 1;
    }
    SendMessageW(
        toolbar,
        TB_ADDBUTTONSW,
        Some(WPARAM(buttons.len())),
        Some(LPARAM(buttons.as_ptr() as isize)),
    );
        }
    }

/// 按钮文案：语言表同号条目的第二行（`提示语\n文案` 的文案半句），
/// 打印那只取菜单行的文案，快捷键与括注都不要。
fn button_text(id: u32) -> String {
    let raw = if id == TB_PRINT {
        lang::string(id).unwrap_or("打印")
    } else {
        let raw = lang::string(id).unwrap_or("");
        raw.split('\n').nth(1).unwrap_or(raw)
    };
    let mut text = raw.split('\t').next().unwrap_or(raw).to_string();
    if let Some(start) = text.find("(&") {
        text.truncate(start);
    }
    if text.is_empty() {
        format!("按钮{id}")
    } else {
        text
    }
}

/// 提示语：同一条目的第一行。
fn button_tip(id: u32) -> String {
    let raw = lang::string(id).unwrap_or("");
    raw.split('\n').next().unwrap_or(raw).to_string()
}

unsafe fn state_of(hwnd: HWND) -> Option<&'static mut PreviewState> {
        unsafe {

    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    if ptr == 0 {
        return None;
    }
    Some(&mut *(ptr as *mut PreviewState))
        }
    }

unsafe fn update_page_status(hwnd: HWND) {
        unsafe {

    if let Some(state) = state_of(hwnd) {
        let cur = (state.cur_page + 1).to_string();
        let total = state.total_pages.max(1).to_string();
        let text = lang::format(2208, &[&cur, &total])
            .unwrap_or_else(|| format!("页 {} of {}", cur, total));
        let textz: Vec<u16> = text.encode_utf16().chain([0]).collect();
        if !state.status.is_invalid() {
            SendMessageW(
                state.status,
                SB_SETTEXTW,
                Some(WPARAM(0)),
                Some(LPARAM(textz.as_ptr() as isize)),
            );
        }
        // 翻页两只的可点态跟着页走。
        if !state.toolbar.is_invalid() {
            let _ = SendMessageW(
                state.toolbar,
                TB_ENABLEBUTTON,
                Some(WPARAM(TB_NEXT as usize)),
                Some(LPARAM((state.cur_page + 1 < state.total_pages) as isize)),
            );
            let _ = SendMessageW(
                state.toolbar,
                TB_ENABLEBUTTON,
                Some(WPARAM(TB_PREV as usize)),
                Some(LPARAM((state.cur_page > 0) as isize)),
            );
        }
    }
        }
    }

unsafe extern "system" fn preview_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
        unsafe {

    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as u32;
            handle_button(hwnd, id);
            LRESULT(0)
        }
        WM_NOTIFY => {
            let nm = lparam.0 as *mut NMTTDISPINFOW;
            if !nm.is_null() && (*nm).hdr.code == TTN_NEEDTEXTW {
                let id = (*nm).hdr.idFrom as u32;
                let tip = button_tip(id);
                let len = tip.chars().count().min(79);
                for (i, ch) in tip.chars().take(len).enumerate() {
                    (*nm).szText[i] = u16::try_from(ch).unwrap_or(0);
                }
                (*nm).szText[len] = 0;
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_SIZE => {
            layout_children(hwnd);
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            destroy_state(hwnd);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
        }
    }

unsafe fn layout_children(hwnd: HWND) {
        unsafe {

    let Some(state) = state_of(hwnd) else { return };
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let w = rc.right - rc.left;
    let h = rc.bottom - rc.top;
    // 工具栏按内容高走；状态栏收一条 WM_SIZE 自己铺底；画布吃剩下的。
    let mut tb_h = 0;
    if !state.toolbar.is_invalid() {
        SendMessageW(state.toolbar, TB_AUTOSIZE, None, None);
        let size = SendMessageW(state.toolbar, TB_GETBUTTONSIZE, None, None).0;
        tb_h = ((size >> 16) & 0xFFFF) as i32;
        if tb_h < 22 {
            tb_h = 22;
        }
        let _ = MoveWindow(state.toolbar, 0, 0, w, tb_h, true);
    }
    let mut sb_h = 0;
    if !state.status.is_invalid() {
        let _ = SendMessageW(state.status, WM_SIZE, None, None);
        let mut src = RECT::default();
        if GetWindowRect(state.status, &mut src).is_ok() {
            sb_h = (src.bottom - src.top).max(20);
        }
    }
    if !state.canvas.is_invalid() {
        let _ = SetWindowPos(
            state.canvas,
            None,
            0,
            tb_h,
            w,
            (h - tb_h - sb_h).max(0),
            windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER,
        );
        let _ = InvalidateRect(Some(state.canvas), None, false);
    }
        }
    }

unsafe fn handle_button(hwnd: HWND, id: u32) {
        unsafe {

    let Some(state) = state_of(hwnd) else { return };
    match id {
        TB_PRINT => {
            let frame = state.frame;
            if let Some(st) = st_of(frame) {
                let _ = run_print(st, frame);
            }
        }
        TB_NEXT => {
            if state.cur_page + 1 < state.total_pages {
                state.cur_page += 1;
                update_page_status(hwnd);
                let _ = InvalidateRect(Some(state.canvas), None, false);
            }
        }
        TB_PREV => {
            if state.cur_page > 0 {
                state.cur_page -= 1;
                update_page_status(hwnd);
                let _ = InvalidateRect(Some(state.canvas), None, false);
            }
        }
        TB_ZOOM_IN => {
            state.zoom = (state.zoom * 1.25).min(4.0);
            canvas_relayout(state);
            let _ = InvalidateRect(Some(state.canvas), None, false);
        }
        TB_ZOOM_OUT => {
            state.zoom = (state.zoom / 1.25).max(0.1);
            canvas_relayout(state);
            let _ = InvalidateRect(Some(state.canvas), None, false);
        }
        TB_CLOSE => {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        TB_COLOR => {
            state.color = SendMessageW(
                state.toolbar,
                TB_ISBUTTONCHECKED,
                Some(WPARAM(TB_COLOR as usize)),
                None,
            )
            .0 != 0;
            let _ = InvalidateRect(Some(state.canvas), None, false);
        }
        TB_WRAP => {
            let frame = state.frame;
            if let Some(st) = st_of(frame) {
                let next = if st.print_wrap_on() {
                    WrapMode::Cancel
                } else {
                    WrapMode::Smart
                };
                st.set_wrap_mode(next, frame);
                repaginate(state, st, hwnd);
            }
        }
        TB_LINES => {
            let frame = state.frame;
            if let Some(st) = st_of(frame) {
                st.toggle_view_flag(frame, dg_render::ViewFlags::LINE_NUMBER);
                repaginate(state, st, hwnd);
            }
        }
        TB_CONFIG => {
            // 打印设置：先关预览（关闭时主编辑区恢复键入），再开选项对话框
            // 的打印页。
            let frame = state.frame;
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            if let Some(st) = st_of(frame) {
                st.show_options(frame, Some(247));
            }
        }
        _ => {}
    }
        }
    }

/// 换行或行号动了：总行数变 → 重算页数，回到第一页，重画。
unsafe fn repaginate(state: &mut PreviewState, st: &mut EditorState, hwnd: HWND) {
        unsafe {

    state.total_pages = state.layout.page_count(st.docs.act().total_rows);
    state.cur_page = 0;
    update_page_status(hwnd);
    let _ = InvalidateRect(Some(state.canvas), None, false);
        }
    }

unsafe fn destroy_state(hwnd: HWND) {
        unsafe {

    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    if ptr != 0 {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        let state = Box::from_raw(ptr as *mut PreviewState);
        // 主编辑区恢复可键入，焦点回到主框架。
        if let Some(st) = st_of(state.frame) {
            let view = st.active_view_hwnd();
            if !view.is_invalid() {
                let _ = EnableWindow(view, true);
            }
            st.print_preview.hwnd = HWND::default();
            let _ = SetFocus(Some(state.frame));
        }
        drop(state);
    }
        }
    }

// ---------------------------------------------------------------------------
// 页面画布
// ---------------------------------------------------------------------------

unsafe extern "system" fn canvas_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
        unsafe {

    let owner = HWND(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut _);
    match msg {
        WM_PAINT => {
            paint_canvas(hwnd, owner);
            LRESULT(0)
        }
        WM_HSCROLL | WM_VSCROLL => {
            scroll_canvas(hwnd, owner, msg, wparam);
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(state) = state_of(owner) {
                canvas_relayout_ptr(state, hwnd);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
        }
    }

/// 画布滚动范围 = 页面在当前缩放下的像素（加一圈留白），滚动条翻页在
/// 画布里平移原点。
fn canvas_relayout(state: &mut PreviewState) {
    unsafe {
        canvas_relayout_ptr(state, state.canvas);
    }
}

unsafe fn canvas_relayout_ptr(state: &mut PreviewState, canvas: HWND) {
        unsafe {

    if canvas.is_invalid() {
        return;
    }
    let (pw, ph) = state.layout.page_screen();
    let zoom = state.zoom;
    let (w, h) = ((pw * zoom) as i32 + 64, (ph * zoom) as i32 + 64);
    let mut rc = RECT::default();
    let _ = GetClientRect(canvas, &mut rc);
    let (cw, ch) = (rc.right - rc.left, rc.bottom - rc.top);
    let mut info = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
        nMin: 0,
        nMax: w,
        nPage: cw.max(1) as u32,
        nPos: state.hscroll.clamp(0, (w - cw).max(0)),
        nTrackPos: 0,
    };
    let _ = SetScrollInfo(canvas, SB_HORZ, &info, true);
    info.nMax = h;
    info.nPage = ch.max(1) as u32;
    info.nPos = state.vscroll.clamp(0, (h - ch).max(0));
    let _ = SetScrollInfo(canvas, SB_VERT, &info, true);
        }
    }

fn paint_canvas(_hwnd: HWND, owner: HWND) {
    unsafe {
        let Some(state) = state_of(owner) else { return };
        let mut ps = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
        let hdc = BeginPaint(_hwnd, &mut ps);
        if hdc.is_invalid() {
            let _ = ValidateRect(Some(_hwnd), None);
            return;
        }
        let mut rc = RECT::default();
        let _ = GetClientRect(_hwnd, &mut rc);
        // 背景：主框架编辑区的面色。
        let bg = st_of(state.frame)
            .map(|st| to_colorref(st.docs.act().theme.background).0)
            .unwrap_or(0xF0F0F0);
        let brush = CreateSolidBrush(COLORREF(bg));
        let _ = FillRect(hdc, &rc, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));
        // 页面：阴影、纸面、边。
        let (pw, ph) = state.layout.page_screen();
        let zoom = state.zoom;
        let px = 32 - state.hscroll;
        let py = 32 - state.vscroll;
        let (w, h) = (pw * zoom, ph * zoom);
        let shadow = RECT {
            left: px + 4,
            top: py + 4,
            right: px + w.round() as i32 + 4,
            bottom: py + h.round() as i32 + 4,
        };
        let shadow_brush = CreateSolidBrush(COLORREF(0x808080));
        let _ = FillRect(hdc, &shadow, shadow_brush);
        let _ = DeleteObject(HGDIOBJ(shadow_brush.0));
        let page = RECT {
            left: px,
            top: py,
            right: px + w.round() as i32,
            bottom: py + h.round() as i32,
        };
        let paper = CreateSolidBrush(COLORREF(0xFFFFFF));
        let _ = FillRect(hdc, &page, paper);
        let _ = DeleteObject(HGDIOBJ(paper.0));
        let pen = CreatePen(PS_SOLID, 1, COLORREF(0x404040));
        let old = SelectObject(hdc, HGDIOBJ(pen.0));
        let old_brush = SelectObject(hdc, HGDIOBJ(std::ptr::null_mut()));
        let _ = windows::Win32::Graphics::Gdi::Rectangle(
            hdc,
            page.left,
            page.top,
            page.right,
            page.bottom,
        );
        SelectObject(hdc, old);
        SelectObject(hdc, old_brush);
        let _ = DeleteObject(HGDIOBJ(pen.0));
        // 内容：纸面原点 + 内容区偏移，全部乘缩放。
        let content_origin = (
            px as f32 + state.layout.content.left as f32 * zoom,
            py as f32 + state.layout.content.top as f32 * zoom,
        );
        if let Some(st) = st_of(state.frame) {
            render_page(
                hdc,
                st,
                &state.fonts,
                &state.layout,
                (&state.header, &state.footer),
                state.doc_path.clone(),
                state.cur_page,
                state.total_pages,
                state.color,
                content_origin,
                zoom,
            );
        }
        let _ = EndPaint(_hwnd, &ps);
    }
}

fn scroll_canvas(hwnd: HWND, owner: HWND, msg: u32, wparam: WPARAM) {
    unsafe {
        let Some(state) = state_of(owner) else { return };
        let kind = if msg == WM_HSCROLL { SB_HORZ } else { SB_VERT };
        let mut info = SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_ALL,
            ..Default::default()
        };
        if GetScrollInfo(hwnd, kind, &mut info).is_err() {
            return;
        }
        let line = 40i32;
        let page = (info.nPage as i32).max(line);
        let old = info.nPos;
        let code = (wparam.0 & 0xFFFF) as u32;
        let pos = match code {
            c if c == SB_LINELEFT.0 as u32 => info.nPos - line,
            c if c == SB_LINERIGHT.0 as u32 => info.nPos + line,
            c if c == SB_PAGELEFT.0 as u32 => info.nPos - page,
            c if c == SB_PAGERIGHT.0 as u32 => info.nPos + page,
            c if c == SB_THUMBPOSITION.0 as u32 || c == SB_THUMBTRACK.0 as u32 => info.nTrackPos,
            _ => info.nPos,
        };
        let clamped = pos.clamp(0, (info.nMax - info.nPage as i32).max(0));
        if clamped != old {
            if kind == SB_HORZ {
                state.hscroll = clamped;
            } else {
                state.vscroll = clamped;
            }
            info.fMask = SIF_POS;
            info.nPos = clamped;
            let _ = SetScrollInfo(hwnd, kind, &info, true);
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
}

// ---------------------------------------------------------------------------
// 主框架的桥
// ---------------------------------------------------------------------------

/// 预览与打印都要拿主框架的 `EditorState`：主框架窗口的
/// `GWLP_USERDATA` 就是它。
unsafe fn st_of(frame: HWND) -> Option<&'static mut EditorState> {
        unsafe {

    let ptr = GetWindowLongPtrW(frame, GWLP_USERDATA);
    if ptr == 0 {
        return None;
    }
    Some(&mut *(ptr as *mut EditorState))
        }
    }
