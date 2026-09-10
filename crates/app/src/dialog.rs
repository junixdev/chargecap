//! AppKit alerts: the "Custom…" input and the error box.
//!
//! Both calls are modal and must run on the main thread. Every entry point
//! takes a [`MainThreadMarker`], which the event loop already holds.

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSTextField};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

/// Width and height of the "Custom…" text field, in points.
const FIELD_SIZE: NSSize = NSSize {
    width: 220.0,
    height: 24.0,
};

/// Asks for a custom limit and returns the raw text the user typed.
///
/// Returns `None` when the user cancels. The caller validates the text with
/// [`proto::validate_limits`].
pub fn ask_custom_limit(mtm: MainThreadMarker, current: u8) -> Option<String> {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str("Custom charge limit"));
    alert.setInformativeText(&NSString::from_str(&format!(
        "Enter a limit between {} and {} percent.",
        proto::MIN_UPPER,
        proto::MAX_UPPER
    )));
    alert.addButtonWithTitle(&NSString::from_str("Set"));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));

    let field = text_field(mtm, &current.to_string());
    alert.setAccessoryView(Some(&field));
    alert.window().setInitialFirstResponder(Some(&field));

    if alert.runModal() != NSAlertFirstButtonReturn {
        return None;
    }
    Some(field.stringValue().to_string())
}

/// Shows an error alert with one "OK" button.
pub fn error(mtm: MainThreadMarker, message: &str) {
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Warning);
    alert.setMessageText(&NSString::from_str("chargecap"));
    alert.setInformativeText(&NSString::from_str(message));
    alert.addButtonWithTitle(&NSString::from_str("OK"));
    alert.runModal();
}

fn text_field(mtm: MainThreadMarker, value: &str) -> Retained<NSTextField> {
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), FIELD_SIZE);
    let field = NSTextField::initWithFrame(NSTextField::alloc(mtm), frame);
    field.setStringValue(&NSString::from_str(value));
    field
}
