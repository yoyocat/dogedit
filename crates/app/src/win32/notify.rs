//! What happens when an external tool finishes: the taskbar button flashes,
//! an overlay icon marks success or failure, and the button's progress bar
//! takes the status colour. Scripts drive the same progress bar through
//! `App.taskbar`.
//!
//! The overlay leaves the title bar and the Alt+Tab entry alone. A frame in
//! the background flashes until it is activated; a frame the user is already
//! looking at gets a bounded run of blinks, because `FLASHW_TIMERNOFG` means
//! "until the window comes to the foreground" and therefore does nothing at
//! all when the window is already there. The progress state is a status
//! colour rather than a meter — a finished tool has no fraction to report,
//! so it uses a full bar.
//!
//! The countdown is only allowed to finish while the frame is in front: a
//! tool that ends behind another window leaves the badge, the bar and the
//! blink up until the user activates the frame from the taskbar. The timer
//! stays armed as a watchdog in case the activation reaches an owned dialog
//! rather than the frame itself.
//!
//! A modern taskbar draws `TBPF_NORMAL` as a ~30x4 px underline in the
//! system accent colour, not as the full-button green fill of Windows 7.
//! The notice leaves that underline at full for as long as it is up; the
//! movement the user sees comes from the button flash, not from animating
//! the bar.

// The module's Win32 entry points are `unsafe fn`s whose bodies are one long
// translation into native calls, the way the frame's own handlers are.
#![allow(unsafe_op_in_unsafe_fn)]

use std::cell::RefCell;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};

use dg_script::TaskbarState;
use windows::core::{BOOL, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, GetDC, ReleaseDC, BI_RGB, BITMAPINFO,
    BITMAPINFOHEADER, DIB_RGB_COLORS, HBITMAP, HGDIOBJ,
};
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_NODEFAULT};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::UI::Shell::{
    ITaskbarList3, TBPF_ERROR, TBPF_INDETERMINATE, TBPF_NOPROGRESS, TBPF_NORMAL, TBPF_PAUSED,
    TBPFLAG, TaskbarList,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, FlashWindowEx, GA_ROOTOWNER, GetAncestor, GetForegroundWindow,
    GetSystemMetrics, IsIconic, KillTimer, SetTimer, FLASHWINFO, FLASHWINFO_FLAGS, FLASHW_STOP,
    FLASHW_TIMERNOFG, FLASHW_TRAY, HICON, ICONINFO, MB_ICONHAND, MB_OK, SM_CXSMICON,
};

/// The frame timer that takes the overlay and the progress bar back down.
/// The frame's own timers use 1..16 and the widget layer uses 0x1000/0x1001,
/// so this one takes a distinctive value: the notice can stay armed for as
/// long as the frame is in the background, and `SetTimer` with an id already
/// in use would silently replace somebody else's timer.
pub(super) const TIMER_ID: usize = 0x4E4F;

/// How long the badge stays on the button while the frame is in front. Long
/// enough to be noticed, short enough that it is gone by the time the result
/// has been read. It is also how often a background notice is re-checked;
/// the check never takes it down while the frame is still behind.
const OVERLAY_MS: u32 = 6000;

/// How often a finished tool blinks a frame that is already in front. The
/// background arm has no count — it blinks until the window is activated —
/// but `FLASHW_TIMERNOFG` is defined as "until the window comes to the
/// foreground", so in front it would stop before the first tick. A bounded
/// count is the only form that turns the whole taskbar button's colour while
/// the user is looking at the frame.
const FOREGROUND_BLINKS: u32 = 5;

/// The badge's two colours. They are full-strength status colours rather than
/// theme colours: a 16 px badge on a taskbar is read at a glance, and the
/// theme's muted palette would not survive the down-scale.
const OK_RGB: [u8; 3] = [0x2F, 0xA8, 0x4A];
const BAD_RGB: [u8; 3] = [0xD1, 0x3B, 0x34];
/// The tick and the cross are white, like every other status badge Windows
/// draws over an application icon.
const GLYPH_RGB: [u8; 3] = [0xFF, 0xFF, 0xFF];

