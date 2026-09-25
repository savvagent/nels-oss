// Accessibility: marketing-header interactive controls must expose a >=44px
// tap target on mobile (WCAG 2.5.5 / issue #26).
//
// jsdom cannot compute layout, so we assert the deliberate class-presence proxy:
// the daisyUI dropdown triggers carry `min-h-11` + `min-w-11` (2.75rem = 44px),
// which floor daisyUI's fixed btn height on mobile. The compact desktop size is
// restored only for mouse users via the `mouse:` variant (@media (pointer: fine)),
// so iPad portrait (768px, but a touch device) keeps the full 44px target. We
// assert the `mouse:min-h-0` / `mouse:min-w-0` tokens are present (the rendered
// DOM class token literally includes the colon) so a future deletion is caught.
// These class-presence checks do not prove computed sizes or responsive/pointer
// behavior (jsdom has no layout); that is confirmed by manual verification at a
// 375px mobile viewport and with a fine pointer, as noted on the PR.
import { describe, it, expect, beforeAll, vi } from 'vitest';
import { render } from '@testing-library/svelte';

// Register all catalogs + init() as an import side effect, then resolve the
// active locale so `$_` returns strings (not message keys) during render.
import '$lib/i18n/index.js';
import { waitLocale } from 'svelte-i18n';

// LanguagePicker reaches for `page.url` at init and calls `goto` on choose;
// neither is auto-mocked under Vitest, so stub them.
vi.mock('$app/navigation', () => ({ goto: () => {} }));
vi.mock('$app/state', () => ({ page: { url: new URL('http://localhost/en') } }));

import ThemePicker from './ThemePicker.svelte';
import LanguagePicker from './LanguagePicker.svelte';
import Cta from './Cta.svelte';

beforeAll(async () => {
  await waitLocale();
});

describe('header tap targets (issue #26)', () => {
  it('ThemePicker trigger carries the 44px tap-target classes', () => {
    const { container } = render(ThemePicker);
    const trigger = /** @type {HTMLElement} */ (container.querySelector('.dropdown > button'));
    expect(trigger).not.toBeNull();
    expect(trigger.classList.contains('min-h-11')).toBe(true);
    expect(trigger.classList.contains('min-w-11')).toBe(true);
    // Compact size is released only for fine pointers, not at the 768px breakpoint.
    expect(trigger.classList.contains('mouse:min-h-0')).toBe(true);
    expect(trigger.classList.contains('mouse:min-w-0')).toBe(true);
  });

  it('LanguagePicker trigger carries the 44px tap-target classes', () => {
    const { container } = render(LanguagePicker);
    const trigger = /** @type {HTMLElement} */ (container.querySelector('.dropdown > button'));
    expect(trigger).not.toBeNull();
    expect(trigger.classList.contains('min-h-11')).toBe(true);
    expect(trigger.classList.contains('min-w-11')).toBe(true);
    // Compact size is released only for fine pointers, not at the 768px breakpoint.
    expect(trigger.classList.contains('mouse:min-h-0')).toBe(true);
    expect(trigger.classList.contains('mouse:min-w-0')).toBe(true);
  });

  it('ThemePicker menu items carry the 44px tap-target height', () => {
    const { container } = render(ThemePicker);
    const items = container.querySelectorAll('.dropdown-content li button');
    expect(items.length).toBeGreaterThan(0);
    for (const item of items) {
      expect(item.classList.contains('min-h-11')).toBe(true);
    }
  });

  it('LanguagePicker menu items carry the 44px tap-target height', () => {
    const { container } = render(LanguagePicker);
    const items = container.querySelectorAll('.dropdown-content li button');
    expect(items.length).toBeGreaterThan(0);
    for (const item of items) {
      expect(item.classList.contains('min-h-11')).toBe(true);
    }
  });

  it('header CTA carries the 44px tap-target floor over the primary button base', () => {
    const { container } = render(Cta, { props: { classes: 'min-h-11 mouse:min-h-0 mouse:btn-sm' } });
    const cta = /** @type {HTMLElement} */ (container.querySelector('a.btn'));
    expect(cta).not.toBeNull();
    expect(cta.classList.contains('btn-primary')).toBe(true);
    expect(cta.classList.contains('min-h-11')).toBe(true);
  });
});
