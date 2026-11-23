//! The matching engine. A pure function from (catalog rules, a card's
//! transactions, the user's manual input) to the state of every benefit
//! period. It always recomputes a card from scratch, so new, changed and
//! removed transactions are all handled the same way and a re-run is
//! idempotent. Manual input is passed in and always wins.

use std::collections::{HashMap, HashSet};

use chrono::{Duration, NaiveDate};
use regex::{Regex, RegexBuilder};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::catalog::Benefit;

/// "2 statement cycles": how long a qualifying purchase may wait for its
/// credit before the period is flagged.
pub const PENDING_GRACE_DAYS: i64 = 60;
/// How long after a purchase a posted credit is still tied back to it.
const CREDIT_LINK_DAYS: i64 = 30;

#[derive(Debug, Clone)]
pub struct Txn {
    pub id: Uuid,
    pub date: NaiveDate,
    /// Positive = purchase, negative = credit / refund.
    pub amount: i64,
    pub merchant: String,
    pub description: String,
    pub mcc: Option<String>,
    pub category: Option<String>,
    pub pending: bool,
}

#[derive(Debug, Clone)]
pub struct PeriodIn {
    pub id: Uuid,
    pub benefit_id: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub value: i64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BenefitFlags {
    pub enrolled: bool,
    pub not_for_me: bool,
}

pub struct Input<'a> {
    pub today: NaiveDate,
    pub benefits: &'a [Benefit],
    pub periods: &'a [PeriodIn],
    pub txns: &'a [Txn],
    /// (transaction, benefit) pairs the user rejected ("not this one").
    pub exclusions: &'a HashSet<(Uuid, String)>,
    /// period -> amount the user marked as used.
    pub manual: &'a HashMap<Uuid, i64>,
    pub flags: &'a HashMap<String, BenefitFlags>,
    pub housing_payment_cents: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchOut {
    pub period_id: Uuid,
    pub transaction_id: Option<Uuid>,
    pub amount: i64,
    pub source: &'static str,
    pub confidence: f32,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PeriodOut {
    pub id: Uuid,
    pub value: i64,
    pub used: i64,
    pub pending: i64,
    pub status: &'static str,
    pub flag: Option<&'static str>,
    pub detail: Option<Value>,
}

#[derive(Debug, Default)]
pub struct Output {
    pub matches: Vec<MatchOut>,
    pub periods: Vec<PeriodOut>,
}

pub fn status_for(value: i64, used: i64, pending: i64, enrollment_required: bool, flags: BenefitFlags) -> &'static str {
    if flags.not_for_me {
        "not_for_me"
    } else if value > 0 && used >= value {
        "used"
    } else if enrollment_required && !flags.enrolled && used == 0 {
        "needs_enrollment"
    } else if pending > 0 {
        "pending_credit"
    } else if used > 0 {
        "partly_used"
    } else {
        "not_started"
    }
}

fn compile(patterns: &[String]) -> Vec<Regex> {
    // Patterns are validated when the catalog loads; a bad one is skipped.
    patterns.iter().filter_map(|p| RegexBuilder::new(p).case_insensitive(true).build().ok()).collect()
}

fn any_match(res: &[Regex], text: &str) -> bool {
    res.iter().any(|r| r.is_match(text))
}

fn looks_like_card_payment(t: &Txn) -> bool {
    let text = format!("{} {}", t.description, t.category.as_deref().unwrap_or("")).to_lowercase();
    text.contains("payment") || text.contains("autopay") || text.contains("thank you")
}

struct Purchase<'a> {
    txn: &'a Txn,
    amount: i64,
    resolved: bool,
}

fn find_period<'a>(periods: &[&'a PeriodIn], date: NaiveDate) -> Option<&'a PeriodIn> {
    periods.iter().copied().find(|p| p.start <= date && date <= p.end)
}

