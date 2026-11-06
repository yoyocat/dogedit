//! Direct3D 11 / DirectComposition / Direct2D — the editor's only renderer
//! since the P3 cut-over.
//!
//! A text surface is not a GDI surface: [`DxSurface::attach`] builds a
//! flip-model exchange chain (`CreateSwapChainForComposition`), hands it to a
//! DirectComposition visual rooted at the target window, and draws into it
//! with a Direct2D device context. DWM composes the visual; there is no memory
//! DC and no `BitBlt` on this path.
//!
//! What is drawn is the editor's `DrawList`: rectangles, glyph runs shaped by
//! [`TextShaper`] (colour emoji through `TranslateColorGlyphRun`), the
//! self-drawn caret and the popup overlay. Glyph runs are antialiased in
//! grayscale, not ClearType (see `text_antialias_mode`). The document view
//! and every output pane get their own surface; the panes' caption strip and
//! the frame's chrome stay GDI windows around them.

use core::mem::ManuallyDrop;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use dg_render::{Color, DrawCmd, Rect};
use windows::core::{Interface, BOOL, IUnknown};
use windows::Win32::Foundation::{HANDLE, HMODULE, HWND, RECT, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap1, ID2D1Device, ID2D1DeviceContext, ID2D1Factory1, ID2D1Image,
    ID2D1SolidColorBrush, D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_TARGET,
    D2D1_BITMAP_PROPERTIES1, D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_TEXT_ANTIALIAS_MODE, D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_COLOR_F, DWRITE_COLOR_GLYPH_RUN, DWRITE_GLYPH_RUN,
    DWRITE_MEASURING_MODE_NATURAL, IDWriteColorGlyphRunEnumerator,
};
use windows_numerics::Vector2;

use crate::win32::caret_layer::{CaretLayer, CaretSpec};
use crate::win32::dwrite::TextShaper;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_10_0,
    D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, DCompositionCreateDevice2, IDCompositionDevice, IDCompositionTarget,
    IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, IDXGIAdapter, IDXGIDevice, IDXGIFactory2, IDXGIOutput, IDXGISurface,
    IDXGISwapChain1, IDXGISwapChain2, DXGI_CREATE_FACTORY_FLAGS, DXGI_PRESENT,
    DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

/// One frame's worth of work: the view's own commands (`cmds`), the popups
/// (recorded in frame coordinates and drawn at `-offset`), the shaper that
/// owns the faces, and the caret box. `x_width`/`wide` are the editor grid's
/// two cell widths, and `measured` the characters the platform measured off
/// that grid (colour emoji): the shaper lays those out by their real advance,
/// which is the same number [`LineMetrics`](dg_render::pipeline::LineMetrics)
/// gave the caret and the hit test.
pub struct DxFrame<'a> {
    pub cmds: &'a [DrawCmd],
    pub overlay: &'a [DrawCmd],
    pub offset: (f32, f32),
    pub shaper: &'a mut TextShaper,
    pub x_width: f32,
    pub wide: f32,
    pub measured: &'a [(char, i32)],
    pub caret: Option<(f32, f32, f32, f32, Color)>,
    /// 显示颜文字 (1334): with this off an emoji face draws its plain
    /// outline instead of the system's colour layers.
    pub show_emoji: bool,
}

/// One `paint` call's verdict.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DxPaint {
    /// The frame is on the visual.
    Presented,
    /// An earlier frame is still queued with the compositor, so this paint
    /// drew nothing. The caller retries from a short timer — the content it
    /// asked for has not been lost, only delayed to the next free buffer.
    Skipped,
    /// The surface cannot be used (device removed, chain refused). The caller
    /// drops it and attaches a new one on a later paint.
    Lost,
}

