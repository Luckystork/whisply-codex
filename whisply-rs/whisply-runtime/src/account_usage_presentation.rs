//! One presentation of the account's Usage snapshot, shared by every terminal
//! surface that shows it.
//!
//! `/status`, `/usage`, `whisply usage`, and `whisply exec` all report the same
//! account at the same instant, so they must not each word it differently. This
//! module owns both halves of that: the broker read, including what to say when
//! it cannot be completed, and the text.

use std::fmt::Write as _;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::ContextualUsageSnapshot;
use crate::ContextualUsageWindow;
use crate::NativeBrokerClient;
use crate::UsageWindowCategory;
use crate::UsageWindowKind;

/// Shown when the broker cannot be reached or the read fails.
pub const ACCOUNT_USAGE_UNAVAILABLE: &str =
    "Whisply Usage is unavailable. Keep the Whisply app running and try again.";
/// Shown when the broker is reachable but no account is bound to it.
pub const ACCOUNT_USAGE_SIGNED_OUT: &str = "Sign in through the Whisply app to view Usage.";

/// Reads the account's Usage snapshot through the already authenticated broker.
///
/// The error is the sentence to show, not a code: every caller here is a
/// terminal surface with nothing else to say.
pub fn read_account_usage(broker: &NativeBrokerClient) -> Result<ContextualUsageSnapshot, String> {
    broker
        .hello()
        .map_err(|_| ACCOUNT_USAGE_UNAVAILABLE.to_string())?;
    let binding = broker
        .status()
        .map_err(|_| ACCOUNT_USAGE_UNAVAILABLE.to_string())?;
    if !binding.authenticated
        || binding
            .opaque_account_key
            .as_deref()
            .map(str::is_empty)
            .unwrap_or(true)
        || binding
            .account_epoch
            .as_deref()
            .map(str::is_empty)
            .unwrap_or(true)
    {
        return Err(ACCOUNT_USAGE_SIGNED_OUT.to_string());
    }
    broker
        .account_usage(&binding)
        .map_err(|_| ACCOUNT_USAGE_UNAVAILABLE.to_string())
}

/// Human-readable name for one metered window.
pub fn account_usage_window_name(category: UsageWindowCategory, window: UsageWindowKind) -> String {
    let category = match category {
        UsageWindowCategory::Usage => "Usage",
        UsageWindowCategory::Transcription => "Transcription",
    };
    let window = match window {
        UsageWindowKind::FiveHour => "five-hour",
        UsageWindowKind::Weekly => "weekly",
    };
    format!("{category} ({window})")
}

/// How much of a window is spoken for, as a fraction of its cap.
///
/// Derived from the amounts rather than read from the snapshot's own
/// `used_fraction`, because the Mac Usage tab derives it the same way: a
/// surface that trusted the field instead would show a different percentage
/// than the app for the very same instant. A window with no usable cap reads as
/// full, so an account whose accounting is broken looks spent rather than free.
pub fn account_usage_used_fraction(window: &ContextualUsageWindow) -> f64 {
    if !window.cap.is_finite() || window.cap <= 0.0 {
        return 1.0;
    }
    let accounted = window.settled.max(0.0) + window.reserved.max(0.0);
    if !accounted.is_finite() {
        return 1.0;
    }
    (accounted / window.cap).clamp(0.0, 1.0)
}

/// Renders the snapshot as the lines every terminal surface prints.
pub fn format_account_usage(snapshot: &ContextualUsageSnapshot) -> String {
    format_account_usage_at(snapshot, OffsetDateTime::now_utc())
}

