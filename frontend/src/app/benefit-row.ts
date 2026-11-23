import { Component, computed, inject, input, output, signal } from '@angular/core';
import { RouterLink } from '@angular/router';
import { Api, Period, errorText } from './api';
import { amount, cadenceName, cardLabel, deadline, statusClass, statusLabel } from './format';

/** One benefit period: what is left, by when, and a one-tap "mark used". */
@Component({
  selector: 'app-benefit-row',
  imports: [RouterLink],
  template: `
    @let p = period();
    <div class="row" [class.row-done]="p.status === 'used' || p.status === 'not_for_me'">
      <div class="row-main">
        <a class="row-title" [routerLink]="['/periods', p.id]">{{ p.benefit_name }}</a>
        <div class="row-sub">
          @if (showCard()) {
            <span>{{ card() }}</span>
          }
          <span>{{ cadence() }}</span>
          <span class="chip" [class]="chipClass()">{{ status() }}</span>
          @if (p.flag) {
            <span class="chip chip-warn">Check enrollment or eligibility</span>
          }
        </div>
        @if (showHow() && p.how_to_use) {
          <p class="row-how">{{ p.how_to_use }}</p>
        }
        @if (error()) {
          <p class="error">{{ error() }}</p>
        }
      </div>
      <div class="row-side">
        <div class="row-amount">
          <strong>{{ left() }}</strong>
          <span class="muted"> left of {{ total() }}</span>
        </div>
        <div class="meter" role="img" [attr.aria-label]="usedLabel()">
          <span class="meter-used" [style.width.%]="usedPct()"></span>
          <span class="meter-pending" [style.width.%]="pendingPct()"></span>
        </div>
        <div class="row-deadline" [class.urgent]="urgent()">{{ due() }}</div>
      </div>
      <div class="row-action">
        @if (canMark()) {
          <button type="button" [disabled]="busy()" (click)="mark()">Mark used</button>
        }
      </div>
    </div>
  `,
})
export class BenefitRow {
  private api = inject(Api);
  readonly period = input.required<Period>();
  readonly showCard = input(true);
  readonly showHow = input(false);
  readonly changed = output<void>();

  protected busy = signal(false);
  protected error = signal('');

  protected card = computed(() => cardLabel(this.period()));
  protected cadence = computed(() => cadenceName(this.period().cadence));
  protected status = computed(() => statusLabel(this.period()));
  protected chipClass = computed(() => statusClass(this.period()));
  protected left = computed(() => amount(this.period().remaining, this.period().unit));
  protected total = computed(() => amount(this.period().value, this.period().unit));
  protected due = computed(() => deadline(this.period()));
  protected urgent = computed(() => {
    const p = this.period();
    return p.days_left != null && p.days_left >= 0 && p.days_left <= 7 && p.remaining > 0 && p.status !== 'not_for_me';
  });
  protected usedPct = computed(() => this.pct(this.period().used));
  protected pendingPct = computed(() => Math.min(this.pct(this.period().pending), 100 - this.usedPct()));
  protected usedLabel = computed(() => `${amount(this.period().used, this.period().unit)} of ${this.total()} used`);
  protected canMark = computed(() => {
    const p = this.period();
    const open = p.days_left == null || p.days_left >= 0;
    return open && p.kind !== 'mortgage_points' && p.remaining > 0 && p.status !== 'not_for_me';
  });

  private pct(part: number): number {
    const value = this.period().value;
    return value > 0 ? Math.max(0, Math.min(100, (part / value) * 100)) : 0;
  }

  async mark() {
    this.busy.set(true);
    this.error.set('');
    try {
      await this.api.markUsed(this.period().id);
      this.changed.emit();
    } catch (e) {
      this.error.set(errorText(e));
    } finally {
      this.busy.set(false);
    }
  }
}
