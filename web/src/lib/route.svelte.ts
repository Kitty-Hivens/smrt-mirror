// Which section the shell is showing. A module-level $state shared by the rail
// (which highlights + sets it) and the content area (which renders by it).
// Persisted so a refresh keeps you where you were.

import { isOperator } from './roles';

export type Section =
  | 'browse'
  | 'overview'
  | 'packs'
  | 'servers'
  | 'mods'
  | 'graph'
  | 'users'
  | 'moderation'
  | 'audit'
  | 'profile'
  | 'settings'
  | 'mypacks';
// The operator's official tabs. `mypacks` is not among them because it is a
// personal surface, not an operator one -- but every account has it, admins
// included (their own community packs under `u/<uid>/`, distinct from the
// official packs they author via `packs`), so `visibleSections` appends it for
// operators too. KNOWN_SECTIONS is the superset used to validate a stored tab.
export const SECTIONS: Section[] = [
  'browse',
  'overview',
  'packs',
  'servers',
  'mods',
  'graph',
  'users',
  'moderation',
  'audit',
  'profile',
  'settings',
];
// Guest sees only the public catalog; a signed-in member also gets their own
// packs and profile; everything else is operator-only.
// Settings is everyone's: the theme is a preference of whoever is looking, not
// a privilege. A guest gets the catalog and their own preferences, nothing else.
export const GUEST_SECTIONS: Section[] = ['browse', 'settings'];
// The registry browser (mods) and the graph are read-only for a member -- the
// views gate their own authoring, and the data is already public per-mod on the
// mod page -- so a member building a community pack gets the same read of what the
// mirror indexes, and the same "does this hold together" view, an operator has.
export const MEMBER_SECTIONS: Section[] = [
  'browse',
  'mods',
  'graph',
  'mypacks',
  'profile',
  'settings',
];
const KNOWN_SECTIONS: Section[] = [...SECTIONS, 'mypacks'];
export function visibleSections(me: { role: string } | null): Section[] {
  if (!me) return GUEST_SECTIONS;
  if (isOperator(me.role)) return [...SECTIONS, 'mypacks'];
  return MEMBER_SECTIONS;
}

const STORAGE_KEY = 'smrt.section';

// The panel's state lives in the URL. It did not before: navigation was a
// variable plus a localStorage key, so the browser had no idea anything had
// happened -- back and forward (and the mouse buttons wired to them) did
// nothing or left the app entirely, a reload lost an open mod page, and there
// was no way to send anyone a link to what you were looking at.
//
// `/` restores your last section, `/<section>` selects one, `/mod/<ref>` opens
// a mod over it. The server serves the app shell for any path it does not
// claim, so these survive a reload.

function sectionFromPath(path: string): Section | null {
  const seg = path.replace(/^\/+|\/+$/g, '').split('/')[0];
  // the old sha1 'cache' tab was replaced by mod management
  if (seg === 'cache') return 'mods';
  return KNOWN_SECTIONS.includes(seg as Section) ? (seg as Section) : null;
}

function storedSection(): Section {
  try {
    const s = localStorage.getItem(STORAGE_KEY);
    if (s === 'cache') return 'mods';
    if (s && KNOWN_SECTIONS.includes(s as Section)) return s as Section;
  } catch {
    // blocked storage -- default below
  }
  return 'browse';
}

/// A mod ref out of `/mod/<ref>`: a numeric id, or `sha1:<hash>` for a jar the
/// pack knows by hash rather than by registry id.
function modFromPath(path: string): string | null {
  const m = path.match(/^\/mod\/(.+)$/);
  return m ? decodeURIComponent(m[1]) : null;
}

/// The pack being edited, out of `/packs/<id>` (operator) or `/mypacks/<id>`
/// (member). A community id carries slashes (`u/<uid>/<pack>`), so it rides
/// percent-encoded in the one segment.
function packFromPath(path: string): string | null {
  // A commit sits under the pack that declared it, so the pack is still open
  // behind it -- and an id written out unencoded (an older link) still resolves,
  // which is why this reads the commit form first rather than tightening the id.
  const c = path.match(/^\/(?:packs|mypacks)\/(.+)\/(?:commit\/[^/]+|thread\/\d+)$/);
  if (c) return decodeURIComponent(c[1]);
  const m = path.match(/^\/(?:packs|mypacks)\/(.+)$/);
  return m ? decodeURIComponent(m[1]) : null;
}

