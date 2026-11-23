# Card Benefits Tracker

A personal web app that tracks the credits and offers on your Capital One, Amex and Bilt cards: it pulls transactions through Plaid (or statement CSVs), works out which benefits you have used, and lists everything still unclaimed with its dollar value and deadline.

One user, self-hosted. Built to run as two rootless Podman containers on a Raspberry Pi.

| Part | What |
| --- | --- |
| `backend/` | Rust: Axum API, matching engine, sync and alert workers, Postgres via sqlx |
| `frontend/` | Angular web app (bun for packages), served by the backend |
| `catalog/` | One TOML file per card product: benefits, values, matching rules |
| `deploy/` | Quadlet units, installer and settings template for Podman |
| `Containerfile` | Single image: API + built web app + catalog |

## Deploy on a Raspberry Pi

Needs a **64-bit** Raspberry Pi OS with **Podman 4.4 or newer** (Raspberry Pi OS "trixie"; `sudo apt install podman`). A Pi 4 or 5 with 2 GB+ is plenty to run it.

**1. Get the image onto the Pi.** Either build it there (works, but the Rust build takes a long while on a Pi and wants ~2 GB of free memory):

```sh
git clone <this repo> && cd smart_credit
podman build -t card-benefits .
```

or cross-build on a faster machine and copy it over. The build stages cross-compile, so no emulation is needed:

```sh
podman build --platform linux/arm64 -t card-benefits .
podman save card-benefits | ssh pi@raspberrypi podman load
```

**2. Install and start**, on the Pi, as the user that will own the service:

```sh
./deploy/install.sh
```

It generates the database password and encryption key as Podman secrets, asks for your sign-in email and password, installs the Quadlet units into `~/.config/containers/systemd/`, enables lingering so the service survives logout and reboot, and starts everything.

**3. Open `http://<pi-address>:8080`**, sign in, and scan the QR code with an authenticator app. Then **Add an account**.

Day-to-day:

```sh
systemctl --user status card-benefits card-benefits-db
journalctl --user -u card-benefits -f
systemctl --user restart card-benefits        # after editing app.env or loading a new image
```

### Settings

Non-secret settings and the optional integrations live in `~/.config/card-benefits/app.env` (template: [`deploy/app.env.example`](deploy/app.env.example)).

| Setting | Purpose |
| --- | --- |
| `PLAID_CLIENT_ID`, `PLAID_SECRET`, `PLAID_ENV` | Turns on account linking. Without them, cards are added manually and filled from statement CSVs. |
| `PLAID_REDIRECT_URI` | Required by Plaid for OAuth institutions, which includes Capital One and Amex. Must be an HTTPS URL ending in `/oauth-return`, registered in the Plaid dashboard. |
| `SMTP_URL`, `MAIL_FROM` | Turns on alert emails. Without them, alerts only show on the Home screen. |
| `COOKIE_SECURE` | Set to `true` once the app is behind HTTPS. |
| `SYNC_HOUR`, `ALERT_HOUR` | Local hour of the nightly sync (default 3) and daily alerts (default 8). |
| `REQUIRE_MFA` | Two-factor sign-in; on by default. |

`ADMIN_EMAIL` and the `cbt-admin-password` secret only create the login on the very first start. Change the password later in Settings.

### HTTPS

The app serves plain HTTP on port 8080. On a home network that is workable, but linking Capital One or Amex through Plaid needs an HTTPS redirect URI, and the session cookie should be `Secure`. Put it behind something that terminates TLS (Tailscale Serve, Caddy, or a reverse proxy you already run), change `PublishPort` in `card-benefits.container` to `127.0.0.1:8080:8080`, and set `COOKIE_SECURE=true`. Do not expose port 8080 to the internet directly.

### Backups

Two things matter: the database, and the encryption key that protects the Plaid tokens inside it.

```sh
podman exec card-benefits-db pg_dump -U benefits benefits | gzip > benefits-$(date +%F).sql.gz
podman secret inspect --showsecret cbt-encryption-key --format '{{.SecretData}}'   # store somewhere safe
```

Restoring the database without the key keeps all your history, but linked accounts must be linked again.

### Updating

Load or build the new image, then `systemctl --user restart card-benefits`. Database migrations run at startup.

## How it works

