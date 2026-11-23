//! Keep the mouse from asking for more frames than the screen can show.
//!
//! # Why this exists
//!
//! `egui` asks for a **new frame the moment the pointer moves**:
//!
//! ```text
//! PointerState::wants_repaint() == !pointer_events.is_empty() || delta != Vec2::ZERO
//! ```
//!
//! and the delay it asks for is a **minimum**: the pointer's zero always wins
//! over whatever the program asked for. There is no public way to say
//! "do not redraw more often than every 33 milliseconds" — the two fields
//! that decide it (`delta` and `pointer_events`) are private to `egui`.
//!
//! So the launcher redraws once per mouse message. A mouse that reports a
//! thousand times a second therefore asks for a thousand frames a second.
//!
//! # What that costs
//!
//! Every frame of this window goes through the desktop compositor, and
//! through everything that watches the screen: the overlay of the graphics
//! driver, the remote-desktop program that is running, the virtual display
//! driver. On a machine with all of those, one frame can take forty
//! milliseconds — and a hundred frames a second then asks for four seconds
//! of work per second.
//!
//! The window does not get slow by itself — it is drawn in three
//! microseconds — but **the desktop does**, and the pointer with it. That is
//! why the machine feels heavy exactly while the pointer is over the
//! launcher, and light again as soon as it leaves: the launcher then draws
//! four frames a second instead of a hundred.
//!
//! # What this does
//!
//! The window's own procedure is replaced by one that **holds back** the
//! mouse messages that come too soon. The newest position is kept and
//! delivered a moment later, so the pointer is never in the wrong place —
//! only a little behind, and only for a moment.
//!
//! A drag is held back like any other move. What a drag needs is not every
//! intermediate position — it is **the newest position on every frame the
//! screen can show**, which is exactly what the hold-back delivers (the
//! key flags of the message, button state included, are kept with it).
//! Letting a drag through at once asked for a frame per mouse report, and
//! on a machine whose frames are expensive that made the pointer itself
//! stumble — the drag felt worst exactly where the bypass was meant to
//! help.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use crate::winapi::*;

/// The timer that delivers the mouse move that was held back.
const TIMER_ID: usize = 0x1D5;

/// The window procedure that was replaced.
static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
/// The mouse message that was held back.
static PENDING: AtomicI64 = AtomicI64::new(0);
/// The key flags (button state) of the message that was held back.
static PENDING_FLAGS: AtomicU64 = AtomicU64::new(0);
/// Whether there is a mouse message waiting.
static HAS_PENDING: AtomicBool = AtomicBool::new(false);
/// When the last mouse message was let through.
static LAST_THROUGH_MS: AtomicU64 = AtomicU64::new(0);
/// The wheel notches that the app has not picked up yet, in `WHEEL_DELTA`
/// units (a notch is 120).
static WHEEL: AtomicI32 = AtomicI32::new(0);
/// Whether the result list wants the wheel right now.
static WHEEL_CAPTURED: AtomicBool = AtomicBool::new(false);
/// Where the list is, in physical pixels of the client area. `i32::MIN`
/// in `LIST_LEFT` means "the list is not on the screen".
static LIST_RECT: [AtomicI32; 4] = [
    AtomicI32::new(i32::MIN),
    AtomicI32::new(0),
    AtomicI32::new(0),
    AtomicI32::new(0),
];
/// The distance from the top of one row to the top of the next, in
/// thousandths of a physical pixel.
static LIST_PITCH: AtomicU32 = AtomicU32::new(0);
/// The row the last mouse move belonged to, or [`i32::MIN`] when the last
/// move was somewhere else than on a row of the list.
static LAST_ROW: AtomicI32 = AtomicI32::new(i32::MIN);
/// Whether a move that stays on the same row may be dropped.
///
/// It may not while a **popup** is open (a context menu, for instance): the
/// menu lies over the list, and a pointer that moves from one menu entry to
/// the next may well stay on "the same list row" underneath — dropping the
/// move would leave the menu highlighting the entry it was on, not the one
/// under the pointer.
static ROW_DROP: AtomicBool = AtomicBool::new(true);

