//! Message language for engine output. English is the default; the app can
//! switch to Vietnamese at runtime.

use std::sync::atomic::{AtomicBool, Ordering};

static VIETNAMESE: AtomicBool = AtomicBool::new(false);

pub fn set_vietnamese(on: bool) {
    VIETNAMESE.store(on, Ordering::Relaxed);
}

pub fn vi() -> bool {
    VIETNAMESE.load(Ordering::Relaxed)
}

/// `l!("English {x}", "Tiếng Việt {x}", args...)`: format in the active
/// language. Both strings take the same arguments.
#[macro_export]
macro_rules! l {
    ($en:literal, $vi:literal $(, $arg:expr)* $(,)?) => {
        if $crate::l10n::vi() { format!($vi $(, $arg)*) } else { format!($en $(, $arg)*) }
    };
}
