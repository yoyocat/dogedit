//! The caret as its own composition layer.
//!
//! Why: the caret used to be one `FillRect` at the end of the text frame, and
//! its blink was a `WM_TIMER` that invalidated the whole view. A blink therefore
//! cost a full rebuild of the draw list, a full re-shape of every visible run
//! (`TextShaper::shape` asks DirectWrite for a glyph index per character) and a
//! full-screen `Present` — twice a second, forever, for a two-pixel bar.
//!
//! This module gives the caret a `IDCompositionSurface` of its own (a few dozen
//! pixels square), parented to the text visual, and moves every animated part of
//! it to `IDCompositionAnimation`. Those run on the compositor's clock: the
//! blink, the breath, the keystroke pulse and the glide all cost this process
//! **nothing** per frame, and the text frame is never touched again for a caret
//! event. The surface's content is drawn once per style/theme/DPI change and
//! then only ever re-shown.
//!
//! Geometry is not decided here: `caret_point`/`caret_size` in [`super::caret`]
//! answer where the caret is and how big, exactly as they do for the system
//! caret. This module adds the padding the halo needs and nothing else.

// Every call below is one compositor method, the same elision the rest of the
// Win32 backend uses.
#![allow(unsafe_op_in_unsafe_fn)]

use std::time::{Duration, Instant};

use dg_render::Color;
use windows::core::{Interface, IUnknown};
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D1_GRADIENT_STOP, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{
    ID2D1DeviceContext, ID2D1SolidColorBrush, D2D1_BUFFER_PRECISION_8BPC_UNORM,
    D2D1_COLOR_INTERPOLATION_MODE_STRAIGHT, D2D1_COLOR_SPACE_SRGB, D2D1_EXTEND_MODE_CLAMP,
    D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
};
use windows::Win32::Graphics::DirectComposition::{
    IDCompositionAnimation, IDCompositionDevice, IDCompositionEffectGroup, IDCompositionSurface,
    IDCompositionVisual, DCOMPOSITION_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM,
};
use windows_numerics::Vector2;

use crate::win32::caret::{BREATH_AMPLITUDE, BREATH_BIAS, BREATH_PHASE};

/// `DOGEDIT_CARET_TRACE=1` — state changes on stderr.
fn trace() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("DOGEDIT_CARET_TRACE").is_some())
}

/// Which stage `DOGEDIT_CARET_FAIL` asks to refuse, lowercased.
fn fail_spec() -> Option<&'static str> {
    static SPEC: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    SPEC.get_or_init(|| std::env::var("DOGEDIT_CARET_FAIL").ok().map(|v| v.to_ascii_lowercase()))
        .as_deref()
}

/// Does the spec `DOGEDIT_CARET_FAIL` would hold select `stage`? The drill for
/// the degradation path: on a machine whose compositor is perfectly healthy
/// there is otherwise no way to see what the user sees when it is not. Stages
/// name themselves the same way their failure message does — `surface`,
/// `content`, `visual`, `effect`, `attach`, `draw` — and `all` refuses the lot.
fn selects(spec: Option<&str>, stage: &str) -> bool {
    matches!(spec, Some(s) if s.eq_ignore_ascii_case(stage) || s.eq_ignore_ascii_case("all"))
}

fn fails(stage: &str) -> bool {
    selects(fail_spec(), stage)
}

/// The colour as a D2D one, its alpha scaled by `alpha`.
fn with_alpha(color: Color, alpha: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: color.r as f32 / 255.0,
        g: color.g as f32 / 255.0,
        b: color.b as f32 / 255.0,
        a: (color.a as f32 / 255.0) * alpha,
    }
}

/// What the layer should draw. Everything here comes from state that already
/// exists — `caret_size` for the box, the theme for the colours — so the layer
/// never has an opinion about the caret, only a copy of one.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CaretSpec {
    /// The caret's own box, in pixels (`Caret::width`/`height`).
    pub w: i32,
    pub h: i32,
    /// The theme's `Caret` colour.
    pub color: Color,
    /// Halo width in pixels, 0 = none (the 柔光 presets only).
    pub glow: i32,
    /// The halo's colour: the theme's `CaretGlow`, or the caret's own colour.
    pub glow_color: Color,
    /// 覆盖 mode with an effect on: the block is drawn translucent so the
    /// character under it stays readable. `Off` keeps the solid block.
    pub translucent: bool,
}

