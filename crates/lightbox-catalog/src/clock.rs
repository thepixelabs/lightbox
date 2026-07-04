// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Timestamp helpers. The catalog stores RFC3339 **UTC** with a fixed six
//! subsecond digits so that lexicographic order == chronological order
//! (spec §4.3 note b) even across rows written in the same second.

use time::format_description::BorrowedFormatItem;
use time::macros::format_description;
use time::OffsetDateTime;

/// `2026-07-05T18:30:00.000000Z` — fixed width, RFC3339-parseable.
const RFC3339_MICROS: &[BorrowedFormatItem<'_>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:6]Z");

/// `2026-07-05-183000` — dated backup directory names (spec §4.4).
const BACKUP_STAMP: &[BorrowedFormatItem<'_>] =
    format_description!("[year]-[month]-[day]-[hour][minute][second]");

/// Current time as fixed-width RFC3339 UTC (what `added_at`/`created_at`/
/// `applied_at`/`started_at` store).
pub(crate) fn now_rfc3339_utc() -> String {
    OffsetDateTime::now_utc()
        .format(&RFC3339_MICROS)
        .expect("formatting a UTC timestamp with a const format cannot fail")
}

/// Current time as a `YYYY-MM-DD-HHMMSS` UTC backup-directory stamp.
pub(crate) fn now_backup_stamp_utc() -> String {
    OffsetDateTime::now_utc()
        .format(&BACKUP_STAMP)
        .expect("formatting a UTC timestamp with a const format cannot fail")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_is_fixed_width_and_utc() {
        let a = now_rfc3339_utc();
        assert_eq!(a.len(), "2026-07-05T18:30:00.000000Z".len());
        assert!(a.ends_with('Z'));
        // Fixed width is what makes lexicographic == chronological.
        let b = now_rfc3339_utc();
        assert_eq!(a.len(), b.len());
        assert!(a <= b);
    }

    #[test]
    fn backup_stamp_shape() {
        let s = now_backup_stamp_utc();
        assert_eq!(s.len(), "2026-07-05-183000".len());
        assert_eq!(s.as_bytes()[4], b'-');
        assert_eq!(s.as_bytes()[7], b'-');
        assert_eq!(s.as_bytes()[10], b'-');
    }
}
