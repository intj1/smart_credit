import { Component, computed, input } from '@angular/core';
import { Period } from './api';
import { money, number } from './format';

/** Bilt housing points for one month: earned so far and what unlocks the rest. */
@Component({
  selector: 'app-mortgage-panel',
  template: `
    @let d = period().detail;
    @if (!d || d.payment_cents == null) {
      <p class="muted">
        No mortgage payment found for this month. If it is paid by bank transfer rather than on the card, set the monthly payment amount on
        the card's page.
      </p>
    } @else {
      <dl class="facts">
        <div><dt>Payment</dt><dd>{{ money(d.payment_cents) }} <span class="muted">({{ d.payment_source === 'transaction' ? 'seen on card' : 'from card setting' }})</span></dd></div>
        <div><dt>Other spend this month</dt><dd>{{ money(d.spend_cents) }} <span class="muted">({{ ratio() }}% of the payment)</span></dd></div>
        <div><dt>Points earned so far</dt><dd>{{ number(d.earned_points ?? 0) }} of {{ number(d.max_points ?? 0) }} <span class="muted">({{ d.multiplier }}x)</span></dd></div>
      </dl>
      <div class="meter" role="img" [attr.aria-label]="ratio() + '% of the top tier reached'">
        <span class="meter-used" [style.width.%]="pct()"></span>
      </div>
      @if (d.next_tier; as next) {
        <p>
          Spend <strong>{{ money(next.spend_needed_cents) }}</strong> more this month to reach {{ next.multiplier }}x
          ({{ number(next.points) }} points).
          @if (d.bilt_cash_for_1x_cents != null) {
            Or redeem {{ money(d.bilt_cash_for_1x_cents) }} of Bilt Cash for 1x.
          }
        </p>
      } @else {
        <p>Top tier reached for this month.</p>
      }
    }
  `,
})
export class MortgagePanel {
  readonly period = input.required<Period>();
  protected money = money;
  protected number = number;
  protected ratio = computed(() => {
    const d = this.period().detail;
    return d?.payment_cents ? Math.round((d.spend_cents / d.payment_cents) * 100) : 0;
  });
  protected pct = computed(() => {
    const d = this.period().detail;
    return d?.max_points ? Math.min(100, ((d.earned_points ?? 0) / d.max_points) * 100) : 0;
  });
}
