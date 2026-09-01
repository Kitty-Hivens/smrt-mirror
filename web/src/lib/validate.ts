import { t, type MsgKey } from './i18n.svelte';

// What the mirror will refuse, said before it refuses it.
//
// Around ten error signals existed across every view, so most fields said
// nothing and a bad value surfaced as a toast after saving: you learned a value
// was wrong after trying to use it, not while typing it (#55).
//
// Every rule here mirrors one the server actually enforces, and says which --
// inventing a stricter client rule would reject values the mirror accepts, and a
// looser one would promise a save that fails. The server stays the authority;
// this is only the same sentence, earlier.

/// `is_flat_id` in `storage.rs`: the id segment of a pack or a server.
export function idError(v: string): string | null {
  const s = v.trim();
  if (!s) return 'required';
  if (s.length > 64) return 'tooLong';
  if (s.startsWith('.')) return 'leadingDot';
  if (!/^[A-Za-z0-9._-]+$/.test(s)) return 'idChars';
  return null;
}

/// `resolve_mod` in `authoring/sources.rs`: the launcher writes `mods/<filename>`,
/// so a separator or a leading dot is a path escape rather than a name.
export function filenameError(v: string): string | null {
  const s = v.trim();
  if (!s) return 'required';
  if (s.startsWith('.')) return 'leadingDot';
  if (s.includes('/') || s.includes('\\')) return 'filenameSlash';
  return null;
}

/// `is_safe_rel_path` in `storage.rs`: a destination inside the instance. Nested
/// directories are allowed; every segment is a plain token, and the charset
/// admits the brackets, spaces and pluses real resourcepack names carry.
export function relPathError(v: string): string | null {
  const s = v.trim();
  if (!s) return 'required';
  if (s.length > 512) return 'tooLong';
  if (s.startsWith('/') || s.includes('\\')) return 'relPathAbsolute';
  const segments = s.split('/');
  if (segments.some((seg) => !seg || seg.startsWith('.'))) return 'relPathSegment';
  if (segments.some((seg) => !/^[A-Za-z0-9._ ()+,[\]-]+$/.test(seg))) return 'relPathChars';
  return null;
}

/// A link the panel will render and a launcher may open. Only the shape is
/// checked -- whether it resolves is not something a form can know.
export function urlError(v: string): string | null {
  const s = v.trim();
  if (!s) return null; // every URL field in the panel is optional
  try {
    const u = new URL(s);
    return u.protocol === 'http:' || u.protocol === 'https:' ? null : 'urlScheme';
  } catch {
    return 'urlShape';
  }
}

/// A pack-card image: either a URL somebody wrote, or a path inside the pack's
/// own static tree, which is what the branding upload fills in and what the
/// build resolves against the mirror (`pack_asset_url` in `authoring/build.rs`).
/// The second is the ordinary case, so it is judged by the same rule the
/// upload's destination is.
export function cardImageError(v: string): string | null {
  const s = v.trim();
  if (!s) return null; // optional, like every image field on the card
  if (/^(https?:\/\/|\/\/)/i.test(s)) return urlError(s);
  return relPathError(s);
}

/// The Java major the launcher provisions: a whole number, and 8 at the oldest
/// since nothing older is shipped.
export function javaError(v: number | null | undefined): string | null {
  if (v == null || Number.isNaN(v)) return 'required';
  if (!Number.isInteger(v)) return 'javaWhole';
  if (v < 8) return 'javaFloor';
  return null;
}

/// Required free text -- a display name, a version string. No shape is imposed
/// on these: the mirror does not impose one either.
export function requiredError(v: string): string | null {
  return v.trim() ? null : 'required';
}

/// The sentence for a verdict. The rules return a key rather than prose so the
/// same verdict reads the same wherever it is shown, in either language.
export function say(code: string | null): string | null {
  return code ? t(`val.${code}` as MsgKey) : null;
}
