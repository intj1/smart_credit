import { Component, OnInit, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Router } from '@angular/router';
import { Api, NewCard, PlaidAccount, Product, errorText } from '../api';
import { MONTHS, PROVIDERS, amount, cadenceName, money, parseDollars } from '../format';
import { openPlaidLink, resumePlaidLink, pendingPlaidLink } from '../plaid-link';

/** One card being set up: either a linked account or a hand-entered card. */
interface Draft {
  account: PlaidAccount | null;
  /** 'new' = its own card, 'skip' = don't track, otherwise the account_id it rolls up under. */
  mode: string;
  productId: string;
  nickname: string;
  last4: string;
  month: number;
  opened: string;
  housing: string;
  enrolled: Record<string, boolean>;
}

@Component({
  selector: 'app-add-account',
  imports: [FormsModule],
  template: `
    <div class="page-head"><h1>Add an account</h1></div>
    @if (error()) { <p class="error" role="alert">{{ error() }}</p> }

    @switch (step()) {
      @case ('provider') {
        <section class="panel">
          <h2>1. Pick a provider</h2>
          <div class="choices">
            @for (p of providers; track p.id) {
              <button type="button" [class.selected]="provider() === p.id" (click)="provider.set(p.id)">{{ p.name }}</button>
            }
          </div>
          @if (provider()) {
            <h2>2. Connect</h2>
            @if (plaidConfigured()) {
              <p>You sign in on {{ providerLabel() }}'s own page through Plaid. This app never sees or stores your password, and the link is read-only.</p>
              @if (provider() === 'bilt') {
                <p class="muted">Bilt linking through Plaid has been unreliable. If it fails or shows no transactions, add the card manually and import the statement CSV from the Cardless app.</p>
              }
              <div class="form-actions">
                <button class="primary" type="button" [disabled]="busy()" (click)="link()">{{ busy() ? 'Waiting for Plaid…' : 'Link with Plaid' }}</button>
                <button type="button" (click)="startManual()">Add manually instead</button>
              </div>
            } @else {
              <p class="muted">Plaid is not configured on this server, so cards are added manually and filled from statement CSVs.</p>
              <div class="form-actions"><button class="primary" type="button" (click)="startManual()">Add manually</button></div>
            }
          }
        </section>
      }
      @case ('cards') {
        <section class="panel">
          <h2>{{ connectionId() ? '3. Choose cards and confirm each product' : '3. Card details' }}</h2>
          @for (d of drafts(); track $index; let i = $index) {
            <fieldset class="draft">
              <legend>{{ d.account ? d.account.name + (d.account.mask ? ' ··' + d.account.mask : '') : 'New card' }}</legend>
              @if (d.account) {
                <label>This account
                  <select [name]="'mode' + i" [(ngModel)]="d.mode" (ngModelChange)="touch()">
                    <option value="new">Track as its own card</option>
                    @for (other of rollupTargets(d); track other.account!.account_id) {
                      <option [value]="other.account!.account_id">Roll up under {{ other.account!.name }} ··{{ other.account!.mask }} (authorized user)</option>
                    }
                    <option value="skip">Don't track</option>
                  </select>
                </label>
              }
              @if (d.mode === 'new') {
                <div class="grid-form">
                  <label>Card product
                    <select [name]="'product' + i" required [(ngModel)]="d.productId" (ngModelChange)="touch()">
                      <option value="" disabled>Choose…</option>
                      @for (p of productsFor(); track p.id) { <option [value]="p.id">{{ p.name }} ({{ money(p.annual_fee_cents) }}/yr)</option> }
                    </select>
                  </label>
                  <label>Nickname (optional) <input [name]="'nick' + i" [(ngModel)]="d.nickname" /></label>
                  @if (!d.account) {
                    <label>Last 4 digits (optional) <input [name]="'last4' + i" maxlength="4" inputmode="numeric" [(ngModel)]="d.last4" /></label>
                  }
                  <label>Anniversary month
                    <select [name]="'month' + i" [(ngModel)]="d.month">
                      @for (m of months; track $index) { <option [ngValue]="$index + 1">{{ m }}</option> }
                    </select>
                  </label>
                  <label>Opened on (optional) <input [name]="'opened' + i" type="date" [(ngModel)]="d.opened" /></label>
                  @if (hasMortgage(d)) {
                    <label>Monthly mortgage payment ($, optional) <input [name]="'housing' + i" inputmode="decimal" [(ngModel)]="d.housing" /></label>
                  }
                </div>
                @if (product(d); as p) {
                  <p class="muted">The anniversary month is when the annual fee posts; cardmember-year credits reset then.</p>
                  <h3>{{ p.benefits.length }} benefits will be tracked</h3>
                  <ul class="plain benefit-list">
                    @for (b of p.benefits; track b.id) {
                      <li>
                        <span>{{ b.name }} <span class="muted">· {{ b.kind === 'mortgage_points' ? 'points' : amount(b.value, b.unit) }} · {{ cadence(b.cadence) }}</span></span>
                        @if (b.enrollment_required) {
                          <label class="check"><input type="checkbox" [name]="'enr' + i + b.id" [(ngModel)]="d.enrolled[b.id]" /> Already enrolled</label>
                        }
                      </li>
                    }
                  </ul>
                }
              }
            </fieldset>
          }
          <div class="form-actions">
            <button class="primary" type="button" [disabled]="busy() || !ready()" (click)="save()">{{ busy() ? 'Saving…' : 'Start tracking' }}</button>
            <button type="button" (click)="cancel()">Cancel</button>
          </div>
        </section>
      }
    }
  `,
})
export class AddAccountPage implements OnInit {
  private api = inject(Api);
  private router = inject(Router);