/// One notch of the wheel, in the units of `WM_MOUSEWHEEL`.
pub const WHEEL_DELTA: i32 = 120;
/// The smallest time between two mouse messages that are let through.
///
/// `egui` draws **two** frames for every mouse message ("each request
/// results in two repaints"), so thirty-three milliseconds — thirty messages
/// a second — are sixty frames a second.
static INTERVAL_MS: AtomicU32 = AtomicU32::new(33);
/// How many mouse messages arrived.
static SEEN: AtomicU64 = AtomicU64::new(0);
/// How many of them were let through.
static PASSED: AtomicU64 = AtomicU64::new(0);

/// The time since the program started, in milliseconds.
fn now_ms() -> u64 {
    use std::sync::OnceLock;
    static START: OnceLock<Instant> = OnceLock::new();
    let start = START.get_or_init(Instant::now);
    start.elapsed().as_millis() as u64
}

/// Set how often the pointer may cause a new frame.
///
/// The default is 16 milliseconds — sixty frames a second, which is what a
/// healthy machine can show. A machine whose frames take longer gets a
/// larger number, so that the window asks for no more frames than the screen
/// can actually show.
pub fn set_interval(millis: u32) {
    INTERVAL_MS.store(millis.clamp(4, 60), Ordering::SeqCst);
}

/// The interval that is in force.
pub fn interval_ms() -> u32 {
    INTERVAL_MS.load(Ordering::SeqCst)
}

/// How many mouse messages arrived, and how many were let through.
pub fn counts() -> (u64, u64) {
    (
        SEEN.load(Ordering::Relaxed),
        PASSED.load(Ordering::Relaxed),
    )
}

/// Tell the throttle whether the result list wants the wheel.
///
/// While this is set, the wheel messages of the window are answered by the
/// launcher itself and are **not passed on** to `egui`.
///
/// # Why this exists
///
/// `egui` smears every wheel notch over a tenth of a second —
/// `wheel_state.rs` keeps an `unprocessed_wheel_delta` and hands it out
/// "reach 90% in 0.1 s", and `wants_repaint` asks for the **next frame at
/// once** as long as more than a fifth of a point is left (egui 0.37,
/// `input_state/mod.rs`). One notch of the wheel is therefore about forty
/// frames, each of them asked for immediately: a loop that no style or
/// option turns off, and that runs at whatever speed the frames can be
/// made. On a machine that needs tens of milliseconds to put a frame on
/// the screen, scrolling a list is seconds of compositor work per second —
/// the fan and the stutter of the pointer.
///
/// The launcher answers the wheel itself instead: a notch moves the list
/// by whole rows, **at once**, and the notches are handed to the app with
/// [`take_wheel`]. A burst of notches costs one frame, because the frame
/// is asked for with `InvalidateRect` and Windows coalesces `WM_PAINT`.
///
/// The messages are only taken away while the list is on the screen; the
/// settings pages keep the wheel of `egui` (their scroll areas are small
/// and are looked at rarely).
pub fn capture_wheel(on: bool) {
    WHEEL_CAPTURED.store(on, Ordering::SeqCst);
}

/// Take the wheel notches that arrived since the last look, in
/// `WHEEL_DELTA` units. Positive is a notch away from the user.
pub fn take_wheel() -> i32 {
    WHEEL.swap(0, Ordering::SeqCst)
}