/// Whether a badge is on the button. Everything here runs on the frame's own
/// thread, so the flag only has to stop the activation path from building an
/// `ITaskbarList3` for a badge that was never raised — which is every
/// activation but the few that follow a tool.
static BADGE_UP: AtomicBool = AtomicBool::new(false);

/// Whether the completion notice put the progress bar up. `expire` only
/// takes the bar down when this is set: a script that has since called
/// `App.taskbar` owns the bar, and must not lose it to a stale completion
/// timer.
static FINISH_PROGRESS_UP: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// The bar a script put up, so a completion notice that borrowed the
    /// button can hand it back once the notice ends — which may be much
    /// later than six seconds when the frame is in the background. Only the
    /// frame thread reads or writes this, like the `ITaskbarList3` cache.
    static SCRIPT_BAR: RefCell<Option<(TaskbarState, u64, u64)>> =
        const { RefCell::new(None) };
}

/// Whether the frame is the window the user is looking at. An owned dialog —
/// the 查找框, the 提示框, the options dialog — still belongs to the frame:
/// `GA_ROOTOWNER` walks the owner chain, the same way the prompt box finds
/// the frame's theme. A zero foreground handle means no window has focus,
/// which is never "active" here.
fn frame_is_active(hwnd: HWND) -> bool {
    if hwnd.is_invalid() {
        return false;
    }
    // A minimized frame can still be the window the system calls foreground.
    // The user is not looking at it, and `FLASHW_TIMERNOFG` would treat that
    // as "already returned" and draw no blink at all.
    if unsafe { IsIconic(hwnd) }.as_bool() {
        return false;
    }
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() {
        return false;
    }
    let frame_root = unsafe { GetAncestor(hwnd, GA_ROOTOWNER) };
    let fg_root = unsafe { GetAncestor(fg, GA_ROOTOWNER) };
    same_root(frame_root, fg_root)
}

/// Kept apart from the Win32 calls so the ownership rule has a test that
/// does not need a desktop. Two null handles must not count as a match.
fn same_root(frame: HWND, foreground: HWND) -> bool {
    !frame.is_invalid() && !foreground.is_invalid() && frame == foreground
}

/// `true` is a clean exit (`code == Some(0)`); anything else — a non-zero
/// code, or a child that was killed before it could report one — is a
/// failure.
pub(super) fn finished(hwnd: HWND, ok: bool) {
    let cfg = super::optionsdlg::load_notify();
    let mut transient = false;
    if cfg.flash {
        unsafe { flash(hwnd, ok) };
        transient = true;
    }
    if cfg.progress && finish_progress(hwnd, ok) {
        transient = true;
    }
    if cfg.sound {
        play(if ok { &cfg.file } else { &cfg.fail_file }, ok);
    }
    // One timer owns both the badge and the bar. It also watches for the
    // user's return: a notice raised behind another window is never cleared
    // while the frame stays in the background.
    if transient {
        unsafe {
            let _ = SetTimer(Some(hwnd), TIMER_ID, OVERLAY_MS, None);
        }
    }
}

/// The [`TIMER_ID`] tick. Only a frame in front may end the countdown; a
/// background notice keeps both its status and its timer so the next tick
/// can notice the return, in case activation reached an owned dialog and
/// the frame never saw `WM_ACTIVATE`. `expire` kills the timer on the
/// normal activation path.
pub(super) fn tick(hwnd: HWND) {
    if countdown_may_finish(frame_is_active(hwnd)) {
        expire(hwnd);
    }
}

/// Written out so the lifetime rule has a test that does not need a desktop.
/// A background frame is never allowed to lose its reminder to a timer.
fn countdown_may_finish(active: bool) -> bool {
    active
}

