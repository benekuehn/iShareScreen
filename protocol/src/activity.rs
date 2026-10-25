//! Shared native viewer power policy. This only budgets decoded output and
//! presentation; compressed reference pictures must still reach the decoder.
use std::ffi::{c_int, c_void};
use std::sync::Mutex;

const SECOND: u64 = 1_000_000_000;
pub struct Activity {
    focused: bool,
    visible: bool,
    background_fps: u32,
    last_render: Option<u64>,
    last_output: Option<u64>,
    last_frame: Option<u64>,
    busy_until: u64,
    outputs_selected: u64,
    outputs_suppressed: u64,
}
#[repr(C)]
#[derive(Default)]
pub struct Snapshot {
    pub focused: u32,
    pub visible: u32,
    pub mode: u32, // 0 active, 1 idle, 2 background, 3 hidden
    pub background_fps: u32,
    pub outputs_selected: u64,
    pub outputs_suppressed: u64,
}
impl Activity {
    pub fn new(background_fps: u32) -> Option<Self> {
        (1..=60).contains(&background_fps).then_some(Self {
            focused: true,
            visible: true,
            background_fps,
            last_render: None,
            last_output: None,
            last_frame: None,
            busy_until: 0,
            outputs_selected: 0,
            outputs_suppressed: 0,
        })
    }
    pub fn window(&mut self, focused: bool, visible: bool, now: u64) {
        let resumed = visible && focused && (!self.focused || !self.visible);
        self.focused = focused;
        self.visible = visible;
        if resumed {
            self.last_render = None;
            self.last_output = None;
            self.input(now);
        }
    }
    pub fn input(&mut self, now: u64) {
        self.busy_until = now.saturating_add(SECOND);
    }
    pub fn take_output(&mut self, now: u64) -> bool {
        if self
            .last_frame
            .is_some_and(|last| now.saturating_sub(last) < SECOND / 5)
        {
            self.busy_until = now.saturating_add(SECOND);
        }
        self.last_frame = Some(now);
        let interval = if !self.visible {
            SECOND
        } else if !self.focused {
            SECOND / self.background_fps as u64
        } else {
            0
        };
        if self
            .last_output
            .is_none_or(|last| now.saturating_sub(last) >= interval)
        {
            self.last_output = Some(now);
            self.outputs_selected += 1;
            true
        } else {
            self.outputs_suppressed += 1;
            false
        }
    }
    pub fn render_delay(&self, now: u64) -> u64 {
        if !self.visible {
            return SECOND / 2;
        }
        if self.focused {
            return 0;
        } // No added foreground pacing/latency.
        self.last_render.map_or(0, |last| {
            last.saturating_add(SECOND / self.background_fps as u64)
                .saturating_sub(now)
        })
    }
    pub fn submitted(&mut self, now: u64) {
        self.last_render = Some(now);
    }
    pub fn event_wait(&self, now: u64) -> u64 {
        if !self.visible {
            SECOND / 2
        } else if !self.focused || now > self.busy_until {
            SECOND / 5
        } else {
            SECOND / 60
        }
    }
    pub fn snapshot(&self, now: u64) -> Snapshot {
        Snapshot {
            focused: self.focused as u32,
            visible: self.visible as u32,
            mode: if !self.visible {
                3
            } else if !self.focused {
                2
            } else if now > self.busy_until {
                1
            } else {
                0
            },
            background_fps: self.background_fps,
            outputs_selected: self.outputs_selected,
            outputs_suppressed: self.outputs_suppressed,
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn iss_activity_new(background_fps: u32) -> *mut c_void {
    Activity::new(background_fps).map_or(std::ptr::null_mut(), |state| {
        Box::into_raw(Box::new(Mutex::new(state))).cast()
    })
}
// Live handles must not be freed during calls. Each operation is short and
// serialized; no socket read, GPU wait or platform event dispatch holds it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_activity_window(
    handle: *mut c_void,
    focused: c_int,
    visible: c_int,
    now: u64,
) {
    if let Some(state) = unsafe { handle.cast::<Mutex<Activity>>().as_ref() } {
        if let Ok(mut state) = state.lock() {
            state.window(focused != 0, visible != 0, now);
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_activity_input(handle: *mut c_void, now: u64) {
    if let Some(state) = unsafe { handle.cast::<Mutex<Activity>>().as_ref() } {
        if let Ok(mut state) = state.lock() {
            state.input(now);
        }
    }
}
/// operation: 0 render delay, 1 event wait, 2 select output, 3 record submit.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_activity_tick(handle: *mut c_void, operation: c_int, now: u64) -> u64 {
    if let Some(state) = unsafe { handle.cast::<Mutex<Activity>>().as_ref() } {
        if let Ok(mut state) = state.lock() {
            return match operation {
                0 => state.render_delay(now),
                1 => state.event_wait(now),
                2 => state.take_output(now) as u64,
                3 => {
                    state.submitted(now);
                    0
                }
                _ => 0,
            };
        }
    }
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_activity_snapshot(handle: *mut c_void, now: u64, out: *mut Snapshot) {
    if out.is_null() {
        return;
    }
    if let Some(state) = unsafe { handle.cast::<Mutex<Activity>>().as_ref() } {
        if let Ok(state) = state.lock() {
            unsafe {
                *out = state.snapshot(now);
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_activity_free(handle: *mut c_void) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle.cast::<Mutex<Activity>>()) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn background_limits_output_and_presentation_without_delaying_focus_restore() {
        let mut state = Activity::new(5).unwrap();
        state.window(false, true, 0);
        assert!(state.take_output(0));
        state.submitted(0);
        for now in [1, 16_000_000, 100_000_000, 199_000_000] {
            assert!(!state.take_output(now));
            assert!(state.render_delay(now) > 0);
        }
        assert!(state.take_output(200_000_000));
        assert_eq!(state.render_delay(200_000_000), 0);
        state.submitted(200_000_000);
        state.window(true, true, 210_000_000);
        assert_eq!(state.render_delay(210_000_000), 0);
        assert!(state.take_output(210_000_000));
        assert!(state.take_output(211_000_000));
        assert_eq!(state.snapshot(211_000_000).mode, 0);
    }
    #[test]
    fn hidden_preserves_low_rate_progress_and_focus_state_wakes_immediately() {
        let mut state = Activity::new(5).unwrap();
        state.window(false, false, 0);
        assert!(state.take_output(0));
        assert!(!state.take_output(SECOND - 1));
        assert!(state.take_output(SECOND));
        assert_eq!(state.event_wait(0), SECOND / 2);
        state.window(true, true, SECOND + 1);
        assert_eq!(state.render_delay(SECOND + 1), 0);
        assert!(state.take_output(SECOND + 1));
    }
    #[test]
    fn idle_waits_for_events_and_activity_boosts_without_a_polling_loop() {
        let mut state = Activity::new(5).unwrap();
        assert_eq!(state.event_wait(2 * SECOND), SECOND / 5);
        state.input(2 * SECOND);
        assert_eq!(state.event_wait(2 * SECOND), SECOND / 60);
        assert_eq!(state.event_wait(4 * SECOND), SECOND / 5);
        assert!(Activity::new(0).is_none());
        assert!(Activity::new(61).is_none());
    }
}
