import { Component, OnInit, computed, inject, input, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Router } from '@angular/router';
import { Api, Card, Period, errorText } from '../api';
import { BenefitRow } from '../benefit-row';
import { MortgagePanel } from '../mortgage-panel';
import { MONTHS, amount, day, money, parseDollars, providerName, statusLabel } from '../format';

interface Group {
  benefit_id: string;
  name: string;
  current: Period | null;
  past: Period[];
  enrollment_required: boolean;
  enrolled: boolean;
  not_for_me: boolean;
}

@Component({
  selector: 'app-card',
  imports: [FormsModule, BenefitRow, MortgagePanel],
  template: `
    @if (card(); as c) {
      <div class="page-head">
        <div>
          <h1>{{ c.name }}</h1>
          <p class="muted">{{ c.product_name }} · {{ provider(c.provider) }}@if (c.last4) { ··{{ c.last4 }} } · {{ c.connection_id ? 'Linked' : 'Manual' }}</p>
        </div>
      </div>
      <section class="tiles">
        <div class="tile"><span class="tile-label">Captured this year</span><span class="tile-value">{{ money(c.captured_ytd_cents) }}</span></div>
        <div class="tile"><span class="tile-label">Annual fee</span><span class="tile-value">{{ money(c.annual_fee_cents) }}</span></div>
        <div class="tile"><span class="tile-label">Still open</span><span class="tile-value">{{ money(c.open_remaining_cents) }}</span></div>
      </section>
      @if (error()) { <p class="error" role="alert">{{ error() }}</p> }
      @if (notice()) { <p class="banner" role="status">{{ notice() }}</p> }

      <section class="panel">
        <h2>Benefits</h2>
        @for (g of groups(); track g.benefit_id) {
          <div class="group">
            @if (g.current; as p) {
              @if (p.kind === 'mortgage_points') {
                <h3>{{ p.benefit_name }}</h3>
                <app-mortgage-panel [period]="p" />
              } @else {
                <app-benefit-row [period]="p" [showCard]="false" (changed)="load()" />
              }
            } @else {
              <div class="row row-done"><div class="row-main"><span class="row-title">{{ g.name }}</span><div class="row-sub"><span>No open period</span></div></div></div>
            }
            <div class="group-tools">
              @if (g.enrollment_required) {
                <label class="check"><input type="checkbox" [checked]="g.enrolled" (change)="setFlag(g, { enrolled: !g.enrolled })" /> Enrolled</label>
              }
              <label class="check"><input type="checkbox" [checked]="g.not_for_me" (change)="setFlag(g, { not_for_me: !g.not_for_me })" /> Not for me</label>
              @if (g.past.length) {
                <details>
                  <summary>Earlier periods ({{ g.past.length }})</summary>
                  <table>
                    <thead><tr><th>Period</th><th>Used</th><th>Status</th></tr></thead>
                    <tbody>
                      @for (p of g.past; track p.id) {
                        <tr>
                          <td>{{ day(p.start_date) }} – {{ day(p.deadline) }}</td>
                          <td>{{ amount(p.used, p.unit) }} of {{ amount(p.value, p.unit) }}</td>
                          <td>{{ status(p) }}</td>
                        </tr>
                      }
                    </tbody>
                  </table>
                </details>
              }
            </div>
          </div>
        }
      </section>

      <section class="panel">
        <h2>Import a statement CSV</h2>
        <p class="muted">
          Use this when the card is not linked, or the link is missing transactions. Export a CSV from the issuer (for Bilt: Cardless app,
          Account, Monthly activity, export). Importing the same file twice is safe.
          {{ c.transaction_count }} transactions on file@if (c.last_transaction_date) {, latest {{ day(c.last_transaction_date) }}}.
        </p>
        <div class="inline-form">
          <input type="file" accept=".csv,text/csv" aria-label="Statement CSV file" (change)="pick($event)" />
          <label>Purchases in this file are
            <select name="sign" [(ngModel)]="sign">
              <option value="auto">detected automatically</option>
              <option value="positive">positive numbers</option>
              <option value="negative">negative numbers</option>
            </select>
          </label>
          <button type="button" [disabled]="!file() || busy()" (click)="importCsv()">Import</button>
        </div>
      </section>

      <section class="panel">
        <h2>Card settings</h2>
        <form class="grid-form" (ngSubmit)="save()">
          <label>Nickname <input name="nickname" [(ngModel)]="nickname" /></label>
          <label>Last 4 digits <input name="last4" maxlength="4" inputmode="numeric" [(ngModel)]="last4" /></label>
          <label>Anniversary month
            <select name="month" [(ngModel)]="month">
              @for (m of months; track $index) { <option [ngValue]="$index + 1">{{ m }}</option> }
            </select>
          </label>
          <label>Opened on (optional) <input name="opened" type="date" [(ngModel)]="opened" /></label>
          @if (c.has_mortgage_benefit) {
            <label>Monthly mortgage payment ($) <input name="housing" inputmode="decimal" placeholder="e.g. 2400" [(ngModel)]="housing" /></label>
          }
          <div class="form-actions">
            <button class="primary" type="submit" [disabled]="busy()">Save</button>
            <button class="danger" type="button" [disabled]="busy()" (click)="remove()">Delete card</button>
          </div>
        </form>
        <p class="muted">The anniversary month sets when cardmember-year benefits reset. Changing it rebuilds those periods.</p>
      </section>
    } @else if (error()) {
      <p class="error" role="alert">{{ error() }}</p>
    } @else {
      <p class="muted">Loading…</p>
    }
  `,
})
export class CardPage implements OnInit {
  private api = inject(Api);
  private router = inject(Router);
  readonly id = input.required<string>();

