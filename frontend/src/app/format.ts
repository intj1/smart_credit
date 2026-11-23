import { Period } from './api';

const usd0 = new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD', maximumFractionDigits: 0 });
const usd2 = new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD' });
const int = new Intl.NumberFormat('en-US');

export function money(cents: number | null | undefined): string {
  if (cents == null) return '—';
  return cents % 100 === 0 ? usd0.format(cents / 100) : usd2.format(cents / 100);
}

/** A benefit amount in its own unit: dollars, miles or points. */
export function amount(value: number, unit: string): string {
  return unit === 'usd' ? money(value) : `${int.format(value)} ${unit}`;
}

export function number(n: number): string {
  return int.format(n);
}

export function day(iso: string | null | undefined): string {
  if (!iso) return '—';
  const [y, m, d] = iso.slice(0, 10).split('-').map(Number);
  return new Date(y, m - 1, d).toLocaleDateString('en-US', { month: 'short', day: 'numeric', year: 'numeric' });
}

export function dateTime(iso: string | null | undefined): string {
  if (!iso) return 'never';
  return new Date(iso).toLocaleString('en-US', { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' });
}

export function deadline(p: { deadline: string | null; days_left: number | null }): string {
  if (p.days_left == null) return 'No deadline';
  if (p.days_left < 0) return `Ended ${day(p.deadline)}`;
  if (p.days_left === 0) return 'Ends today';
  if (p.days_left === 1) return 'Ends tomorrow';
  if (p.days_left <= 60) return `${p.days_left} days left`;
  return `Ends ${day(p.deadline)}`;
}

const STATUS: Record<string, string> = {
  not_started: 'Not started',
  partly_used: 'Partly used',
  pending_credit: 'Pending credit',
  needs_enrollment: 'Needs enrollment',
  used: 'Used',
  not_for_me: 'Not for me',
};

export function statusLabel(p: Period): string {
  if (p.days_left != null && p.days_left < 0 && p.status !== 'used' && p.status !== 'not_for_me') {
    return p.used > 0 ? 'Partly used, expired' : 'Expired unused';
  }
  return STATUS[p.status] ?? p.status;
}

export function statusClass(p: Period): string {
  if (p.days_left != null && p.days_left < 0 && p.status !== 'used' && p.status !== 'not_for_me') return 'chip-muted';
  return (
    {
      used: 'chip-good',
      partly_used: 'chip-info',
      pending_credit: 'chip-info',
      needs_enrollment: 'chip-warn',
      not_for_me: 'chip-muted',
    }[p.status] ?? ''
  );
}

export const PROVIDERS: { id: string; name: string }[] = [
  { id: 'capital_one', name: 'Capital One' },
  { id: 'amex', name: 'Amex' },
  { id: 'bilt', name: 'Bilt' },
];

export function providerName(id: string): string {
  return PROVIDERS.find((p) => p.id === id)?.name ?? id;
}

export const CADENCES: { id: string; name: string }[] = [
  { id: 'monthly', name: 'Monthly' },
  { id: 'quarterly', name: 'Quarterly' },
  { id: 'semiannual', name: 'Semiannual' },
  { id: 'annual', name: 'Annual' },
  { id: 'one_time', name: 'One-time' },
];

export function cadenceName(id: string): string {
  return CADENCES.find((c) => c.id === id)?.name ?? id;
}

export const CATEGORIES = ['travel', 'dining', 'shopping', 'subscriptions', 'other'];

export const MONTHS = ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December'];

export function cardLabel(c: { name?: string; card_name?: string; last4?: string | null; card_last4?: string | null }): string {
  const name = c.name ?? c.card_name ?? '';
  const last4 = c.last4 ?? c.card_last4;
  return last4 ? `${name} ··${last4}` : name;
}

/** "12.50" -> 1250. Returns null for anything that is not a non-negative amount. */
export function parseDollars(text: string | number | null | undefined): number | null {
  if (text == null || text === '') return null;
  const n = Number(String(text).replace(/[$,\s]/g, ''));
  return Number.isFinite(n) && n >= 0 ? Math.round(n * 100) : null;
}
