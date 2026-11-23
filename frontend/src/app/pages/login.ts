import { Component, OnInit, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Router } from '@angular/router';
import { Api, errorText } from '../api';
import { AuthState, NextStep } from '../auth';

@Component({
  selector: 'app-login',
  imports: [FormsModule],
  template: `
    <section class="narrow">
      <h1>Card Benefits</h1>
      @switch (step()) {
        @case ('password') {
          <form class="stack" (ngSubmit)="login()">
            <label>Email <input name="email" type="email" autocomplete="username" required [(ngModel)]="email" /></label>
            <label>Password <input name="password" type="password" autocomplete="current-password" required [(ngModel)]="password" /></label>
            <button class="primary" type="submit" [disabled]="busy()">Sign in</button>
          </form>
        }
        @case ('totp_setup') {
          <h2>Set up two-factor sign-in</h2>
          <p>Scan this code with an authenticator app (1Password, Google Authenticator, Authy), then enter the 6-digit code it shows.</p>
          @if (qr()) {
            <img class="qr" [src]="qr()" alt="QR code for your authenticator app" width="220" height="220" />
            <p class="muted">Can't scan? Enter this key by hand: <code>{{ secret() }}</code></p>
          }
          <form class="stack" (ngSubmit)="verify()">
            <label>6-digit code <input name="code" inputmode="numeric" autocomplete="one-time-code" maxlength="6" required [(ngModel)]="code" /></label>
            <button class="primary" type="submit" [disabled]="busy()">Turn on and continue</button>
          </form>
        }
        @case ('totp') {
          <form class="stack" (ngSubmit)="verify()">
            <label>Code from your authenticator app
              <input name="code" inputmode="numeric" autocomplete="one-time-code" maxlength="6" required [(ngModel)]="code" />
            </label>
            <button class="primary" type="submit" [disabled]="busy()">Verify</button>
            <button class="link" type="button" (click)="restart()">Use a different account</button>
          </form>
        }
      }
      @if (error()) {
        <p class="error" role="alert">{{ error() }}</p>
      }
    </section>
  `,
})
export class LoginPage implements OnInit {
  private api = inject(Api);
  private auth = inject(AuthState);
  private router = inject(Router);

  protected step = signal<'password' | 'totp' | 'totp_setup'>('password');
  protected busy = signal(false);
  protected error = signal('');
  protected qr = signal('');
  protected secret = signal('');
  protected email = '';
  protected password = '';
  protected code = '';

  async ngOnInit() {
    const next = await this.auth.check();
    if (next) await this.advance(next);
  }

  private async advance(next: NextStep) {
    if (next === 'ok') {
      this.auth.signedIn.set(true);
      await this.router.navigateByUrl('/');
      return;
    }
    if (next === 'totp_setup') {
      const setup = await this.api.post<{ secret: string; qr_svg: string }>('/auth/totp/setup');
      this.secret.set(setup.secret);
      this.qr.set('data:image/svg+xml;base64,' + btoa(setup.qr_svg));
    }
    this.step.set(next);
  }

  async login() {
    await this.run(async () => {
      const res = await this.api.post<{ next: NextStep }>('/auth/login', { email: this.email, password: this.password });
      this.password = '';
      await this.advance(res.next);
    });
  }

  async verify() {
    await this.run(async () => {
      const res = await this.api.post<{ next: NextStep }>('/auth/totp/verify', { code: this.code.trim() });
      this.code = '';
      await this.advance(res.next);
    });
  }

  async restart() {
    await this.api.post('/auth/logout').catch(() => undefined);
    this.step.set('password');
  }

  private async run(action: () => Promise<void>) {
    this.busy.set(true);
    this.error.set('');
    try {
      await action();
    } catch (e) {
      this.error.set(errorText(e));
    } finally {
      this.busy.set(false);
    }
  }
}