/// Raise the badge and start the blink. [`TIMER_ID`] is armed by `finished`,
/// which owns the countdown for every one of the completion notices.
unsafe fn flash(hwnd: HWND, ok: bool) {
    if let Some(icon) = badge(ok)
        && let Some(list) = taskbar()
    {
        // The description is what a screen reader announces for the
        // overlay, so it says which way the run went.
        let text: Vec<u16> = if ok {
            "外部工具执行成功"
        } else {
            "外部工具执行失败"
        }
        .encode_utf16()
        .chain([0])
        .collect();
        let _ = list.SetOverlayIcon(hwnd, icon, PCWSTR(text.as_ptr()));
    }
    BADGE_UP.store(true, Ordering::Relaxed);
    let (flags, count) = flash_plan(frame_is_active(hwnd));
    let info = FLASHWINFO {
        cbSize: size_of::<FLASHWINFO>() as u32,
        hwnd,
        dwFlags: flags,
        uCount: count,
        dwTimeout: 0,
    };
    let _ = FlashWindowEx(&info);
}

/// The two flash shapes. Behind another window the blink runs until the
/// frame is activated; the frame in front cannot use that arm — it is
/// already at its stopping point — so it gets a fixed number of blinks.
/// Both are `FLASHW_TRAY`: the whole taskbar button takes the shell's own
/// attention colour, which is the part that reads from across the screen.
/// The outcome colour cannot ride on the flash — the shell owns it — so
/// green and red live on the badge and the progress state instead. Kept
/// apart from the Win32 calls so the choice has a test that does not need
/// a desktop.
fn flash_plan(active: bool) -> (FLASHWINFO_FLAGS, u32) {
    if active {
        (FLASHW_TRAY, FOREGROUND_BLINKS)
    } else {
        (FLASHW_TRAY | FLASHW_TIMERNOFG, 0)
    }
}

/// Take the badge and the bar back down and stop the blink. Runs on
/// [`TIMER_ID`], and again on activation so a window the user has already
/// returned to does not keep stale status.
pub(super) fn expire(hwnd: HWND) {
    let badge = BADGE_UP.swap(false, Ordering::Relaxed);
    let bar = FINISH_PROGRESS_UP.swap(false, Ordering::Relaxed);
    // The lifetime timer repeats until it is killed, so this has to happen
    // even when a script has taken the bar over and there is nothing left
    // to take down.
    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER_ID);
    }
    if !badge && !bar {
        return;
    }
    unsafe {
        let list = taskbar();
        if badge {
            // A null icon is how the shell is told to drop the overlay.
            if let Some(list) = list.as_ref() {
                let _ = list.SetOverlayIcon(hwnd, HICON::default(), PCWSTR::null());
            }
            let info = FLASHWINFO {
                cbSize: size_of::<FLASHWINFO>() as u32,
                hwnd,
                dwFlags: FLASHW_STOP,
                uCount: 0,
                dwTimeout: 0,
            };
            let _ = FlashWindowEx(&info);
        }
        if let (true, Some(list)) = (bar, list.as_ref()) {
            let _ = list.SetProgressState(hwnd, TBPF_NOPROGRESS);
            // An unrelated tool finishing must not wipe the bar of a script
            // that is still reporting its own work: put it back as it was.
            if let Some((state, done, total)) = SCRIPT_BAR.with(|slot| *slot.borrow()) {
                let _ = apply(list, hwnd, state, done, total);
            }
        }
    }
}

/// `App.taskbar`'s landing point. A script that drives the bar owns it from
/// here on, so a completion notice still waiting to expire must not clear
/// the script's bar out from under it.
pub(super) fn set_progress(
    hwnd: HWND,
    state: TaskbarState,
    completed: u64,
    total: u64,
) -> bool {
    FINISH_PROGRESS_UP.store(false, Ordering::Relaxed);
    SCRIPT_BAR.with(|slot| *slot.borrow_mut() = script_bar(state, completed, total));
    let Some(list) = taskbar() else {
        return false;
    };
    unsafe { apply(&list, hwnd, state, completed, total) }
}

/// What to remember for a script: `None` means it took its bar down, so a
/// later completion notice has nothing to hand back.
fn script_bar(state: TaskbarState, completed: u64, total: u64) -> Option<(TaskbarState, u64, u64)> {
    (!matches!(state, TaskbarState::None))
        .then(|| (state, completed.min(total), total.max(1)))
}