impl CaretSpec {
    /// Pixels of halo on each side. The surface is the caret box grown by this
    /// on all four sides, so the visual's offset is always `point - pad`.
    pub fn pad(&self) -> i32 {
        self.glow.max(0)
    }

    pub fn surface_size(&self) -> (u32, u32) {
        let p = self.pad() * 2;
        (
            (self.w.max(1) + p).max(1) as u32,
            (self.h.max(1) + p).max(1) as u32,
        )
    }

    /// Identity of the drawn content. Equal keys mean the surface already holds
    /// the right pixels, which is what keeps a blink from ever reaching D2D.
    fn key(&self) -> u64 {
        fn mix(h: u64, v: u64) -> u64 {
            (h ^ v).wrapping_mul(0x100_0000_01b3)
        }
        let rgba = |c: Color| {
            u32::from_be_bytes([c.r, c.g, c.b, c.a]) as u64
        };
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        h = mix(h, self.w as u64);
        h = mix(h, self.h as u64);
        h = mix(h, rgba(self.color));
        h = mix(h, self.glow as u64);
        h = mix(h, rgba(self.glow_color));
        h = mix(h, self.translucent as u64);
        h
    }
}

/// One caret layer: the surface that holds its pixels, the visual that places
/// it, and the effect group that animates its opacity.
pub struct CaretLayer {
    surface: IDCompositionSurface,
    /// Never moved and never given content: it exists only to carry the clip.
    /// `SetClip2` takes its rectangle in the *visual's own* space, so clipping
    /// the moving visual would mean re-expressing the text rect relative to the
    /// caret on every keystroke — and getting it wrong clips the caret away
    /// entirely. The holder sits at the view's origin, so the text rect means
    /// what it says.
    holder: IDCompositionVisual,
    visual: IDCompositionVisual,
    effect: IDCompositionEffectGroup,
    size: (u32, u32),
    key: u64,
    pad: i32,
    origin: (i32, i32),
    /// The `pad` the visual's offset was last computed with. Changing the halo
    /// width moves every pixel by the difference while `origin` stays put, so
    /// without this the visual keeps the offset it was given for the old
    /// padding and the caret lands a few pixels off after a preset change.
    placed_pad: i32,
    /// False until the first placement: the very first move must jump, not
    /// glide in from the visual's default (0, 0).
    placed: bool,
    visible: bool,
    /// The blink the layer should run while visible: `(period seconds, breath)`.
    /// `None` until the view has focus and told us the system's interval.
    blink: Option<(f64, bool)>,
    /// The animation currently installed on the effect. Kept so a later
    /// `SetOpacity2` can tell "static" from "compositor-driven", and so the
    /// object outlives the commit that copied it.
    anim: Option<IDCompositionAnimation>,
    /// When the running 击键脉冲 ends. A keystroke moves the caret, and the move
    /// asks for a blink restart in the very next frame — which would overwrite
    /// the pulse before its first frame ever reached the screen. The restart
    /// is dropped while this is in the future.
    pulse_until: Option<Instant>,
    clip: Option<(i32, i32, i32, i32)>,
}

