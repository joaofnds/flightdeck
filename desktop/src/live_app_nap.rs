use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::NSString;

/// Keeps App Nap off for as long as the app runs. macOS naps an app none of whose windows
/// show, and a napped app reads on the slower cores at a lower priority. A window shown again
/// would then wait on a napped first read, and the window under the control socket, which
/// never shows and reads its page every second, would measure napped reads. A hidden window
/// reads no page, so the nap has little left to save.
pub(super) struct AppNapExemption {
    activity: Retained<AnyObject>,
}

impl gpui::Global for AppNapExemption {}

/// `NSActivityUserInitiated` in Foundation's NSProcessInfo.h.
const ACTIVITY_USER_INITIATED: u64 = 0x00FF_FFFF;
/// `NSActivityIdleSystemSleepDisabled` in Foundation's NSProcessInfo.h.
const ACTIVITY_IDLE_SYSTEM_SLEEP_DISABLED: u64 = 1 << 20;
/// `NSActivityUserInitiatedAllowingIdleSystemSleep` in Foundation's NSProcessInfo.h.
const ACTIVITY_USER_INITIATED_ALLOWING_IDLE_SYSTEM_SLEEP: u64 = 0x00EF_FFFF;
const _: () = {
    assert!(
        ACTIVITY_USER_INITIATED_ALLOWING_IDLE_SYSTEM_SLEEP
            == ACTIVITY_USER_INITIATED & !ACTIVITY_IDLE_SYSTEM_SLEEP_DISABLED
    );
};

impl AppNapExemption {
    pub(super) fn begin() -> Self {
        let reason = NSString::from_str("Flightdeck reads at full speed while it is hidden");
        // SAFETY: NSProcessInfo declares beginActivityWithOptions:reason: with a uint64_t and an
        // NSString, and returns the activity's token.
        let activity: Retained<AnyObject> = unsafe {
            msg_send![
                &*process_info(),
                beginActivityWithOptions: ACTIVITY_USER_INITIATED_ALLOWING_IDLE_SYSTEM_SLEEP,
                reason: &*reason
            ]
        };

        Self { activity }
    }
}

impl Drop for AppNapExemption {
    fn drop(&mut self) {
        // SAFETY: endActivity: takes the token beginActivityWithOptions:reason: returned.
        let () = unsafe { msg_send![&*process_info(), endActivity: &*self.activity] };
    }
}

fn process_info() -> Retained<AnyObject> {
    // SAFETY: processInfo is a class method of NSProcessInfo that returns the shared object.
    unsafe { msg_send![objc2::class!(NSProcessInfo), processInfo] }
}