fn format_account_usage_at(snapshot: &ContextualUsageSnapshot, now: OffsetDateTime) -> String {
    let mut output = format!("Whisply Usage ({})\n", snapshot.tier);
    for window in &snapshot.windows {
        let reset = window.resets_at.as_deref().unwrap_or("not scheduled");
        let _ = writeln!(
            output,
            "{}: {:.1}% used — settled {:.3}, reserved {:.3}, cap {:.3}; resets {reset}",
            account_usage_window_name(window.category, window.window),
            account_usage_used_fraction(window) * 100.0,
            window.settled,
            window.reserved,
            window.cap,
        );
    }
    for explanation in account_usage_advance_explanations(snapshot, now.unix_timestamp()) {
        let _ = writeln!(output, "{explanation}");
    }
    let _ = write!(
        output,
        "Rate card {} · {} {}",
        snapshot.metering.rate_card_version, snapshot.metering.basis, snapshot.metering.currency,
    );
    output
}

/// Explanatory text shared with `/status`. These rows never become quota bars.
pub fn account_usage_advance_explanations(
    snapshot: &ContextualUsageSnapshot,
    now_unix_seconds: i64,
) -> Vec<String> {
    let Ok(now) = OffsetDateTime::from_unix_timestamp(now_unix_seconds) else {
        return Vec::new();
    };
    let mut output = Vec::new();
    if snapshot.validate().is_ok() {
        for window in snapshot.advance_windows.as_deref().unwrap_or_default() {
            let Some(start) = window
                .starts_at
                .as_deref()
                .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
            else {
                continue;
            };
            let Some(end) = window
                .resets_at
                .as_deref()
                .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
            else {
                continue;
            };
            if end <= now {
                continue;
            }
            let period = if start > now { "next" } else { "current" };
            output.push(format!(
                "Exam Mode — {period} five-hour window (starts {}): {:.1}% used in advance; {:.1}% reserved in advance",
                window.starts_at.as_deref().unwrap_or_default(),
                window.settled / window.cap * 100.0, window.reserved / window.cap * 100.0));
        }
    }
    if !output.is_empty() {
        output.push(
            "These amounts are already counted toward their corresponding five-hour allowance."
                .to_string(),
        );
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;
    use sha2::Sha256;

    const FIXTURE: &str =
        include_str!("../../../../contracts/fixtures/whisply-usage-presentation-v1.json");

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct PresentationFixture {
        snapshot: ContextualUsageSnapshot,
        expected: Expected,
        invalid_cap_window: InvalidCapWindow,
    }

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Expected {
        windows: Vec<ExpectedWindow>,
    }

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ExpectedWindow {
        id: String,
        used_percent: f64,
        settled: f64,
        reserved: f64,
        cap: f64,
    }

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct InvalidCapWindow {
        category: UsageWindowCategory,
        window: UsageWindowKind,
        settled: f64,
        reserved: f64,
        cap: f64,
        used_fraction: f64,
        starts_at: Option<String>,
        resets_at: Option<String>,
        rate_card_version: Option<String>,
        expected_used_percent: f64,
    }

    impl InvalidCapWindow {
        fn window(&self) -> ContextualUsageWindow {
            ContextualUsageWindow {
                category: self.category,
                window: self.window,
                settled: self.settled,
                reserved: self.reserved,
                cap: self.cap,
                used_fraction: self.used_fraction,
                starts_at: self.starts_at.clone(),
                resets_at: self.resets_at.clone(),
                rate_card_version: self.rate_card_version.clone(),
            }
        }
    }

    fn fixture() -> PresentationFixture {
        let digest = Sha256::digest(FIXTURE.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(
            digest,
            include_str!("../../../../contracts/fixtures/whisply-usage-presentation-v1.sha256")
                .trim(),
            "the shared usage presentation fixture changed without its lock"
        );
        serde_json::from_str(FIXTURE).expect("usage presentation fixture")
    }

    fn window_id(window: &ContextualUsageWindow) -> String {
        let category = match window.category {
            UsageWindowCategory::Usage => "usage",
            UsageWindowCategory::Transcription => "transcription",
        };
        let kind = match window.window {
            UsageWindowKind::FiveHour => "five_hour",
            UsageWindowKind::Weekly => "weekly",
        };
        format!("{category}:{kind}")
    }

    #[test]
    fn the_terminal_presents_the_amounts_every_other_surface_presents() {
        let fixture = fixture();
        fixture
            .snapshot
            .validate()
            .expect("the shared fixture must be a valid account snapshot");

        for expected in &fixture.expected.windows {
            let window = fixture
                .snapshot
                .windows
                .iter()
                .find(|window| window_id(window) == expected.id)
                .unwrap_or_else(|| panic!("fixture is missing window {}", expected.id));
            assert_eq!(window.settled, expected.settled, "{}", expected.id);
            assert_eq!(window.reserved, expected.reserved, "{}", expected.id);
            assert_eq!(window.cap, expected.cap, "{}", expected.id);
            assert!(
                (account_usage_used_fraction(window) * 100.0 - expected.used_percent).abs() < 1e-9,
                "{} presented {} instead of {}",
                expected.id,
                account_usage_used_fraction(window) * 100.0,
                expected.used_percent
            );
        }
    }

    #[test]
    fn a_window_with_no_usable_cap_reads_as_spent_rather_than_free() {
        let fixture = fixture();

        assert!(
            (account_usage_used_fraction(&fixture.invalid_cap_window.window()) * 100.0
                - fixture.invalid_cap_window.expected_used_percent)
                .abs()
                < 1e-9
        );
    }

    #[test]
    fn the_snapshots_own_used_fraction_is_never_what_the_user_is_shown() {
        let fixture = fixture();
        let five_hour = fixture
            .snapshot
            .windows
            .iter()
            .find(|window| window_id(window) == "usage:five_hour")
            .expect("five-hour window");

        // The fixture deliberately carries a server fraction that disagrees
        // with the amounts, which is the case a surface reading the field
        // straight through would get wrong.
        assert_ne!(
            five_hour.used_fraction,
            account_usage_used_fraction(five_hour)
        );

        let text = format_account_usage(&fixture.snapshot);
        assert!(text.contains("47.5% used"), "{text}");
        assert!(!text.contains("90.0% used"), "{text}");
    }

    #[test]
    fn reserved_cost_is_always_shown_apart_from_settled_cost() {
        let fixture = fixture();

        let text = format_account_usage(&fixture.snapshot);

        assert!(
            text.contains("settled 4.250, reserved 0.500, cap 10.000"),
            "{text}"
        );
        assert!(
            text.contains("settled 61.000, reserved 2.000, cap 100.000"),
            "{text}"
        );
    }

    #[test]
    fn advance_explanations_never_add_to_the_three_usage_meters() {
        let value: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../contracts/fixtures/whisply-usage-advance-v1.json"
        ))
        .unwrap();
        let snapshot: ContextualUsageSnapshot =
            serde_json::from_value(value["snapshot"].clone()).unwrap();
        let now = OffsetDateTime::parse(&snapshot.generated_at, &Rfc3339).unwrap();
        let text = format_account_usage_at(&snapshot, now);
        assert!(text.contains("Usage (five-hour): 50.0% used"), "{text}");
        assert!(text.contains("Usage (weekly): 45.0% used"), "{text}");
        assert!(
            text.contains("10.0% used in advance; 5.0% reserved in advance"),
            "{text}"
        );
        assert!(
            text.contains("0.0% used in advance; 20.0% reserved in advance"),
            "{text}"
        );
        assert_eq!(text.matches("Usage (five-hour):").count(), 1);
        assert_eq!(text.matches("Exam Mode —").count(), 2);

        let rolled = account_usage_advance_explanations(
            &snapshot,
            OffsetDateTime::parse("2026-09-07T15:00:00Z", &Rfc3339)
                .unwrap()
                .unix_timestamp(),
        );
        assert_eq!(rolled.len(), 2); // One current explanation plus its note.
        assert!(rolled[0].contains("current five-hour window"));
        assert!(rolled[0].contains("20.0% reserved in advance"));
        let ended = account_usage_advance_explanations(
            &snapshot,
            OffsetDateTime::parse("2026-09-07T20:00:00Z", &Rfc3339)
                .unwrap()
                .unix_timestamp(),
        );
        assert!(ended.is_empty());
    }
}