impl CaretLayer {
    /// Builds the surface, the visual and the effect group, draws the content
    /// once and hangs the visual above the text. `None` means the compositor
    /// refused — the caller falls back to the in-frame caret.
    pub fn new(
        device: &IDCompositionDevice,
        root: &IDCompositionVisual,
        spec: &CaretSpec,
    ) -> Option<Self> {
        let size = spec.surface_size();
        unsafe {
            // Each stage names itself on stderr when it refuses: the layer is
            // optional, and a silent fallback is how a broken compositor path
            // hides for months. Every stage also answers to `DOGEDIT_CARET_FAIL`
            // so that fallback can be exercised without a broken driver.
            if fails("surface") {
                eprintln!("dx: caret CreateSurface {}x{} refused (drill)", size.0, size.1);
                return None;
            }
            let surface = match device.CreateSurface(
                size.0,
                size.1,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_ALPHA_MODE_PREMULTIPLIED,
            ) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("dx: caret CreateSurface {}x{} failed: {e:?}", size.0, size.1);
                    return None;
                }
            };
            if fails("visual") {
                eprintln!("dx: caret CreateVisual refused (drill)");
                return None;
            }
            let Ok(visual) = device.CreateVisual() else {
                eprintln!("dx: caret CreateVisual failed");
                return None;
            };
            let Ok(holder) = device.CreateVisual() else {
                eprintln!("dx: caret holder CreateVisual failed");
                return None;
            };
            let Ok(unknown) = surface.cast::<IUnknown>() else {
                eprintln!("dx: caret surface cast failed");
                return None;
            };
            if fails("content") {
                eprintln!("dx: caret SetContent refused (drill)");
                return None;
            }
            if let Err(e) = visual.SetContent(&unknown) {
                eprintln!("dx: caret SetContent failed: {e:?}");
                return None;
            }
            if fails("effect") {
                eprintln!("dx: caret CreateEffectGroup refused (drill)");
                return None;
            }
            let Ok(effect) = device.CreateEffectGroup() else {
                eprintln!("dx: caret CreateEffectGroup failed");
                return None;
            };
            // Invisible until the view says it has focus.
            let _ = effect.SetOpacity2(0.0);
            if let Err(e) = visual.SetEffect(&effect) {
                eprintln!("dx: caret SetEffect failed: {e:?}");
                return None;
            }
            // A caret is axis-aligned pixel art: never let the compositor
            // resample it into a blurry bar.
            let _ = visual.SetBitmapInterpolationMode(
                DCOMPOSITION_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
            );
            if fails("attach") {
                eprintln!("dx: caret AddVisual refused (drill)");
                return None;
            }
            if let Err(e) = root.AddVisual(&holder, true, None::<&IDCompositionVisual>) {
                eprintln!("dx: caret AddVisual(holder) failed: {e:?}");
                return None;
            }
            if let Err(e) = holder.AddVisual(&visual, true, None::<&IDCompositionVisual>) {
                eprintln!("dx: caret AddVisual(visual) failed: {e:?}");
                return None;
            }
            let mut layer = Self {
                surface,
                holder,
                visual,
                effect,
                size,
                key: 0,
                pad: spec.pad(),
                origin: (0, 0),
                placed_pad: spec.pad(),
                placed: false,
                visible: false,
                blink: None,
                anim: None,
                pulse_until: None,
                clip: None,
            };
            if !layer.draw(spec) {
                // The drill already said which stage refused; saying it twice
                // reads as two failures.
                if !fails("draw") {
                    eprintln!("dx: caret surface BeginDraw/EndDraw failed");
                }
                return None;
            }
            layer.key = spec.key();
            let _ = device.Commit();
            Some(layer)
        }
    }

    /// Brings the drawn content in line with `spec`, rebuilding the surface only
    /// when its size changed. Returns `false` when the compositor refused and
    /// the caller should drop the layer.
    pub fn sync(&mut self, device: &IDCompositionDevice, spec: &CaretSpec) -> bool {
        let size = spec.surface_size();
        if size != self.size {
            unsafe {
                let Ok(surface) = device.CreateSurface(
                    size.0,
                    size.1,
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    DXGI_ALPHA_MODE_PREMULTIPLIED,
                ) else {
                    return false;
                };
                let Ok(unknown) = surface.cast::<IUnknown>() else {
                    return false;
                };
                if self.visual.SetContent(&unknown).is_err() {
                    return false;
                }
                self.surface = surface;
                self.size = size;
            }
            // New surface, no pixels: force the redraw below.
            self.key = 0;
        }
        self.pad = spec.pad();
        if self.key != spec.key() {
            if !self.draw(spec) {
                return false;
            }
            self.key = spec.key();
            unsafe {
                let _ = device.Commit();
            }
        }
        true
    }

    /// Moves the visual. `glide_ms` is the 滑动 easing; 0 jumps. A move that
    /// crosses more than three rows is always a jump — an eased scroll across
    /// the screen reads as a smear, not as a caret.
    pub fn place(&mut self, device: &IDCompositionDevice, x: i32, y: i32, glide_ms: u32) {
        if !needs_place(self.origin, self.pad, self.placed_pad, x, y) {
            return;
        }
        let from = self.origin;
        let repad = self.pad != self.placed_pad;
        self.origin = (x, y);
        self.placed_pad = self.pad;
        // A padding change is a jump: the caret has not moved, only its halo
        // grew, and easing that would read as the bar sliding sideways.
        let first = !self.placed || repad;
        self.placed = true;
        // The visual's origin is the surface's top-left, and the surface is the
        // caret box grown by the halo — so the point the caller passes is the
        // caret's own, and the padding comes off here.
        let (tx, ty) = (x - self.pad, y - self.pad);
        let (fx, fy) = (from.0 - self.pad, from.1 - self.pad);
        let far = (ty - fy).abs() > self.size.1 as i32 * 3;
        let dur = if glide_ms > 0 && !far && !first {
            (glide_ms as f64 / 1000.0).clamp(0.02, 0.3)
        } else {
            0.0
        };
        unsafe {
            if dur > 0.0
                && let (Some(ax), Some(ay)) = (
                    self.cubic(device, fx as f32, tx as f32, dur),
                    self.cubic(device, fy as f32, ty as f32, dur),
                )
            {
                let r = self
                    .visual
                    .SetOffsetX(&ax)
                    .and_then(|_| self.visual.SetOffsetY(&ay));
                let c = device.Commit();
                self.anim = Some(ax);
                if trace() {
                    eprintln!(
                        "caret: glide {fx},{fy} -> {tx},{ty} in {dur:.3}s set={r:?} commit={c:?}"
                    );
                }
                return;
            }
            let r = self.visual.SetOffsetX2(tx as f32).and_then(|_| self.visual.SetOffsetY2(ty as f32));
            let _ = device.Commit();
            if trace() {
                eprintln!("caret: layer at {tx},{ty} (no glide) set={r:?}");
            }
        }
    }

    /// Focus in/out, and every "the caret is not on screen" case. Showing arms
    /// the blink; hiding parks the opacity at 0 without tearing anything down.
    pub fn show(&mut self, device: &IDCompositionDevice, on: bool) {
        if on == self.visible {
            return;
        }
        self.visible = on;
        if trace() {
            eprintln!("caret: layer show={on}");
        }
        if on {
            self.arm_blink(device);
        } else {
            unsafe {
                let _ = self.effect.SetOpacity2(0.0);
                let _ = device.Commit();
            }
            self.anim = None;
        }
    }

    /// The interval and curve the layer should use while visible. Unchanged
    /// values are a no-op: this is called from every paint.
    pub fn set_blink(&mut self, device: &IDCompositionDevice, period: f64, breath: bool) {
        if self.blink == Some((period, breath)) {
            return;
        }
        self.blink = Some((period, breath));
        if self.visible {
            self.arm_blink(device);
        }
    }

    /// 击键脉冲: one dip and back. The caller re-arms the blink from a one-shot
    /// timer when it ends — a compositor animation cannot chain into another.
    pub fn pulse(&mut self, device: &IDCompositionDevice, ms: u32) {
        if !self.visible {
            if trace() {
                eprintln!("caret: pulse skipped (layer hidden)");
            }
            return;
        }
        let dur = ((ms.max(20) as f64) / 1000.0).min(0.3);
        self.pulse_until = Some(Instant::now() + Duration::from_secs_f64(dur));
        unsafe {
            if let Some(anim) = self.cubic(device, 1.0, 0.5, dur) {
                let s = self.effect.SetOpacity(&anim);
                let c = device.Commit();
                self.anim = Some(anim);
                if trace() {
                    eprintln!("caret: pulse {dur:.3}s set={s:?} commit={c:?}");
                }
            }
        }
    }

    /// Restores the blink after a pulse ended (the timer the caller arms).
    pub fn resume(&mut self, device: &IDCompositionDevice) {
        self.pulse_until = None;
        if trace() {
            eprintln!("caret: pulse done, resuming (visible={})", self.visible);
        }
        if self.visible {
            self.arm_blink(device);
        }
    }

    /// True while a 击键脉冲 owns the opacity. Kept as a method so the rule is
    /// in one place: a pulse in flight outranks a blink restart, because the
    /// restart comes from the same keystroke that started the pulse.
    fn pulsing(&self) -> bool {
        self.pulse_until.is_some_and(|t| Instant::now() < t)
    }

    /// Restarts the blink from its first frame. This is what `SetCaretPos` does
    /// to the system caret, and what makes a caret that just moved solid
    /// instead of appearing mid-fade. Dropped while a pulse is running.
    pub fn restart(&mut self, device: &IDCompositionDevice) {
        if self.pulsing() {
            if trace() {
                eprintln!("caret: blink restart skipped (pulse in flight)");
            }
            return;
        }
        if self.visible {
            self.arm_blink(device);
        }
    }

    /// Clips the visual to the text area, so a halo wider than the gutter's
    /// first column cannot bleed into the line numbers.
    pub fn set_clip(&mut self, device: &IDCompositionDevice, rect: (i32, i32, i32, i32)) {
        if self.clip == Some(rect) {
            return;
        }
        self.clip = Some(rect);
        let r = D2D_RECT_F {
            left: rect.0 as f32,
            top: rect.1 as f32,
            right: rect.2 as f32,
            bottom: rect.3 as f32,
        };
        unsafe {
            let r = self.holder.SetClip2(&r);
            let c = device.Commit();
            if trace() {
                eprintln!(
                    "caret: clip l={} t={} r={} b={} set={r:?} commit={c:?} origin={:?}",
                    rect.0, rect.1, rect.2, rect.3, self.origin
                );
            }
        }
    }

    /// The blink itself. A hard blink is a square wave built from two steep
    /// cubics per cycle (DirectComposition has no step segment), the breath is
    /// one sinusoid starting at its peak.
    fn arm_blink(&mut self, device: &IDCompositionDevice) {
        unsafe {
            if std::env::var_os("DOGEDIT_CARET_STATIC").is_some() {
                let _ = self.effect.SetOpacity2(1.0);
                let _ = device.Commit();
                return;
            }
            let Some((period, breath)) = self.blink else {
                let _ = self.effect.SetOpacity2(1.0);
                let _ = device.Commit();
                self.anim = None;
                return;
            };
            let Ok(anim) = device.CreateAnimation() else {
                let _ = self.effect.SetOpacity2(1.0);
                let _ = device.Commit();
                return;
            };
            if breath {
                // 1.0 at t=0 (phase 0.25), 0.55 half a cycle later, looping.
                let _ = anim.AddSinusoidal(
                    0.0,
                    BREATH_BIAS,
                    BREATH_AMPLITUDE,
                    (1.0 / period) as f32,
                    BREATH_PHASE,
                );
            } else {
                // 4 ms edges: fast enough to read as a step, slow enough that
                // the compositor's linear interpolation never tears.
                let eps = 0.004f64;
                let half = period * 0.5;
                let _ = anim.AddCubic(0.0, 1.0, 0.0, 0.0, 0.0);
                let _ = anim.AddCubic(half - eps, 1.0, (-1.0 / eps) as f32, 0.0, 0.0);
                let _ = anim.AddCubic(half, 0.0, 0.0, 0.0, 0.0);
                let _ = anim.AddCubic(period - eps, 0.0, (1.0 / eps) as f32, 0.0, 0.0);
            }
            let _ = anim.AddRepeat(period, period);
            let r = self.effect.SetOpacity(&anim);
            let c = device.Commit();
            if trace() {
                eprintln!(
                    "caret: blink armed period={period:.3} breath={breath} set={r:?} commit={c:?}"
                );
            }
            self.anim = Some(anim);
        }
    }

    /// An ease-out cubic from `from` to `to` over `dur` seconds, expressed in
    /// the polynomial form `IDCompositionAnimation::AddCubic` takes.
    fn cubic(
        &self,
        device: &IDCompositionDevice,
        from: f32,
        to: f32,
        dur: f64,
    ) -> Option<IDCompositionAnimation> {
        unsafe {
            let anim = device.CreateAnimation().ok()?;
            let (c1, c2, c3) = ease_coeffs(from, to, dur);
            anim.AddCubic(0.0, from, c1, c2, c3).ok()?;
            anim.End(dur, to).ok()?;
            Some(anim)
        }
    }

    /// Draws the caret into the surface: the halo (a horizontal gradient across
    /// the whole surface, so the bar's two sides fade) and the core box.
    ///
    /// Two properties of a `BeginDraw` device context are load-bearing here,
    /// both as observed on the Intel UHD 630 this was written on:
    ///
    /// 1. **Never call `SetTransform`.** A single call silently disables every
    /// geometry draw that follows — a fill of the whole surface painted
    /// nothing with it and 20 px without it — while `Clear` keeps working,
    /// which is what makes the failure look like a colour problem.
    /// 2. **Geometry is drawn at `coord - offset`.** The offset `BeginDraw`
    /// reports is the surface coordinate of drawing coordinate zero, so a
    /// rect has to be pushed by it or the caret lands a pixel up and to the
    /// left. `Clear` ignores all of this and fills the target edge to edge,
    /// which is why the no-halo case uses it: there the surface *is* the
    /// caret.
    fn draw(&mut self, spec: &CaretSpec) -> bool {
        if fails("draw") {
            eprintln!("dx: caret surface BeginDraw refused (drill)");
            return false;
        }
        unsafe {
            let mut offset = POINT::default();
            let Ok(ctx) = self
                .surface
                .BeginDraw::<ID2D1DeviceContext>(None, &mut offset)
            else {
                return false;
            };
            // A premultiplied target starts from nothing, not from black.
            let clear = D2D1_COLOR_F {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            };
            ctx.Clear(Some(&clear));
            let g = Geom::new(self.size, offset, spec);
            let alpha = if spec.translucent { 0.62 } else { 1.0 };
            let color = with_alpha(spec.color, alpha);
            if trace() {
                let core = g.core();
                eprintln!(
                    "caret: draw {}x{} #{:02X}{:02X}{:02X}{:02X} glow={} core={:.0},{:.0}-{:.0},{:.0} off={},{}",
                    self.size.0, self.size.1,
                    spec.color.r, spec.color.g, spec.color.b, spec.color.a,
                    spec.glow, core.left, core.top, core.right, core.bottom,
                    offset.x, offset.y
                );
            }
            if spec.glow > 0 {
                self.halo(&ctx, spec, &g);
            }
            let core = g.core();
            if spec.glow > 0 {
                if let Some(brush) = solid(&ctx, spec.color, alpha) {
                    ctx.FillRectangle(&core, &brush);
                }
            } else {
                // No halo: the surface is the caret, so fill all of it.
                ctx.Clear(Some(&color));
            }
            if spec.translucent {
                // A translucent block still needs an edge, or it reads as a
                // smudge rather than as a caret. Half a pixel in, so the
                // surface edge does not eat the outline itself.
                let edge = D2D_RECT_F {
                    left: core.left + 0.5,
                    top: core.top + 0.5,
                    right: core.right - 0.5,
                    bottom: core.bottom - 0.5,
                };
                if let Some(brush) = solid(&ctx, spec.color, 0.95) {
                    ctx.DrawRectangle(&edge, &brush, 1.0, None);
                }
            }
            self.surface.EndDraw().is_ok()
        }
    }

    /// The halo: one horizontal gradient across the whole surface, transparent
    /// at both edges and `CaretGlow`'s alpha in the middle, so the bar's two
    /// sides fade instead of ending on a hard edge.
    unsafe fn halo(&self, ctx: &ID2D1DeviceContext, spec: &CaretSpec, g: &Geom) {
        let c = spec.glow_color;
        let at = |alpha: f32| D2D1_COLOR_F {
            r: c.r as f32 / 255.0,
            g: c.g as f32 / 255.0,
            b: c.b as f32 / 255.0,
            a: (c.a as f32 / 255.0) * alpha,
        };
        let width = g.sw - g.ox;
        let stops = [
            D2D1_GRADIENT_STOP {
                position: 0.0,
                color: at(0.0),
            },
            D2D1_GRADIENT_STOP {
                position: (g.pad / width).clamp(0.0, 1.0),
                color: at(0.55),
            },
            D2D1_GRADIENT_STOP {
                position: ((g.pad + g.cw) / width).clamp(0.0, 1.0),
                color: at(0.55),
            },
            D2D1_GRADIENT_STOP {
                position: 1.0,
                color: at(0.0),
            },
        ];
        // The device context's own overload (D2D 1.1): sRGB in and out, 8 bpc,
        // clamped — the halo is a handful of pixels, not a colour-managed pass.
        let Ok(collection) = ctx.CreateGradientStopCollection(
            &stops,
            D2D1_COLOR_SPACE_SRGB,
            D2D1_COLOR_SPACE_SRGB,
            D2D1_BUFFER_PRECISION_8BPC_UNORM,
            D2D1_EXTEND_MODE_CLAMP,
            D2D1_COLOR_INTERPOLATION_MODE_STRAIGHT,
        ) else {
            return;
        };
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
            startPoint: Vector2 { X: g.ox, Y: g.oy },
            endPoint: Vector2 { X: g.sw, Y: g.oy },
        };
        let Ok(brush) = ctx.CreateLinearGradientBrush(&props, None, &collection) else {
            return;
        };
        ctx.FillRectangle(&g.all(), &brush);
    }
}

