import { Routes } from '@angular/router';
import { authGuard } from './auth';
import { AddAccountPage } from './pages/add-account';
import { BenefitPage } from './pages/benefit';
import { CardPage } from './pages/card';
import { CardsPage } from './pages/cards';
import { HomePage } from './pages/home';
import { LoginPage } from './pages/login';
import { OffersPage } from './pages/offers';
import { SettingsPage } from './pages/settings';
import { UnclaimedPage } from './pages/unclaimed';

export const routes: Routes = [
  { path: 'login', component: LoginPage, title: 'Sign in · Card Benefits' },
  {
    path: '',
    canActivate: [authGuard],
    children: [
      { path: '', component: HomePage, title: 'Home · Card Benefits' },
      { path: 'unclaimed', component: UnclaimedPage, title: 'Unclaimed · Card Benefits' },
      { path: 'cards', component: CardsPage, title: 'Cards · Card Benefits' },
      { path: 'cards/:id', component: CardPage, title: 'Card · Card Benefits' },
      { path: 'periods/:id', component: BenefitPage, title: 'Benefit · Card Benefits' },
      { path: 'offers', component: OffersPage, title: 'Offers · Card Benefits' },
      { path: 'add', component: AddAccountPage, title: 'Add account · Card Benefits' },
      // Plaid sends OAuth institutions (Capital One, Amex) back here.
      { path: 'oauth-return', component: AddAccountPage, title: 'Add account · Card Benefits' },
      { path: 'settings', component: SettingsPage, title: 'Settings · Card Benefits' },
    ],
  },
  { path: '**', redirectTo: '' },
];
