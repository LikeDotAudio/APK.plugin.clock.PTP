// Part of the APK.audio project — http://APK.audio — made by Anthony Kuzub
// MIT Licence. Free, for everyone, for ever. Full text in LICENSE at the root.
//! PTPv2 / gPTP message parsing — IEEE 1588-2008 and IEEE 802.1AS.
//!
//! The parser is the shared AoIP reader `apkaudio_ravenna::ptp` (PLUGIN.RAVENNA,
//! taken with `default-features = false`): one implementation that this
//! monitor and PLUGIN.AES67's user-space slave both read PTP with, so a fix to
//! how a header or an Announce is read reaches both. This module keeps the
//! path every other module here (and the `ptp-monitor` bin) names it by.
//! PTPv1, which shares the wire but not the header layout, stays in [`crate::v1`].
pub use apkaudio_ravenna::ptp::*;
