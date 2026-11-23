import { Component, inject } from '@angular/core';
import { Router, RouterLink, RouterLinkActive, RouterOutlet } from '@angular/router';
import { Api } from './api';
import { AuthState } from './auth';

@Component({
  selector: 'app-root',
  imports: [RouterOutlet, RouterLink, RouterLinkActive],
  template: `
    @if (auth.signedIn()) {
      <header class="topbar">
        <a class="brand" routerLink="/">Card Benefits</a>
        <nav aria-label="Main">
          <a routerLink="/" routerLinkActive="active" [routerLinkActiveOptions]="{ exact: true }">Home</a>
          <a routerLink="/unclaimed" routerLinkActive="active">Unclaimed</a>
          <a routerLink="/cards" routerLinkActive="active">Cards</a>
          <a routerLink="/offers" routerLinkActive="active">Offers</a>
          <a routerLink="/settings" routerLinkActive="active">Settings</a>
        </nav>
        <button class="link" type="button" (click)="signOut()">Sign out</button>
      </header>
    }
    <main class="page"><router-outlet /></main>
  `,
})
export class App {
  protected auth = inject(AuthState);
  private api = inject(Api);
  private router = inject(Router);

  async signOut() {
    try {
      await this.api.post('/auth/logout');
    } finally {
      this.auth.signedIn.set(false);
      this.router.navigateByUrl('/login');
    }
  }
}