pub fn run(input: &Input) -> Output {
    let mut out = Output::default();
    let mut txns: Vec<&Txn> = input.txns.iter().collect();
    txns.sort_by_key(|t| (t.date, t.id));

    for benefit in input.benefits {
        let periods: Vec<&PeriodIn> = input.periods.iter().filter(|p| p.benefit_id == benefit.id).collect();
        if periods.is_empty() {
            continue;
        }
        let flags = input.flags.get(&benefit.id).copied().unwrap_or_default();
        let mut states: HashMap<Uuid, PeriodOut> = periods
            .iter()
            .map(|p| {
                (p.id, PeriodOut { id: p.id, value: p.value, used: 0, pending: 0, status: "not_started", flag: None, detail: None })
            })
            .collect();

        match benefit.kind.as_str() {
            "credit" => match_credits(input, benefit, &txns, &periods, &mut states, &mut out.matches),
            "mortgage_points" => mortgage_points(input, benefit, &txns, &periods, &mut states),
            _ => {} // checkoff: manual only
        }

        for p in &periods {
            let s = states.get_mut(&p.id).expect("state exists");
            if let Some(&manual) = input.manual.get(&p.id) {
                // Manual input always wins over anything the transactions say.
                s.used = if s.value > 0 { manual.clamp(0, s.value) } else { manual.max(0) };
                s.pending = 0;
                s.flag = None;
            }
            s.status = status_for(s.value, s.used, s.pending, benefit.enrollment_required, flags);
        }
        let mut list: Vec<PeriodOut> = states.into_values().collect();
        list.sort_by_key(|s| s.id);
        out.periods.extend(list);
    }
    out
}

