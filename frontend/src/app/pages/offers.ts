import { Component, OnInit, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Api, Card, Offer, errorText } from '../api';
import { cardLabel, day, money, parseDollars } from '../format';

@Component({
  selector: 'app-offers',
  imports: [FormsModule],
  template: `
    <div class="page-head"><h1>Offers</h1></div>
    <p class="muted">
      Amex Offers, Capital One Offers and Bilt deals cannot be read automatically, so add the ones you have activated. An offer is marked
      redeemed when a matching purchase shows up on the card.
    </p>
    @if (error()) { <p class="error" role="alert">{{ error() }}</p> }

    <section class="panel">
      <h2>Add an offer</h2>
      @if (cards().length) {
        <form class="grid-form" (ngSubmit)="add()">
          <label>Card
            <select name="card" required [(ngModel)]="cardId">
              @for (c of cards(); track c.id) { <option [value]="c.id">{{ label(c) }}</option> }
            </select>
          </label>
          <label>Merchant <input name="merchant" required placeholder="As it appears on your statement" [(ngModel)]="merchant" /></label>
          <label>Reward <input name="reward" placeholder="e.g. $10 back on $50" [(ngModel)]="reward" /></label>
          <label>Reward value ($, optional) <input name="rewardValue" inputmode="decimal" [(ngModel)]="rewardValue" /></label>
          <label>Minimum spend ($) <input name="minSpend" inputmode="decimal" [(ngModel)]="minSpend" /></label>
          <label>Expires <input name="expires" type="date" required [(ngModel)]="expires" /></label>
          <div class="form-actions"><button class="primary" type="submit" [disabled]="busy()">Add offer</button></div>
        </form>
      } @else {
        <p class="muted">Add a card first.</p>
      }
    </section>

    <section class="panel">
      <h2>Your offers</h2>
      @for (o of offers(); track o.id) {
        <div class="row" [class.row-done]="o.status !== 'active'">
          <div class="row-main">
            <span class="row-title">{{ o.merchant }}</span>
            <div class="row-sub">
              <span>{{ o.card_name }}</span>
              @if (o.reward) { <span>{{ o.reward }}</span> }
              @if (o.min_spend_cents) { <span>min {{ money(o.min_spend_cents) }}</span> }
              <span class="chip" [class.chip-good]="o.status === 'redeemed'" [class.chip-muted]="o.status === 'expired'">{{ o.status }}</span>
            </div>
            @if (o.redeemed_by; as t) { <p class="row-how">Redeemed by {{ t.description }} on {{ day(t.date) }} ({{ money(t.amount_cents) }}).</p> }
          </div>
          <div class="row-side">
            <div class="row-deadline" [class.urgent]="o.status === 'active' && o.days_left <= 7">
              {{ o.status === 'active' ? (o.days_left === 0 ? 'Expires today' : o.days_left + ' days left') : 'Expires ' + day(o.expires) }}
            </div>
          </div>
          <div class="row-action">
            @if (o.status === 'active') {
              <button type="button" (click)="setStatus(o, 'redeemed')">Mark redeemed</button>
            } @else {
              <button type="button" (click)="setStatus(o, 'active')">Reopen</button>
            }
            <button class="link" type="button" (click)="remove(o)">Delete</button>
          </div>
        </div>
      } @empty {
        <p class="muted">{{ loaded() ? 'No offers yet.' : 'Loading…' }}</p>
      }
    </section>
  `,
})
export class OffersPage implements OnInit {
  private api = inject(Api);
  protected offers = signal<Offer[]>([]);
  protected cards = signal<Card[]>([]);
  protected loaded = signal(false);
  protected busy = signal(false);
  protected error = signal('');
  protected cardId = '';
  protected merchant = '';
  protected reward = '';
  protected rewardValue = '';
  protected minSpend = '';
  protected expires = '';
  protected money = money;
  protected day = day;
  protected label = cardLabel;

  async ngOnInit() {
    const cards = (await this.api.get<{ cards: Card[] }>('/cards').catch(() => ({ cards: [] }))).cards;
    this.cardId = cards[0]?.id ?? '';
    this.cards.set(cards);
    await this.load();
  }

  async load() {
    try {
      this.offers.set((await this.api.get<{ offers: Offer[] }>('/offers')).offers);
    } catch (e) {
      this.error.set(errorText(e));
    } finally {
      this.loaded.set(true);
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

  add() {
    return this.run(async () => {
      await this.api.post('/offers', {
        card_id: this.cardId,
        merchant: this.merchant,
        reward: this.reward,
        reward_cents: parseDollars(this.rewardValue),
        min_spend_cents: parseDollars(this.minSpend) ?? 0,
        expires: this.expires,
      });
      this.merchant = this.reward = this.rewardValue = this.minSpend = this.expires = '';
    });
  }

  setStatus(o: Offer, status: string) {
    return this.run(() => this.api.patch(`/offers/${o.id}`, { status }));
  }

  remove(o: Offer) {
    return this.run(() => this.api.delete(`/offers/${o.id}`));
  }
}
