use std::sync::atomic::{AtomicU32, Ordering};

struct Diagnostics;

static DIAGNOSTICS: Diagnostics = Diagnostics;
static MESSAGES: AtomicU32 = AtomicU32::new(0);
const MESSAGES_MAX: u32 = 256;

pub fn initialize() -> Result<(), String> {
    log::set_logger(&DIAGNOSTICS).map_err(|error| error.to_string())?;
    log::set_max_level(log::LevelFilter::Warn);
    Ok(())
}

pub fn verify_text_backend(system: &gpui::TextSystem) -> Result<(), String> {
    let font = system.resolve_font(&gpui::font(".SystemUIFont"));
    let wide = system
        .advance(font, gpui::px(16.0), 'W')
        .map_err(|error| error.to_string())?;
    let thin = system
        .advance(font, gpui::px(16.0), 'i')
        .map_err(|error| error.to_string())?;
    if thin.width <= gpui::px(0.0) {
        return Err("System font has no advance".into());
    }
    if wide.width <= thin.width {
        return Err(format!(
            "Text backend is a placeholder: W={:?}, i={:?}; enable font-kit",
            wide.width, thin.width
        ));
    }
    let bounds = system
        .typographic_bounds(font, gpui::px(16.0), 'W')
        .map_err(|error| error.to_string())?;
    if bounds.size.width <= gpui::px(0.0) {
        return Err("System glyph has no width".into());
    }
    if bounds.size.height <= gpui::px(0.0) {
        return Err("System glyph has no height".into());
    }
    eprintln!(
        "text-backend: W={:?} i={:?} bounds={:?}",
        wide.width, thin.width, bounds
    );
    Ok(())
}

impl log::Log for Diagnostics {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let index = MESSAGES.load(Ordering::Relaxed);
        if index > MESSAGES_MAX {
            return;
        }
        if MESSAGES
            .compare_exchange(index, index + 1, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        if index < MESSAGES_MAX {
            eprintln!(
                "GPUI {} {}: {}",
                record.level(),
                record.target(),
                record.args()
            );
        }
        if index == MESSAGES_MAX {
            eprintln!("Further GPUI diagnostics suppressed");
        }
    }

    fn flush(&self) {}
}
