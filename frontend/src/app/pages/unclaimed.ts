import { Component, OnInit, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Api, Card, Period, Totals, errorText } from '../api';
import { BenefitRow } from '../benefit-row';
import { CADENCES, CATEGORIES, PROVIDERS, cardLabel, money } from '../format';

@Component({
  selector: 'app-unclaimed',
  imports: [FormsModule, BenefitRow],
  template: `
    <div class="page-head"><h1>Unclaimed</h1></div>
    @if (totals(); as t) {
      <section class="tiles">
        <div class="tile"><span class="tile-label">Expiring this month</span><span class="tile-value">{{ money(t.expiring_month_cents) }}</span></div>
        <div class="tile"><span class="tile-label">Expiring this year</span><span class="tile-value">{{ money(t.expiring_year_cents) }}</span></div>
        <div class="tile"><span class="tile-label">Total left in open periods</span><span class="tile-value">{{ money(t.total_remaining_cents) }}</span></div>
      </section>
    }
    <form class="filters" (ngSubmit)="load()">
      <label>Provider
        <select name="provider" [(ngModel)]="provider" (ngModelChange)="load()">
          <option value="">All</option>
          @for (p of providers; track p.id) { <option [value]="p.id">{{ p.name }}</option> }
        </select>
      </label>
      <label>Card
        <select name="card" [(ngModel)]="card" (ngModelChange)="load()">
          <option value="">All</option>
          @for (c of cards(); track c.id) { <option [value]="c.id">{{ label(c) }}</option> }
        </select>
      </label>
      <label>Cadence
        <select name="cadence" [(ngModel)]="cadence" (ngModelChange)="load()">
          <option value="">All</option>
          @for (c of cadences; track c.id) { <option [value]="c.id">{{ c.name }}</option> }
        </select>
      </label>
      <label>Category
        <select name="category" [(ngModel)]="category" (ngModelChange)="load()">
          <option value="">All</option>
          @for (c of categories; track c) { <option [value]="c">{{ c }}</option> }
        </select>
      </label>
      <label class="check"><input type="checkbox" name="hidden" [(ngModel)]="hidden" (ngModelChange)="load()" /> Show "Not for me"</label>
    </form>
    @if (error()) { <p class="error" role="alert">{{ error() }}</p> }
    <section class="panel">
      @for (p of rows(); track p.id) {
        <app-benefit-row [period]="p" [showHow]="true" (changed)="load()" />
      } @empty {
        <p class="muted">{{ loaded() ? 'Nothing unclaimed matches these filters.' : 'Loading…' }}</p>
      }
    </section>
  `,
})
export class UnclaimedPage implements OnInit {
  private api = inject(Api);
  protected rows = signal<Period[]>([]);
  protected totals = signal<Totals | null>(null);
  protected cards = signal<Card[]>([]);
  protected loaded = signal(false);
  protected error = signal('');
  protected provider = '';
  protected card = '';
  protected cadence = '';
  protected category = '';
  protected hidden = false;
  protected providers = PROVIDERS;
  protected cadences = CADENCES;
  protected categories = CATEGORIES;
  protected money = money;
  protected label = cardLabel;

  async ngOnInit() {
    this.load();
    this.cards.set((await this.api.get<{ cards: Card[] }>('/cards').catch(() => ({ cards: [] }))).cards);
  }

  async load() {
    const params: Record<string, string> = {};
    if (this.provider) params['provider'] = this.provider;
    if (this.card) params['card'] = this.card;
    if (this.cadence) params['cadence'] = this.cadence;
    if (this.category) params['category'] = this.category;
    if (this.hidden) params['include_hidden'] = 'true';
    try {
      const res = await this.api.get<{ rows: Period[]; totals: Totals }>('/unclaimed', params);
      this.rows.set(res.rows);
      this.totals.set(res.totals);
      this.error.set('');
    } catch (e) {
      this.error.set(errorText(e));
    } finally {
      this.loaded.set(true);
    }
  }
}