fn match_credits(
    input: &Input,
    benefit: &Benefit,
    txns: &[&Txn],
    periods: &[&PeriodIn],
    states: &mut HashMap<Uuid, PeriodOut>,
    matches: &mut Vec<MatchOut>,
) {
    let rules = &benefit.rules.0;
    let credit_re = compile(&rules.credit_patterns);
    let merchant_re = compile(&rules.merchants);

    let mut purchases: Vec<Purchase> = Vec::new();
    let mut credits: Vec<&Txn> = Vec::new();
    let mut reversals: Vec<&Txn> = Vec::new();
    let mut refunds: Vec<&Txn> = Vec::new();

    for &t in txns {
        if input.exclusions.contains(&(t.id, benefit.id.clone())) {
            continue;
        }
        let text = format!("{} {}", t.merchant, t.description);
        if any_match(&credit_re, &text) {
            // 1. Credit postings: the strongest signal.
            if t.amount < 0 && !t.pending {
                credits.push(t);
            } else if t.amount > 0 {
                reversals.push(t);
            }
            continue;
        }
        let qualifies = any_match(&merchant_re, &text)
            || t.mcc.as_ref().is_some_and(|m| rules.mccs.contains(m))
            || t.category.as_ref().is_some_and(|c| rules.categories.contains(c));
        if !qualifies {
            continue;
        }
        if t.amount > 0 {
            if t.amount >= rules.min_amount_cents {
                purchases.push(Purchase { txn: t, amount: t.amount, resolved: false });
            }
        } else if t.amount < 0 {
            refunds.push(t);
        }
    }

    // 4a. Refunds hand the amount back: shrink the most recent open purchase.
    for r in refunds {
        let mut left = -r.amount;
        for p in purchases.iter_mut().rev().filter(|p| p.txn.date <= r.date) {
            if left == 0 {
                break;
            }
            let take = left.min(p.amount);
            p.amount -= take;
            left -= take;
        }
    }

    let mut credited: HashMap<Uuid, i64> = HashMap::new();

    for c in credits {
        let amount = -c.amount;
        // Tie the credit to the purchase that earned it, so a credit posting
        // just after a period rolls over still lands in the right period.
        let linked = purchases
            .iter_mut()
            .filter(|p| !p.resolved && p.amount > 0 && p.txn.date <= c.date && c.date - p.txn.date <= Duration::days(CREDIT_LINK_DAYS))
            .min_by_key(|p| ((p.amount.min(benefit.value) - amount).abs(), p.txn.date));
        let (period, confidence, note) = match linked {
            Some(p) => {
                p.resolved = true;
                let period = find_period(periods, p.txn.date);
                if let Some(period) = period {
                    matches.push(MatchOut {
                        period_id: period.id,
                        transaction_id: Some(p.txn.id),
                        amount: 0,
                        source: "purchase",
                        confidence: 0.9,
                        note: "Qualifying purchase; its credit has posted".into(),
                    });
                }
                (period, 0.99, "Statement credit matched to a qualifying purchase")
            }
            None => (find_period(periods, c.date), 0.9, "Statement credit matched by description"),
        };
        let Some(period) = period else { continue };
        *credited.entry(period.id).or_default() += amount;
        matches.push(MatchOut {
            period_id: period.id,
            transaction_id: Some(c.id),
            amount,
            source: "credit",
            confidence,
            note: note.into(),
        });
    }

    // 4b. Reversed credits take the amount back out.
    for r in reversals {
        let Some(period) = find_period(periods, r.date) else { continue };
        *credited.entry(period.id).or_default() -= r.amount;
        matches.push(MatchOut {
            period_id: period.id,
            transaction_id: Some(r.id),
            amount: -r.amount,
            source: "credit",
            confidence: 0.9,
            note: "Credit reversed by the issuer".into(),
        });
    }

    for p in periods {
        let s = states.get_mut(&p.id).expect("state exists");
        s.used = credited.get(&p.id).copied().unwrap_or(0).clamp(0, p.value);
    }

    // 2. Qualifying purchases still waiting for their credit -> pending,
    // 3. unless they have waited longer than two statement cycles.
    for purchase in purchases.iter().filter(|p| !p.resolved && p.amount > 0) {
        let Some(period) = find_period(periods, purchase.txn.date) else { continue };
        let s = states.get_mut(&period.id).expect("state exists");
        let room = (s.value - s.used - s.pending).max(0);
        if room == 0 {
            continue;
        }
        if input.today - purchase.txn.date > Duration::days(PENDING_GRACE_DAYS) {
            s.flag = Some("check_enrollment");
            matches.push(MatchOut {
                period_id: period.id,
                transaction_id: Some(purchase.txn.id),
                amount: 0,
                source: "purchase",
                confidence: 0.5,
                note: "Qualifying purchase, but no credit posted after 2 statement cycles".into(),
            });
        } else {
            let counted = purchase.amount.min(room);
            s.pending += counted;
            matches.push(MatchOut {
                period_id: period.id,
                transaction_id: Some(purchase.txn.id),
                amount: counted,
                source: "purchase",
                confidence: if purchase.txn.pending { 0.5 } else { 0.7 },
                note: "Qualifying purchase; waiting for the statement credit".into(),
            });
        }
    }
}

