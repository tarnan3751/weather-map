//! Time arithmetic shared by the feeds.

use chrono::{DateTime, Duration, Utc};

/// Round down to a multiple of `step` since the Unix epoch (whole hours, six
/// hours and ten minutes all line up with UTC days).
pub fn floor(t: DateTime<Utc>, step: Duration) -> DateTime<Utc> {
    let s = step.num_seconds().max(1);
    let ts = t.timestamp();
    DateTime::from_timestamp(ts - ts.rem_euclid(s), 0).unwrap_or(t)
}

/// How long to sleep until `at`, clamped to `[min, max]`.
pub fn wait(
    now: DateTime<Utc>,
    at: DateTime<Utc>,
    min: Duration,
    max: Duration,
) -> std::time::Duration {
    (at - now).clamp(min, max).to_std().unwrap_or_default()
}

/// Sleep between cycles. On shutdown this never returns: the supervisor
/// sees the cancellation and aborts the task, rather than racing it with a
/// feed that "exited" and logging a failure.
pub async fn pause(ctx: &wx_core::FeedCtx, d: std::time::Duration) {
    if !ctx.sleep(d).await {
        std::future::pending::<()>().await;
    }
}

/// The UTC days `[from, to]` touches, oldest first.
pub fn days(from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<chrono::NaiveDate> {
    let mut out = Vec::new();
    let mut d = from.date_naive();
    while d <= to.date_naive() && out.len() < 16 {
        out.push(d);
        match d.succ_opt() {
            Some(n) => d = n,
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn flooring_and_waiting() {
        let t = Utc.with_ymd_and_hms(2026, 11, 6, 13, 47, 12).unwrap();
        assert_eq!(
            floor(t, Duration::hours(1)),
            Utc.with_ymd_and_hms(2026, 11, 6, 13, 0, 0).unwrap()
        );
        assert_eq!(
            floor(t, Duration::hours(6)),
            Utc.with_ymd_and_hms(2026, 11, 6, 12, 0, 0).unwrap()
        );
        assert_eq!(
            floor(t, Duration::minutes(10)),
            Utc.with_ymd_and_hms(2026, 11, 6, 13, 40, 0).unwrap()
        );
        let s = Duration::seconds;
        assert_eq!(wait(t, t + s(90), s(5), s(60)).as_secs(), 60);
        assert_eq!(wait(t, t - s(90), s(5), s(60)).as_secs(), 5);
        assert_eq!(wait(t, t + s(30), s(5), s(60)).as_secs(), 30);
        assert_eq!(days(t - Duration::hours(14), t).len(), 2);
        assert_eq!(days(t, t).len(), 1);
    }
}