/// The commit being read, out of `/packs/<id>/commit/<sha>`. A checkpoint is a
/// place: it has an address, back leaves it, and the link can be handed to
/// whoever asks what a build was made from.
function commitFromPath(path: string): string | null {
  const m = path.match(/^\/(?:packs|mypacks)\/.+\/commit\/([^/]+)$/);
  return m ? decodeURIComponent(m[1]) : null;
}

/// The discussion being read, out of `/packs/<id>/thread/<n>` inside an editor,
/// or `/thread/<n>` on its own. A thread is a place for the same reason a commit
/// is: it is what somebody links to when they say "see the report" -- and the
/// bare form is the one that can be handed to somebody who has no account here,
/// which is the whole point of a discussion being public.
function threadFromPath(path: string): number | null {
  const m = path.match(/^(?:\/(?:packs|mypacks)\/.+)?\/thread\/(\d+)$/);
  return m ? Number(m[1]) : null;
}

/// The catalog pack being read, out of `/browse/<id>`. The catalog is the
/// showcase, and a pack in it is a place somebody links to, so it has an
/// address the way a mod does. A community id rides percent-encoded in the one
/// segment, like the editor's.
function catalogFromPath(path: string): string | null {
  const m = path.match(/^\/browse\/(.+)$/);
  return m ? decodeURIComponent(m[1]) : null;
}

/// What the page that linked to a mod already knew about it, carried in the
/// history entry. A jar the registry has not indexed has no page of its own to
/// load, yet the manifest that listed it knows its name, source and size. The
/// entry keeps it across a reload. A shared link carries none, and the page
/// says only what the address does.
export type ModHint = {
  name: string;
  filename: string;
  size_bytes: number;
  source: string;
  pack_id?: string;
  pack_name?: string;
};

function hintFromHistory(): ModHint | null {
  const h = (history.state as { modHint?: ModHint } | null)?.modHint;
  return h && typeof h.filename === 'string' ? h : null;
}

function initial(): Section {
  return sectionFromPath(location.pathname) ?? storedSection();
}

let section = $state<Section>(initial());
// A focused mod page overlays whatever section is active: set, the content area
// renders the mod page instead of the section; cleared, it returns to `section`.
// Reachable from the registry, a pack's mod list, and the graph, so it lives here
// rather than as one view's local state. Not persisted -- a refresh lands on the
// underlying section, not a deep mod link (the store has no URL to restore from).
// The value is a mod ref the API accepts: a numeric id (graph / registry) or
// `sha1:<hash>` (a pack's mod list has the jar's sha1, not the id).
let focusMod = $state<string | null>(modFromPath(location.pathname));
// The open pack editor, held here rather than in the view that mounts it: it is
// the deepest surface in the panel and it used to be pure local state, so
// nothing about opening it entered history and back could not close it (#54).
// As a location it closes on back, survives a reload and can be linked to.
let editPack = $state<string | null>(packFromPath(location.pathname));
// The commit open over the editor. Same reasoning as the pack itself: it is a
// location, so it survives a reload and can be linked to.
let focusCommit = $state<string | null>(commitFromPath(location.pathname));
let focusThread = $state<number | null>(threadFromPath(location.pathname));
let focusCatalog = $state<string | null>(catalogFromPath(location.pathname));
let modHint = $state<ModHint | null>(typeof history !== 'undefined' ? hintFromHistory() : null);
// The name of the place on screen, for the crumb. The route knows a mod or a
// pack by its ref; only the page that loaded it knows what it is called.
let crumbLabel = $state<string | null>(null);

// What the editor wants asked before it is left with edits the server has not
// accepted. It lives on the route rather than on the Close button, because
// leaving is now something back, a trackpad gesture and the rail can all do --
// each of them would otherwise skip the very check the button exists for.
let leaveGuard: (() => Promise<boolean>) | null = null;

/// Run the pending guard, if leaving would abandon an open editor. `true` means
/// go ahead; a guard that answers once is spent, so the same question is not
/// asked twice on the way out.
async function mayLeaveEditor(): Promise<boolean> {
  if (!leaveGuard || editPack === null) return true;
  if (!(await leaveGuard())) return false;
  leaveGuard = null;
  return true;
}

/// Push a URL for a state the user navigated to, so it becomes a history entry
/// they can come back from. Replacing (rather than pushing) the very first
/// entry keeps `/` from sitting behind every session as a dead step.
/// How many entries this session pushed. Going back is only a way out of a
/// pane if the app put something behind it: a link opened straight into a
/// commit has the previous site there, and `history.back()` would leave.
let pushed = 0;

