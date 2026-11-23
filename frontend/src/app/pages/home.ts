import { Component, OnInit, inject, signal } from '@angular/core';
import { RouterLink } from '@angular/router';
import { Alert, Api, Period, Totals, errorText } from '../api';
import { BenefitRow } from '../benefit-row';
import { MortgagePanel } from '../mortgage-panel';
import { dateTime, money, providerName } from '../format';

interface Home {
  totals: Totals;
  unclaimed_count: number;
  next_deadlines: Period[];
  mortgage: Period[];
  broken_connections: { id: string; provider: string; institution_name: string | null; status: string }[];
  alerts: Alert[];
  last_sync_at: string | null;
  card_count: number;
}

@Component({
  selector: 'app-home',
  imports: [RouterLink, BenefitRow, MortgagePanel],
  template: `
    <div class="page-head">
      <h1>Home</h1>
      <div class="head-actions">
        <span class="muted">Last sync: {{ lastSync() }}</span>
        <button type="button" [disabled]="syncing()" (click)="refresh()">{{ syncing() ? 'Refreshing…' : 'Refresh' }}</button>
      </div>
    </div>
    @if (error()) {
      <p class="error" role="alert">{{ error() }}</p>
    }
    @if (data(); as d) {
      @if (d.card_count === 0) {
        <section class="panel empty">
          <h2>No cards yet</h2>
          <p>Link a Capital One, Amex or Bilt card to see which credits you still have to use.</p>
          <a class="button primary" routerLink="/add">Add an account</a>
        </section>
      } @else {
        @for (c of d.broken_connections; track c.id) {
          <p class="banner warn">
            {{ c.institution_name || provider(c.provider) }} stopped syncing.
            <a routerLink="/settings">{{ c.status === 'needs_reauth' ? 'Reconnect' : 'Check the connection' }}</a>
          </p>
        }
        <section class="tiles">
          <div class="tile">
            <span class="tile-label">Expiring this month</span>
            <span class="tile-value">{{ money(d.totals.expiring_month_cents) }}</span>
          </div>
          <div class="tile">
            <span class="tile-label">Expiring this year</span>
            <span class="tile-value">{{ money(d.totals.expiring_year_cents) }}</span>
          </div>
          <div class="tile">
            <span class="tile-label">Unclaimed benefits</span>
            <span class="tile-value">{{ d.unclaimed_count }}</span>
          </div>
        </section>

        <section class="panel">
          <div class="panel-head">
            <h2>Next deadlines</h2>
            <a routerLink="/unclaimed">All unclaimed</a>
          </div>
          @for (p of d.next_deadlines; track p.id) {
            <app-benefit-row [period]="p" (changed)="load()" />
          } @empty {
            <p class="muted">Nothing with a deadline is waiting. Nice.</p>
          }
        </section>

        @for (m of d.mortgage; track m.id) {
          <section class="panel">
            <div class="panel-head">
              <h2>{{ m.benefit_name }} · {{ m.card_name }}</h2>
              <a [routerLink]="['/periods', m.id]">Details</a>
            </div>
            <app-mortgage-panel [period]="m" />
          </section>
        }

        @if (d.alerts.length) {
          <section class="panel">
            <h2>Alerts</h2>
            @for (a of d.alerts; track a.id) {
              <div class="alert">
                <div>
                  <strong>{{ a.title }}</strong>
                  @if (a.body) {
                    <p class="muted pre">{{ a.body }}</p>
                  }
                </div>
                <button class="link" type="button" (click)="dismiss(a)">Dismiss</button>
              </div>
            }
          </section>
        }
      }
    } @else if (!error()) {
      <p class="muted">Loading…</p>
    }
  `,
})
export class HomePage implements OnInit {
  private api = inject(Api);
  protected data = signal<Home | null>(null);
  protected error = signal('');
  protected syncing = signal(false);
  protected money = money;
  protected provider = providerName;

  ngOnInit() {
    this.load();
  }

  protected lastSync() {
    return dateTime(this.data()?.last_sync_at);
  }

  async load() {
    try {
      this.data.set(await this.api.get<Home>('/home'));
      this.error.set('');
    } catch (e) {
      this.error.set(errorText(e));
    }
  }

  async refresh() {
    this.syncing.set(true);
    try {
      const res = await this.api.post<{ summary: { failed: number } }>('/sync');
      await this.load();
      if (res.summary.failed) this.error.set('Some connections could not be synced. See Settings.');
    } catch (e) {
      this.error.set(errorText(e));
    } finally {
      this.syncing.set(false);
    }
  }

  async dismiss(a: Alert) {
    await this.api.post(`/alerts/${a.id}/dismiss`).catch(() => undefined);
    await this.load();
  }
}