/// One composed view surface: device, exchange chain, visual, D2D context and
/// the bitmap that wraps the current back buffer.
pub struct DxSurface {
    swapchain: IDXGISwapChain1,
    context: ID2D1DeviceContext,
    target: Option<ID2D1Bitmap1>,
    /// Size of the buffers `target` was created from.
    size: (u32, u32),
    /// Solid brushes by packed RGBA — a frame asks for a dozen colours and
    /// creates each one once.
    brushes: HashMap<u32, ID2D1SolidColorBrush>,
    /// `DOGEDIT_DX_DUMP=<path.bmp>`: write the composed back buffer there,
    /// rate-limited to one write per 300 ms. This is the visible gate's eye
    /// when the session has no capturable screen (a locked or disconnected
    /// desktop): it dumps exactly the pixels Present is about to hand DWM.
    dump: Option<(std::path::PathBuf, Instant)>,
    /// Which surface this is (the capture's file name), and the two drill knobs:
    /// drop this paint to exercise the device-lost rebuild, and how many
    /// frames have been presented.
    tag: String,
    drop_at: Option<u64>,
    paints: u64,
    /// `IDXGISwapChain2`'s latency waitable object, when the chain took the
    /// waitable flag: the UI thread waits for a free buffer instead of
    /// blocking inside `Present`.
    waitable: Option<HANDLE>,
    /// The flags the chain was created with. `ResizeBuffers` must be handed
    /// the same set: a waitable chain rejects a resize that forgets its flag.
    chain_flags: DXGI_SWAP_CHAIN_FLAG,
    // Held for the lifetime of the composition tree. Dropping any of these
    // would take the surface down with it; the D2D context keeps its own
    // references, so the fields exist to own the objects, not to be read.
    #[allow(dead_code)]
    d3d: ID3D11Device,
    #[allow(dead_code)]
    d2d_factory: ID2D1Factory1,
    #[allow(dead_code)]
    d2d_device: ID2D1Device,
    #[allow(dead_code)]
    dcomp_device: IDCompositionDevice,
    #[allow(dead_code)]
    dcomp_target: IDCompositionTarget,
    #[allow(dead_code)]
    visual: IDCompositionVisual,
    /// The caret's own composition layer — a `IDCompositionSurface` the size of
    /// the caret plus its halo, hung above the text visual. `None` until the
    /// view asks for one, and again after the compositor refuses one; the
    /// in-frame caret is the fallback in both cases.
    caret: Option<CaretLayer>,
    /// The layer was refused once and must not be retried on every paint: the
    /// failure is structural (a compositor without D2D surfaces), not transient.
    caret_failed: bool,
}