/// The completion notice: the bar at full in the status colour — the system
/// accent for a clean exit, red for anything else. Full rather than a
/// fraction because the tool runner reports an outcome, not an amount of
/// work. The bar stays exactly like this for as long as the notice is up;
/// the button flash is what moves.
fn finish_progress(hwnd: HWND, ok: bool) -> bool {
    let Some(list) = taskbar() else {
        return false;
    };
    let applied = unsafe { apply(&list, hwnd, finish_state(ok), 1, 1) };
    FINISH_PROGRESS_UP.store(applied, Ordering::Relaxed);
    applied
}

/// The completion notice's colour: the system accent for a clean exit, red
/// otherwise.
fn finish_state(ok: bool) -> TaskbarState {
    if ok {
        TaskbarState::Normal
    } else {
        TaskbarState::Error
    }
}

fn tbp_flag(state: TaskbarState) -> TBPFLAG {
    match state {
        TaskbarState::None => TBPF_NOPROGRESS,
        TaskbarState::Indeterminate => TBPF_INDETERMINATE,
        TaskbarState::Normal => TBPF_NORMAL,
        TaskbarState::Error => TBPF_ERROR,
        TaskbarState::Paused => TBPF_PAUSED,
    }
}

/// The value goes first: the state that draws it must not paint one frame
/// with the previous run's value. States without a scale ignore the value.
unsafe fn apply(
    list: &ITaskbarList3,
    hwnd: HWND,
    state: TaskbarState,
    completed: u64,
    total: u64,
) -> bool {
    if matches!(
        state,
        TaskbarState::Normal | TaskbarState::Error | TaskbarState::Paused
    ) && list
        .SetProgressValue(hwnd, completed.min(total), total.max(1))
        .is_err()
    {
        return false;
    }
    list.SetProgressState(hwnd, tbp_flag(state)).is_ok()
}

/// The frame thread's `ITaskbarList3`. COM is already up on this thread —
/// the frame's start-up calls `OleInitialize` for the drop target — so this
/// only has to build the object. It is cached because a script may set the
/// bar from a short timer; the interface is not `Send`, and every caller
/// here runs on the frame's thread, so a thread-local slot is the right
/// cache. `None` on a shell without the interface (XP, or a COM failure)
/// simply means no bar; the flash and the sound still happen.
fn taskbar() -> Option<ITaskbarList3> {
    thread_local! {
        static LIST: RefCell<Option<ITaskbarList3>> = const { RefCell::new(None) };
    }
    LIST.with(|slot| {
        if let Some(list) = slot.borrow().as_ref() {
            return Some(list.clone());
        }
        let list: ITaskbarList3 = unsafe {
            CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER).ok()?
        };
        unsafe { list.HrInit().ok()? };
        *slot.borrow_mut() = Some(list.clone());
        Some(list)
    })
}

/// The badge for this outcome, made once per process and kept: the shell keeps
/// referring to the icon it was handed, so destroying it after
/// `SetOverlayIcon` returns would leave the button drawing from a dead bitmap.
fn badge(ok: bool) -> Option<HICON> {
    static GOOD: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    static BAD: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let slot = if ok { &GOOD } else { &BAD };
    let handle = *slot.get_or_init(|| unsafe {
        let size = badge_size();
        let icon = make_icon(&badge_pixels(size, ok), size);
        icon.map(|i| i.0 as usize).unwrap_or(0)
    });
    (handle != 0).then_some(HICON(handle as *mut core::ffi::c_void))
}

/// The taskbar draws its overlay from the small-icon metric, so that is the
/// size the badge is painted at.
fn badge_size() -> usize {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) };
    size.clamp(12, 64) as usize
}

