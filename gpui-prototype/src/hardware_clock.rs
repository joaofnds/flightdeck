use std::rc::Rc;
use std::time::Duration;

use gpui::{App, Entity};

use crate::hardware_motion::HardwareState;
use crate::live_timer::Timer;

pub fn install<T: 'static>(
    entity: &Entity<T>,
    state: &Rc<HardwareState>,
    cx: &mut App,
) -> Result<(), String> {
    let view = entity.downgrade();
    let weak = Rc::downgrade(state);
    let application = cx.to_async();
    let timer = Timer::with_interval(Duration::from_secs_f64(1.0 / 30.0), move || {
        let Some(state) = weak.upgrade() else {
            return;
        };
        if state.needs_frame() {
            if let Err(error) = application.update(|cx| {
                if let Err(error) = view.update(cx, |_, cx| cx.notify()) {
                    eprintln!("Hardware view update: {error}");
                }
            }) {
                eprintln!("Hardware application update: {error}");
            }
        } else {
            if let Some(timer) = state.clock.borrow().as_ref() {
                timer.set_running(false);
            }
        }
    })?;
    timer.set_running(false);
    *state.clock.borrow_mut() = Some(timer);
    Ok(())
}