/// Tell the throttle where the rows of the result list are.
///
/// # Why this exists
///
/// The selection of the list follows **rows**, not pixels: a move of the
/// pointer within one and the same row changes nothing at all on the
/// screen. But `egui` asks for two frames for every mouse message it is
/// given, and on a machine that needs tens of milliseconds to put a frame
/// on the screen, a pointer that is merely moving *within* a row — which
/// is most of the moves, a row is two thirds of a centimetre tall — keeps
/// the compositor busy for nothing. That is what the moving pointer still
/// felt like it stuttered.
///
/// So a move that stays on the row of the last delivered move is dropped
/// right here, before `egui` ever hears of it. Crossing into another row
/// is delivered at once — the selection follows the pointer exactly, and
/// a sweep through the list costs one message per row it crosses instead
/// of one per report of the mouse. A drag is never dropped: dragging has
/// to follow the pointer to the pixel.
pub fn set_list_hit_area(left: i32, top: i32, right: i32, bottom: i32, pitch_px: f32) {
    // The row of the last delivered move is **not** reset here: the area is
    // published on every frame, and a reset on every frame would make every
    // move look like the first one on a row — nothing would ever be dropped.
    // The remembered row stays valid across frames; scrolling the list under
    // a resting pointer does not invalidate it either, because the hover
    // selection is recomputed by the frame that scrolled, with no move
    // needed.
    LIST_RECT[0].store(left, Ordering::SeqCst);
    LIST_RECT[1].store(top, Ordering::SeqCst);
    LIST_RECT[2].store(right, Ordering::SeqCst);
    LIST_RECT[3].store(bottom, Ordering::SeqCst);
    LIST_PITCH
        .store((pitch_px * 1000.0).max(0.0) as u32, Ordering::SeqCst);
}

/// Forget where the rows are — the list is not on the screen.
pub fn clear_list_hit_area() {
    LIST_RECT[0].store(i32::MIN, Ordering::SeqCst);
    LAST_ROW.store(i32::MIN, Ordering::SeqCst);
}

/// The row the interface has selected. A move may only be dropped when it
/// stays on the row the interface has *settled on* and is deep inside it;
/// while the pointer is on a row not yet taken — the hysteresis is still
/// deciding — every move is delivered, or one dropped move at the wrong
/// moment leaves the highlight stuck on the old row.
static SELECTED_ROW: AtomicI32 = AtomicI32::new(i32::MIN);

/// Tell the throttle which row is selected.
pub fn set_selected_row(row: usize) {
    SELECTED_ROW.store(row as i32, Ordering::SeqCst);
}

/// Whether moves within one row may be dropped right now.
pub fn set_row_drop(enabled: bool) {
    ROW_DROP.store(enabled, Ordering::SeqCst);
}

/// Whether the move stays on the row of the last delivered move.
/// Such a move changes nothing on the screen and is dropped.
fn stays_on_the_same_row(lparam: LPARAM) -> bool {
    if !ROW_DROP.load(Ordering::SeqCst) {
        LAST_DELIVERED_ROW.store(i32::MIN, Ordering::SeqCst);
        return false;
    }
    let left = LIST_RECT[0].load(Ordering::SeqCst);
    if left == i32::MIN {
        return false;
    }
    let x = (lparam & 0xffff) as u16 as i16 as i32;
    let y = ((lparam >> 16) & 0xffff) as u16 as i16 as i32;
    let top = LIST_RECT[1].load(Ordering::SeqCst);
    let right = LIST_RECT[2].load(Ordering::SeqCst);
    let bottom = LIST_RECT[3].load(Ordering::SeqCst);
    let inside = x >= left && x < right && y >= top && y < bottom;
    if !inside {
        // Somewhere else than on a row: from now on the next row entry is
        // a change again.
        LAST_ROW.store(i32::MIN, Ordering::SeqCst);
        return false;
    }
    let pitch = LIST_PITCH.load(Ordering::SeqCst) as i64;
    if pitch == 0 {
        return false;
    }
    let row = ((y - top) as i64 * 1000 / pitch) as i32;
    // How deep inside its row the pointer is, in thousandths of a pixel.
    let within = ((y - top) as i64 * 1000 - row as i64 * pitch) as i64;
    // The strip at either edge of a row where the selection decides between
    // this row and its neighbour (the hysteresis of the app is four points
    // deep). A move in that strip is **delivered**, even when it stays on
    // the same row: the decision needs fresh positions to be made, and a
    // dropped move would leave the interface holding a stale one — the
    // pointer could move deep into the new row while the highlight stays on
    // the old one, stuck until the next crossing.
    let edge = pitch / 15; // ≈ 4 points of a 62-point row
    let near_edge = within < edge || within > pitch - edge;
    let settled_on = row == SELECTED_ROW.load(Ordering::SeqCst);
    let last = LAST_ROW.swap(row, Ordering::SeqCst);
    if last == row && !near_edge && settled_on {
        // Dropped. If this row is not the row the interface last heard of,
        // the drop just ate a crossing — the interface would keep showing
        // the old row while the pointer is on a new one. That is a bug, and
        // it says so (loudly enough to be found, quietly enough to not
        // spam: once a second).
        if row != LAST_DELIVERED_ROW.load(Ordering::SeqCst) {
            static LAST_WARN: AtomicU64 = AtomicU64::new(0);
            let now = now_ms();
            let was = LAST_WARN.swap(now, Ordering::SeqCst);
            if now.saturating_sub(was) > 1000 {
                log::warn!(
                    "a crossing was dropped: pointer row {row}, \
                     interface last heard of row {}",
                    LAST_DELIVERED_ROW.load(Ordering::SeqCst)
                );
            }
        }
        return true;
    }
    // This move will reach the interface (at once, or by the timer): it is
    // the row the interface will know about from now on.
    LAST_DELIVERED_ROW.store(row, Ordering::SeqCst);
    false
}