/// Bilt-style housing points: how much of this month's mortgage payment has
/// earned points through everyday spend, and what unlocks the rest.
fn mortgage_points(
    input: &Input,
    benefit: &Benefit,
    txns: &[&Txn],
    periods: &[&PeriodIn],
    states: &mut HashMap<Uuid, PeriodOut>,
) {
    let rules = &benefit.rules.0;
    let payment_re = compile(&rules.payment_patterns);
    let Some(max_multiplier) = rules.tiers.last().map(|t| t.multiplier) else { return };

    for p in periods.iter().filter(|p| p.start <= input.today) {
        let mut detected: Option<i64> = None;
        let mut spend = 0i64;
        for t in txns.iter().filter(|t| t.date >= p.start && t.date <= p.end) {
            let text = format!("{} {}", t.merchant, t.description);
            if any_match(&payment_re, &text) {
                if t.amount > 0 {
                    detected = Some(detected.unwrap_or(0).max(t.amount));
                }
            } else if t.amount > 0 || !looks_like_card_payment(t) {
                spend += t.amount;
            }
        }
        let spend = spend.max(0);
        let s = states.get_mut(&p.id).expect("state exists");
        let Some(payment) = detected.or(input.housing_payment_cents).filter(|&c| c > 0) else {
            s.detail = Some(json!({ "payment_cents": null, "spend_cents": spend }));
            continue;
        };
        let ratio = spend as f64 / payment as f64;
        let tier = rules.tiers.iter().rev().find(|t| ratio + 1e-9 >= t.min_spend_ratio);
        let points = |mult: f64| (payment as f64 / 100.0 * mult).floor() as i64;
        let earned = tier.map(|t| points(t.multiplier)).unwrap_or(rules.base_points);
        let next = rules.tiers.iter().find(|t| ratio + 1e-9 < t.min_spend_ratio).map(|t| {
            json!({
                "multiplier": t.multiplier,
                "points": points(t.multiplier),
                "spend_needed_cents": ((t.min_spend_ratio * payment as f64).ceil() as i64 - spend).max(0),
            })
        });
        s.value = points(max_multiplier);
        s.used = earned.min(s.value);
        s.detail = Some(json!({
            "payment_cents": payment,
            "payment_source": if detected.is_some() { "transaction" } else { "card_setting" },
            "spend_cents": spend,
            "multiplier": tier.map(|t| t.multiplier).unwrap_or(0.0),
            "earned_points": earned,
            "max_points": s.value,
            "next_tier": next,
            "bilt_cash_for_1x_cents": rules.cash_per_1000_points_cents.map(|c| payment * c / 100_000),
        }));
    }
}

/// A hand-entered merchant offer is redeemed by the first purchase at that
/// merchant, of at least the minimum spend, between the day it was added and
/// its expiry.
pub fn offer_redeemed_by(
    merchant: &str,
    min_spend_cents: i64,
    added: NaiveDate,
    expires: NaiveDate,
    txns: &[Txn],
) -> Option<Uuid> {
    let needle = normalize(merchant);
    if needle.is_empty() {
        return None;
    }
    txns.iter()
        .filter(|t| t.amount > 0 && t.amount >= min_spend_cents && t.date >= added && t.date <= expires)
        .filter(|t| normalize(&format!("{} {}", t.merchant, t.description)).contains(&needle))
        .min_by_key(|t| (t.date, t.id))
        .map(|t| t.id)
}