  protected step = signal<'provider' | 'cards'>('provider');
  protected provider = signal('');
  protected plaidConfigured = signal(false);
  protected products = signal<Product[]>([]);
  protected drafts = signal<Draft[]>([]);
  protected connectionId = signal('');
  protected busy = signal(false);
  protected error = signal('');
  protected providers = PROVIDERS;
  protected months = MONTHS;
  protected money = money;
  protected amount = amount;
  protected cadence = cadenceName;

  protected providerLabel = computed(() => PROVIDERS.find((p) => p.id === this.provider())?.name ?? '');
  protected productsFor = computed(() => this.products().filter((p) => p.provider === this.provider()));
  protected ready = computed(() => {
    const tracked = this.drafts().filter((d) => d.mode === 'new');
    return tracked.length > 0 && tracked.every((d) => d.productId);
  });

  async ngOnInit() {
    try {
      const [catalog, conns] = await Promise.all([
        this.api.get<{ products: Product[] }>('/catalog'),
        this.api.get<{ plaid_configured: boolean }>('/connections'),
      ]);
      this.products.set(catalog.products);
      this.plaidConfigured.set(conns.plaid_configured);
      // Returning from an OAuth sign-in at the issuer: finish the Plaid flow.
      const pending = pendingPlaidLink();
      if (pending && location.pathname.startsWith('/oauth-return')) {
        this.provider.set(pending.provider);
        this.busy.set(true);
        const result = await resumePlaidLink();
        this.busy.set(false);
        if (result && pending.connectionId) {
          // This was a reconnect of an existing connection, not a new link.
          await this.api.post(`/connections/${pending.connectionId}/reconnected`);
          await this.router.navigateByUrl('/settings');
        } else if (result) {
          await this.exchange(result.publicToken, result.institution);
        } else {
          await this.router.navigateByUrl('/add');
        }
      }
    } catch (e) {
      this.busy.set(false);
      this.error.set(errorText(e));
    }
  }

  /** ngModel edits mutate drafts in place; this re-runs the computed checks. */
  protected touch() {
    this.drafts.update((d) => [...d]);
  }

  protected product(d: Draft): Product | undefined {
    return this.products().find((p) => p.id === d.productId);
  }

  protected hasMortgage(d: Draft): boolean {
    return !!this.product(d)?.benefits.some((b) => b.kind === 'mortgage_points');
  }

  protected rollupTargets(d: Draft): Draft[] {
    return this.drafts().filter((o) => o !== d && o.account && o.mode === 'new');
  }

  private blank(account: PlaidAccount | null): Draft {
    const only = this.productsFor().length === 1 ? this.productsFor()[0].id : '';
    return {
      account,
      mode: account?.tracked ? 'skip' : 'new',
      productId: account?.suggested_product_id ?? only,
      nickname: '',
      last4: account?.mask ?? '',
      month: new Date().getMonth() + 1,
      opened: '',
      housing: '',
      enrolled: {},
    };
  }

  startManual() {
    this.connectionId.set('');
    this.drafts.set([this.blank(null)]);
    this.step.set('cards');
  }

  async link() {
    this.busy.set(true);
    this.error.set('');
    try {
      const { link_token } = await this.api.post<{ link_token: string }>('/plaid/link-token', {});
      const result = await openPlaidLink(link_token, this.provider());
      if (result) await this.exchange(result.publicToken, result.institution);
    } catch (e) {
      this.error.set(errorText(e));
    } finally {
      this.busy.set(false);
    }
  }

  private async exchange(publicToken: string, institution: string | null) {
    const res = await this.api.post<{ connection_id: string; accounts: PlaidAccount[] }>('/plaid/exchange', {
      public_token: publicToken,
      provider: this.provider(),
      institution_name: institution,
    });
    if (!res.accounts.length) {
      await this.api.delete(`/connections/${res.connection_id}`).catch(() => undefined);
      throw new Error('That login has no credit card accounts. Nothing was linked.');
    }
    this.connectionId.set(res.connection_id);
    this.drafts.set(res.accounts.map((a) => this.blank(a)));
    this.step.set('cards');
  }

  async save() {
    this.busy.set(true);
    this.error.set('');
    try {
      const cards: NewCard[] = [];
      for (const d of this.drafts().filter((x) => x.mode === 'new')) {
        const housing = d.housing.trim() === '' ? null : parseDollars(d.housing);
        if (d.housing.trim() !== '' && housing == null) throw new Error('Enter the mortgage payment as a dollar amount.');
        const rolled = this.drafts().filter((o) => o.account && d.account && o.mode === d.account.account_id);
        cards.push({
          product_id: d.productId,
          nickname: d.nickname,
          last4: d.last4 || null,
          anniversary_month: Number(d.month),
          opened_date: d.opened || null,
          housing_payment_cents: housing,
          enrolled_benefit_ids: Object.keys(d.enrolled).filter((k) => d.enrolled[k]),
          account_ids: d.account ? [d.account.account_id, ...rolled.map((o) => o.account!.account_id)] : [],
        });
      }
      if (this.connectionId()) {
        await this.api.post(`/connections/${this.connectionId()}/cards`, { cards });
        await this.router.navigateByUrl('/cards');
      } else {
        const res = await this.api.post<{ id: string }>('/cards', cards[0]);
        await this.router.navigate(['/cards', res.id]);
      }
    } catch (e) {
      this.error.set(errorText(e));
    } finally {
      this.busy.set(false);
    }
  }

  async cancel() {
    // An abandoned link should not leave a live connection behind.
    if (this.connectionId()) await this.api.delete(`/connections/${this.connectionId()}`).catch(() => undefined);
    this.connectionId.set('');
    this.step.set('provider');
  }
}