/// Where one draw puts its pixels.
///
/// `BeginDraw` hands back an offset, and this driver draws geometry at
/// `coord - offset` — so every rectangle below is expressed *in the shifted
/// space*, which is what `ox`/`oy` are for. Keeping the five numbers in one
/// struct is the difference between a painter that mixes the two spaces and one
/// that cannot.
#[derive(Clone, Copy)]
struct Geom {
    /// The context's draw offset.
    ox: f32,
    oy: f32,
    /// The surface's size, already shifted into the same space.
    sw: f32,
    sh: f32,
    /// Halo width on each side.
    pad: f32,
    /// The caret's own box.
    cw: f32,
    ch: f32,
}

impl Geom {
    fn new(size: (u32, u32), offset: POINT, spec: &CaretSpec) -> Self {
        let (ox, oy) = (offset.x as f32, offset.y as f32);
        Self {
            ox,
            oy,
            sw: size.0 as f32 + ox,
            sh: size.1 as f32 + oy,
            pad: spec.pad().max(0) as f32,
            cw: spec.w.max(1) as f32,
            ch: spec.h.max(1) as f32,
        }
    }

    /// The caret itself: the surface inset by the halo.
    fn core(&self) -> D2D_RECT_F {
        D2D_RECT_F {
            left: self.pad + self.ox,
            top: self.pad + self.oy,
            right: self.pad + self.cw + self.ox,
            bottom: self.pad + self.ch + self.oy,
        }
    }

