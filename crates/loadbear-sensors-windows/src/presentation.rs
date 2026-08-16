//! Staying on top of the taskbar, and knowing when not to.
//!
//! # Why this exists
//!
//! The taskbar strip has to reclaim the top of the topmost band every few
//! hundred milliseconds, because the taskbar is topmost too and Explorer
//! raises it whenever a person touches it. Raising repeatedly is the only way
//! to win that, and it is also exactly the behaviour nobody wants over a game
//! or a full screen video: a 360x48 bar that will not go away and cannot be
//! clicked past.
//!
//! Windows already answers this question, and answers it the same way it
//! answers it for notifications, which is the right precedent. A monitor that
//! plants itself over a presentation has stopped being a monitor.

/// Raise a window to the front of the topmost band.
///
/// # Why this is not `Window::set_always_on_top(true)`
///
/// Because that does nothing when the window is already always on top. Read
/// out of `tao-0.35.3/src/platform_impl/windows/window_state.rs` on 2026-08-16:
/// `set_always_on_top` routes into `apply_diff`, which computes `self ^ new`
/// and returns early if the result is empty. The flag was set at creation, so
/// every later call is a no-op and no `SetWindowPos` is ever issued.
///
/// That matters because being topmost is not a position. It is a band, and
/// inside it ordinary z-order applies. The taskbar is topmost too, and Explorer
/// raises it whenever a person touches it, which puts it in front of a strip
/// sitting on top of it. Winning that means asking again, and asking again
/// means calling `SetWindowPos` directly.
///
/// Measured before it was written, rather than assumed: with LoadBear raised
/// this way it sits at z-order position 2 against the taskbar's 3, so a plain
/// `HWND_TOPMOST` does beat the Windows 11 taskbar and no window band trick is
/// needed.
///
/// `hwnd` is the raw handle as an `isize`, which is what keeps this crate free
/// of a dependency on whichever window library the caller happens to use.
pub fn raise_to_the_front(hwnd: isize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOPMOST, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };

    if hwnd == 0 {
        return;
    }

    // SAFETY: a window handle owned by this process for the life of the
    // application. No move, no resize, and no activation, so this changes the
    // z-order and nothing else, and cannot steal focus from what the person is
    // typing into.
    unsafe {
        SetWindowPos(
            hwnd as *mut core::ffi::c_void,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_ASYNCWINDOWPOS | SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

/// Is the machine running something full screen, presenting, or in a state
/// that asks not to be interrupted?
///
/// `SHQueryUserNotificationState` is the documented question, and it is the one
/// the shell uses to decide whether to show a toast. Four of its answers mean
/// stay off the screen:
///
/// | State | Meaning |
/// |---|---|
/// | `QUNS_BUSY` | A full screen application that is not Direct3D |
/// | `QUNS_RUNNING_D3D_FULL_SCREEN` | A full screen Direct3D application, so a game |
/// | `QUNS_PRESENTATION_MODE` | Presentation settings are on |
/// | `QUNS_APP` | An application asked for quiet, through its own full screen state |
///
/// A failed call answers `false`. Refusing to draw because the shell would not
/// say is the wrong default: it would hide the strip on any machine where the
/// call misbehaves, and the visible failure is worse than the invisible one.
pub fn should_stay_out_of_the_way() -> bool {
    use windows_sys::Win32::UI::Shell::{
        SHQueryUserNotificationState, QUERY_USER_NOTIFICATION_STATE, QUNS_APP, QUNS_BUSY,
        QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
    };

    let mut state: QUERY_USER_NOTIFICATION_STATE = 0;
    // SAFETY: the shell writes one enum value into a local we own. The call
    // takes no other pointer and keeps nothing.
    let hr = unsafe { SHQueryUserNotificationState(&mut state) };
    if hr < 0 {
        return false;
    }

    matches!(
        state,
        QUNS_BUSY | QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE | QUNS_APP
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raising_a_handle_that_is_not_a_window_is_survivable() {
        // The handle arrives as a bare isize from another crate, so the two
        // ways it can be wrong are zero and stale. Zero is refused here; a
        // stale handle makes SetWindowPos fail and return, which is why its
        // result is not checked.
        raise_to_the_front(0);
    }

    #[test]
    fn the_question_can_be_asked_without_crashing() {
        // The answer depends on what is on screen while the tests run, so
        // there is nothing to assert about it. What is worth asserting is that
        // the call is well formed, since it is the sort of thing that fails by
        // corrupting a stack rather than by returning an error.
        let _ = should_stay_out_of_the_way();
    }

    #[test]
    fn an_ordinary_desktop_is_not_treated_as_a_presentation() {
        // A test run is an ordinary desktop unless someone is playing a game
        // over it. This is the case that matters: getting it wrong the other
        // way means the strip never appears and nothing says why.
        assert!(
            !should_stay_out_of_the_way(),
            "an ordinary desktop must not read as full screen. If this fails, \
             check whether something is running full screen on this machine"
        );
    }
}
