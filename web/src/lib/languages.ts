// The languages a pack's own text can be written in.
//
// Not the same list as the languages the panel speaks, and the two were one
// list until a card could carry translations at all. The panel has dictionaries
// for two languages because somebody wrote them; what a curator writes a card
// or a release note in is bounded instead by what a reader's client can render,
// and the launcher reads four. Tying the card to the panel's own two meant a
// German or Japanese player was unreachable whatever the client did.
//
// A mirror serving another audience edits this line. The wire takes any tag and
// the mirror lower-cases it on the way in, so nothing but this list is in the
// way of one more.
export const TEXT_LANGUAGES = ['ru', 'en', 'de', 'ja'];

/// The tags to offer for one pack: the ones above, plus any this pack already
/// carries. A language written before this list knew about it stays editable
/// rather than turning invisible the moment nobody offers its tab.
export function languagesFor(...written: (Record<string, string> | null | undefined)[]): string[] {
  const out = [...TEXT_LANGUAGES];
  for (const map of written) {
    for (const tag of Object.keys(map ?? {})) {
      const clean = tag.trim().toLowerCase();
      if (clean && !out.includes(clean)) out.push(clean);
    }
  }
  return out;
}