function pushPath(path: string, replace = false, state: object = {}) {
  if (location.pathname === path) return;
  history[replace ? 'replaceState' : 'pushState'](state, '', path);
  if (!replace) pushed += 1;
}

function remember(s: Section) {
  try {
    localStorage.setItem(STORAGE_KEY, s);
  } catch {
    // session-only navigation still works
  }
}

// The URL is the truth on the way back: whatever the browser restores, the
// store follows -- without pushing, or every back press would leave a new entry.
if (typeof window !== 'undefined') {
  window.addEventListener('popstate', async () => {
    if (pushed > 0) pushed -= 1;
    const pack = packFromPath(location.pathname);
    // History has already moved by the time this fires, so a refused leave has
    // to put the editor's entry back. `forward()` walks to the entry we just
    // left instead of pushing a new one, so declining does not pile up steps.
    if (pack === null && !(await mayLeaveEditor())) {
      history.forward();
      return;
    }
    editPack = pack;
    focusCommit = commitFromPath(location.pathname);
    focusThread = threadFromPath(location.pathname);
    focusCatalog = catalogFromPath(location.pathname);
    const mod = modFromPath(location.pathname);
    focusMod = mod;
    modHint = mod ? hintFromHistory() : null;
    if (!mod) {
      const s = sectionFromPath(location.pathname);
      if (s) {
        section = s;
        remember(s);
      }
    }
  });
  // a bare `/` restores the last section without leaving an extra entry behind;
  // a deep link to a mod or a discussion is the address it already is
  if (
    !sectionFromPath(location.pathname) &&
    !modFromPath(location.pathname) &&
    threadFromPath(location.pathname) === null
  ) {
    pushPath(`/${section}`, true);
  }
  remember(section);
}

/// The address a destination has, for the `href` of the control that goes
/// there. Every one of these was a click handler on a button: the addresses
/// existed and nothing wore them, so the browser had nothing to act on -- no
/// middle click, no ctrl-click, nothing to copy, and a screen reader announcing
/// "button" for a place.
export const href = {
  section: (s: Section) => `/${s}`,
  mod: (ref: number | string) => `/mod/${encodeURIComponent(String(ref))}`,
  pack: (id: string, from: Section = section) => `/${from}/${encodeURIComponent(id)}`,
  commit: (id: string, commitId: string, from: Section = section) =>
    `/${from}/${encodeURIComponent(id)}/commit/${encodeURIComponent(commitId)}`,
  thread: (id: string, threadId: number, from: Section = section) =>
    `/${from}/${encodeURIComponent(id)}/thread/${threadId}`,
  /// A discussion with no editor under it -- what the catalog links to, and
  /// what a guest can be handed.
  discussion: (threadId: number) => `/thread/${threadId}`,
  /// A pack's page in the catalog.
  catalog: (id: string) => `/browse/${encodeURIComponent(id)}`,
};

/// True for a click the app should handle itself. A modified or middle click is
/// the browser being asked for a new tab or window, and must be left alone.
export function plainClick(e: MouseEvent): boolean {
  return !e.defaultPrevented && e.button === 0 && !e.metaKey && !e.ctrlKey && !e.shiftKey && !e.altKey;
}

