//! Benefit periods: the window in which one instance of a benefit can be used.
//! All boundaries are calendar dates; "today" is resolved in the user's
//! timezone by the caller, which is what makes a period end at local midnight.

use chrono::{Datelike, Duration, NaiveDate};

use crate::catalog::Benefit;

/// Sentinel end date for benefits with no deadline.
pub fn no_deadline() -> NaiveDate {
    NaiveDate::from_ymd_opt(9999, 12, 31).unwrap()
}

fn ymd(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("valid date")
}

fn add_months(date: NaiveDate, months: u32) -> NaiveDate {
    date.checked_add_months(chrono::Months::new(months)).expect("date in range")
}

/// What a card contributes to period boundaries.
#[derive(Debug, Clone, Copy)]
pub struct CardDates {
    pub anniversary_month: u32,
    pub opened_date: Option<NaiveDate>,
    /// Day the card was added to the app; anchors one-time benefits when the
    /// opened date is unknown.
    pub added_date: NaiveDate,
}

/// The period of `benefit` that contains `date`, as inclusive (start, end).
pub fn period_containing(benefit: &Benefit, card: CardDates, date: NaiveDate) -> (NaiveDate, NaiveDate) {
    let (y, m) = (date.year(), date.month());
    let span = |start: NaiveDate, months: u32| (start, add_months(start, months) - Duration::days(1));
    match benefit.cadence.as_str() {
        "monthly" => span(ymd(y, m, 1), 1),
        "quarterly" => span(ymd(y, (m - 1) / 3 * 3 + 1, 1), 3),
        "semiannual" => span(ymd(y, if m <= 6 { 1 } else { 7 }, 1), 6),
        "annual" => {
            if benefit.reset_basis == "cardmember_year" {
                let am = card.anniversary_month.clamp(1, 12);
                let start_year = if m >= am { y } else { y - 1 };
                span(ymd(start_year, am, 1), 12)
            } else {
                span(ymd(y, 1, 1), 12)
            }
        }
        // one_time
        _ => {
            let anchor = card.opened_date.unwrap_or_else(|| ymd(card.added_date.year(), 1, 1));
            match benefit.rules.renews_every_years {
                Some(years) if years > 0 && date >= anchor => {
                    let months = years as u32 * 12;
                    let mut start = anchor;
                    while add_months(start, months) <= date {
                        start = add_months(start, months);
                    }
                    span(start, months)
                }
                _ => (anchor.min(date), no_deadline()),
            }
        }
    }
}

pub fn period_value(benefit: &Benefit, start: NaiveDate) -> i64 {
    benefit.rules.month_values.get(&start.month().to_string()).copied().unwrap_or(benefit.value)
}

/// All periods of `benefit` overlapping [from, to], honouring the benefit's
/// effective dates. Returns (start, end, value).
pub fn periods_between(
    benefit: &Benefit,
    card: CardDates,
    from: NaiveDate,
    to: NaiveDate,
) -> Vec<(NaiveDate, NaiveDate, i64)> {
    let mut out = Vec::new();
    let mut cursor = from;
    while cursor <= to {
        let (start, end) = period_containing(benefit, card, cursor);
        let live = benefit.effective_from.is_none_or(|f| end >= f) && benefit.effective_to.is_none_or(|t| start <= t);
        if live {
            out.push((start, end, period_value(benefit, start)));
        }
        if end >= no_deadline() {
            break;
        }
        cursor = end + Duration::days(1);
    }
    out
}

/// Window of periods the app keeps for a card: back to the start of last year
/// (so year-to-date value and late-posting credits have a home) and forward
/// one period past today.
pub fn generation_window(today: NaiveDate) -> (NaiveDate, NaiveDate) {
    (ymd(today.year() - 1, 1, 1), today)
}

#[cfg(test)]
pub(crate) fn test_benefit(id: &str, cadence: &str, value: i64) -> Benefit {
    Benefit {
        id: id.into(),
        product_id: "p".into(),
        name: id.into(),
        kind: "credit".into(),
        unit: "usd".into(),
        value,
        cadence: cadence.into(),
        reset_basis: "calendar_year".into(),
        enrollment_required: false,
        category: "other".into(),
        rules: sqlx::types::Json(Default::default()),
        how_to_use: String::new(),
        source_url: String::new(),
        effective_from: None,
        effective_to: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> CardDates {
        CardDates { anniversary_month: 9, opened_date: None, added_date: ymd(2026, 11, 23) }
    }

    #[test]
    fn calendar_cadences() {
        let d = ymd(2026, 11, 23);
        assert_eq!(period_containing(&test_benefit("m", "monthly", 1), card(), d), (ymd(2026, 11, 1), ymd(2026, 11, 30)));
        assert_eq!(period_containing(&test_benefit("q", "quarterly", 1), card(), d), (ymd(2026, 10, 1), ymd(2026, 12, 31)));
        assert_eq!(period_containing(&test_benefit("s", "semiannual", 1), card(), d), (ymd(2026, 7, 1), ymd(2026, 12, 31)));
        assert_eq!(period_containing(&test_benefit("a", "annual", 1), card(), d), (ymd(2026, 1, 1), ymd(2026, 12, 31)));
        assert_eq!(
            period_containing(&test_benefit("m", "monthly", 1), card(), ymd(2024, 2, 29)),
            (ymd(2024, 2, 1), ymd(2024, 2, 29))
        );
    }

    #[test]
    fn cardmember_year() {
        let mut b = test_benefit("a", "annual", 1);
        b.reset_basis = "cardmember_year".into();
        assert_eq!(period_containing(&b, card(), ymd(2026, 11, 23)), (ymd(2026, 9, 1), ymd(2027, 8, 31)));
        assert_eq!(period_containing(&b, card(), ymd(2026, 8, 31)), (ymd(2025, 9, 1), ymd(2026, 8, 31)));
    }

    #[test]
    fn one_time_and_renewing() {
        let b = test_benefit("o", "one_time", 1);
        assert_eq!(period_containing(&b, card(), ymd(2026, 11, 23)), (ymd(2026, 1, 1), no_deadline()));
        let mut r = test_benefit("r", "one_time", 1);
        r.rules.renews_every_years = Some(4);
        let c = CardDates { opened_date: Some(ymd(2021, 3, 15)), ..card() };
        assert_eq!(period_containing(&r, c, ymd(2026, 11, 23)), (ymd(2025, 3, 15), ymd(2029, 3, 14)));
    }

    #[test]
    fn month_overrides_and_effective_dates() {
        let mut b = test_benefit("u", "monthly", 1500);
        b.rules.month_values.insert("12".into(), 3500);
        b.effective_to = Some(ymd(2026, 12, 31));
        let ps = periods_between(&b, card(), ymd(2026, 11, 5), ymd(2027, 2, 1));
        assert_eq!(ps, vec![(ymd(2026, 11, 1), ymd(2026, 11, 30), 1500), (ymd(2026, 12, 1), ymd(2026, 12, 31), 3500)]);
    }
}
