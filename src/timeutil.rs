use chrono::{DateTime, FixedOffset, SecondsFormat, Utc};

const UTC8_OFFSET_SECONDS: i32 = 8 * 60 * 60;

pub fn utc8_offset() -> FixedOffset {
    FixedOffset::east_opt(UTC8_OFFSET_SECONDS).expect("UTC+8 offset should be valid")
}

pub fn now_utc8_rfc3339_micros() -> String {
    Utc::now()
        .with_timezone(&utc8_offset())
        .to_rfc3339_opts(SecondsFormat::Micros, false)
}

pub fn format_utc8(dt: &DateTime<Utc>) -> String {
    dt.with_timezone(&utc8_offset())
        .to_rfc3339_opts(SecondsFormat::Secs, false)
}
