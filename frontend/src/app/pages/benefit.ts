import { Component, OnInit, computed, inject, input, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { RouterLink } from '@angular/router';
import { Api, Match, Period, Txn, errorText } from '../api';
import { BenefitRow } from '../benefit-row';
import { MortgagePanel } from '../mortgage-panel';
import { amount, cardLabel, day, money, parseDollars, statusLabel } from '../format';

interface Rules {
  credit_patterns: string[];
  merchants: string[];
  mccs: string[];
  categories: string[];
  min_amount_cents: number;
}

interface Detail {
  period: Period;
  matches: Match[];
  excluded: Txn[];
  rules: Rules;
  history: Period[];
}

@Component({
  selector: 'app-benefit',
  imports: [FormsModule, RouterLink, BenefitRow, MortgagePanel],
  template: `
    @if (data(); as d) {
      @let p = d.period;
      <div class="page-head">
        <div>
          <h1>{{ p.benefit_name }}</h1>
          <p class="muted"><a [routerLink]="['/cards', p.card_id]">{{ card() }}</a> · {{ day(p.start_date) }} – {{ p.deadline ? day(p.deadline) : 'no deadline' }}</p>
        </div>
      </div>
      @if (error()) { <p class="error" role="alert">{{ error() }}</p> }

      <section class="panel">
        @if (p.kind === 'mortgage_points') {
          <app-mortgage-panel [period]="p" />
        } @else {
          <app-benefit-row [period]="p" [showCard]="false" (changed)="load()" />
        }
        @if (p.flag) {
          <p class="banner warn">A qualifying purchase went 2 statement cycles without its credit. Check enrollment or eligibility with the issuer.</p>
        }
        @if (p.status === 'needs_enrollment') {
          <p class="banner warn">This benefit needs enrollment with the issuer. <button class="link" type="button" (click)="enroll()">I'm enrolled</button></p>
        }
      </section>

      <section class="panel">
        <h2>How to use it</h2>
        <p>{{ p.how_to_use || 'No instructions in the catalog.' }}</p>
        @if (p.source_url) { <p><a [href]="p.source_url" target="_blank" rel="noopener noreferrer">Source for this benefit</a></p> }
        @if (p.kind === 'credit') {
          <h3>What counts</h3>
          <dl class="facts">
            @if (d.rules.merchants.length) { <div><dt>Purchases matching</dt><dd><code>{{ d.rules.merchants.join(', ') }}</code></dd></div> }
            @if (d.rules.categories.length || d.rules.mccs.length) { <div><dt>Categories</dt><dd>{{ d.rules.categories.concat(d.rules.mccs).join(', ') }}</dd></div> }
            @if (d.rules.min_amount_cents) { <div><dt>Minimum purchase</dt><dd>{{ money(d.rules.min_amount_cents) }}</dd></div> }
            @if (d.rules.credit_patterns.length) { <div><dt>Credit appears as</dt><dd><code>{{ d.rules.credit_patterns.join(', ') }}</code></dd></div> }
          </dl>
        } @else if (p.kind === 'checkoff') {
          <p class="muted">This benefit never appears on the card statement, so it is tracked by check-off.</p>
        }
      </section>

      @if (p.kind !== 'mortgage_points') {
        <section class="panel">
          <h2>Why it is marked this way</h2>
          @if (d.matches.length) {
            <table>
              <thead><tr><th>Date</th><th>Transaction</th><th class="num">Charged</th><th class="num">Counted</th><th>Source</th><th></th></tr></thead>
              <tbody>
                @for (m of d.matches; track m.id) {
                  <tr [class.muted]="p.manual && m.source !== 'manual'">
                    <td>{{ m.transaction ? day(m.transaction.date) : '—' }}</td>
                    <td>
                      {{ m.transaction ? m.transaction.merchant || m.transaction.description : 'Your entry' }}
                      <div class="muted small">{{ m.note }}</div>
                    </td>
                    <td class="num">{{ m.transaction ? money(m.transaction.amount_cents) : '' }}</td>
                    <td class="num">{{ amount(m.amount, p.unit) }}</td>
                    <td>{{ m.source }}@if (m.source !== 'manual') { <span class="muted small"> · {{ percent(m.confidence) }}</span> }</td>
                    <td>@if (m.transaction) { <button class="link" type="button" (click)="exclude(m.transaction.id)">Not this one</button> }</td>
                  </tr>
                }
              </tbody>
            </table>
            @if (p.manual) { <p class="muted">Your entry overrides the transactions shown in grey.</p> }
          } @else {
            <p class="muted">No transactions or entries for this period yet.</p>
          }
          @if (d.excluded.length) {
            <h3>Marked "not this one"</h3>
            <ul class="plain">
              @for (t of d.excluded; track t.id) {
                <li>{{ day(t.date) }} · {{ t.merchant || t.description }} · {{ money(t.amount_cents) }} <button class="link" type="button" (click)="unexclude(t.id)">Undo</button></li>
              }
            </ul>
          }
        </section>

        <section class="panel">
          <h2>Set it yourself</h2>
          <p class="muted">Your entry always wins and is never overwritten by a later sync.</p>
          <form class="inline-form" (ngSubmit)="setAmount()">
            <label>{{ p.unit === 'usd' ? 'Amount used ($)' : 'Amount used (' + p.unit + ')' }}
              <input name="amount" inputmode="decimal" [(ngModel)]="manual" />
            </label>
            <button type="submit" [disabled]="busy()">Mark this amount used</button>
            <button type="button" [disabled]="busy()" (click)="override('used')">Mark all used</button>
            @if (p.manual) { <button type="button" [disabled]="busy()" (click)="override('clear')">Back to automatic</button> }
          </form>
        </section>
      }

      @if (d.history.length > 1) {
        <section class="panel">
          <h2>All periods</h2>
          <table>
            <thead><tr><th>Period</th><th>Used</th><th>Status</th></tr></thead>
            <tbody>
              @for (h of d.history; track h.id) {
                <tr>
                  <td><a [routerLink]="['/periods', h.id]">{{ day(h.start_date) }} – {{ h.deadline ? day(h.deadline) : 'no deadline' }}</a></td>
                  <td>{{ amount(h.used, h.unit) }} of {{ amount(h.value, h.unit) }}</td>
                  <td>{{ status(h) }}</td>
                </tr>
              }
            </tbody>
          </table>
        </section>
      }
    } @else if (error()) {
      <p class="error" role="alert">{{ error() }}</p>
    } @else {
      <p class="muted">Loading…</p>
    }
  `,
})
export class BenefitPage implements OnInit {
  private api = inject(Api);
  readonly id = input.required<string>();
  protected data = signal<Detail | null>(null);
  protected error = signal('');
  protected busy = signal(false);
  protected manual = '';
  protected card = computed(() => (this.data() ? cardLabel(this.data()!.period) : ''));
  protected money = money;
  protected amount = amount;
  protected day = day;
  protected status = statusLabel;

  private loadedId = '';

  ngOnInit() {
    this.load();
  }

  // The router reuses this component when moving between periods.
  ngDoCheck() {
    if (this.loadedId && this.loadedId !== this.id()) this.load();
  }

  protected percent(confidence: number) {
    return `${Math.round(confidence * 100)}% sure`;
  }

  async load() {
    this.loadedId = this.id();
    try {
      const d = await this.api.get<Detail>(`/periods/${this.id()}`);
      d.history.sort((a, b) => b.start_date.localeCompare(a.start_date));
      this.data.set(d);
    } catch (e) {
      this.error.set(errorText(e));
    }
  }

  private async run(action: () => Promise<unknown>) {
    this.busy.set(true);
    this.error.set('');
    try {
      await action();
      await this.load();
    } catch (e) {
      this.error.set(errorText(e));
    } finally {
      this.busy.set(false);
    }
  }

  override(mode: 'used' | 'clear') {
    return this.run(() => this.api.post(`/periods/${this.id()}/override`, { mode }));
  }

  setAmount() {
    const unit = this.data()!.period.unit;
    const value = unit === 'usd' ? parseDollars(this.manual) : Number.isFinite(Number(this.manual)) && this.manual !== '' ? Math.round(Number(this.manual)) : null;
    if (value == null || value < 0) {
      this.error.set('Enter the amount used as a number.');
      return Promise.resolve();
    }
    return this.run(async () => {
      await this.api.post(`/periods/${this.id()}/override`, { mode: 'amount', amount: value });
      this.manual = '';
    });
  }

  exclude(transactionId: string) {
    return this.run(() => this.api.post(`/periods/${this.id()}/exclusions`, { transaction_id: transactionId }));
  }

  unexclude(transactionId: string) {
    return this.run(() => this.api.delete(`/periods/${this.id()}/exclusions/${transactionId}`));
  }

  enroll() {
    const p = this.data()!.period;
    return this.run(() => this.api.put(`/cards/${p.card_id}/benefits/${p.benefit_id}`, { enrolled: true }));
  }
}