    /// The whole surface.
    fn all(&self) -> D2D_RECT_F {
        D2D_RECT_F {
            left: self.ox,
            top: self.oy,
            right: self.sw,
            bottom: self.sh,
        }
    }
}

/// Does a `place` call have to move anything?
///
/// The point alone is not enough: the offset the visual needs is
/// `point - pad`, so a halo-width change moves every pixel while the caret
/// stays exactly where it is. Leaving the padding out of this test is how
/// switching to 柔光 at runtime used to leave the caret three pixels up and to
/// the left of its own column.
fn needs_place(origin: (i32, i32), pad: i32, placed_pad: i32, x: i32, y: i32) -> bool {
    (x, y) != origin || pad != placed_pad
}

/// The `AddCubic` coefficients for `3u² - 2u³` scaled from `from` to `to` over
/// `dur` seconds: zero velocity at both ends, so a pulse or a glide does not
/// start or stop with a visible snap. `AddCubic` takes
/// `f(t) = base + c1·t + c2·t² + c3·t³`, so the base is `from` and only the
/// quadratic and cubic terms are non-zero.
fn ease_coeffs(from: f32, to: f32, dur: f64) -> (f32, f32, f32) {
    let d = (to - from) as f64;
    (
        0.0,
        (3.0 * d / (dur * dur)) as f32,
        (-2.0 * d / (dur * dur * dur)) as f32,
    )
}

