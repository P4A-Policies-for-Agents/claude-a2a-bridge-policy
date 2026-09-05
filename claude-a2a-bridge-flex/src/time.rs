// Copyright 2026 Salesforce, Inc. All rights reserved.

//! Generic Unix ↔ ISO-8601 time helpers.

use chrono::{DateTime, SecondsFormat, Utc};

/// Current UTC time as Unix seconds.
pub fn now_unix_secs() -> u64 {
    Utc::now().timestamp().max(0) as u64
}

/// Unix seconds → ISO-8601 string (e.g. `"2026-07-06T11:39:00Z"`).
pub fn unix_secs_to_iso(secs: u64) -> Option<String> {
    DateTime::<Utc>::from_timestamp(secs as i64, 0)
        .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Secs, true))
}
