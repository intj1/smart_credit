import { Component, OnInit, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { RouterLink } from '@angular/router';
import { Api, Connection, errorText } from '../api';
import { dateTime, providerName } from '../format';
import { openPlaidLink } from '../plaid-link';

interface Settings {
  email: string;
  timezone: string;
  alert_email: string | null;
  alert_days: number[];
  alerts_enabled: boolean;
  totp_enabled: boolean;
  mail_configured: boolean;
  plaid_configured: boolean;
  sync_hour: number;
  alert_hour: number;
  catalog: { id: string; name: string; catalog_version: string }[];
}

interface AuditEntry {
  id: number;
  at: string;
  action: string;
  detail: unknown;
}

@Component({
  selector: 'app-settings',
  imports: [FormsModule, RouterLink],
  template: `
    <div class="page-head"><h1>Settings</h1></div>
    @if (error()) { <p class="error" role="alert">{{ error() }}</p> }
    @if (notice()) { <p class="banner" role="status">{{ notice() }}</p> }

    @if (settings(); as s) {
      <section class="panel">
        <h2>Alerts</h2>
        <form class="grid-form" (ngSubmit)="save()">
          <label>Send alerts to <input name="alertEmail" type="email" [(ngModel)]="alertEmail" /></label>
          <label>Days before a deadline <input name="alertDays" placeholder="7, 2" [(ngModel)]="alertDays" /></label>
          <label>Timezone
            <input name="timezone" list="zones" required [(ngModel)]="timezone" />
            <datalist id="zones">@for (z of zones; track z) { <option [value]="z"></option> }</datalist>
          </label>
          <label class="check"><input name="alertsEnabled" type="checkbox" [(ngModel)]="alertsEnabled" /> Email alerts on</label>
          <div class="form-actions">
            <button class="primary" type="submit" [disabled]="busy()">Save</button>
            <button type="button" [disabled]="busy() || !s.mail_configured" (click)="testEmail()">Send a test email</button>
          </div>
        </form>
        <p class="muted">
          Your timezone decides when a credit period ends. Transactions sync nightly after {{ s.sync_hour }}:00 and alerts go out after {{ s.alert_hour }}:00.
          @if (!s.mail_configured) { Email is not configured on this server (SMTP_URL), so alerts only appear on the Home screen. }
        </p>
      </section>

      <section class="panel">
        <div class="panel-head"><h2>Connections</h2><a routerLink="/add">Add an account</a></div>
        @for (c of connections(); track c.id) {
          <div class="row">
            <div class="row-main">
              <span class="row-title">{{ c.institution_name || provider(c.provider) }}</span>
              <div class="row-sub">
                <span class="chip" [class.chip-good]="c.status === 'ok'" [class.chip-warn]="c.status !== 'ok'">{{ c.status === 'ok' ? 'Syncing' : c.status === 'needs_reauth' ? 'Sign-in needed' : 'Error' }}</span>
                <span>Last sync {{ time(c.last_sync_at) }}</span>
                <span>{{ cardNames(c) }}</span>
              </div>
              @if (c.error) { <p class="row-how">{{ c.error }}</p> }
            </div>
            <div class="row-action">
              @if (c.status !== 'ok' && s.plaid_configured) { <button type="button" [disabled]="busy()" (click)="reconnect(c)">Reconnect</button> }
              <button class="danger" type="button" [disabled]="busy()" (click)="disconnect(c)">Disconnect</button>
            </div>
          </div>
        } @empty {
          <p class="muted">No linked accounts. {{ s.plaid_configured ? '' : 'Plaid is not configured on this server; cards are added manually.' }}</p>
        }
        <p class="muted">Disconnecting revokes the link at Plaid and deletes that account's cards and transactions here.</p>
      </section>

      <section class="panel">
        <h2>Sign-in</h2>
        <p class="muted">Signed in as {{ s.email }}. Two-factor sign-in is {{ s.totp_enabled ? 'on' : 'off' }}.</p>
        <form class="grid-form" (ngSubmit)="changePassword()">
          <label>Current password <input name="current" type="password" autocomplete="current-password" required [(ngModel)]="current" /></label>
          <label>New password (10+ characters) <input name="next" type="password" autocomplete="new-password" required minlength="10" [(ngModel)]="next" /></label>
          <div class="form-actions"><button type="submit" [disabled]="busy()">Change password</button></div>
        </form>
      </section>

      <section class="panel">
        <h2>Your data</h2>
        <p>Transaction data is used only to track benefits. It is never shared, and there is no advertising or third-party analytics.</p>
        <div class="form-actions"><a class="button" href="/api/export" download>Export everything (JSON)</a></div>
        <h3>Delete my data</h3>
        <p class="muted">Revokes every link and deletes all cards, transactions, benefit history, offers and logs. Your login stays. This cannot be undone.</p>
        <form class="inline-form" (ngSubmit)="deleteData()">
          <label>Type DELETE to confirm <input name="confirm" autocomplete="off" [(ngModel)]="confirmText" /></label>
          <button class="danger" type="submit" [disabled]="busy() || confirmText !== 'DELETE'">Delete my data</button>
        </form>
      </section>

      <section class="panel">
        <h2>Catalog</h2>
        <ul class="plain">
          @for (c of s.catalog; track c.id) { <li>{{ c.name }} <span class="muted">· reviewed {{ c.catalog_version }}</span></li> }
        </ul>
        <p class="muted">Benefit values and matching rules live in the catalog files on the server; review them monthly against the issuers' pages.</p>
      </section>

      <section class="panel">
        <details (toggle)="loadAudit($event)">
          <summary>Activity log</summary>
          <table>
            <thead><tr><th>When</th><th>Action</th><th>Detail</th></tr></thead>
            <tbody>
              @for (a of audit(); track a.id) {
                <tr><td>{{ time(a.at) }}</td><td>{{ a.action }}</td><td><code class="small">{{ json(a.detail) }}</code></td></tr>
              }
            </tbody>
          </table>
        </details>
      </section>
    } @else if (!error()) {
      <p class="muted">Loading…</p>
    }
  `,
})
export class SettingsPage implements OnInit {
  private api = inject(Api);
  protected settings = signal<Settings | null>(null);
  protected connections = signal<Connection[]>([]);
  protected audit = signal<AuditEntry[]>([]);
  protected busy = signal(false);
  protected error = signal('');
  protected notice = signal('');
  protected alertEmail = '';
  protected alertDays = '';
  protected timezone = '';
  protected alertsEnabled = true;
  protected current = '';
  protected next = '';
  protected confirmText = '';
  protected provider = providerName;
  protected time = dateTime;
  protected zones: string[] = (Intl as unknown as { supportedValuesOf?: (k: string) => string[] }).supportedValuesOf?.('timeZone') ?? [];

  async ngOnInit() {
    await this.load();
  }

  protected json(v: unknown) {
    return JSON.stringify(v);
  }

  protected cardNames(c: Connection) {
    return c.cards.length ? c.cards.map((k) => (k.last4 ? `${k.name} ··${k.last4}` : k.name)).join(', ') : 'No cards tracked';
  }

  async load() {
    try {
      const [s, conns] = await Promise.all([
        this.api.get<Settings>('/settings'),
        this.api.get<{ connections: Connection[] }>('/connections'),
      ]);
      this.alertEmail = s.alert_email ?? '';
      this.alertDays = s.alert_days.join(', ');
      this.timezone = s.timezone;
      this.alertsEnabled = s.alerts_enabled;
      this.connections.set(conns.connections);
      this.settings.set(s);
    } catch (e) {
      this.error.set(errorText(e));
    }
  }

  private async run(action: () => Promise<void>) {
    this.busy.set(true);
    this.error.set('');
    this.notice.set('');
    try {
      await action();
    } catch (e) {
      this.error.set(errorText(e));
    } finally {
      this.busy.set(false);
    }
  }

  save() {
    return this.run(async () => {
      const days = this.alertDays.split(/[,\s]+/).filter(Boolean).map(Number);
      if (days.some((d) => !Number.isInteger(d) || d < 0 || d > 60)) throw new Error('Days before a deadline must be whole numbers from 0 to 60, e.g. "7, 2".');
      await this.api.put('/settings', {
        timezone: this.timezone.trim(),
        alert_email: this.alertEmail.trim() || null,
        alert_days: days,
        alerts_enabled: this.alertsEnabled,
      });
      await this.load();
      this.notice.set('Saved.');
    });
  }

  testEmail() {
    return this.run(async () => {
      const res = await this.api.post<{ to: string }>('/settings/test-email');
      this.notice.set(`Test email sent to ${res.to}.`);
    });
  }

  changePassword() {
    return this.run(async () => {
      await this.api.post('/settings/password', { current: this.current, new: this.next });
      this.current = this.next = '';
      this.notice.set('Password changed. Other sessions were signed out.');
    });
  }

  reconnect(c: Connection) {
    return this.run(async () => {
      const { link_token } = await this.api.post<{ link_token: string }>('/plaid/link-token', { connection_id: c.id });
      const result = await openPlaidLink(link_token, c.provider, c.id);
      if (result) {
        await this.api.post(`/connections/${c.id}/reconnected`);
        await this.load();
        this.notice.set('Reconnected. A sync is running.');
      }
    });
  }

  disconnect(c: Connection) {
    if (!confirm(`Disconnect ${c.institution_name || providerName(c.provider)}? Its cards and transactions will be deleted here.`)) return Promise.resolve();
    return this.run(async () => {
      const res = await this.api.delete<{ warning: string | null }>(`/connections/${c.id}`);
      await this.load();
      this.notice.set(res.warning ?? 'Disconnected and revoked at Plaid.');
    });
  }

  deleteData() {
    return this.run(async () => {
      const res = await this.api.post<{ warnings: string[] }>('/delete-my-data', { confirm: this.confirmText });
      this.confirmText = '';
      await this.load();
      this.notice.set(['All card data was deleted.', ...res.warnings].join(' '));
    });
  }

  async loadAudit(event: Event) {
    if (!(event.target as HTMLDetailsElement).open) return;
    try {
      this.audit.set((await this.api.get<{ entries: AuditEntry[] }>('/audit')).entries);
    } catch (e) {
      this.error.set(errorText(e));
    }
  }
}
