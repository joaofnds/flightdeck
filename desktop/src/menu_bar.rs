//! Flightdeck's item in the macOS menu bar. GPUI has no menu bar item of its own, so this
//! drives the macOS status bar directly.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSEventMask, NSEventModifierFlags, NSEventType, NSImage, NSMenu, NSMenuItem,
    NSSquareStatusItemLength, NSStatusBar, NSStatusItem,
};
use objc2_foundation::NSString;

/// The SF Symbol the item shows, a gauge as on the dashboard of a vehicle.
const SYMBOL: &str = "gauge.with.dots.needle.67percent";

/// What the user asked of the menu bar item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Show,
    Quit,
}

/// The item while it stands in the menu bar. Dropping it removes it from the bar.
pub struct MenuBarItem {
    item: Retained<NSStatusItem>,
    _target: Retained<Target>,
}

impl MenuBarItem {
    /// Puts the item in the menu bar. A click shows the window, and a right click or a
    /// Control-click opens a menu that can also quit. macOS calls `on_command` on the main
    /// thread, outside any GPUI update.
    pub fn new(on_command: impl Fn(Command) + 'static) -> Result<Self, String> {
        let main_thread =
            MainThreadMarker::new().ok_or("The menu bar item must be made on the main thread")?;
        let image = image()?;

        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSSquareStatusItemLength);

        let button = item
            .button(main_thread)
            .ok_or("The menu bar item has no button")?;
        let menu = NSMenu::new(main_thread);
        let target = Target::new(
            main_thread,
            Ivars {
                on_command: Box::new(on_command),
                item: item.clone(),
                menu: menu.clone(),
            },
        );
        menu.addItem(&menu_item(
            main_thread,
            &target,
            "Open Flightdeck",
            sel!(show:),
            "",
        ));
        menu.addItem(&NSMenuItem::separatorItem(main_thread));
        menu.addItem(&menu_item(
            main_thread,
            &target,
            "Quit Flightdeck",
            sel!(quit:),
            "q",
        ));
        button.setImage(Some(&image));
        // The button holds its target without retaining it, so this item keeps it alive.
        unsafe {
            button.setTarget(Some(&target));
            button.setAction(Some(sel!(click:)));
        }
        button.sendActionOn(NSEventMask::LeftMouseUp | NSEventMask::RightMouseUp);
        Ok(Self {
            item,
            _target: target,
        })
    }
}

impl Drop for MenuBarItem {
    fn drop(&mut self) {
        NSStatusBar::systemStatusBar().removeStatusItem(&self.item);
    }
}

fn image() -> Result<Retained<NSImage>, String> {
    let name = NSString::from_str(SYMBOL);
    let description = NSString::from_str("Flightdeck");
    let image =
        NSImage::imageWithSystemSymbolName_accessibilityDescription(&name, Some(&description))
            .ok_or_else(|| format!("macOS has no {SYMBOL} symbol"))?;
    // A template image takes the menu bar's colour, light or dark.
    image.setTemplate(true);
    Ok(image)
}

fn menu_item(
    main_thread: MainThreadMarker,
    target: &Target,
    title: &str,
    action: Sel,
    key: &str,
) -> Retained<NSMenuItem> {
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(main_thread),
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(key),
        )
    };
    unsafe { item.setTarget(Some(target)) };
    item
}

struct Ivars {
    on_command: Box<dyn Fn(Command)>,
    item: Retained<NSStatusItem>,
    menu: Retained<NSMenu>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and Target does not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FlightdeckMenuBarTarget"]
    #[ivars = Ivars]
    struct Target;

    impl Target {
        #[unsafe(method(click:))]
        fn click(&self, _sender: Option<&AnyObject>) {
            let main_thread = self.mtm();
            let wants_menu = NSApplication::sharedApplication(main_thread)
                .currentEvent()
                .is_some_and(|event| {
                    if event.r#type() == NSEventType::RightMouseUp {
                        return true;
                    }
                    event.modifierFlags().contains(NSEventModifierFlags::Control)
                });
            if wants_menu {
                self.open_menu();
            } else {
                (self.ivars().on_command)(Command::Show);
            }
        }

        #[unsafe(method(show:))]
        fn show(&self, _sender: Option<&AnyObject>) {
            (self.ivars().on_command)(Command::Show);
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            (self.ivars().on_command)(Command::Quit);
        }
    }

    unsafe impl NSObjectProtocol for Target {}
);

impl Target {
    fn new(main_thread: MainThreadMarker, ivars: Ivars) -> Retained<Self> {
        let this = Self::alloc(main_thread).set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }

    /// Opens the menu under the item. An item that holds a menu opens it on a click instead
    /// of sending its action, so the menu is lent to the item for this one click.
    fn open_menu(&self) {
        let ivars = self.ivars();
        let Some(button) = ivars.item.button(self.mtm()) else {
            eprintln!("The menu bar item lost its button");
            return;
        };
        ivars.item.setMenu(Some(&ivars.menu));
        unsafe { button.performClick(None) };
        ivars.item.setMenu(None);
    }
}
