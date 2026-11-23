//! The benefit catalog: one hand-curated TOML file per card product, loaded
//! into `card_products` / `benefits` at startup. Fixing a credit description
//! or a value is a data update (edit the file, restart), not a release.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

pub const CADENCES: &[&str] = &["monthly", "quarterly", "semiannual", "annual", "one_time"];
pub const KINDS: &[&str] = &["credit", "checkoff", "mortgage_points"];
pub const UNITS: &[&str] = &["usd", "miles", "points"];
pub const CATEGORIES: &[&str] = &["travel", "dining", "shopping", "subscriptions", "other"];
pub const PROVIDERS: &[&str] = &["capital_one", "amex", "bilt"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tier {
    /// Non-housing spend as a fraction of the housing payment.
    pub min_spend_ratio: f64,
    /// Points earned per dollar of the housing payment at this tier.
    pub multiplier: f64,
}

/// Matching rules, stored as JSON on the benefit row.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Rules {
    /// Regexes (case-insensitive) against a credit's description: the
    /// statement credit the issuer posts for this benefit.
    pub credit_patterns: Vec<String>,
    /// Regexes (case-insensitive) against merchant + description of a purchase.
    pub merchants: Vec<String>,
    pub mccs: Vec<String>,
    /// Aggregator categories (Plaid personal_finance_category), exact match.
    pub categories: Vec<String>,
    pub min_amount_cents: i64,
    /// Month number ("1".."12") -> value for periods starting in that month.
    pub month_values: BTreeMap<String, i64>,
    /// For one_time benefits that come back, e.g. Global Entry every 4 years.
    pub renews_every_years: Option<i32>,
    // --- mortgage_points only ---
    pub payment_patterns: Vec<String>,
    pub tiers: Vec<Tier>,
    /// Flat points earned when spend is below the first tier.
    pub base_points: i64,
    pub cash_per_1000_points_cents: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileProduct {
    id: String,
    provider: String,
    issuer: String,
    name: String,
    annual_fee: f64,
    #[serde(default)]
    name_hints: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileBenefit {
    id: String,
    name: String,
    #[serde(default = "default_kind")]
    kind: String,
    #[serde(default = "default_unit")]
    unit: String,
    /// Dollars for usd benefits, whole units for miles / points.
    value: f64,
    cadence: String,
    #[serde(default = "default_basis")]
    reset_basis: String,
    #[serde(default)]
    enrollment_required: bool,
    #[serde(default = "default_category")]
    category: String,
    #[serde(default)]
    how_to_use: String,
    #[serde(default)]
    source_url: String,
    /// TOML dates, e.g. `effective_to = 2026-06-30`.
    effective_from: Option<toml::value::Datetime>,
    effective_to: Option<toml::value::Datetime>,
    #[serde(default)]
    credit_patterns: Vec<String>,
    #[serde(default)]
    merchants: Vec<String>,
    #[serde(default)]
    mccs: Vec<String>,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(default)]
    min_amount: f64,
    #[serde(default)]
    month_values: BTreeMap<String, f64>,
    renews_every_years: Option<i32>,
    #[serde(default)]
    payment_patterns: Vec<String>,
    #[serde(default)]
    tiers: Vec<Tier>,
    #[serde(default)]
    base_points: i64,
    cash_per_1000_points: Option<f64>,
}

fn default_kind() -> String {
    "credit".into()
}
fn default_unit() -> String {
    "usd".into()
}
fn default_basis() -> String {
    "calendar_year".into()
}
fn default_category() -> String {
    "other".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogFile {
    version: String,
    product: FileProduct,
    #[serde(default, rename = "benefit")]
    benefits: Vec<FileBenefit>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Product {
    pub id: String,
    pub provider: String,
    pub issuer: String,
    pub name: String,
    pub annual_fee_cents: i64,
    pub catalog_version: String,
    pub name_hints: Vec<String>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Benefit {
    pub id: String,
    pub product_id: String,
    pub name: String,
    pub kind: String,
    pub unit: String,
    pub value: i64,
    pub cadence: String,
    pub reset_basis: String,
    pub enrollment_required: bool,
    pub category: String,
    pub rules: sqlx::types::Json<Rules>,
    pub how_to_use: String,
    pub source_url: String,
    pub effective_from: Option<NaiveDate>,
    pub effective_to: Option<NaiveDate>,
}

pub const BENEFIT_COLUMNS: &str = "id, product_id, name, kind, unit, value, cadence, reset_basis, \
     enrollment_required, category, rules, how_to_use, source_url, effective_from, effective_to";

#[derive(Debug, Clone)]
pub struct Catalog {
    pub products: Vec<Product>,
    pub benefits: Vec<Benefit>,
}

pub fn cents(dollars: f64) -> i64 {
    (dollars * 100.0).round() as i64
}

fn check_regexes(what: &str, id: &str, patterns: &[String]) -> Result<()> {
    for p in patterns {
        RegexBuilder::new(p)
            .case_insensitive(true)
            .build()
            .with_context(|| format!("benefit {id}: bad {what} regex {p:?}"))?;
    }
    Ok(())
}

fn one_of(what: &str, id: &str, value: &str, allowed: &[&str]) -> Result<()> {
    if !allowed.contains(&value) {
        bail!("{id}: {what} must be one of {allowed:?}, got {value:?}");
    }
    Ok(())
}

pub fn parse_file(text: &str) -> Result<(Product, Vec<Benefit>)> {
    let file: CatalogFile = toml::from_str(text)?;
    let p = file.product;
    one_of("provider", &p.id, &p.provider, PROVIDERS)?;
    let product = Product {
        id: p.id.clone(),
        provider: p.provider,
        issuer: p.issuer,
        name: p.name,
        annual_fee_cents: cents(p.annual_fee),
        catalog_version: file.version,
        name_hints: p.name_hints.iter().map(|h| h.to_lowercase()).collect(),
    };

    let mut benefits = Vec::new();
    for b in file.benefits {
        let id = b.id.clone();
        one_of("kind", &id, &b.kind, KINDS)?;
        one_of("unit", &id, &b.unit, UNITS)?;
        one_of("cadence", &id, &b.cadence, CADENCES)?;
        one_of("reset_basis", &id, &b.reset_basis, &["calendar_year", "cardmember_year"])?;
        one_of("category", &id, &b.category, CATEGORIES)?;
        check_regexes("credit_patterns", &id, &b.credit_patterns)?;
        check_regexes("merchants", &id, &b.merchants)?;
        check_regexes("payment_patterns", &id, &b.payment_patterns)?;
        if !id.starts_with(&format!("{}-", product.id)) {
            bail!("benefit id {id} must start with its product id {}-", product.id);
        }
        if b.kind == "mortgage_points" && (b.cadence != "monthly" || b.tiers.is_empty()) {
            bail!("{id}: mortgage_points benefits must be monthly and define tiers");
        }
        if b.kind == "credit" && b.unit != "usd" {
            bail!("{id}: credit benefits must be in usd; use kind = \"checkoff\" for miles or points");
        }
        for m in b.month_values.keys() {
            match m.parse::<u32>() {
                Ok(1..=12) => {}
                _ => bail!("{id}: month_values keys must be \"1\"..\"12\", got {m:?}"),
            }
        }
        let date = |what: &str, d: Option<toml::value::Datetime>| -> Result<Option<NaiveDate>> {
            d.map(|d| d.to_string().parse::<NaiveDate>().with_context(|| format!("{id}: {what} must be a plain date (YYYY-MM-DD)")))
                .transpose()
        };
        let effective_from = date("effective_from", b.effective_from)?;
        let effective_to = date("effective_to", b.effective_to)?;
        let scale = |v: f64| if b.unit == "usd" { cents(v) } else { v.round() as i64 };
        let mut tiers = b.tiers;
        tiers.sort_by(|a, c| a.min_spend_ratio.total_cmp(&c.min_spend_ratio));
        benefits.push(Benefit {
            id,
            product_id: product.id.clone(),
            name: b.name,
            kind: b.kind,
            value: scale(b.value),
            cadence: b.cadence,
            reset_basis: b.reset_basis,
            enrollment_required: b.enrollment_required,
            category: b.category,
            rules: sqlx::types::Json(Rules {
                credit_patterns: b.credit_patterns,
                merchants: b.merchants,
                mccs: b.mccs,
                categories: b.categories,
                min_amount_cents: cents(b.min_amount),
                month_values: b.month_values.iter().map(|(k, v)| (k.clone(), scale(*v))).collect(),
                renews_every_years: b.renews_every_years,
                payment_patterns: b.payment_patterns,
                tiers,
                base_points: b.base_points,
                cash_per_1000_points_cents: b.cash_per_1000_points.map(cents),
            }),
            unit: b.unit,
            how_to_use: b.how_to_use.trim().to_string(),
            source_url: b.source_url,
            effective_from,
            effective_to,
        });
    }
    Ok((product, benefits))
}

pub fn load_dir(dir: &Path) -> Result<Catalog> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("reading catalog dir {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    paths.sort();
    if paths.is_empty() {
        bail!("no catalog files (*.toml) in {}", dir.display());
    }
    let mut catalog = Catalog { products: vec![], benefits: vec![] };
    let mut seen = HashSet::new();
    for path in paths {
        let text = std::fs::read_to_string(&path)?;
        let (product, benefits) = parse_file(&text).with_context(|| format!("catalog file {}", path.display()))?;
        if !seen.insert(product.id.clone()) {
            bail!("duplicate product id {}", product.id);
        }
        for b in &benefits {
            if !seen.insert(b.id.clone()) {
                bail!("duplicate benefit id {}", b.id);
            }
        }
        catalog.products.push(product);
        catalog.benefits.extend(benefits);
    }
    Ok(catalog)
}

/// Upserts the catalog. Entries that disappeared from the files are kept (old
/// periods still reference them) but marked inactive.
pub async fn sync_to_db(pool: &PgPool, catalog: &Catalog) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("update card_products set active = false").execute(&mut *tx).await?;
    sqlx::query("update benefits set active = false").execute(&mut *tx).await?;
    for p in &catalog.products {
        sqlx::query(
            "insert into card_products (id, provider, issuer, name, annual_fee_cents, catalog_version, name_hints, active)
             values ($1,$2,$3,$4,$5,$6,$7,true)
             on conflict (id) do update set provider = excluded.provider, issuer = excluded.issuer,
               name = excluded.name, annual_fee_cents = excluded.annual_fee_cents,
               catalog_version = excluded.catalog_version, name_hints = excluded.name_hints, active = true",
        )
        .bind(&p.id)
        .bind(&p.provider)
        .bind(&p.issuer)
        .bind(&p.name)
        .bind(p.annual_fee_cents)
        .bind(&p.catalog_version)
        .bind(&p.name_hints)
        .execute(&mut *tx)
        .await?;
    }
    for b in &catalog.benefits {
        sqlx::query(
            "insert into benefits (id, product_id, name, kind, unit, value, cadence, reset_basis,
               enrollment_required, category, rules, how_to_use, source_url, effective_from, effective_to, active)
             values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,true)
             on conflict (id) do update set product_id = excluded.product_id, name = excluded.name,
               kind = excluded.kind, unit = excluded.unit, value = excluded.value, cadence = excluded.cadence,
               reset_basis = excluded.reset_basis, enrollment_required = excluded.enrollment_required,
               category = excluded.category, rules = excluded.rules, how_to_use = excluded.how_to_use,
               source_url = excluded.source_url, effective_from = excluded.effective_from,
               effective_to = excluded.effective_to, active = true",
        )
        .bind(&b.id)
        .bind(&b.product_id)
        .bind(&b.name)
        .bind(&b.kind)
        .bind(&b.unit)
        .bind(b.value)
        .bind(&b.cadence)
        .bind(&b.reset_basis)
        .bind(b.enrollment_required)
        .bind(&b.category)
        .bind(&b.rules)
        .bind(&b.how_to_use)
        .bind(&b.source_url)
        .bind(b.effective_from)
        .bind(b.effective_to)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Best guess of the card product from an aggregator account name.
pub fn suggest_product<'a>(products: &'a [Product], provider: &str, account_name: &str) -> Option<&'a Product> {
    let name = account_name.to_lowercase();
    products
        .iter()
        .filter(|p| p.provider == provider)
        .filter_map(|p| {
            let best = p.name_hints.iter().filter(|h| name.contains(h.as_str())).map(|h| h.len()).max()?;
            Some((best, p))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, p)| p)
}