impl DxSurface {
    /// Builds the full stack and roots it at `hwnd`. `None` when the driver or
    /// the composition device cannot be had; the failing stage is named on
    /// stderr. `tag` names the surface in the debug dump (`DOGEDIT_DX_DUMP` is
    /// a path prefix, one BMP per surface).
    pub fn attach(hwnd: HWND, tag: &str) -> Option<Self> {
        unsafe {
            let force_warp = std::env::var_os("DOGEDIT_DX_WARP").is_some();
            let d3d = match create_device(force_warp) {
                Some(d) => d,
                None => {
                    eprintln!("dx: D3D11 device unavailable");
                    return None;
                }
            };
            let dxgi_device: IDXGIDevice = match d3d.cast() {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("dx: IDXGIDevice cast failed: {e:?}");
                    return None;
                }
            };
            let d2d_factory: ID2D1Factory1 =
                match D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("dx: D2D1CreateFactory failed: {e:?}");
                        return None;
                    }
                };
            let d2d_device = match d2d_factory.CreateDevice(&dxgi_device) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("dx: ID2D1Factory1::CreateDevice failed: {e:?}");
                    return None;
                }
            };
            let context = match d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("dx: CreateDeviceContext failed: {e:?}");
                    return None;
                }
            };
            // 抗锯齿方式落在上下文上，第一次 BeginDraw 之前就该是它：之后的
            // 每次 SetTarget 都继承这一格设置，只有重建表面才需要再来一次。
            let cleartype = std::env::var_os("DOGEDIT_DX_CLEARTYPE").is_some();
            context.SetTextAntialiasMode(text_antialias_mode(cleartype));
            let (w, h) = client_size(hwnd);
            let (swapchain, waitable) = match create_swapchain(&dxgi_device, w, h) {
                Some(s) => s,
                None => {
                    eprintln!("dx: CreateSwapChainForComposition failed");
                    return None;
                }
            };

            let dcomp_device: IDCompositionDevice = match DCompositionCreateDevice2(&d2d_device) {
                Ok(d) => d,
                Err(e) => {
                    // A device made from the D2D device is what lets
                    // `IDCompositionSurface::BeginDraw` hand back an
                    // `ID2D1DeviceContext` (the caret layer needs that). When
                    // the system refuses, the DXGI device still builds a
                    // compositor — the caret then stays in the text frame.
                    eprintln!(
                        "dx: DCompositionCreateDevice2 failed: {e:?}; falling back to the DXGI device"
                    );
                    match DCompositionCreateDevice(&dxgi_device) {
                        Ok(d) => d,
                        Err(e) => {
                            eprintln!("dx: DCompositionCreateDevice failed: {e:?}");
                            return None;
                        }
                    }
                }
            };
            let dcomp_target = match dcomp_device.CreateTargetForHwnd(hwnd, true) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("dx: CreateTargetForHwnd failed: {e:?}");
                    return None;
                }
            };
            let visual = match dcomp_device.CreateVisual() {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("dx: CreateVisual failed: {e:?}");
                    return None;
                }
            };
            let unknown: IUnknown = match swapchain.cast() {
                Ok(u) => u,
                Err(e) => {
                    eprintln!("dx: swapchain IUnknown cast failed: {e:?}");
                    return None;
                }
            };
            if let Err(e) = visual.SetContent(&unknown) {
                eprintln!("dx: IDCompositionVisual::SetContent failed: {e:?}");
                return None;
            }
            if let Err(e) = dcomp_target.SetRoot(&visual) {
                eprintln!("dx: IDCompositionTarget::SetRoot failed: {e:?}");
                return None;
            }
            // Until the first commit the visual is inert: DWM never sees the
            // swapchain.
            if let Err(e) = dcomp_device.Commit() {
                eprintln!("dx: IDCompositionDevice::Commit failed: {e:?}");
                return None;
            }

            let mut surface = Self {
                swapchain,
                context,
                target: None,
                size: (0, 0),
                brushes: HashMap::new(),
                dump: std::env::var_os("DOGEDIT_DX_DUMP").map(|prefix| {
                    (
                        std::path::PathBuf::from(format!(
                            "{}_{tag}.bmp",
                            prefix.to_string_lossy()
                        )),
                        Instant::now() - Duration::from_secs(1),
                    )
                }),
                tag: tag.to_string(),
                drop_at: drop_at(),
                paints: 0,
                chain_flags: if waitable.is_some() {
                    DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT
                } else {
                    DXGI_SWAP_CHAIN_FLAG(0)
                },
                waitable,
                d3d,
                d2d_factory,
                d2d_device,
                dcomp_device,
                dcomp_target,
                visual,
                caret: None,
                caret_failed: false,
            };
            surface.bind_target().ok()?;
            surface.size = (w, h);
            eprintln!(
                "dx: surface attached {w}x{h}{}",
                if waitable.is_some() { " (frame-latency waitable)" } else { "" }
            );
            Some(surface)
        }
    }
    /// Presents one frame: resizes the chain if the window changed, draws the
    /// frame's commands, and commits through the exchange chain. A frame is
    /// only drawn when the chain has a buffer free; scrolling outruns the
    /// compositor often enough that blocking on the last frame — or drawing
    /// frames nobody will see — is the wrong answer.
    pub fn paint(&mut self, w: u32, h: u32, bg: Color, frame: DxFrame) -> DxPaint {
        if w == 0 || h == 0 {
            return DxPaint::Lost;
        }
        if self.size != (w, h) && !self.resize(w, h) {
            return DxPaint::Lost;
        }
        if let Some(handle) = self.waitable {
            // Zero timeout: a still-pending frame means this paint would only
            // queue a stale image. Skip it; the caller retries from a timer.
            if unsafe { WaitForSingleObject(handle, 0) } != WAIT_OBJECT_0 {
                return DxPaint::Skipped;
            }
        }
        self.paints += 1;
        if self.drop_at == Some(self.paints) {
            eprintln!("dx: {} dropped for the device-lost drill", self.tag);
            return DxPaint::Lost;
        }
        let clear = D2D1_COLOR_F {
            r: bg.r as f32 / 255.0,
            g: bg.g as f32 / 255.0,
            b: bg.b as f32 / 255.0,
            a: 1.0,
        };
        unsafe {
            // `ID2D1DeviceContext::BeginDraw` has no failure to report; the
            // device-lost verdict lands in `EndDraw`.
            self.context.BeginDraw();
            let _ = self.context.Clear(Some(&clear));
            let mut shaper = frame.shaper;
            for cmd in frame.cmds {
                self.draw_cmd(
                    cmd,
                    0.0,
                    0.0,
                    &mut shaper,
                    frame.x_width,
                    frame.wide,
                    frame.measured,
                    frame.show_emoji,
                );
            }
            if !frame.overlay.is_empty() {
                let (dx, dy) = (-frame.offset.0, -frame.offset.1);
                for cmd in frame.overlay {
                    self.draw_cmd(
                        cmd,
                        dx,
                        dy,
                        &mut shaper,
                        frame.x_width,
                        frame.wide,
                        frame.measured,
                        frame.show_emoji,
                    );
                }
            }
            if let Some((x, y, cw, ch, color)) = frame.caret {
                self.fill(
                    &Rect::new(x, y, cw, ch),
                    color,
                    0.0,
                    0.0,
                    1.0,
                );
            }
            // EndDraw is where D2D flushes; `D2DERR_RECREATE_TARGET` lands
            // here after a driver update or a display change.
            if let Err(e) = self.context.EndDraw(None, None) {
                eprintln!("dx: EndDraw failed: {e:?}");
                return DxPaint::Lost;
            }
            self.dump_backbuffer();
            // Sync interval 0: DWM composes the visual on its own clock, and
            // the waitable object above already keeps the queue at one frame.
            let present = self.swapchain.Present(0, DXGI_PRESENT(0));
            if !present.is_ok() {
                eprintln!("dx: Present failed: {present:?}");
                return DxPaint::Lost;
            }
            DxPaint::Presented
        }
    }

    fn draw_cmd(
        &mut self,
        cmd: &DrawCmd,
        dx: f32,
        dy: f32,
        shaper: &mut TextShaper,
        x_width: f32,
        wide: f32,
        measured: &[(char, i32)],
        show_emoji: bool,
    ) {
        match cmd {
            DrawCmd::FillRect { rect, color } => self.fill(rect, *color, dx, dy, 1.0),
            DrawCmd::AlphaBlend { rect, color, alpha } => self.fill(rect, *color, dx, dy, *alpha),
            DrawCmd::TextRun { x, y, text, color, style } => self.draw_text(
                *x + dx,
                *y + dy,
                text,
                *color,
                *style,
                shaper,
                x_width,
                wide,
                measured,
                show_emoji,
            ),
        }
    }

    fn fill(&mut self, rect: &Rect, color: Color, dx: f32, dy: f32, alpha: f32) {
        let Some(brush) = self.brush(solid(color, alpha)) else {
            return;
        };
        let r = D2D_RECT_F {
            left: rect.x + dx,
            top: rect.y + dy,
            right: rect.x + dx + rect.w,
            bottom: rect.y + dy + rect.h,
        };
        unsafe { self.context.FillRectangle(&r, &brush) };
    }

    /// One `TextRun` at the row's top-left: shape it onto the grid, then one
    /// `DrawGlyphRun` per face run. A face with a colour table goes through
    /// `TranslateColorGlyphRun` first — that is where emoji become the
    /// system's coloured bitmaps instead of monochrome outlines — and falls
    /// back to the plain run when DirectWrite answers `DWRITE_E_NOCOLOR`.
    /// `style` 只挑**字面**：加粗/斜体那两张 face 的推进量在这里不参与 ——
    /// 每一格的宽度仍来自布局网格，所以换字面不会挪动任何一列。
    fn draw_text(
        &mut self,
        x: f32,
        y: f32,
        text: &str,
        color: Color,
        style: u8,
        shaper: &mut TextShaper,
        x_width: f32,
        wide: f32,
        measured: &[(char, i32)],
        show_emoji: bool,
    ) {
        let runs = shaper.shape(text, style, x_width, wide, measured);
        let baseline = y + shaper.ascent_px;
        let Some(brush) = self.brush(solid(color, 1.0)) else {
            return;
        };
        let mut pen = x;
        for run in runs {
            let face = shaper.face(run.face_key).clone();
            // 第二只字体（[FontN] CJKFont=）带着自己的字号，em 逐 run 取；
            // 其余 face 都跟随编辑器的 em。
            let em = shaper.em_of(run.face_key);
            let mut glyph_run = DWRITE_GLYPH_RUN {
                fontFace: ManuallyDrop::new(Some(face)),
                fontEmSize: em,
                glyphCount: run.glyphs.len() as u32,
                glyphIndices: run.glyphs.as_ptr(),
                glyphAdvances: run.advances.as_ptr(),
                glyphOffsets: std::ptr::null(),
                isSideways: BOOL(0),
                bidiLevel: 0,
            };
            let mut colored = false;
            if show_emoji && shaper.is_color(run.face_key) {
                if let Some(factory) = shaper.factory2() {
                    if let Ok(layers) = unsafe {
                        factory.TranslateColorGlyphRun(
                            pen,
                            baseline,
                            &glyph_run,
                            None,
                            DWRITE_MEASURING_MODE_NATURAL,
                            None,
                            0,
                        )
                    } {
                        colored = self.draw_color_layers(&layers, color);
                    }
                }
            }
            if !colored {
                let origin = Vector2 {
                    X: pen,
                    Y: baseline,
                };
                unsafe {
                    let _ = self.context.DrawGlyphRun(
                        origin,
                        &glyph_run,
                        None,
                        &brush,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
            }
            // The struct's face is a `ManuallyDrop`: the ABI demands it, but
            // nothing drops it
            // back here or every frame leaks one per run.
            unsafe { ManuallyDrop::drop(&mut glyph_run.fontFace) };
            pen += run.advances.iter().sum::<f32>();
        }
    }

    /// Walks the colour layers of one run. `paletteIndex == 0xFFFF` is
    /// DirectWrite's marker for "draw this with the caller's text colour".
    fn draw_color_layers(
        &mut self,
        layers: &IDWriteColorGlyphRunEnumerator,
        text: Color,
    ) -> bool {
        let mut any = false;
        loop {
            match unsafe { layers.MoveNext() } {
                Ok(more) if more.as_bool() => {}
                _ => break,
            }
            let Ok(layer) = (unsafe { layers.GetCurrentRun() }) else {
                break;
            };
            if layer.is_null() {
                break;
            }
            let layer: &DWRITE_COLOR_GLYPH_RUN = unsafe { &*layer };
            let color = if layer.paletteIndex == 0xFFFF {
                solid(text, 1.0)
            } else {
                layer.runColor
            };
            let Some(brush) = self.brush(color) else {
                continue;
            };
            let origin = Vector2 {
                X: layer.baselineOriginX,
                Y: layer.baselineOriginY,
            };
            unsafe {
                let _ = self.context.DrawGlyphRun(
                    origin,
                    &layer.glyphRun,
                    None,
                    &brush,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
            any = true;
        }
        any
    }

    /// A solid brush for `color`, cached on its 8-bit channels.
    fn brush(&mut self, color: DWRITE_COLOR_F) -> Option<ID2D1SolidColorBrush> {
        let key = ((color.r.clamp(0.0, 1.0) * 255.0) as u32) << 24
            | ((color.g.clamp(0.0, 1.0) * 255.0) as u32) << 16
            | ((color.b.clamp(0.0, 1.0) * 255.0) as u32) << 8
            | (color.a.clamp(0.0, 1.0) * 255.0) as u32;
        if let Some(brush) = self.brushes.get(&key) {
            return Some(brush.clone());
        }
        let d2d = D2D1_COLOR_F {
            r: color.r,
            g: color.g,
            b: color.b,
            a: color.a,
        };
        let brush = unsafe { self.context.CreateSolidColorBrush(&d2d, None) }.ok()?;
        self.brushes.insert(key, brush.clone());
        Some(brush)
    }

    /// See [`Self::dump`]. BMP rather than PNG on purpose: the gate only has
    /// to look at the pixels, and a BMP is a header plus the staging rows.
    fn dump_backbuffer(&mut self) {
        let Some((path, last)) = &mut self.dump else {
            return;
        };
        if last.elapsed() < Duration::from_millis(300) {
            return;
        }
        *last = Instant::now();
        unsafe {
            let Ok(back) = self.swapchain.GetBuffer::<ID3D11Texture2D>(0) else {
                return;
            };
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            back.GetDesc(&mut desc);
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            desc.MiscFlags = 0;
            let mut staging: Option<ID3D11Texture2D> = None;
            if self
                .d3d
                .CreateTexture2D(&desc, None, Some(&mut staging))
                .is_err()
            {
                return;
            }
            let Some(staging) = staging else { return };
            let Ok(context) = self.d3d.GetImmediateContext() else {
                return;
            };
            context.CopyResource(&staging, &back);
            let mut map = D3D11_MAPPED_SUBRESOURCE::default();
            if context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut map))
                .is_err()
            {
                return;
            }
            let (w, h) = (desc.Width as usize, desc.Height as usize);
            let pitch = map.RowPitch as usize;
            let image = (w * h * 4) as u32;
            let mut bmp = Vec::with_capacity(54 + image as usize);
            bmp.extend_from_slice(b"BM");
            bmp.extend_from_slice(&(54 + image).to_le_bytes());
            bmp.extend_from_slice(&[0, 0, 0, 0]);
            bmp.extend_from_slice(&54u32.to_le_bytes());
            bmp.extend_from_slice(&40u32.to_le_bytes());
            bmp.extend_from_slice(&(w as i32).to_le_bytes());
            // Negative height: rows are stored top-down like the surface.
            bmp.extend_from_slice(&(-(h as i32)).to_le_bytes());
            bmp.extend_from_slice(&1u16.to_le_bytes());
            bmp.extend_from_slice(&32u16.to_le_bytes());
            bmp.extend_from_slice(&0u32.to_le_bytes());
            bmp.extend_from_slice(&image.to_le_bytes());
            bmp.extend_from_slice(&2835u32.to_le_bytes());
            bmp.extend_from_slice(&2835u32.to_le_bytes());
            bmp.extend_from_slice(&0u32.to_le_bytes());
            bmp.extend_from_slice(&0u32.to_le_bytes());
            let src = map.pData as *const u8;
            for y in 0..h {
                let row = std::slice::from_raw_parts(src.add(y * pitch), w * 4);
                bmp.extend_from_slice(row);
            }
            context.Unmap(&staging, 0);
            let _ = std::fs::write(&*path, bmp);
        }
    }

    /// True once the caret has a composition layer of its own. While it does,
    /// the text frame must not draw a caret box: two of them would be two
    /// carets.
    pub fn caret_live(&self) -> bool {
        self.caret.is_some()
    }

    /// Builds the layer on first use and keeps its pixels in step with `spec`.
    ///
    /// `false` means the compositor refused (no surface, no visual, no effect
    /// group) or the layer's surface stopped accepting draws. The caller then
    /// leaves `DxFrame::caret` in place and never calls the rest of the
    /// `caret_*` family — the in-frame caret is the fallback.
    pub fn caret_ensure(&mut self, spec: &CaretSpec) -> bool {
        if self.caret_failed {
            return false;
        }
        if self.caret.is_none() {
            self.caret = CaretLayer::new(&self.dcomp_device, &self.visual, spec);
            if self.caret.is_none() {
                self.caret_failed = true;
                eprintln!("dx: caret layer unavailable; the caret stays in the text frame");
                return false;
            }
            eprintln!(
                "dx: caret layer attached {}x{}",
                spec.surface_size().0,
                spec.surface_size().1
            );
        }
        let Some(layer) = self.caret.as_mut() else {
            return false;
        };
        if layer.sync(&self.dcomp_device, spec) {
            return true;
        }
        // The surface died under a live layer — a lost device, a size change
        // the compositor would not honour. Keeping it would be the one state
        // with no caret at all: `caret_live()` would still tell the text frame
        // to skip its own caret, and the layer has nothing to show. Drop it so
        // the fallback takes over for good.
        self.caret = None;
        self.caret_failed = true;
        eprintln!("dx: caret layer lost its surface; the caret stays in the text frame");
        false
    }

    /// Places the layer at the caret's point, in view-client coordinates. The
    /// layer owns the halo padding, so callers pass the plain `caret_point`
    /// answer.
    pub fn caret_place(&mut self, x: i32, y: i32, glide_ms: u32) {
        if let Some(layer) = self.caret.as_mut() {
            layer.place(&self.dcomp_device, x, y, glide_ms);
        }
    }

    /// Shows or parks the layer. Showing arms the blink.
    pub fn caret_show(&mut self, on: bool) {
        if let Some(layer) = self.caret.as_mut() {
            layer.show(&self.dcomp_device, on);
        }
    }

    /// The blink the layer runs while visible: the system's interval, and
    /// whether it fades (呼吸) or switches.
    pub fn caret_blink(&mut self, period_s: f64, breath: bool) {
        if let Some(layer) = self.caret.as_mut() {
            layer.set_blink(&self.dcomp_device, period_s, breath);
        }
    }

    /// 击键脉冲 — one dip, then the caller re-arms with [`Self::caret_resume`].
    pub fn caret_pulse(&mut self, ms: u32) {
        if let Some(layer) = self.caret.as_mut() {
            layer.pulse(&self.dcomp_device, ms);
        }
    }

    pub fn caret_resume(&mut self) {
        if let Some(layer) = self.caret.as_mut() {
            layer.resume(&self.dcomp_device);
        }
    }

    /// A moved caret starts its blink solid, the way `SetCaretPos` restarts the
    /// system caret's.
    pub fn caret_restart(&mut self) {
        if let Some(layer) = self.caret.as_mut() {
            layer.restart(&self.dcomp_device);
        }
    }

    /// Clips the layer to the text area, in view-client coordinates.
    pub fn caret_clip(&mut self, rect: (i32, i32, i32, i32)) {
        if let Some(layer) = self.caret.as_mut() {
            layer.set_clip(&self.dcomp_device, rect);
        }
    }

    fn resize(&mut self, w: u32, h: u32) -> bool {
        unsafe {
            // The target bitmap is a view of the back buffer: it must be gone
            // before the buffers may be re-created.
            self.context.SetTarget(None::<&ID2D1Image>);
            self.target = None;
            if self
                .swapchain
                .ResizeBuffers(2, w, h, DXGI_FORMAT_B8G8R8A8_UNORM, self.chain_flags)
                .is_err()
            {
                // Leave the log: the caller answers `false` by rebuilding the
                // whole surface, and silent rebuilds hid the missing chain
                // flag that caused this in the first place.
                eprintln!("dx: ResizeBuffers {w}x{h} refused");
                return false;
            }
            self.size = (w, h);
            if self.bind_target().is_err() {
                eprintln!("dx: bind_target failed after resize");
                return false;
            }
            true
        }
    }

    fn bind_target(&mut self) -> windows::core::Result<()> {
        unsafe {
            let surface: IDXGISurface = self.swapchain.GetBuffer(0)?;
            // 96 DPI so one DIP is one physical pixel: the layout was reimplemented
            // in physical pixels and must not be rescaled behind its back.
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                colorContext: ManuallyDrop::new(None),
            };
            let bitmap = self.context.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
            self.context.SetTarget(&bitmap);
            self.target = Some(bitmap);
            Ok(())
        }
    }
}

