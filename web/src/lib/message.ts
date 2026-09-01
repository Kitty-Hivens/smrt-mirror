// Turning a dictionary entry into the string on screen: picking the form the
// number calls for, then filling the placeholders.
//
// Separate from `i18n.svelte.ts` because none of it is reactive. The rune module
// owns which locale is current, this owns what a message reads like, and being
// plain means it can be checked without a browser or a Svelte runtime.

/// A counted string, in the forms the language actually distinguishes. English
/// uses one and other. Russian needs few and many as well, because 1 mod, 2 mods
/// and 5 mods are three different words. Which form applies is
/// `Intl.PluralRules`' decision, so a locale added later brings its own rules
/// rather than a rule written here.
export type PluralForms = {
  one: string;
  few?: string;
  many?: string;
  other: string;
};

export type Entry = string | PluralForms;

// One per locale, built once. Constructing an Intl object is not free, and a
// counted label can appear on every row of a list.
const rules = new Map<string, Intl.PluralRules>();
function rulesFor(locale: string): Intl.PluralRules {
  let r = rules.get(locale);
  if (!r) {
    r = new Intl.PluralRules(locale);
    rules.set(locale, r);
  }
  return r;
}

/// The count drives the choice, so a counted entry needs one. `n` is the name
/// nearly every such message uses, `count` is the older one a few still carry.
/// A form the language does not distinguish falls back to `other`.
export function pickForm(
  forms: PluralForms,
  locale: string,
  params?: Record<string, string | number>,
): string {
  const raw = params?.n ?? params?.count;
  const n = typeof raw === 'number' ? raw : Number(raw ?? 0);
  const category = rulesFor(locale).select(Number.isFinite(n) ? n : 0);
  return forms[category as keyof PluralForms] ?? forms.other;
}

export function resolve(
  entry: Entry,
  locale: string,
  params?: Record<string, string | number>,
): string {
  let s = typeof entry === 'string' ? entry : pickForm(entry, locale, params);
  if (params) {
    for (const [k, v] of Object.entries(params)) {
      s = s.replaceAll(`{${k}}`, String(v));
    }
  }
  return s;
}
