import { Component, OnInit, inject, signal } from '@angular/core';
import { RouterLink } from '@angular/router';
import { Api, Card, errorText } from '../api';
import { money, providerName } from '../format';

@Component({
  selector: 'app-cards',
  imports: [RouterLink],
  template: `
    <div class="page-head">
      <h1>Cards</h1>
      <a class="button primary" routerLink="/add">Add an account</a>
    </div>
    @if (error()) { <p class="error" role="alert">{{ error() }}</p> }
    <div class="card-grid">
      @for (c of cards(); track c.id) {
        <a class="panel card-tile" [routerLink]="['/cards', c.id]">
          <div class="card-tile-head">
            <strong>{{ c.name }}</strong>
            <span class="muted">{{ provider(c.provider) }}@if (c.last4) { ··{{ c.last4 }} }</span>
          </div>
          <div class="fee-line">
            <span><strong>{{ money(c.captured_ytd_cents) }}</strong> captured this year</span>
            <span class="muted">{{ money(c.annual_fee_cents) }} annual fee</span>
          </div>
          <div class="meter" role="img" [attr.aria-label]="pct(c) + '% of the annual fee captured'">
            <span class="meter-used" [class.meter-good]="c.captured_ytd_cents >= c.annual_fee_cents" [style.width.%]="pct(c)"></span>
          </div>
          <div class="row-sub">
            <span>{{ money(c.open_remaining_cents) }} still open</span>
            @if (c.connection_status && c.connection_status !== 'ok') { <span class="chip chip-warn">Not syncing</span> }
            @if (!c.connection_id) { <span class="chip">Manual</span> }
          </div>
        </a>
      } @empty {
        @if (loaded()) {
          <section class="panel empty"><p>No cards yet.</p></section>
        }
      }
    </div>
  `,
})
export class CardsPage implements OnInit {
  private api = inject(Api);
  protected cards = signal<Card[]>([]);
  protected loaded = signal(false);
  protected error = signal('');
  protected money = money;
  protected provider = providerName;

  async ngOnInit() {
    try {
      this.cards.set((await this.api.get<{ cards: Card[] }>('/cards')).cards);
    } catch (e) {
      this.error.set(errorText(e));
    } finally {
      this.loaded.set(true);
    }
  }

  protected pct(c: Card): number {
    return c.annual_fee_cents > 0 ? Math.min(100, Math.round((c.captured_ytd_cents / c.annual_fee_cents) * 100)) : 100;
  }
}