/// A solid brush at `alpha` times the colour's own alpha.
fn solid(ctx: &ID2D1DeviceContext, color: Color, alpha: f32) -> Option<ID2D1SolidColorBrush> {
    let c = with_alpha(color, alpha);
    unsafe {
        match ctx.CreateSolidColorBrush(&c, None) {
            Ok(b) => Some(b),
            Err(e) => {
                if trace() {
                    eprintln!("caret: CreateSolidColorBrush failed: {e:?} a={}", c.a);
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(w: i32, h: i32, glow: i32) -> CaretSpec {
        CaretSpec {
            w,
            h,
            color: Color::rgb(0xE6, 0xE9, 0xEF),
            glow,
            glow_color: Color::rgb(0xE6, 0xE9, 0xEF),
            translucent: false,
        }
    }

    #[test]
    fn surface_is_the_box_grown_by_the_halo() {
        // 默认 2 px bar, no halo: the surface is exactly the caret.
        assert_eq!(spec(2, 18, 0).surface_size(), (2, 18));
        // 柔光 3 px each side: 2 + 6 wide, 18 + 6 tall.
        assert_eq!(spec(2, 18, 3).surface_size(), (8, 24));
        // A degenerate box still gets a drawable surface.
        assert_eq!(spec(0, 0, 0).surface_size(), (1, 1));
    }

    #[test]
    fn pad_is_the_offset_the_visual_needs() {
        assert_eq!(spec(2, 18, 0).pad(), 0);
        assert_eq!(spec(2, 18, 3).pad(), 3);
        // A negative setting reads as no halo rather than as a negative pad.
        assert_eq!(spec(2, 18, -2).pad(), 0);
    }

    #[test]
    fn content_key_moves_only_with_the_pixels() {
        let base = spec(2, 18, 3);
        assert_eq!(base.key(), spec(2, 18, 3).key());
        // Every field the painter reads changes the key...
        assert_ne!(base.key(), spec(3, 18, 3).key());
        assert_ne!(base.key(), spec(2, 19, 3).key());
        assert_ne!(base.key(), spec(2, 18, 4).key());
        let mut other = spec(2, 18, 3);
        other.color = Color::rgb(0xFF, 0, 0);
        assert_ne!(base.key(), other.key());
        other = spec(2, 18, 3);
        other.glow_color = Color::rgb(0xFF, 0, 0);
        assert_ne!(base.key(), other.key());
        other = spec(2, 18, 3);
        other.translucent = true;
        assert_ne!(base.key(), other.key());
        //...and nothing else does, which is what keeps a blink off the GPU.
        let mut same = spec(2, 18, 3);
        same.color.a = base.color.a;
        assert_eq!(base.key(), same.key());
    }

    #[test]
    fn blink_period_and_curve_are_the_ones_the_layer_arms() {
        // The layer loops on `AddRepeat(period, period)`; the breath starts at
        // its peak so a caret that just moved is solid.
        let at = |t: f64, period: f64| {
            BREATH_BIAS
                + BREATH_AMPLITUDE
                    * (2.0 * std::f64::consts::PI * (t / period + BREATH_PHASE as f64)).sin() as f32
        };
        let period = crate::win32::caret::blink_period_s(530);
        assert!((at(0.0, period) - 1.0).abs() < 1e-5);
        assert!((at(period, period) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn ease_starts_and_ends_with_zero_velocity() {
        // The pulse and the glide both ride this curve. Evaluate the polynomial
        // the way the compositor will, then check the ends.
        let (from, to, dur) = (1.0f32, 0.5f32, 0.14f64);
        let (c1, c2, c3) = ease_coeffs(from, to, dur);
        let at = |t: f64| from + c1 * t as f32 + c2 * (t * t) as f32 + c3 * (t * t * t) as f32;

        assert!((at(0.0) - from).abs() < 1e-6);
        assert!((at(dur) - to).abs() < 1e-5);
        // Halfway through, the curve is at its midpoint -- and strictly between
        // the ends, so the pulse dips rather than snapping.
        let mid = at(dur * 0.5);
        assert!((mid - (from + to) / 2.0).abs() < 1e-3);
        // Velocity at t=0 is c1, and it is zero: no visible snap at the start.
        assert!(c1.abs() < 1e-6);
        //...and the curve is monotone, so the opacity never bounces.
        let mut prev = at(0.0);
        for i in 1..=20 {
            let v = at(dur * i as f64 / 20.0);
            assert!(v <= prev + 1e-6, "not monotone at step {i}: {v} > {prev}");
            prev = v;
        }
    }

    #[test]
    fn a_halo_width_change_moves_the_visual_even_though_the_caret_did_not() {
        // `place` early-returns when the point is unchanged, which is what
        // keeps a paint from re-committing every frame. The padding is part of
        // the offset it computes, so it has to be part of that test too.
        let p = (100, 40);
        // Same point, same padding: nothing to do.
        assert!(!needs_place(p, 3, 3, p.0, p.1));
        // Same point, padding grew 0 -> 3: the visual has to move.
        assert!(needs_place(p, 3, 0, p.0, p.1));
        //...and back again.
        assert!(needs_place(p, 0, 3, p.0, p.1));
        // A real move is still a move.
        assert!(needs_place(p, 3, 3, p.0 + 4, p.1));
        assert!(needs_place(p, 3, 3, p.0, p.1 + 21));
        // The offset the visual ends up with, for the two paddings.
        let offset = |pad: i32| (p.0 - pad, p.1 - pad);
        assert_eq!(offset(0), (100, 40));
        assert_eq!(offset(3), (97, 37));
    }

    #[test]
    fn the_drill_refuses_exactly_one_stage_unless_told_otherwise() {
        // Unset: nothing refuses, which is the shipping behaviour.
        assert!(!selects(None, "surface"));
        assert!(!selects(None, "draw"));
        // One stage by name, case-insensitively (the env var is lowercased).
        assert!(selects(Some("surface"), "surface"));
        assert!(!selects(Some("surface"), "draw"));
        assert!(selects(Some("SURFACE"), "surface"));
        // `all` refuses everything, and an unknown word refuses nothing: a typo
        // must not silently turn the caret off.
        for stage in ["surface", "content", "visual", "effect", "attach", "draw"] {
            assert!(selects(Some("all"), stage), "all should refuse {stage}");
            assert!(!selects(Some("surfac"), stage), "typo should refuse nothing");
        }
    }
}