/// Put the throttle on the window.
///
/// It is put on the window that already exists, so the window procedure is
/// only replaced once and the setting is kept.
pub fn install(hwnd: HWND) {
    if hwnd.is_null() {
        return;
    }
    unsafe {
        let previous = GetWindowLongPtrW(hwnd, GWLP_WNDPROC);
        if previous == 0 {
            return;
        }
        ORIGINAL.store(previous as usize, Ordering::SeqCst);
        SetWindowLongPtrW(
            hwnd,
            GWLP_WNDPROC,
            window_proc as *const () as isize,
        );

        // **The raw mouse input is taken away from this window.**
        //
        // The raw input is what a mouse reports a thousand times a second,
        // and `eframe` feeds every one of those reports straight into the
        // user interface — so the window asked for a thousand frames a
        // second, and nothing could be done about it from the outside.
        //
        // Without it the pointer comes from the ordinary mouse messages,
        // which Windows merges on their own: the position is always the
        // newest one, and only one message per mouse move is left over.
        // Those are the messages the throttle above can hold back.
        let device = RAWINPUTDEVICE {
            usUsagePage: 0x01, // the mouse
            usUsage: 0x02,
            dwFlags: RIDEV_REMOVE,
            hwndTarget: std::ptr::null_mut(),
        };
        let ok = RegisterRawInputDevices(
            &device,
            1,
            std::mem::size_of::<RAWINPUTDEVICE>() as u32,
        );
        if ok == 0 {
            log::warn!("the raw mouse input could not be taken away: {}", GetLastError());
        } else {
            log::info!("the raw mouse input was taken away from the window");
        }
    }
    let interval = interval_ms();
    log::info!("the mouse is throttled: at most one frame every {interval} ms");
}

/// The row of the last move that was **delivered** (not dropped), for the
/// diagnostic that catches a crossing dropped by mistake.
static LAST_DELIVERED_ROW: AtomicI32 = AtomicI32::new(i32::MIN);

/// Whether a mouse button is held down.
fn button_down() -> bool {
    unsafe {
        GetAsyncKeyState(VK_LBUTTON) < 0
            || GetAsyncKeyState(VK_RBUTTON) < 0
            || GetAsyncKeyState(VK_MBUTTON) < 0
    }
}

