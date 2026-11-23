//! Statement CSV import: the fallback when a card cannot be linked (notably
//! Bilt via Cardless). Issuers disagree on column names and on the sign of a
//! purchase, so headers are detected and the sign convention is inferred
//! unless the caller states it.

use anyhow::{anyhow, bail, Result};
use chrono::NaiveDate;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct CsvTxn {
    pub date: NaiveDate,
    /// Positive = purchase, negative = credit / refund.
    pub amount_cents: i64,
    pub merchant: String,
    pub description: String,
    pub mcc: Option<String>,
    pub category: Option<String>,
    /// Stable id for de-duplicating repeated imports of overlapping files.
    pub aggregator_id: String,
}

fn find(headers: &[String], names: &[&str]) -> Option<usize> {
    names.iter().find_map(|n| headers.iter().position(|h| h == n))
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    let s = s.trim();
    let s = s.split(['T', ' ']).next().unwrap_or(s);
    ["%Y-%m-%d", "%m/%d/%Y", "%m/%d/%y", "%m-%d-%Y", "%Y/%m/%d", "%d %b %Y", "%b %d, %Y"]
        .iter()
        .find_map(|f| NaiveDate::parse_from_str(s, f).ok())
        .or_else(|| NaiveDate::parse_from_str(s.trim(), "%b %d, %Y").ok())
}

fn parse_amount(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let negative = s.starts_with('-') || (s.starts_with('(') && s.ends_with(')')) || s.ends_with("CR");
    let digits: String = s.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
    let value: f64 = digits.parse().ok()?;
    let cents = (value * 100.0).round() as i64;
    Some(if negative { -cents } else { cents })
}

/// `purchases_negative`: Some(true) if the file shows purchases as negative
/// numbers, Some(false) if positive, None to infer (most rows on a card
/// statement are purchases).
pub fn parse(text: &str, purchases_negative: Option<bool>) -> Result<Vec<CsvTxn>> {
    let text = text.trim_start_matches('\u{feff}');
    let mut reader = csv::ReaderBuilder::new().flexible(true).trim(csv::Trim::All).from_reader(text.as_bytes());
    let headers: Vec<String> = reader.headers()?.iter().map(|h| h.trim().to_lowercase()).collect();

    let date_col = find(&headers, &["posted date", "post date", "posting date", "transaction date", "date", "trans. date", "trans date"])
        .ok_or_else(|| anyhow!("no date column found (looked for Date, Posted Date, Transaction Date)"))?;
    let desc_col = find(&headers, &["description", "merchant", "name", "payee", "transaction description", "details", "memo"])
        .ok_or_else(|| anyhow!("no description column found (looked for Description, Merchant, Name)"))?;
    let merchant_col = find(&headers, &["merchant", "merchant name", "appears on your statement as"]).filter(|&c| c != desc_col);
    let amount_col = find(&headers, &["amount", "transaction amount", "amount (usd)"]);
    let debit_col = find(&headers, &["debit", "debits", "charge", "charges"]);
    let credit_col = find(&headers, &["credit", "credits", "payment", "payments"]);
    let mcc_col = find(&headers, &["mcc", "merchant category code"]);
    let category_col = find(&headers, &["category", "type"]);
    if amount_col.is_none() && debit_col.is_none() {
        bail!("no amount column found (looked for Amount, or Debit and Credit)");
    }

    let mut rows = Vec::new();
    for (line, record) in reader.records().enumerate() {
        let record = record.map_err(|e| anyhow!("row {}: {e}", line + 2))?;
        let get = |i: usize| record.get(i).unwrap_or("").trim();
        if record.iter().all(|f| f.trim().is_empty()) {
            continue;
        }
        let date = parse_date(get(date_col)).ok_or_else(|| anyhow!("row {}: unrecognised date {:?}", line + 2, get(date_col)))?;
        // (amount, sign already known?)
        let (amount, signed) = match (amount_col.and_then(|c| parse_amount(get(c))), debit_col, credit_col) {
            (Some(a), _, _) => (a, false),
            (None, Some(d), c) => {
                let debit = parse_amount(get(d)).unwrap_or(0).abs();
                let credit = c.and_then(|c| parse_amount(get(c))).unwrap_or(0).abs();
                (debit - credit, true)
            }
            _ => bail!("row {}: no amount", line + 2),
        };
        rows.push((date, amount, signed, get(desc_col).to_string(), merchant_col.map(|c| get(c).to_string()).unwrap_or_default(),
                   mcc_col.map(|c| get(c).to_string()).filter(|s| !s.is_empty()), category_col.map(|c| get(c).to_string()).filter(|s| !s.is_empty())));
    }

    let unsigned: Vec<i64> = rows.iter().filter(|r| !r.2).map(|r| r.1).collect();
    let invert = purchases_negative.unwrap_or_else(|| {
        let negatives = unsigned.iter().filter(|a| **a < 0).count();
        negatives * 2 > unsigned.len()
    });

    let mut seen: HashMap<String, u32> = HashMap::new();
    Ok(rows
        .into_iter()
        .map(|(date, amount, signed, description, merchant, mcc, category)| {
            let amount_cents = if !signed && invert { -amount } else { amount };
            // Identical rows in one file are distinct purchases: number them.
            let base = format!("{date}|{amount_cents}|{}", description.to_lowercase());
            let n = seen.entry(base.clone()).or_insert(0);
            *n += 1;
            let hash = data_encoding::HEXLOWER.encode(&Sha256::digest(format!("{base}|{n}").as_bytes()));
            CsvTxn { date, amount_cents, merchant, description, mcc, category, aggregator_id: format!("csv:{}", &hash[..24]) }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amex_style_positive_purchases() {
        let csv = "Date,Description,Amount\n11/03/2026,RESY* LILIA,142.50\n11/05/2026,Platinum Resy Credit,-100.00\n11/06/2026,UBER EATS,23.10\n";
        let rows = parse(csv, None).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].amount_cents, 14250);
        assert_eq!(rows[1].amount_cents, -10000);
        assert_eq!(rows[0].date, NaiveDate::from_ymd_opt(2026, 11, 3).unwrap());
    }

    #[test]
    fn negative_purchases_are_inferred_and_flipped() {
        let csv = "Transaction Date,Name,Amount\n2026-11-03,WALGREENS,-12.00\n2026-11-04,LYFT,-18.40\n2026-11-09,REFUND LYFT,5.00\n";
        let rows = parse(csv, None).unwrap();
        assert_eq!(rows.iter().map(|r| r.amount_cents).collect::<Vec<_>>(), vec![1200, 1840, -500]);
        // Stated explicitly, inference is skipped.
        assert_eq!(parse(csv, Some(false)).unwrap()[0].amount_cents, -1200);
    }

    #[test]
    fn debit_credit_columns_and_duplicates() {
        let csv = "Posted Date,Description,Category,Debit,Credit\n2026-11-03,COFFEE,Dining,4.50,\n2026-11-03,COFFEE,Dining,4.50,\n2026-11-08,TRAVEL CREDIT,,,300.00\n";
        let rows = parse(csv, None).unwrap();
        assert_eq!(rows.iter().map(|r| r.amount_cents).collect::<Vec<_>>(), vec![450, 450, -30000]);
        assert_ne!(rows[0].aggregator_id, rows[1].aggregator_id);
        assert_eq!(parse(csv, None).unwrap()[0].aggregator_id, rows[0].aggregator_id);
    }

    #[test]
    fn missing_columns_are_reported() {
        assert!(parse("Foo,Bar\n1,2\n", None).unwrap_err().to_string().contains("no date column"));
    }
}
