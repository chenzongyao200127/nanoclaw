use chrono::{DateTime, Utc};
use chrono_tz::Tz;

pub fn format_local_time(utc_iso: &str, timezone: &str) -> String {
    let parsed = match DateTime::parse_from_rfc3339(utc_iso) {
        Ok(value) => value.with_timezone(&Utc),
        Err(_) => return utc_iso.to_string(),
    };
    let tz = timezone.parse::<Tz>().unwrap_or(chrono_tz::UTC);
    let local = parsed.with_timezone(&tz);

    local.format("%b %-d, %Y, %-I:%M %p").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_utc_time() {
        let actual = format_local_time("2024-01-01T00:00:00.000Z", "UTC");
        assert_eq!(actual, "Jan 1, 2024, 12:00 AM");
    }

    #[test]
    fn formats_named_timezone() {
        let actual = format_local_time("2024-01-01T18:30:00.000Z", "America/New_York");
        assert_eq!(actual, "Jan 1, 2024, 1:30 PM");
    }
}