/// The window procedure that holds the mouse messages back.
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let original: WNDPROC =
        unsafe { std::mem::transmute(ORIGINAL.load(Ordering::SeqCst) as usize) };
    match msg {
        WM_MOUSEMOVE => {
            SEEN.fetch_add(1, Ordering::Relaxed);
            // A move that stays on the row of the last one delivered
            // changes nothing on the screen — it is dropped before `egui`
            // can turn it into frames (see [`set_list_hit_area`]).
            if !button_down() && stays_on_the_same_row(lparam) {
                return 0;
            }
            // A drag is held back like any other move.
            //
            // It used to be let through at once, on the ground that "a drag
            // has to follow the pointer exactly". But a mouse that reports
            // a thousand times a second then asked for a frame per report,
            // and on a machine whose frames are expensive the *pointer*
            // itself — which is drawn by the same compositor — began to
            // stumble: the drag felt worst exactly on the machines the
            // bypass was meant to help. What a drag really needs is not
            // every intermediate position, it is **the newest position on
            // every frame the screen can show** — which the hold-back below
            // delivers, and the interval adapts to what the screen can take
            // (16 ms on a healthy one, more on one whose frames are slow).
            let now = now_ms();
            let last = LAST_THROUGH_MS.load(Ordering::SeqCst);
            let interval = INTERVAL_MS.load(Ordering::SeqCst) as u64;
            if now.saturating_sub(last) >= interval {
                LAST_THROUGH_MS.store(now, Ordering::SeqCst);
                HAS_PENDING.store(false, Ordering::SeqCst);
                PASSED.fetch_add(1, Ordering::Relaxed);
                return unsafe { CallWindowProcW(original, hwnd, msg, wparam, lparam) };
            }
            // Too soon. The newest position is kept and delivered a moment
            // later, so the pointer is never lost — only a little behind.
            PENDING.store(lparam as i64, Ordering::SeqCst);
            // The key flags of the message are kept too: they say whether a
            // button is down (`MK_LBUTTON` and friends), and a drag that is
            // delivered by the timer **must** still look like a drag, or the
            // interface would believe the button was let go of mid-drag.
            PENDING_FLAGS.store(wparam as u64, Ordering::SeqCst);
            HAS_PENDING.store(true, Ordering::SeqCst);
            let remaining = (interval - (now - last)).max(1) as u32;
            unsafe {
                SetTimer(hwnd, TIMER_ID, remaining, std::ptr::null_mut());
            }
            return 0;
        }
        WM_TIMER => {
            if wparam == TIMER_ID {
                unsafe {
                    KillTimer(hwnd, TIMER_ID);
                }
                if HAS_PENDING.swap(false, Ordering::SeqCst) {
                    let pending = PENDING.swap(0, Ordering::SeqCst) as LPARAM;
                    let flags = PENDING_FLAGS.swap(0, Ordering::SeqCst) as WPARAM;
                    LAST_THROUGH_MS.store(now_ms(), Ordering::SeqCst);
                    PASSED.fetch_add(1, Ordering::Relaxed);
                    return unsafe {
                        CallWindowProcW(original, hwnd, WM_MOUSEMOVE, flags, pending)
                    };
                }
                return 0;
            }
        }
        WM_MOUSEWHEEL => {
            // The wheel belongs to the result list: the notch is counted and
            // the message is answered here, so that `egui` never hears of it
            // and never starts the smear that costs forty frames a notch
            // (see [`capture_wheel`]). One frame is asked for; a burst of
            // notches still costs one frame, because `WM_PAINT` is coalesced
            // by Windows itself.
            if WHEEL_CAPTURED.load(Ordering::SeqCst) {
                // The high word of `wParam` is the turn of the wheel, in
                // `WHEEL_DELTA` units, positive away from the user.
                let delta = ((wparam >> 16) & 0xffff) as u16 as i16 as i32;
                WHEEL.fetch_add(delta, Ordering::SeqCst);
                crate::hotkey::wake(hwnd as usize);
                return 0;
            }
        }
        _ => {}
    }
    unsafe { CallWindowProcW(original, hwnd, msg, wparam, lparam) }
}
