//! Shared macOS glass chrome for examples: inset traffic lights over a
//! transparent titlebar and a rounded, behind-window blur.
use objc2::{
    MainThreadMarker, msg_send,
    rc::Retained,
    runtime::{AnyClass, AnyObject},
    sel,
};
use objc2_app_kit::{
    NSApplication, NSAutoresizingMaskOptions, NSWindow, NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_foundation::{NSObjectProtocol, NSString};

/// `hiddenInset` chrome: transparent titlebar, hidden title, content under
/// the traffic lights, a compact unified toolbar to centre them in a 40pt
/// strip, and a blurred desktop behind the translucent window.
#[allow(dead_code)]
pub fn inset_titlebar(title: &str) -> bool {
    inset_titlebar_with(title, HUD_WINDOW)
}

/// `NSVisualEffectMaterialHUDWindow`: a dark, saturated HUD glass.
#[allow(dead_code)]
const HUD_WINDOW: isize = 13;
/// `NSVisualEffectMaterialUnderWindowBackground`: the colourless material
/// Zeron (gpui) uses behind its own window tint.
#[allow(dead_code)]
pub const UNDER_WINDOW_BACKGROUND: isize = 21;

/// `inset_titlebar` with a chosen backdrop material.
#[allow(dead_code)]
pub fn inset_titlebar_with(title: &str, material: isize) -> bool {
    let Some(window) = find(title) else {
        eprintln!("glass: native window {title:?} not found");
        return false;
    };
    style(&window, material);
    true
}

fn find(title: &str) -> Option<Retained<NSWindow>> {
    let mtm = MainThreadMarker::new().expect("window setup runs on the UI thread");
    NSApplication::sharedApplication(mtm)
        .windows()
        .into_iter()
        .find(|window| window.title().to_string() == title)
}

/// Tune the backdrop as gpui's `BlurredView` does on every layer update: no
/// desktop tint (`CAChameleonLayer` hidden), a 60 pt gaussian, and no
/// saturation boost. AppKit may rebuild the layers (on focus or appearance
/// changes), so callers reapply it now and then; it is a short layer walk.
// Only some examples use these.
#[allow(dead_code)]
pub fn plain_backdrop(title: &str) {
    let Some(window) = find(title) else {
        return;
    };
    let Some(frame) = window.contentView().and_then(|c| unsafe { c.superview() }) else {
        return;
    };
    let Some(class) = AnyClass::get(c"NSVisualEffectView") else {
        return;
    };
    for view in frame.subviews().iter() {
        if view.isKindOfClass(class) {
            unsafe {
                let layer: *mut AnyObject = msg_send![&*view, layer];
                if let Some(layer) = layer.as_ref() {
                    strip_layer(layer);
                }
            }
        }
    }
}

unsafe fn strip_layer(layer: &AnyObject) {
    unsafe {
        let none: *const AnyObject = std::ptr::null();
        let _: () = msg_send![layer, setBackgroundColor: none];
        let class_name: Retained<NSString> = msg_send![layer, className];
        if class_name.to_string() == "CAChameleonLayer" {
            let _: () = msg_send![layer, setHidden: true];
            return;
        }
        let filters: *mut AnyObject = msg_send![layer, filters];
        if let Some(filters) = filters.as_ref() {
            let count: usize = msg_send![filters, count];
            let mut kept: Vec<*mut AnyObject> = Vec::with_capacity(count);
            for i in 0..count {
                let filter: *mut AnyObject = msg_send![filters, objectAtIndex: i];
                let Some(filter_ref) = filter.as_ref() else {
                    continue;
                };
                let description: Retained<NSString> = msg_send![filter_ref, description];
                let description = description.to_string();
                if description.contains("Saturat") {
                    continue;
                }
                if description.contains("Blur") {
                    let number_class = AnyClass::get(c"NSNumber").expect("NSNumber");
                    let radius: Retained<AnyObject> =
                        msg_send![number_class, numberWithDouble: 60.0_f64];
                    let key = NSString::from_str("inputRadius");
                    let _: () = msg_send![filter_ref, setValue: &*radius, forKey: &*key];
                }
                kept.push(filter);
            }
            if let Some(array_class) = AnyClass::get(c"NSArray") {
                let array: Retained<AnyObject> = msg_send![
                    array_class,
                    arrayWithObjects: kept.as_ptr(),
                    count: kept.len()
                ];
                let _: () = msg_send![layer, setFilters: &*array];
            }
        }
        let sublayers: *mut AnyObject = msg_send![layer, sublayers];
        if let Some(sublayers) = sublayers.as_ref() {
            let count: usize = msg_send![sublayers, count];
            for i in 0..count {
                let sublayer: *mut AnyObject = msg_send![sublayers, objectAtIndex: i];
                if let Some(sublayer) = sublayer.as_ref() {
                    strip_layer(sublayer);
                }
            }
        }
    }
}

fn style(window: &NSWindow, material: isize) {
    window.setStyleMask(window.styleMask() | NSWindowStyleMask::FullSizeContentView);
    window.setTitlebarAppearsTransparent(true);
    window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    unsafe {
        // NSTitlebarSeparatorStyleNone
        let _: () = msg_send![window, setTitlebarSeparatorStyle: 1_isize];
        if let Some(class) = AnyClass::get(c"NSToolbar") {
            let id = NSString::from_str("zgui.glass.titlebar");
            let toolbar: Retained<AnyObject> =
                msg_send![msg_send![class, alloc], initWithIdentifier: &*id];
            let _: () = msg_send![&*toolbar, setShowsBaselineSeparator: false];
            let _: () = msg_send![window, setToolbar: &*toolbar];
            // NSWindowToolbarStyleUnifiedCompact
            let _: () = msg_send![window, setToolbarStyle: 4_isize];
        }
        frost(window, material);
    }
}

/// Put a behind-window `NSVisualEffectView` under the (transparent) zgui
/// view. Unlike the private CGS window blur, it can be clipped to the
/// window's rounded shape, so neither blur nor shadow is left square.
unsafe fn frost(window: &NSWindow, material: isize) {
    let Some(content) = window.contentView() else {
        return;
    };
    let Some(frame) = (unsafe { content.superview() }) else {
        return;
    };
    let Some(class) = AnyClass::get(c"NSVisualEffectView") else {
        return;
    };
    // The corner radius varies by macOS version and toolbar style.
    let radius: f64 = if window.class().responds_to(sel!(_cornerRadius)) {
        unsafe { msg_send![window, _cornerRadius] }
    } else {
        10.
    };
    unsafe {
        let effect: Retained<objc2_app_kit::NSView> =
            msg_send![msg_send![class, alloc], initWithFrame: frame.bounds()];
        effect.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        // Behind-window blending, always active.
        let _: () = msg_send![&*effect, setMaterial: material];
        let _: () = msg_send![&*effect, setBlendingMode: 0_isize];
        let _: () = msg_send![&*effect, setState: 1_isize];
        effect.setWantsLayer(true);
        let layer: *mut AnyObject = msg_send![&*effect, layer];
        if let Some(layer) = layer.as_ref() {
            let continuous = NSString::from_str("continuous");
            let _: () = msg_send![layer, setCornerRadius: radius];
            let _: () = msg_send![layer, setCornerCurve: &*continuous];
            let _: () = msg_send![layer, setMasksToBounds: true];
        }
        // NSWindowBelow
        let _: () = msg_send![
            &*frame,
            addSubview: &*effect,
            positioned: -1_isize,
            relativeTo: &*content
        ];
    }
    window.invalidateShadow();
}