/// 文字的抗锯齿方式：**灰度**，不是 ClearType。
///
/// 这块表面是合成树里的一张不透明 B8G8R8A8，D2D 默认按 ClearType 走子像素 ——
/// 三个子像素各算各的覆盖率，笔画边上就带出彩色描边，深色主题下最显眼（灰字
/// 压深底，一条边能偏成红或蓝）。灰度只算整格的覆盖率，字和背景之间只剩亮度
/// 差：换主题、改字号、被 DWM 摆到半像素上，颜色都不会变。
///
/// `DOGEDIT_DX_CLEARTYPE=1` 把子像素那套放回来 —— 两种画法要逐像素对照时，
/// 同一个二进制跑两遍就有两套像素。
fn text_antialias_mode(cleartype: bool) -> D2D1_TEXT_ANTIALIAS_MODE {
    if cleartype {
        D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE
    } else {
        D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE
    }
}

fn create_device(force_warp: bool) -> Option<ID3D11Device> {
    const LEVELS: [D3D_FEATURE_LEVEL; 3] = [
        D3D_FEATURE_LEVEL_11_0,
        D3D_FEATURE_LEVEL_10_1,
        D3D_FEATURE_LEVEL_10_0,
    ];
    // Hardware first, then the software rasterizer: a session on a GPU-less
    // machine (RDP, a VM without 3D) still gets the DX renderer.
    // `DOGEDIT_DX_WARP=1` forces the rasterizer, which is the P4 smoke.
    let drivers: &[windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE] = if force_warp {
        &[D3D_DRIVER_TYPE_WARP]
    } else {
        &[D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP]
    };
    for &driver in drivers {
        let mut device = None;
        let mut level = D3D_FEATURE_LEVEL_11_0;
        let created = unsafe {
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&LEVELS),
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut level),
                None,
            )
        };
        if created.is_ok() {
            if let Some(device) = device {
                let adapter = unsafe {
                    device
                        .cast::<IDXGIDevice>()
                        .ok()
                        .and_then(|g| g.GetAdapter().ok())
                        .and_then(|a| a.GetDesc().ok())
                        .map(|d| {
                            String::from_utf16_lossy(&d.Description)
                                .trim_end_matches('\0')
                                .to_string()
                        })
                        .unwrap_or_default()
                };
                eprintln!(
                    "dx: device feature level 0x{:x} on {}",
                    level.0,
                    if adapter.is_empty() { "unknown adapter" } else { &adapter }
                );
                return Some(device);
            }
        }
    }
    None
}

