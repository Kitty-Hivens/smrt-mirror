// Tiny reactive i18n. A module-level $state holds the active locale, `t()` reads
// it, so any component calling `t(...)` in its markup re-renders on a switch.
// Hand-rolled rather than a dependency: two locales and flat keys. Counted
// strings carry their forms and are selected through `Intl.PluralRules`.

import { en, type Dict } from './locales/en';
import { resolve } from './message';
import { ru } from './locales/ru';

export type Locale = 'ru' | 'en';
export const LOCALES: Locale[] = ['ru', 'en'];

const dicts: Record<Locale, Dict> = { ru, en };
import { withTransition } from './transition.svelte';

const STORAGE_KEY = 'smrt.locale';

function initialLocale(): Locale {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (saved === 'ru' || saved === 'en') return saved;
  } catch {
    // private mode / blocked storage -- fall through to default
  }
  return 'ru';
}

const startLocale = initialLocale();
let current = $state<Locale>(startLocale);
if (typeof document !== 'undefined') document.documentElement.lang = startLocale;

export const i18n = {
  get locale(): Locale {
    return current;
  },
  set(loc: Locale) {
    // The text of the whole panel changes at once. Crossfaded, that reads as one
    // act; unfaded it is a flicker across every word on screen, which is the one
    // change in this product where the eye has nothing to hold onto.
    withTransition('locale', () => {
      current = loc;
      try {
        localStorage.setItem(STORAGE_KEY, loc);
      } catch {
        // ignore -- in-memory locale still works for the session
      }
      if (typeof document !== 'undefined') document.documentElement.lang = loc;
    });
  },
  toggle() {
    this.set(current === 'ru' ? 'en' : 'ru');
  },
};

export type MsgKey = keyof Dict;

/// The three counted nouns of a change set, ready to drop into a sentence that
/// mentions all three. A sentence can only be counted on one number, so the
/// counting happens here and the sentence takes finished words.
export function changeWords(c: { add: number; remove: number; change: number }): {
  add: string;
  remove: string;
  change: string;
} {
  return {
    add: t('chg.nArrivals', { n: c.add }),
    remove: t('chg.nDepartures', { n: c.remove }),
    change: t('chg.nChanges', { n: c.change }),
  };
}

export function t(key: MsgKey, params?: Record<string, string | number>): string {
  const entry = dicts[current][key] ?? en[key];
  return entry === undefined ? key : resolve(entry, current, params);
}