/// A 32 bpp BGRA badge, top-down: a filled circle with a white tick or cross.
/// Every pixel is supersampled 4×4 so the circle keeps a clean edge at 16 px,
/// where a hard mask would visibly stair-step.
fn badge_pixels(size: usize, ok: bool) -> Vec<u8> {
    let mut px = vec![0u8; size * size * 4];
    let radius = 0.5 * size as f32;
    let centre = radius - 0.5;
    let unit = 1.0 / size as f32;
    let rgb = if ok { OK_RGB } else { BAD_RGB };
    for y in 0..size {
        for x in 0..size {
            let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let fx = x as f32 + (sx as f32 + 0.5) / 4.0 - centre;
                    let fy = y as f32 + (sy as f32 + 0.5) / 4.0 - centre;
                    if fx * fx + fy * fy > radius * radius {
                        continue;
                    }
                    let ux = fx * unit + 0.5;
                    let uy = fy * unit + 0.5;
                    let ink = if ok { on_tick(ux, uy) } else { on_cross(ux, uy) };
                    let c = if ink { GLYPH_RGB } else { rgb };
                    r += c[0] as u32;
                    g += c[1] as u32;
                    b += c[2] as u32;
                    n += 1;
                }
            }
            if n == 0 {
                continue;
            }
            let o = (y * size + x) * 4;
            px[o] = (b / n) as u8;
            px[o + 1] = (g / n) as u8;
            px[o + 2] = (r / n) as u8;
            px[o + 3] = (n * 255 / 16) as u8;
        }
    }
    px
}

/// The tick, in the 0..1 space of the badge.
fn on_tick(x: f32, y: f32) -> bool {
    near(x, y, (0.26, 0.51), (0.43, 0.68), 0.14) || near(x, y, (0.43, 0.68), (0.75, 0.33), 0.14)
}

/// The cross, the same weight as the tick.
fn on_cross(x: f32, y: f32) -> bool {
    near(x, y, (0.33, 0.33), (0.67, 0.67), 0.14) || near(x, y, (0.67, 0.33), (0.33, 0.67), 0.14)
}

/// Whether `(x, y)` lies within `width` of the segment `a`-`b`.
fn near(x: f32, y: f32, a: (f32, f32), b: (f32, f32), width: f32) -> bool {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = (((x - a.0) * dx + (y - a.1) * dy) / len2).clamp(0.0, 1.0);
    let (px, py) = (a.0 + t * dx - x, a.1 + t * dy - y);
    px * px + py * py <= (width * 0.5) * (width * 0.5)
}

