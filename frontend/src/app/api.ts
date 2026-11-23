import { HttpClient, HttpErrorResponse } from '@angular/common/http';
import { Injectable, inject } from '@angular/core';
import { firstValueFrom } from 'rxjs';

export interface Period {
  id: string;
  card_id: string;
  card_name: string;
  card_last4: string | null;
  provider: string;
  product_name: string;
  benefit_id: string;
  benefit_name: string;
  kind: 'credit' | 'checkoff' | 'mortgage_points';
  unit: 'usd' | 'miles' | 'points';
  category: string;
  cadence: string;
  start_date: string;
  deadline: string | null;
  days_left: number | null;
  value: number;
  used: number;
  pending: number;
  remaining: number;
  status: string;
  flag: string | null;
  enrollment_required: boolean;
  enrolled: boolean;
  not_for_me: boolean;
  manual: boolean;
  how_to_use: string;
  source_url: string;
  detail: MortgageDetail | null;
}

export interface MortgageDetail {
  payment_cents: number | null;
  payment_source?: 'transaction' | 'card_setting';
  spend_cents: number;
  multiplier?: number;
  earned_points?: number;
  max_points?: number;
  next_tier?: { multiplier: number; points: number; spend_needed_cents: number } | null;
  bilt_cash_for_1x_cents?: number | null;
}

export interface Totals {
  expiring_month_cents: number;
  expiring_year_cents: number;
  total_remaining_cents: number;
}

export interface Card {
  id: string;
  nickname: string;
  name: string;
  product_id: string;
  product_name: string;
  provider: string;
  last4: string | null;
  anniversary_month: number;
  opened_date: string | null;
  connection_id: string | null;
  connection_status: string | null;
  housing_payment_cents: number | null;
  annual_fee_cents: number;
  captured_ytd_cents: number;
  open_remaining_cents: number;
  has_mortgage_benefit: boolean;
  transaction_count: number;
  last_transaction_date: string | null;
}

export interface CatalogBenefit {
  id: string;
  name: string;
  kind: string;
  unit: string;
  value: number;
  cadence: string;
  enrollment_required: boolean;
  category: string;
  how_to_use: string;
}

export interface Product {
  id: string;
  provider: string;
  issuer: string;
  name: string;
  annual_fee_cents: number;
  catalog_version: string;
  benefits: CatalogBenefit[];
}

export interface Txn {
  id: string;
  date: string;
  amount_cents: number;
  merchant: string;
  description: string;
  pending?: boolean;
}

export interface Match {
  id: string;
  source: 'credit' | 'purchase' | 'manual';
  amount: number;
  confidence: number;
  note: string;
  transaction: Txn | null;
}

export interface Offer {
  id: string;
  card_id: string;
  card_name: string;
  provider: string;
  merchant: string;
  reward: string;
  reward_cents: number | null;
  min_spend_cents: number;
  added_date: string;
  expires: string;
  status: 'active' | 'redeemed' | 'expired';
  days_left: number;
  redeemed_by: { date: string; amount_cents: number; description: string } | null;
}

export interface Connection {
  id: string;
  provider: string;
  institution_name: string | null;
  status: string;
  error: string | null;
  last_sync_at: string | null;
  cards: { id: string; name: string; last4: string | null }[];
}

export interface Alert {
  id: string;
  kind: string;
  title: string;
  body: string;
  created_at: string;
}

export interface PlaidAccount {
  account_id: string;
  name: string;
  mask: string | null;
  suggested_product_id: string | null;
  tracked: boolean;
}

export interface NewCard {
  product_id: string;
  nickname?: string;
  last4?: string | null;
  anniversary_month: number;
  opened_date?: string | null;
  account_ids?: string[];
  enrolled_benefit_ids?: string[];
  housing_payment_cents?: number | null;
}

/** Message to show for a failed request. */
export function errorText(e: unknown): string {
  if (e instanceof HttpErrorResponse) {
    if (e.status === 0) return 'Cannot reach the server.';
    return e.error?.error ?? `Request failed (${e.status}).`;
  }
  return e instanceof Error ? e.message : 'Something went wrong.';
}

@Injectable({ providedIn: 'root' })
export class Api {
  private http = inject(HttpClient);

  get<T>(path: string, params?: Record<string, string>): Promise<T> {
    return firstValueFrom(this.http.get<T>(`/api${path}`, { params }));
  }
  post<T>(path: string, body: unknown = {}): Promise<T> {
    return firstValueFrom(this.http.post<T>(`/api${path}`, body));
  }
  put<T>(path: string, body: unknown): Promise<T> {
    return firstValueFrom(this.http.put<T>(`/api${path}`, body));
  }
  patch<T>(path: string, body: unknown): Promise<T> {
    return firstValueFrom(this.http.patch<T>(`/api${path}`, body));
  }
  delete<T>(path: string): Promise<T> {
    return firstValueFrom(this.http.delete<T>(`/api${path}`));
  }

  /** One-tap check-off, shared by every list. */
  markUsed(periodId: string) {
    return this.post<{ period: Period }>(`/periods/${periodId}/override`, { mode: 'used' });
  }
}