- **Catalog.** Each file in `catalog/` describes one card product and its benefits: value, cadence, reset basis, enrollment, qualifying-purchase patterns, statement-credit patterns, how-to text, source link and effective dates. It is loaded at startup, so fixing a pattern or a value is a file edit plus restart, not a release. To edit it on the Pi without rebuilding, copy `catalog/` to `~/.config/card-benefits/catalog` and uncomment the `Volume=` line in `card-benefits.container`.
- **Periods.** For every card and benefit the app generates the periods a credit can be used in (monthly, quarterly, semiannual, annual by calendar or cardmember year, one-time), ending at midnight in your timezone. The Unclaimed list is the set of open periods with something left.
- **Matching engine** ([`backend/src/engine.rs`](backend/src/engine.rs)). Recomputed per card on every sync, import or edit:
  1. a credit whose description matches a benefit's pattern is applied to that benefit, in the period of the purchase that earned it;
  2. a qualifying purchase with no credit yet shows as *Pending credit*;
  3. pending for more than 2 statement cycles (60 days) is flagged *Check enrollment or eligibility*;
  4. refunds and reversed credits give the amount back;
  5. anything you set by hand wins and is never overwritten.
- **Check-offs.** Uber Cash, Bilt Cash credits, the Venture X travel credit (taken off at checkout) and anniversary miles never appear as statement credits, so they are one-tap check-offs.
- **Bilt mortgage points** are tracked per month from the payment and your other spend on the card: which tier you have reached, and how much more spend (or Bilt Cash) unlocks the next. The payment is usually pulled by ACH and does not show on the card, so set the monthly amount on the card's page.
- **Alerts**: a period ending in 7 and 2 days with money left (configurable), new monthly and quarterly credits on the first run of each month, enrollment still needed, a pending credit that never posted, and a connection that stopped syncing. Sent as one daily digest.

### Security and privacy

- Plaid is used with the read-only `transactions` product. Nothing in the app can move money.
- Issuer passwords never reach the app. Plaid access tokens and the TOTP secret are encrypted at rest (AES-256-GCM) with the `cbt-encryption-key` secret.
- Only the last four digits of a card are stored.
- Password (argon2) plus TOTP sign-in; every sync, override and settings change is written to the activity log in Settings.
- Disconnecting an account revokes it at Plaid and deletes its cards and transactions. Settings also has a full JSON export and delete-my-data.
- No analytics and no third-party requests, apart from Plaid Link when you link an account.

## Before relying on it

- **Review the catalog.** Values were collected in November 2026 from secondary sources (linked on each entry) and could not be checked against the issuers' own terms pages. Statement-credit description patterns come from cardholder reports; only the Amex lodging credit wording is documented by the issuer. If a credit you received is not picked up, open the benefit, compare the description with "Credit appears as", and adjust the pattern in the catalog file.
- **Bilt.** Plaid linking for Cardless-serviced cards has been unreliable, and the Cardless CSV column layout is not documented. The importer detects common column names and the sign convention, with a manual switch on the import form. Confirm against your first real export.
- **Bilt 2027 anniversary conversion** ($200 Bilt Cash + $400 hotel credit becoming $600 Bilt Cash) is prepared but commented out in `catalog/bilt-palladium.toml`; switch it on when it applies to your card.
- **Plaid access.** Production access for OAuth institutions needs Plaid's approval of your application, and pricing for personal use needs confirming with Plaid.

## Development

Needs Rust, bun, Node 22 and a local Postgres.

```sh
# backend (http://127.0.0.1:8080)
cd backend
export DATABASE_URL=postgres://postgres@127.0.0.1:5432/benefits
export APP_ENCRYPTION_KEY=$(openssl rand -base64 32)
export ADMIN_EMAIL=me@example.com ADMIN_PASSWORD='a long password' REQUIRE_MFA=false
export CATALOG_DIR=../catalog STATIC_DIR=../frontend/dist/frontend/browser
cargo run

# frontend with live reload (http://localhost:4200, proxies /api to the backend)
cd frontend
bun install
bun run start

# tests: unit tests always run; the API tests need a Postgres they can create databases in
cd backend
TEST_DATABASE_URL=postgres://postgres@127.0.0.1:5432/postgres cargo test
```

The API tests cover the full flow against a mock Plaid server: linking, sync with modified and removed transactions, CSV import, matching, overrides, offers, alerts, MFA, export and deletion.