/// Turn the painted pixels into an `HICON`. The mask is all zeros — a 32 bpp
/// icon is drawn from its alpha channel, and the mask is only consulted for
/// the pixels that channel leaves fully transparent. `CreateBitmap` leaves the
/// bits undefined when it is handed no buffer, so the zeros are passed in
/// rather than assumed.
unsafe fn make_icon(px: &[u8], size: usize) -> Option<HICON> {
    // One bit per pixel, rows padded to a DWORD.
    let stride = size.div_ceil(32) * 4;
    let mask_bits = vec![0u8; stride * size];
    let mask = CreateBitmap(
        size as i32,
        size as i32,
        1,
        1,
        Some(mask_bits.as_ptr() as *const core::ffi::c_void),
    );
    if mask.is_invalid() {
        return None;
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size as i32,
            // Negative: the rows are handed over top-down, the way the badge
            // is painted.
            biHeight: -(size as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let screen = GetDC(None);
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let color: HBITMAP =
        CreateDIBSection(Some(screen), &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default();
    ReleaseDC(None, screen);
    if color.is_invalid() || bits.is_null() {
        let _ = DeleteObject(HGDIOBJ(mask.0));
        return None;
    }
    std::ptr::copy_nonoverlapping(px.as_ptr(), bits as *mut u8, px.len());
    let ii = ICONINFO {
        fIcon: BOOL(1),
        hbmMask: mask,
        hbmColor: color,
        ..Default::default()
    };
    let icon = CreateIconIndirect(&ii).ok();
    // The icon owns copies of both bitmaps from here on.
    let _ = DeleteObject(HGDIOBJ(color.0));
    let _ = DeleteObject(HGDIOBJ(mask.0));
    icon
}

/// The outcome's configured sound, or the system's own beep when none is set
/// (or the file has gone away since it was chosen). Success and failure read
/// different keys, and their fallback beeps differ too, which is the same
/// distinction the badge makes in colour.
fn play(file: &str, ok: bool) {
    let file = file.trim();
    if !file.is_empty() {
        let wide: Vec<u16> = file.encode_utf16().chain([0]).collect();
        let played = unsafe {
            PlaySoundW(
                PCWSTR(wide.as_ptr()),
                None,
                SND_FILENAME | SND_ASYNC | SND_NODEFAULT,
            )
        };
        if played.as_bool() {
            return;
        }
    }
    unsafe {
        let _ = MessageBeep(if ok { MB_OK } else { MB_ICONHAND });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::DestroyIcon;

    fn at(px: &[u8], size: usize, x: usize, y: usize) -> [u8; 4] {
        let o = (y * size + x) * 4;
        px[o..o + 4].try_into().unwrap()
    }

    #[test]
    fn every_progress_state_has_its_own_shell_flag() {
        let cases = [
            (TaskbarState::None, TBPF_NOPROGRESS),
            (TaskbarState::Indeterminate, TBPF_INDETERMINATE),
            (TaskbarState::Normal, TBPF_NORMAL),
            (TaskbarState::Error, TBPF_ERROR),
            (TaskbarState::Paused, TBPF_PAUSED),
        ];
        for (state, flag) in cases {
            assert_eq!(tbp_flag(state), flag, "{}", state.name());
        }
        let flags: Vec<i32> = cases.iter().map(|(_, f)| f.0).collect();
        let mut unique = flags.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), flags.len(), "no two states share a flag");
    }

    #[test]
    fn a_failed_tool_uses_the_red_state_and_a_good_one_the_green() {
        // Only `Some(0)` is a success at the call site; either way the
        // notice shows a full bar, not a fraction derived from the code.
        assert_eq!(finish_state(true), TaskbarState::Normal);
        assert_eq!(finish_state(false), TaskbarState::Error);
        assert_eq!(tbp_flag(finish_state(true)), TBPF_NORMAL);
        assert_eq!(tbp_flag(finish_state(false)), TBPF_ERROR);
    }

    #[test]
    fn a_script_bar_is_remembered_until_the_script_takes_it_down() {
        assert_eq!(
            script_bar(TaskbarState::Normal, 7, 10),
            Some((TaskbarState::Normal, 7, 10))
        );
        // The same clamp `apply` uses, so handing it back draws the same bar.
        assert_eq!(
            script_bar(TaskbarState::Paused, 99, 4),
            Some((TaskbarState::Paused, 4, 4))
        );
        assert_eq!(script_bar(TaskbarState::None, 3, 10), None);
    }

    /// The whole taskbar button has to change colour in both foreground
    /// cases: the background arm stops on activation, which is exactly why
    /// it cannot be reused in front — there it would stop before blinking.
    #[test]
    fn a_frame_in_front_gets_a_bounded_button_blink() {
        let (back_flags, back_count) = flash_plan(false);
        assert_eq!(back_flags & FLASHW_TIMERNOFG, FLASHW_TIMERNOFG);
        assert_eq!(back_count, 0, "the background arm runs until activation");

        let (front_flags, front_count) = flash_plan(true);
        assert_eq!(front_flags & FLASHW_TRAY, FLASHW_TRAY);
        assert_eq!(front_flags & FLASHW_TIMERNOFG, FLASHWINFO_FLAGS(0));
        assert!(front_count > 0, "a count of zero in front is no flash at all");
        // Five blinks at the default caret rate is about 2.6 s: long enough
        // to read, and over well inside the six-second notice.
        let ms = u64::from(front_count) * 530;
        assert!(ms >= 1500, "{ms} ms is too short to notice");
        assert!(ms <= u64::from(OVERLAY_MS), "{ms} ms outlives the notice");
    }

    #[test]
    fn only_a_frame_in_front_may_end_a_countdown() {
        // The whole point of the reminder: while the frame is behind another
        // window the six-second tick must leave the badge and the bar alone.
        assert!(countdown_may_finish(true));
        assert!(!countdown_may_finish(false));
    }

    #[test]
    fn two_null_handles_are_not_an_active_frame() {
        assert!(!same_root(HWND::default(), HWND::default()));
        let a = HWND(1 as *mut core::ffi::c_void);
        let b = HWND(2 as *mut core::ffi::c_void);
        assert!(same_root(a, a));
        assert!(!same_root(a, b));
        assert!(!same_root(a, HWND::default()));
        // A frame with no handle can never be the foreground window.
        assert!(!frame_is_active(HWND::default()));
    }

    #[test]
    fn the_badge_is_a_disc_that_leaves_the_corners_clear() {
        let px = badge_pixels(16, true);
        assert_eq!(at(&px, 16, 0, 0)[3], 0, "corner stays transparent");
        assert_eq!(at(&px, 16, 15, 15)[3], 0, "corner stays transparent");
        assert_eq!(at(&px, 16, 8, 1)[3], 255, "the disc's top is solid");
        assert_eq!(at(&px, 16, 8, 8)[3], 255, "the centre is solid");
        // Halfway between the centre and the rim the disc is still solid; the
        // antialiased rim is the only partly covered ring.
        assert_eq!(at(&px, 16, 4, 8)[3], 255);
    }

    /// The pixels the glyph touched: the tick and the cross are white, so
    /// every channel is above both badge colours there.
    fn glyph(px: &[u8], size: usize) -> Vec<(usize, usize)> {
        (0..size)
            .flat_map(|y| (0..size).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let p = at(px, size, x, y);
                p[3] > 0 && p[0] > 0xC0 && p[1] > 0xC0 && p[2] > 0xC0
            })
            .collect()
    }

    #[test]
    fn the_two_badges_differ_in_colour_and_in_glyph() {
        let good = badge_pixels(16, true);
        let bad = badge_pixels(16, false);
        // A disc pixel clear of both glyphs carries the badge colour. The
        // pixels are BGRA, so blue, green and red are 0, 1 and 2.
        let g = at(&good, 16, 2, 8);
        let b = at(&bad, 16, 2, 8);
        assert!(g[1] > g[0] && g[1] > g[2], "green badge: {g:?}");
        assert!(b[2] > b[1] && b[2] > b[0], "red badge: {b:?}");
        // The tick starts at 0.26,0.51, left of anything the cross draws, so
        // that pixel is ink on one badge and disc on the other. White ink
        // raises the two channels the red disc leaves dark.
        let tick_only = (at(&good, 16, 3, 7), at(&bad, 16, 3, 7));
        assert!(tick_only.0[1] > tick_only.1[1], "the tick reaches left");
        assert!(tick_only.0[0] > tick_only.1[0], "the tick reaches left");
        // The cross's upper-left arm ends at 0.33,0.33, where the tick is not.
        let cross_only = (at(&good, 16, 4, 4), at(&bad, 16, 4, 4));
        assert!(cross_only.1[1] > cross_only.0[1], "the cross reaches up-left");
        assert!(cross_only.1[0] > cross_only.0[0], "the cross reaches up-left");
        assert!(!glyph(&good, 16).is_empty() && !glyph(&bad, 16).is_empty());
    }

    #[test]
    fn the_glyph_is_the_same_shape_at_every_size() {
        // 16 px is the taskbar's default; 32 is what a 200% taskbar asks for.
        for size in [16, 24, 32] {
            let px = badge_pixels(size, true);
            let mid = size / 2;
            assert_eq!(at(&px, size, 0, 0)[3], 0, "corner at {size}");
            assert_eq!(at(&px, size, mid, 1)[3], 255, "disc at {size}");
            // The tick covers the same fraction of the badge at every size:
            // the stroke width scales with the disc.
            let ink = glyph(&px, size).len();
            let area = size * size;
            assert!(
                ink * 100 > area * 2 && ink * 100 < area * 35,
                "{ink} of {area} at {size}"
            );
        }
    }

    #[test]
    fn the_badge_becomes_a_real_icon() {
        // The GDI half: a DIB section plus a zeroed AND mask, handed to
        // `CreateIconIndirect`. That is as far as a test can go — the overlay
        // call itself needs a taskbar button to hang the icon on.
        for ok in [true, false] {
            let icon = unsafe { make_icon(&badge_pixels(16, ok), 16) }
                .expect("the badge is a valid icon");
            assert!(!icon.is_invalid());
            unsafe {
                let _ = DestroyIcon(icon);
            }
        }
    }
}