  protected card = signal<Card | null>(null);
  protected periods = signal<Period[]>([]);
  protected error = signal('');
  protected notice = signal('');
  protected busy = signal(false);
  protected file = signal<File | null>(null);
  protected sign = 'auto';
  protected nickname = '';
  protected last4 = '';
  protected month = 1;
  protected opened = '';
  protected housing = '';
  protected months = MONTHS;
  protected money = money;
  protected amount = amount;
  protected day = day;
  protected status = statusLabel;
  protected provider = providerName;

  protected groups = computed<Group[]>(() => {
    const byBenefit = new Map<string, Period[]>();
    for (const p of this.periods()) {
      byBenefit.set(p.benefit_id, [...(byBenefit.get(p.benefit_id) ?? []), p]);
    }
    return [...byBenefit.entries()]
      .map(([benefit_id, list]) => {
        const current = list.find((p) => p.days_left == null || p.days_left >= 0) ?? null;
        const any = current ?? list[0];
        return {
          benefit_id,
          name: any.benefit_name,
          current,
          past: list.filter((p) => p !== current).sort((a, b) => b.start_date.localeCompare(a.start_date)),
          enrollment_required: any.enrollment_required,
          enrolled: any.enrolled,
          not_for_me: any.not_for_me,
        };
      })
      .sort((a, b) => Number(a.not_for_me) - Number(b.not_for_me) || (a.current?.days_left ?? 1e9) - (b.current?.days_left ?? 1e9));
  });

  async ngOnInit() {
    await this.load(true);
  }

  async load(resetForm = false) {
    try {
      const res = await this.api.get<{ card: Card; periods: Period[] }>(`/cards/${this.id()}`);
      if (resetForm) {
        this.nickname = res.card.nickname;
        this.last4 = res.card.last4 ?? '';
        this.month = res.card.anniversary_month;
        this.opened = res.card.opened_date ?? '';
        this.housing = res.card.housing_payment_cents != null ? String(res.card.housing_payment_cents / 100) : '';
      }
      this.periods.set(res.periods);
      this.card.set(res.card);
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

  setFlag(g: Group, flags: { enrolled?: boolean; not_for_me?: boolean }) {
    return this.run(async () => {
      await this.api.put(`/cards/${this.id()}/benefits/${g.benefit_id}`, flags);
      await this.load();
    });
  }

  pick(event: Event) {
    this.file.set((event.target as HTMLInputElement).files?.[0] ?? null);
  }

  importCsv() {
    return this.run(async () => {
      const csv = await this.file()!.text();
      const res = await this.api.post<{ rows: number; imported: number; duplicates: number; from: string; to: string }>(
        `/cards/${this.id()}/import`,
        { csv, purchases_negative: this.sign === 'auto' ? null : this.sign === 'negative' },
      );
      await this.load();
      this.notice.set(
        `Imported ${res.imported} of ${res.rows} transactions (${day(res.from)} – ${day(res.to)})` +
          (res.duplicates ? `; ${res.duplicates} were already on file.` : '.'),
      );
    });
  }

  save() {
    return this.run(async () => {
      const housing = this.housing.trim() === '' ? null : parseDollars(this.housing);
      if (this.housing.trim() !== '' && housing == null) throw new Error('Enter the mortgage payment as a dollar amount.');
      await this.api.patch(`/cards/${this.id()}`, {
        nickname: this.nickname,
        last4: this.last4,
        anniversary_month: Number(this.month),
        opened_date: this.opened || null,
        housing_payment_cents: housing,
      });
      await this.load(true);
      this.notice.set('Saved.');
    });
  }

  remove() {
    if (!confirm('Delete this card with its transactions and benefit history? This cannot be undone.')) return Promise.resolve();
    return this.run(async () => {
      await this.api.delete(`/cards/${this.id()}`);
      await this.router.navigateByUrl('/cards');
    });
  }
}
