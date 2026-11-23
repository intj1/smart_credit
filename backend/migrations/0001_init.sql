-- Card Benefits Tracker schema.
-- Money is stored as integer minor units (cents). Benefits valued in miles or
-- points store whole units and carry unit = 'miles' | 'points'.
-- Transaction amounts follow the aggregator convention: positive = purchase,
-- negative = credit/refund.

create table users (
    id              uuid primary key,
    email           text not null unique,
    password_hash   text not null,
    totp_secret_enc text,
    totp_enabled    boolean not null default false,
    totp_last_step  bigint not null default 0,
    timezone        text not null default 'America/New_York',
    alert_email     text,
    alert_days      integer[] not null default '{7,2}',
    alerts_enabled  boolean not null default true,
    created_at      timestamptz not null default now()
);

create table sessions (
    token_hash   text primary key,
    user_id      uuid not null references users(id) on delete cascade,
    mfa_verified boolean not null default false,
    created_at   timestamptz not null default now(),
    expires_at   timestamptz not null
);

create table connections (
    id               uuid primary key,
    user_id          uuid not null references users(id) on delete cascade,
    provider         text not null,
    institution_name text,
    item_id          text not null unique,
    access_token_enc text not null,
    sync_cursor      text,
    status           text not null default 'ok',
    error            text,
    last_sync_at     timestamptz,
    created_at       timestamptz not null default now()
);

create table card_products (
    id               text primary key,
    provider         text not null,
    issuer           text not null,
    name             text not null,
    annual_fee_cents bigint not null default 0,
    catalog_version  text not null,
    name_hints       text[] not null default '{}',
    active           boolean not null default true
);

create table benefits (
    id                  text primary key,
    product_id          text not null references card_products(id),
    name                text not null,
    kind                text not null default 'credit',
    unit                text not null default 'usd',
    value               bigint not null,
    cadence             text not null,
    reset_basis         text not null default 'calendar_year',
    enrollment_required boolean not null default false,
    category            text not null default 'other',
    rules               jsonb not null default '{}',
    how_to_use          text not null default '',
    source_url          text not null default '',
    effective_from      date,
    effective_to        date,
    active              boolean not null default true
);

create table cards (
    id                    uuid primary key,
    user_id               uuid not null references users(id) on delete cascade,
    connection_id         uuid references connections(id) on delete cascade,
    product_id            text not null references card_products(id),
    last4                 text,
    nickname              text not null default '',
    anniversary_month     integer not null check (anniversary_month between 1 and 12),
    opened_date           date,
    account_ids           text[] not null default '{}',
    housing_payment_cents bigint,
    created_at            timestamptz not null default now()
);

-- Per-card state for a benefit that outlives a single period.
create table card_benefits (
    card_id    uuid not null references cards(id) on delete cascade,
    benefit_id text not null references benefits(id),
    enrolled   boolean not null default false,
    not_for_me boolean not null default false,
    primary key (card_id, benefit_id)
);

create table benefit_periods (
    id         uuid primary key,
    card_id    uuid not null references cards(id) on delete cascade,
    benefit_id text not null references benefits(id),
    start_date date not null,
    end_date   date not null,
    value      bigint not null,
    used       bigint not null default 0,
    pending    bigint not null default 0,
    status     text not null default 'not_started',
    flag       text,
    detail     jsonb,
    unique (card_id, benefit_id, start_date)
);
create index benefit_periods_open on benefit_periods (end_date, start_date);

create table transactions (
    id            uuid primary key,
    card_id       uuid not null references cards(id) on delete cascade,
    posted_date   date not null,
    amount_cents  bigint not null,
    merchant      text not null default '',
    description   text not null default '',
    mcc           text,
    category      text,
    pending       boolean not null default false,
    aggregator_id text not null,
    source        text not null default 'plaid',
    unique (card_id, aggregator_id)
);
create index transactions_card_date on transactions (card_id, posted_date);

create table benefit_matches (
    id             uuid primary key,
    period_id      uuid not null references benefit_periods(id) on delete cascade,
    transaction_id uuid references transactions(id) on delete cascade,
    amount         bigint not null,
    source         text not null,
    confidence     real not null default 1.0,
    note           text not null default '',
    created_at     timestamptz not null default now()
);
create index benefit_matches_period on benefit_matches (period_id);

-- "Not this one": a transaction the user said does not belong to a benefit.
create table match_exclusions (
    transaction_id uuid not null references transactions(id) on delete cascade,
    benefit_id     text not null references benefits(id),
    primary key (transaction_id, benefit_id)
);

create table offers (
    id                      uuid primary key,
    card_id                 uuid not null references cards(id) on delete cascade,
    merchant                text not null,
    reward                  text not null default '',
    reward_cents            bigint,
    min_spend_cents         bigint not null default 0,
    added_date              date not null,
    expires                 date not null,
    status                  text not null default 'active',
    redeemed_transaction_id uuid references transactions(id) on delete set null,
    created_at              timestamptz not null default now()
);

create table audit_log (
    id     bigserial primary key,
    at     timestamptz not null default now(),
    action text not null,
    detail jsonb not null default '{}'
);

create table alerts (
    id           uuid primary key,
    key          text not null unique,
    kind         text not null,
    title        text not null,
    body         text not null default '',
    created_at   timestamptz not null default now(),
    emailed_at   timestamptz,
    dismissed_at timestamptz
);
