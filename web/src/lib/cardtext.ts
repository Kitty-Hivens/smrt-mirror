// A pack card written in more than one language, read in the viewer's (#195).
//
// What ships is settled on the mirror: a language left blank is dropped from the
// map, and the untagged field is filled from the map when the curator wrote only
// translations. So nothing is decided here beyond the lookup itself -- the
// reader's language when the card has it, the untagged copy otherwise, which is
// the rule the API guide states and the one a launcher follows. The preview
// claims to be what a player sees, and that claim only holds while both ends
// pick the same text.

export type ByLanguage = { [key: string]: string } | null | undefined;

export function inLanguage(
  untagged: string | null | undefined,
  byLanguage: ByLanguage,
  locale: string,
): string {
  const written = byLanguage?.[locale.trim().toLowerCase()]?.trim();
  return written || untagged?.trim() || '';
}
