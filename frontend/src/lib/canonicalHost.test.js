import { describe, it, expect } from 'vitest';
import { canonicalRedirect } from './canonicalHost.js';

const loc = (hostname, pathname = '/', search = '', hash = '') => ({ hostname, pathname, search, hash });

describe('canonicalRedirect', () => {
  it('sends the nels.pages.dev alias to app.nels.money, keeping path, query and hash', () => {
    expect(canonicalRedirect(loc('nels.pages.dev', '/budgets', '?x=1', '#top')))
      .toBe('https://app.nels.money/budgets?x=1#top');
  });

  it('leaves the canonical domain alone', () => {
    expect(canonicalRedirect(loc('app.nels.money', '/budgets'))).toBeNull();
  });

  it('leaves preview deployments and local dev alone', () => {
    expect(canonicalRedirect(loc('abc123.nels.pages.dev'))).toBeNull();
    expect(canonicalRedirect(loc('localhost'))).toBeNull();
  });
});
