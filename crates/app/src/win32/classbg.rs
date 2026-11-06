//! 窗口类的底色画刷（`WNDCLASSW::hbrBackground`）。
//!
//! 一个窗口在它第一次 `WM_PAINT` 之前，屏幕上显示的不是它画的东西，而是系统交给
//! DWM 的那块表面：有类画刷时那块就是刷子的颜色，没有时是白的。启动时窗口先露面、
//! 内容稍后才画 —— 会话恢复要读文件、正文的 DX 表面要等第一次绘制才挂上 —— 这段
//! 时间里正文那块就是白的，深色主题下很显眼。给它配上主题底色，白板就没了。
//!
//! 画刷**不删**：窗口类一直指着它，进程活多久它就在多久。它只决定「第一次绘制
//! 之前」那块表面（以及 `WM_ERASEBKGND` 填的色）—— 之后每一帧都是窗口自己画的，
//! 擦底那一笔在同一轮绘制里就被盖掉了，所以换主题时不必跟着换。

use windows::Win32::Graphics::Gdi::{CreateSolidBrush, HBRUSH};

use dg_render::Theme;

/// 编辑区的底色画刷：正文和它周围那圈框架用的是同一块底色（`Theme::background`），
/// 所以两个窗口类共用这一支。
pub(super) fn editor_brush(theme: &Theme) -> HBRUSH {
    unsafe { CreateSolidBrush(crate::win32::window::to_colorref(theme.background)) }
}
