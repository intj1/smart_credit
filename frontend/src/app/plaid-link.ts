// Thin wrapper around Plaid Link (loaded from Plaid's CDN on demand). The
// user signs in on the issuer's own page; this app only receives a one-time
// public token, which the backend exchanges.

interface PlaidHandler {
  open(): void;
  destroy(): void;
}

declare global {
  interface Window {
    Plaid?: { create(config: Record<string, unknown>): PlaidHandler };
  }
}

export interface LinkResult {
  publicToken: string;
  institution: string | null;
}

const SCRIPT = 'https://cdn.plaid.com/link/v2/stable/link-initialize.js';
const PENDING_KEY = 'cbt.plaid.pending';

function loadScript(): Promise<void> {
  if (window.Plaid) return Promise.resolve();
  return new Promise((resolve, reject) => {
    const s = document.createElement('script');
    s.src = SCRIPT;
    s.onload = () => resolve();
    s.onerror = () => reject(new Error('Could not load Plaid Link. Check the network connection.'));
    document.head.appendChild(s);
  });
}

async function run(token: string, receivedRedirectUri?: string): Promise<LinkResult | null> {
  await loadScript();
  return new Promise((resolve, reject) => {
    const handler = window.Plaid!.create({
      token,
      ...(receivedRedirectUri ? { receivedRedirectUri } : {}),
      onSuccess: (publicToken: string, metadata: { institution?: { name?: string } | null }) => {
        sessionStorage.removeItem(PENDING_KEY);
        handler.destroy();
        resolve({ publicToken, institution: metadata?.institution?.name ?? null });
      },
      onExit: (err: { display_message?: string; error_message?: string } | null) => {
        sessionStorage.removeItem(PENDING_KEY);
        handler.destroy();
        if (err) reject(new Error(err.display_message || err.error_message || 'Plaid could not link that account.'));
        else resolve(null);
      },
    });
    handler.open();
  });
}

/** Opens Link. Resolves null when the user closes it without linking. */
export function openPlaidLink(token: string, provider: string, connectionId?: string): Promise<LinkResult | null> {
  // Kept for OAuth institutions, which leave this page and come back.
  sessionStorage.setItem(PENDING_KEY, JSON.stringify({ token, provider, connectionId }));
  return run(token);
}

export function pendingPlaidLink(): { token: string; provider: string; connectionId?: string } | null {
  try {
    return JSON.parse(sessionStorage.getItem(PENDING_KEY) ?? 'null');
  } catch {
    return null;
  }
}

/** Continues a Link session after the issuer redirected back to /oauth-return. */
export function resumePlaidLink(): Promise<LinkResult | null> {
  const pending = pendingPlaidLink();
  if (!pending) return Promise.resolve(null);
  return run(pending.token, window.location.href);
}