fn create_swapchain(
    device: &IDXGIDevice,
    w: u32,
    h: u32,
) -> Option<(IDXGISwapChain1, Option<HANDLE>)> {
    unsafe {
        let factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).ok()?;
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            // A chain cannot be born zero-sized; the first paint resizes it to
            // the window's real client rect.
            Width: w.max(1),
            Height: h.max(1),
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: 0,
        };
        let unknown: IUnknown = device.cast().ok()?;
        // Latency-1 waitable chain: DWM paces the visual, and the waitable
        // object lets a paint wait for a free buffer outside Present. Older
        // systems refuse the flag; the plain chain is the fallback.
        let mut waitable_desc = desc.clone();
        waitable_desc.Flags = DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32;
        if let Ok(chain) =
            factory.CreateSwapChainForComposition(&unknown, &waitable_desc, None::<&IDXGIOutput>)
        {
            let object = chain.cast::<IDXGISwapChain2>().ok().map(|chain2| {
                let _ = chain2.SetMaximumFrameLatency(1);
                chain2.GetFrameLatencyWaitableObject()
            });
            return Some((chain, object));
        }
        let chain = factory
            .CreateSwapChainForComposition(&unknown, &desc, None::<&IDXGIOutput>)
            .ok()?;
        Some((chain, None))
    }
}