fn normalize(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Tier;
    use crate::periods::test_benefit;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn txn(date: NaiveDate, amount: i64, desc: &str) -> Txn {
        Txn { id: Uuid::new_v4(), date, amount, merchant: String::new(), description: desc.into(), mcc: None, category: None, pending: false }
    }

    fn resy() -> Benefit {
        let mut b = test_benefit("p-resy", "quarterly", 10_000);
        b.rules.credit_patterns = vec!["resy.*credit".into()];
        b.rules.merchants = vec![r"\bresy\b".into()];
        b
    }

    fn quarter(b: &Benefit, start: NaiveDate, end: NaiveDate) -> PeriodIn {
        PeriodIn { id: Uuid::new_v4(), benefit_id: b.id.clone(), start, end, value: b.value }
    }

    struct Case {
        today: NaiveDate,
        benefits: Vec<Benefit>,
        periods: Vec<PeriodIn>,
        txns: Vec<Txn>,
        exclusions: HashSet<(Uuid, String)>,
        manual: HashMap<Uuid, i64>,
        flags: HashMap<String, BenefitFlags>,
        housing: Option<i64>,
    }

    impl Case {
        fn new(today: NaiveDate, benefit: Benefit, periods: Vec<PeriodIn>, txns: Vec<Txn>) -> Self {
            Case { today, benefits: vec![benefit], periods, txns, exclusions: Default::default(), manual: Default::default(), flags: Default::default(), housing: None }
        }
        fn run(&self) -> Output {
            run(&Input {
                today: self.today,
                benefits: &self.benefits,
                periods: &self.periods,
                txns: &self.txns,
                exclusions: &self.exclusions,
                manual: &self.manual,
                flags: &self.flags,
                housing_payment_cents: self.housing,
            })
        }
        fn state(&self, period: usize) -> PeriodOut {
            let id = self.periods[period].id;
            self.run().periods.into_iter().find(|p| p.id == id).unwrap()
        }
    }

    #[test]
    fn partial_credit_is_partly_used() {
        let b = resy();
        let q4 = quarter(&b, d(2026, 10, 1), d(2026, 12, 31));
        let case = Case::new(d(2026, 11, 23), b, vec![q4], vec![txn(d(2026, 11, 3), -6_000, "Platinum Resy Credit")]);
        let s = case.state(0);
        assert_eq!((s.used, s.pending, s.status), (6_000, 0, "partly_used"));
    }

    #[test]
    fn credit_posting_after_rollover_lands_in_purchase_period() {
        let b = resy();
        let q3 = quarter(&b, d(2026, 7, 1), d(2026, 9, 30));
        let q4 = quarter(&b, d(2026, 10, 1), d(2026, 12, 31));
        let case = Case::new(
            d(2026, 11, 23),
            b,
            vec![q3, q4],
            vec![txn(d(2026, 9, 29), 14_250, "RESY* LILIA BROOKLYN"), txn(d(2026, 10, 2), -10_000, "Platinum Resy Credit")],
        );
        assert_eq!(case.state(0).status, "used");
        assert_eq!(case.state(1).status, "not_started");
        let out = case.run();
        assert!(out.matches.iter().any(|m| m.source == "credit" && m.amount == 10_000 && m.confidence > 0.95));
    }

    #[test]
    fn purchase_is_pending_then_flagged_after_two_cycles() {
        let b = resy();
        let q4 = quarter(&b, d(2026, 10, 1), d(2026, 12, 31));
        let mut case = Case::new(d(2026, 10, 20), b, vec![q4], vec![txn(d(2026, 10, 5), 25_000, "RESY* VIA CAROTA")]);
        let s = case.state(0);
        assert_eq!((s.used, s.pending, s.status, s.flag), (0, 10_000, "pending_credit", None));

        case.today = d(2026, 12, 20);
        let s = case.state(0);
        assert_eq!((s.pending, s.status, s.flag), (0, "not_started", Some("check_enrollment")));
    }

    #[test]
    fn refund_and_reversal_give_the_amount_back() {
        let b = resy();
        let q4 = quarter(&b, d(2026, 10, 1), d(2026, 12, 31));
        let case = Case::new(
            d(2026, 11, 23),
            b.clone(),
            vec![q4.clone()],
            vec![txn(d(2026, 11, 1), 8_000, "RESY* DINNER"), txn(d(2026, 11, 4), -8_000, "RESY* DINNER REFUND")],
        );
        assert_eq!((case.state(0).pending, case.state(0).status), (0, "not_started"));

        let case = Case::new(
            d(2026, 11, 23),
            b,
            vec![q4],
            vec![
                txn(d(2026, 11, 1), 8_000, "RESY* DINNER"),
                txn(d(2026, 11, 3), -8_000, "Platinum Resy Credit"),
                txn(d(2026, 11, 9), 8_000, "Platinum Resy Credit Reversal"),
            ],
        );
        assert_eq!((case.state(0).used, case.state(0).status), (0, "not_started"));
    }

    #[test]
    fn manual_input_wins_and_exclusion_removes_a_match() {
        let b = resy();
        let q4 = quarter(&b, d(2026, 10, 1), d(2026, 12, 31));
        let credit = txn(d(2026, 11, 3), -6_000, "Platinum Resy Credit");
        let mut case = Case::new(d(2026, 11, 23), b.clone(), vec![q4.clone()], vec![credit.clone()]);
        case.manual.insert(q4.id, 10_000);
        assert_eq!((case.state(0).used, case.state(0).status), (10_000, "used"));

        case.manual.clear();
        case.exclusions.insert((credit.id, b.id.clone()));
        assert_eq!((case.state(0).used, case.state(0).status), (0, "not_started"));
    }

    #[test]
    fn enrollment_and_not_for_me() {
        let mut b = resy();
        b.enrollment_required = true;
        let q4 = quarter(&b, d(2026, 10, 1), d(2026, 12, 31));
        let mut case = Case::new(d(2026, 11, 23), b.clone(), vec![q4], vec![]);
        assert_eq!(case.state(0).status, "needs_enrollment");
        case.flags.insert(b.id.clone(), BenefitFlags { enrolled: true, not_for_me: false });
        assert_eq!(case.state(0).status, "not_started");
        case.flags.insert(b.id.clone(), BenefitFlags { enrolled: true, not_for_me: true });
        assert_eq!(case.state(0).status, "not_for_me");
    }

    #[test]
    fn checkoff_ignores_transactions() {
        let mut b = test_benefit("p-uber", "monthly", 1_500);
        b.kind = "checkoff".into();
        let nov = PeriodIn { id: Uuid::new_v4(), benefit_id: b.id.clone(), start: d(2026, 11, 1), end: d(2026, 11, 30), value: 1_500 };
        let mut case = Case::new(d(2026, 11, 23), b, vec![nov.clone()], vec![txn(d(2026, 11, 2), 2_300, "UBER TRIP")]);
        assert_eq!(case.state(0).status, "not_started");
        case.manual.insert(nov.id, 1_500);
        assert_eq!(case.state(0).status, "used");
    }

    #[test]
    fn mortgage_tiers() {
        let mut b = test_benefit("p-mortgage", "monthly", 0);
        b.kind = "mortgage_points".into();
        b.unit = "points".into();
        b.rules.payment_patterns = vec!["mortgage".into()];
        b.rules.base_points = 250;
        b.rules.cash_per_1000_points_cents = Some(3_000);
        b.rules.tiers = vec![
            Tier { min_spend_ratio: 0.25, multiplier: 0.5 },
            Tier { min_spend_ratio: 0.5, multiplier: 0.75 },
            Tier { min_spend_ratio: 0.75, multiplier: 1.0 },
            Tier { min_spend_ratio: 1.0, multiplier: 1.25 },
        ];
        let nov = PeriodIn { id: Uuid::new_v4(), benefit_id: b.id.clone(), start: d(2026, 11, 1), end: d(2026, 11, 30), value: 0 };
        let mut case = Case::new(
            d(2026, 11, 23),
            b,
            vec![nov],
            vec![
                txn(d(2026, 11, 2), 60_000, "WHOLE FOODS"),
                txn(d(2026, 11, 5), -200_000, "PAYMENT THANK YOU"),
                txn(d(2026, 11, 9), -5_000, "WHOLE FOODS REFUND"),
            ],
        );
        // No payment known yet.
        assert_eq!(case.state(0).value, 0);

        // $2,000 payment, $550 spend = 27.5% -> 0.5x = 1,000 of a possible 2,500.
        case.housing = Some(200_000);
        let s = case.state(0);
        assert_eq!((s.value, s.used, s.status), (2_500, 1_000, "partly_used"));
        let detail = s.detail.unwrap();
        assert_eq!(detail["spend_cents"], 55_000);
        assert_eq!(detail["next_tier"]["spend_needed_cents"], 45_000);
        assert_eq!(detail["bilt_cash_for_1x_cents"], 6_000);

        // A payment seen on the card overrides the setting.
        case.txns.push(txn(d(2026, 11, 1), 100_000, "BILT MORTGAGE PAYMENT"));
        let s = case.state(0);
        assert_eq!((s.value, s.used), (1_250, 750)); // 55% -> 0.75x of $1,000
    }

    #[test]
    fn offers() {
        let txns = vec![txn(d(2026, 11, 10), 4_000, "SQ *BLUE BOTTLE COFFEE"), txn(d(2026, 11, 12), 6_000, "BLUE BOTTLE COFFEE #12")];
        assert_eq!(offer_redeemed_by("Blue Bottle", 5_000, d(2026, 11, 1), d(2026, 12, 31), &txns), Some(txns[1].id));
        assert_eq!(offer_redeemed_by("Blue Bottle", 5_000, d(2026, 11, 1), d(2026, 11, 11), &txns), None);
    }
}
