import { HttpErrorResponse, HttpInterceptorFn } from '@angular/common/http';
import { Injectable, inject, signal } from '@angular/core';
import { CanActivateFn, Router } from '@angular/router';
import { tap } from 'rxjs';
import { Api } from './api';

export type NextStep = 'ok' | 'totp' | 'totp_setup';

@Injectable({ providedIn: 'root' })
export class AuthState {
  private api = inject(Api);
  readonly signedIn = signal(false);
  readonly email = signal('');

  /** Where the session stands, or null when there is none. */
  async check(): Promise<NextStep | null> {
    try {
      const me = await this.api.get<{ email: string; next: NextStep }>('/auth/me');
      this.email.set(me.email);
      this.signedIn.set(me.next === 'ok');
      return me.next;
    } catch {
      this.signedIn.set(false);
      return null;
    }
  }
}

export const authGuard: CanActivateFn = async () => {
  const auth = inject(AuthState);
  const router = inject(Router);
  if (auth.signedIn()) return true;
  return (await auth.check()) === 'ok' ? true : router.parseUrl('/login');
};

/** A session that expires mid-use sends the user back to the login screen. */
export const unauthorizedInterceptor: HttpInterceptorFn = (req, next) => {
  const auth = inject(AuthState);
  const router = inject(Router);
  return next(req).pipe(
    tap({
      error: (e) => {
        if (e instanceof HttpErrorResponse && e.status === 401 && !req.url.includes('/auth/')) {
          auth.signedIn.set(false);
          router.navigateByUrl('/login');
        }
      },
    }),
  );
};