export const route = {
  get section(): Section {
    return section;
  },
  get mod(): string | null {
    return focusMod;
  },
  /// The pack whose editor is open, or null. Driven by the URL, so back closes
  /// the editor and a link reopens it.
  get pack(): string | null {
    return editPack;
  },
  /// The commit being read over the open editor, or null.
  get commit(): string | null {
    return focusCommit;
  },
  /// The discussion open over the editor, or null.
  get thread(): number | null {
    return focusThread;
  },
  /// The catalog pack whose page is open, or null.
  get catalog(): string | null {
    return focusCatalog;
  },
  /// What the linking page knew about the open mod, or null.
  get modHint(): ModHint | null {
    return modHint;
  },
  get crumb(): string | null {
    return crumbLabel;
  },
  /// Name the place on screen; `null` when the page leaves.
  setCrumb(label: string | null) {
    crumbLabel = label;
  },
  /// Open a pack's page in the catalog. Always under `browse`, whichever
  /// section the link was on: the page belongs to the catalog.
  openCatalog(id: string) {
    focusMod = null;
    editPack = null;
    focusCommit = null;
    focusThread = null;
    section = 'browse';
    remember('browse');
    focusCatalog = id;
    pushPath(href.catalog(id));
  },
  /// Back to the catalog the way back does, or onto it when this session put
  /// nothing behind the page (a shared link, a reload).
  closeCatalog() {
    if (catalogFromPath(location.pathname) && pushed > 0) {
      history.back();
      return;
    }
    focusCatalog = null;
    pushPath('/browse', true);
  },
  /// `replace` is for a correction rather than a navigation -- landing on a
  /// section your role cannot see should not leave a step to go back to.
  async go(s: Section, replace = false) {
    if (!(await mayLeaveEditor())) return;
    focusMod = null; // picking a section leaves any open mod page
    modHint = null;
    focusCatalog = null;
    editPack = null;
    focusCommit = null;
    focusThread = null;
    section = s;
    remember(s);
    pushPath(`/${s}`, replace);
  },
  /// Open a pack's editor as a location under the section it was opened from,
  /// so back closes it and the URL can be shared.
  openPack(id: string) {
    editPack = id;
    focusCommit = null;
    focusThread = null;
    pushPath(`/${section}/${encodeURIComponent(id)}`);
  },
  /// Open a pack's editor from another section (the overview's recent list), as
  /// a single step: the section and the editor arrive in one history entry
  /// rather than two, so back goes where it was clicked from.
  openPackIn(s: Section, id: string) {
    focusMod = null;
    section = s;
    remember(s);
    editPack = id;
    focusCommit = null;
    focusThread = null;
    pushPath(href.pack(id, s));
  },
  /// Close the editor the same way the back button does, so both routes through
  /// the unsaved-changes guard are the one route.
  closePack() {
    focusCommit = null;
    if (packFromPath(location.pathname) && pushed > 0) {
      history.back();
      return;
    }
    editPack = null;
    pushPath(`/${section}`, true);
  },
  /// Open a commit over the pack that declared it. The editor stays where it is
  /// underneath, so closing the commit returns to it rather than to a section.
  openCommit(packId: string, commitId: string) {
    editPack = packId;
    focusCommit = commitId;
    pushPath(href.commit(packId, commitId));
  },
  /// Leave a commit the same way back does, so both routes are one route.
  /// Open a discussion over the pack it belongs to.
  openThread(packId: string, threadId: number) {
    editPack = packId;
    focusCommit = null;
    focusThread = threadId;
    pushPath(href.thread(packId, threadId));
  },
  /// Read a discussion on its own, without opening the pack behind it -- the
  /// catalog's way in, and the only one a guest has.
  readThread(threadId: number) {
    editPack = null;
    focusCommit = null;
    focusMod = null;
    focusThread = threadId;
    pushPath(href.discussion(threadId));
  },
  /// Leave it the way back does, falling back to the pack when this session
  /// pushed nothing to go back to (a shared link, a reload) -- or to the
  /// section, when the discussion was standing on its own.
  closeThread() {
    if (threadFromPath(location.pathname) !== null && pushed > 0) {
      history.back();
      return;
    }
    focusThread = null;
    pushPath(editPack !== null ? href.pack(editPack) : `/${section}`, true);
  },
  closeCommit() {
    if (commitFromPath(location.pathname) && pushed > 0) {
      history.back();
      return;
    }
    // Nothing of ours behind it -- a shared link, or a reload. Going back would
    // leave the panel, so the pack it belongs to takes its place instead.
    focusCommit = null;
    if (editPack !== null) pushPath(href.pack(editPack), true);
  },
  /// Register what to ask before an open editor is left; `null` clears it. The
  /// editor sets this while it holds edits the server has not accepted.
  setLeaveGuard(fn: (() => Promise<boolean>) | null) {
    leaveGuard = fn;
  },
  // Open a mod's page over the current section; `closeMod` returns to it. `ref`
  // is a numeric mod id or a `sha1:<hash>` artifact reference.
  openMod(ref: number | string, hint: ModHint | null = null) {
    focusMod = String(ref);
    modHint = hint;
    pushPath(`/mod/${encodeURIComponent(String(ref))}`, false, hint ? { modHint: hint } : {});
  },
  closeMod() {
    // back rather than a fresh entry: the mod page was opened from the section
    // underneath, and closing it is the same move as pressing back -- unless
    // this session pushed nothing, which is what a shared link looks like, and
    // back would leave the panel.
    if (modFromPath(location.pathname) && pushed > 0) {
      history.back();
      return;
    }
    focusMod = null;
    modHint = null;
    pushPath(focusCatalog !== null ? href.catalog(focusCatalog) : `/${section}`, true);
  },
};
