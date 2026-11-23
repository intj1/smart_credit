//! Read models shared by the API and the alert job.

use chrono::NaiveDate;
use serde::Serialize;
use serde_json::Value;
use sqlx::{PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::periods::no_deadline;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct PeriodView {
    pub id: Uuid,
    pub card_id: Uuid,
    pub card_name: String,
    pub card_last4: Option<String>,
    pub provider: String,
    pub product_name: String,
    pub benefit_id: String,
    pub benefit_name: String,
    pub kind: String,
    pub unit: String,
    pub category: String,
    pub cadence: String,
    pub start_date: NaiveDate,
    /// None when the benefit has no deadline.
    #[sqlx(skip)]
    pub deadline: Option<NaiveDate>,
    #[serde(skip)]
    pub end_date: NaiveDate,
    #[sqlx(skip)]
    pub days_left: Option<i64>,
    pub value: i64,
    pub used: i64,
    pub pending: i64,
    #[sqlx(skip)]
    pub remaining: i64,
    pub status: String,
    pub flag: Option<String>,
    pub enrollment_required: bool,
    pub enrolled: bool,
    pub not_for_me: bool,
    pub manual: bool,
    pub how_to_use: String,
    pub source_url: String,
    pub detail: Option<Value>,
}

#[derive(Debug, Default, Clone)]
pub struct PeriodFilter {
    pub id: Option<Uuid>,
    pub card_id: Option<Uuid>,
    /// Only periods open on this date.
    pub open_on: Option<NaiveDate>,
    pub provider: Option<String>,
    pub cadence: Option<String>,
    pub category: Option<String>,
    pub flagged: bool,
    pub ended_after: Option<NaiveDate>,
}

pub async fn period_views(pool: &PgPool, f: &PeriodFilter, today: NaiveDate) -> Result<Vec<PeriodView>, sqlx::Error> {
    let mut q: QueryBuilder<Postgres> = QueryBuilder::new(
        "select p.id, p.card_id,
                case when c.nickname <> '' then c.nickname else cp.name end as card_name,
                c.last4 as card_last4, cp.provider, cp.name as product_name,
                b.id as benefit_id, b.name as benefit_name, b.kind, b.unit, b.category, b.cadence,
                p.start_date, p.end_date, p.value, p.used, p.pending, p.status, p.flag,
                b.enrollment_required, coalesce(cb.enrolled, false) as enrolled,
                coalesce(cb.not_for_me, false) as not_for_me,
                exists (select 1 from benefit_matches m where m.period_id = p.id and m.source = 'manual') as manual,
                b.how_to_use, b.source_url, p.detail
         from benefit_periods p
         join cards c on c.id = p.card_id
         join benefits b on b.id = p.benefit_id
         join card_products cp on cp.id = c.product_id
         left join card_benefits cb on cb.card_id = p.card_id and cb.benefit_id = p.benefit_id
         where true",
    );
    if let Some(id) = f.id {
        q.push(" and p.id = ").push_bind(id);
    }
    if let Some(card) = f.card_id {
        q.push(" and p.card_id = ").push_bind(card);
    }
    if let Some(d) = f.open_on {
        q.push(" and p.start_date <= ").push_bind(d).push(" and p.end_date >= ").push_bind(d);
    }
    if let Some(v) = &f.provider {
        q.push(" and cp.provider = ").push_bind(v.clone());
    }
    if let Some(v) = &f.cadence {
        q.push(" and b.cadence = ").push_bind(v.clone());
    }
    if let Some(v) = &f.category {
        q.push(" and b.category = ").push_bind(v.clone());
    }
    if f.flagged {
        q.push(" and p.flag is not null");
    }
    if let Some(d) = f.ended_after {
        q.push(" and p.end_date >= ").push_bind(d);
    }
    // Soonest deadline first, then the most money left.
    q.push(" order by p.end_date, (p.value - p.used) desc, b.name, p.id");
    let mut rows: Vec<PeriodView> = q.build_query_as().fetch_all(pool).await?;
    for r in &mut rows {
        r.remaining = (r.value - r.used).max(0);
        if r.end_date < no_deadline() {
            r.deadline = Some(r.end_date);
            r.days_left = Some((r.end_date - today).num_days());
        }
    }
    Ok(rows)
}

/// Whether a period belongs on the Unclaimed list.
pub fn is_unclaimed(p: &PeriodView) -> bool {
    p.kind != "mortgage_points" && p.remaining > 0 && !matches!(p.status.as_str(), "used" | "not_for_me")
}

pub fn fmt_amount(amount: i64, unit: &str) -> String {
    fn grouped(n: i64) -> String {
        let s = n.abs().to_string();
        let mut out = String::new();
        for (i, c) in s.chars().enumerate() {
            if i > 0 && (s.len() - i) % 3 == 0 {
                out.push(',');
            }
            out.push(c);
        }
        out
    }
    match unit {
        "usd" if amount % 100 == 0 => format!("${}", grouped(amount / 100)),
        "usd" => format!("${}.{:02}", grouped(amount / 100), amount.abs() % 100),
        other => format!("{} {other}", grouped(amount)),
    }
}

#[cfg(test)]
mod tests {
    use super::fmt_amount;

    #[test]
    fn amounts() {
        assert_eq!(fmt_amount(30000, "usd"), "$300");
        assert_eq!(fmt_amount(1295, "usd"), "$12.95");
        assert_eq!(fmt_amount(123456700, "usd"), "$1,234,567");
        assert_eq!(fmt_amount(10000, "miles"), "10,000 miles");
    }
}