/// `DOGEDIT_DX_DROP=<n>`: the nth paint of the first surface reports a lost
/// device, exercising the rebuild. Once per process, so the rebuilt surface
/// does not drop again.
fn drop_at() -> Option<u64> {
    static DRILLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let n: u64 = std::env::var("DOGEDIT_DX_DROP").ok()?.parse().ok()?;
    if DRILLED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return None;
    }
    Some(n)
}

/// `Color` as the float colour D2D and DWrite take. `alpha` multiplies the
/// colour's own alpha, which is what an `AlphaBlend` command asks for; theme
/// colours are opaque.
fn solid(color: Color, alpha: f32) -> DWRITE_COLOR_F {
    DWRITE_COLOR_F {
        r: color.r as f32 / 255.0,
        g: color.g as f32 / 255.0,
        b: color.b as f32 / 255.0,
        a: (color.a as f32 / 255.0) * alpha,
    }
}

fn client_size(hwnd: HWND) -> (u32, u32) {
    let mut rc = RECT::default();
    let ok = unsafe { GetClientRect(hwnd, &mut rc) }.is_ok();
    if !ok {
        return (1, 1);
    }
    (
        (rc.right - rc.left).max(1) as u32,
        (rc.bottom - rc.top).max(1) as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认就是灰度：只有显式要 ClearType 时才给子像素。这条守的是默认值
    /// 本身 —— 它一变，整棵树的字都跟着变，而 `--selftest` 看不出来。
    #[test]
    fn text_antialias_defaults_to_grayscale() {
        assert_eq!(
            text_antialias_mode(false),
            D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE
        );
        assert_eq!(
            text_antialias_mode(true),
            D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE
        );
    }
}
