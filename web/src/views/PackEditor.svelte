<script lang="ts">
  import { untrack } from 'svelte';
  import { flip } from 'svelte/animate';
  import { api, ApiError } from '../lib/api';
  import { dialogs } from '../lib/dialogs.svelte';
  import { route } from '../lib/route.svelte';
  import { t } from '../lib/i18n.svelte';
  import { advertisesModList } from '../lib/handshake';
  import { assetPath } from '../lib/packassets';
  import { arrive, stagger } from '../lib/motion.svelte';
  import { openPackSession, type PackSession } from '../lib/packsession.svelte';
  import { JAVA_MAJORS, suggestedJava } from '../lib/java';
  import { changedPaths, createTouches } from '../lib/touched.svelte';
  import type { LoaderVersions, MinecraftVersions, SpoofReport } from '../lib/types';
  import { detailOf, notifyFail, toasts } from '../lib/toasts.svelte';
  import { isDebug } from '../lib/roles';
  import type {
    CommitLogEntry,
    DeclaredAsset,
    DeclaredMod,
    JobStatus,
    PackConfig,
    PackEvent,
    PackLevel,
    ResolveReport,
    SourceDecl,
    ValidateReport,
  } from '../lib/types';
  import BuildConsole from './BuildConsole.svelte';
  import CommitPage from './CommitPage.svelte';
  import PackLog from './PackLog.svelte';
  import PackAccess from './PackAccess.svelte';
  import PackThreads from './PackThreads.svelte';
  import ThreadPage from './ThreadPage.svelte';
  import BrandingEditor from './BrandingEditor.svelte';
  import PackGraph from './PackGraph.svelte';
  import JobLog from './JobLog.svelte';
  import ModIcon from './ModIcon.svelte';
  import ResolvePanel from './ResolvePanel.svelte';
  import ModrinthPicker from './ModrinthPicker.svelte';
  import MirrorPicker from './MirrorPicker.svelte';
  import ModPicker from './ModPicker.svelte';
  import GithubPicker from './GithubPicker.svelte';
  import PackPreview from './PackPreview.svelte';
  import DropZone from './ui/DropZone.svelte';
  import Field from './ui/Field.svelte';
  import {
    cardImageError,
    filenameError,
    javaError,
    relPathError,
    requiredError,
    say,
  } from '../lib/validate';
  import Section from './ui/Section.svelte';
  import Select from './ui/Select.svelte';
  import TabStrip from './ui/TabStrip.svelte';
  import FloatDock from './ui/FloatDock.svelte';

  const MOD_SOURCE_OPTIONS = [
    { value: 'smrt_cache', label: 'cache' },
    { value: 'modrinth', label: 'modrinth' },
    { value: 'smrt_static', label: 'static' },
  ];
  const ASSET_SOURCE_OPTIONS = [
    { value: 'smrt_static', label: 'static' },
    { value: 'modrinth', label: 'modrinth' },
    { value: 'smrt_cache', label: 'cache' },
  ];
  // The loaders the registry models via loader_parent, offered as a picker rather
  // than a free-text field. An unrecognised value already on a config (a loader we
  // don't list) is kept as its own option so editing never silently drops it.
  const KNOWN_LOADERS = ['forge', 'cleanroom', 'neoforge', 'fabric', 'quilt'];

  let {
    packId,
    onClose,
    me,
  }: { packId: string; onClose: () => void; me: { login: string } } = $props();

  // GitHub-style danger delete: type "<login>/<pack>" in a modal to confirm.
  async function deletePack() {
    const expected = `${me.login}/${packId.split('/').pop()}`;
    const typed = await dialogs.prompt(t('packs.deleteConfirm', { id: expected }), {
      title: t('packs.deleteTitle'),
      placeholder: expected,
    });
    if (typed == null) return;
    if (typed.trim() !== expected) {
      toasts.push({ kind: 'error', text: t('packs.deleteMismatch') });
      return;
    }
    try {
      await api.deletePack(packId);
      onClose();
    } catch (e) {
      fail(e);
    }
  }

  // debug operators can force an archival upload past the Modrinth-coverage gate
  let canDebug = $state(false);
  api
    .me()
    .then((m) => (canDebug = isDebug(m?.role)))
    .catch(() => {});

  // Upload a self-hosted jar for this community pack -- it enters the moderation
  // queue; once approved it is in the shared cache to add via "from mirror". The
  // uploader names the jar's upstream origin for archival provenance.
  async function onUploadJar(e: Event) {
    const input = e.target as HTMLInputElement;
    const file = input.files?.[0];
    input.value = '';
    if (!file) return;
    const maintainer = await dialogs.prompt(t('pe.uploadMaintainer'), {
      title: t('pe.uploadJar'),
      placeholder: t('pe.uploadMaintainerHint'),
    });
    if (maintainer == null) return; // cancelled
    const opts = { maintainer: maintainer.trim() || undefined };
    try {
      await api.uploadJar(packId, file, opts);
      await dialogs.confirm(t('pe.uploadQueued', { name: file.name }), {
        title: t('pe.uploadJar'),
      });
    } catch (x) {
      // A coverage rejection ("Modrinth already carries ...") can be forced only
      // by a debug operator -- the repackage-for-FML-handshake exception (#37/#44).
      const coverage =
        x instanceof ApiError && x.status === 400 && x.body.includes('already carries');
      if (canDebug && coverage) {
        const force = await dialogs.confirm(t('pe.uploadForce', { name: file.name }), {
          title: t('pe.uploadForceTitle'),
          danger: true,
        });
        if (force) {
          try {
            await api.uploadJar(packId, file, { ...opts, force: true });
            await dialogs.confirm(t('pe.uploadQueued', { name: file.name }), {
              title: t('pe.uploadJar'),
            });
          } catch (y) {
            fail(y);
          }
          return;
        }
      }
      fail(x);
    }
  }

  type Tab = 'config' | 'branding' | 'graph' | 'build' | 'threads' | 'access';
  let tab = $state<Tab>('config');
  let previewOpen = $state(false);
  let previewToken = $state(0);

  // bootstrap-from-SC-archive (only shown when there is no config yet)
  let bootstrapMode = $state(false);
  let bootMc = $state('1.12.2');
  let bootLoader = $state('');
  let bootName = $state('');
  let bootBusy = $state(false);
  let bootJobId = $state<string | null>(null);

  // source picker: { src, row } -- row null means "add a new mod"
  // 'search' finds a mod across both sources (#101); 'cache' is the other
  // question -- copying from a build, or a raw jar by hash -- and keeps its own
  // picker; 'modrinth' remains for re-pinning a row that is already a Modrinth
  // source, where the project is known and only the version is in question.
  let pick = $state<{
    src: 'search' | 'cache' | 'modrinth' | 'github';
    row: number | null;
  } | null>(null);
  // a resolve-report suggestion routed into the Modrinth picker as its search
  let suggestQuery = $state('');
  let dropBusy = $state(false);
  // asset Modrinth picker: which folder + Modrinth project kind
  let assetPick = $state<{ folder: string; projectType: 'resourcepack' | 'shader' } | null>(null);
  let assetDropBusy = $state(false);


  let cfg = $state<PackConfig | null>(null);
  let tagsStr = $state('');
  // pack-card gallery as newline-separated text, mirrored into cfg.pack_meta on save
  let cardGalleryStr = $state('');
  let loading = $state(true);
  // Failures are notices, not banners wedged above the form: reporting a
  // problem must not move the thing the operator was working on.
  const fail = notifyFail;

  // This editor's seat in the pack's document (#115). While it is open the
  // document is the writer: edits are merged rather than saved conditionally,
  // so there is no base to go stale and no version to lose a race with.
  let session = $state<PackSession | null>(null);

  // autosave
  type SaveState = 'idle' | 'saving' | 'saved' | 'error' | 'conflict';
  let saveState = $state<SaveState>('idle');
  let saveErr = $state('');
  // The revision this editor loaded and has been saving against; it rides every
  // save as a precondition, so a save the mirror would apply over someone else's
  // is refused instead (#52). Null until the pack has a stored config.
  let rev = $state<string | null>(null);
  // Bumped whenever the pack's history moves (#122). The editor does not hold
  // the history -- the build console does, because a build is made from a
  // commit -- but this is where the pack's event stream is read, so the fact
  // that it moved is passed down rather than subscribed to twice.
  let historyTick = $state(0);
  // What this viewer may do here, asked of the gate that enforces it (ADR
  // 0006). It used to be guessed from the pack id and the caller's role, which
  // got the admin and the namespace owner right and hid merging, moderation and
  // the access list from everybody who reached the pack by grant -- the one case
  // grants exist for.
  let level = $state<PackLevel | null>(null);
  const canOwn = $derived(level === 'own');
  const canEdit = $derived(level === 'own' || level === 'edit');
  $effect(() => {
    const pack = packId;
    void (async () => {
      level = await api
        .myPackLevel(pack)
        .then((r) => r.level ?? null)
        .catch(() => null);
    })();
  });

  // What has been checkpointed, held here rather than in the build console: it
  // is consulted while working on something else, so it opens as a dock over
  // whatever tab is up (ADR 0005) and the console it used to live in is not
  // mounted on most of them.
  let log = $state<CommitLogEntry[]>([]);
  let logNext = $state<string | null>(null);
  let logFailed = $state(false);
  let logBusy = $state(false);
  let logOpen = $state(false);
  // Which read of the log is the current one. A page fetched against an older
  // cursor must not be appended to a list that has since been replaced -- the
  // paging is keyset, so the row the stale cursor started after is no longer
  // where the new list ends, and the join would leave a gap in the history.
  let logGeneration = 0;

  async function readLog() {
    const generation = ++logGeneration;
    try {
      const page = await api.commits(packId);
      if (generation !== logGeneration) return;
      log = page.rows;
      logNext = page.next;
      logFailed = false;
    } catch {
      // an unread history and a pack that never declared one look identical
      // from an empty list, and only one of them is worth acting on
      logFailed = true;
    }
  }

  /// The next page, appended. Reading further back is a step somebody takes,
  /// not something the editor does on its own on a pack with hundreds.
  async function moreLog() {
    if (!logNext || logBusy) return;
    const generation = logGeneration;
    logBusy = true;
    try {
      const page = await api.commitsPage(logNext);
      // a refresh that landed meanwhile owns the list now
      if (generation !== logGeneration) return;
      const seen = new Set(log.map((c) => c.id));
      log = [...log, ...page.rows.filter((c) => !seen.has(c.id))];
      logNext = page.next;
    } catch (e) {
      notifyFail(e);
    } finally {
      logBusy = false;
    }
  }

  $effect(() => {
    // re-read when the pack changes, and whenever anyone in it commits
    void packId;
    void historyTick;
    void readLog();
  });

  // A build a commit page asked for; handed to the console, which owns building.
  let buildFrom = $state<string | null>(null);
  // The build in flight. Held here because the console is one surface among
  // several in this editor: opening a commit over it must not lose a running
  // build, re-enable the button, and let a second one start.
  // Bumped when a discussion moves, so the list behind it re-reads.
  let threadTick = $state(0);
  let buildJobId = $state<string | null>(null);
  let buildBusy = $state(false);
  // Who else has this pack open, from the stream. Names, not a count: "bo is
  // here" is a different fact from "1 other person", and it is the one that
  // changes what you do next.
  let alsoHere = $state<string[]>([]);
  // The revision the stream last announced. Compared against our own so an
  // editor can tell its own save coming back from someone else's.
  let streamRev = $state<string | null>(null);

  // A refused save is a fork in the road, not a retry: until the operator says
  // whose version wins, autosave stops rather than re-sending a base the mirror
  // has already rejected once.
  let conflict = $state(false);
  // one slot for the save state, reused: a rejection that persists is one
  // notice, not one per attempt
  let saveToast: number | null = null;
  // signature of the last-persisted state; autosave fires only when it differs,
  // which also keeps the initial load from triggering a spurious save
  let lastSig = '';
  let saveTimer: ReturnType<typeof setTimeout> | undefined;

  // validate the saved config against an uploaded instance archive
  let validating = $state(false);
  let valReport = $state<ValidateReport | null>(null);

  // resolve the saved config against the registry dependency graph
  let resolving = $state(false);
  let resReport = $state<ResolveReport | null>(null);

  // published build versions, for "revert config to build" (config edits autosave
  // with no undo, so the last built state is the recovery point). The picker is an
  // action menu -- `revertPick` resets to the placeholder after each choice.
  // The Minecraft versions the mirror knows. Best-effort: the field is free
  // text either way, so a list that failed to load costs an affordance rather
  // than the ability to edit.
  let mcVersions = $state<MinecraftVersions | null>(null);
  api
    .minecraftVersions()
    .then((v) => (mcVersions = v))
    .catch(() => {});
  /// What the picker offers: releases, newest first. Snapshots are in the list
  /// the hint checks against but not in the offer -- there are eight hundred of
  /// them and nobody builds a pack on one by picking it from a menu.
  const mcReleases = $derived(
    (mcVersions?.versions ?? []).filter((v) => v.version_type === 'release').map((v) => v.version),
  );
  /// Said when the mirror has never heard of the version typed. Not an error:
  /// a version released an hour ago is exactly the case where the operator is
  /// right and the list is behind.
  const mcHint = $derived.by(() => {
    if (!cfg || !mcVersions || mcVersions.versions.length === 0) return null;
    const typed = cfg.minecraft_version.trim();
    if (!typed || mcVersions.versions.some((v) => v.version === typed)) return null;
    return t(mcVersions.stale ? 'pe.mcUnknownStale' : 'pe.mcUnknown');
  });

  // Builds of the pack's loader. Re-fetched when the loader changes; a loader
  // with no published list answers 404, and the field stays free text, which is
  // what it always was.
  let loaderList = $state<LoaderVersions | null>(null);
  let loaderAsked = '';
  $effect(() => {
    const name = cfg?.loader.name?.trim().toLowerCase() ?? '';
    if (!name || name === loaderAsked) return;
    loaderAsked = name;
    loaderList = null;
    api
      .loaderVersions(name)
      .then((v) => {
        if (loaderAsked === name) loaderList = v;
      })
      .catch(() => {});
  });
  /// What the picker offers: the builds for this pack's Minecraft version, or
  /// all of them for a loader whose builds do not tie to one. Recommended
  /// first, then newest -- the order in which a curator wants to see them.
  const loaderBuilds = $derived.by(() => {
    const mc = cfg?.minecraft_version?.trim();
    const all = loaderList?.builds ?? [];
    const mine = all.filter((b) => !b.minecraft || b.minecraft === mc);
    // Not capped. The list arrives newest first and a pack's own Minecraft
    // version narrows it to a few hundred; cutting it is how the build this
    // deployment actually runs fell off the end.
    return [...mine].sort((a, b) => Number(b.recommended) - Number(a.recommended));
  });

  const javaOptions = JAVA_MAJORS.map((v) => ({ value: String(v), label: String(v) }));
  /// What this pack most likely needs, when that disagrees with what it says.
  const javaHint = $derived.by(() => {
    if (!cfg) return null;
    const want = suggestedJava(cfg.minecraft_version, cfg.loader.name);
    if (want === null || want === cfg.java_major) return null;
    return t('pe.javaHint', { want: String(want) });
  });

  // What the others have touched lately, for the markers. Swept on a timer:
  // without it the map grows for as long as the editor is open, and a decayed
  // entry would keep being filtered on every render.
  const touches = createTouches();
  $effect(() => {
    const timer = setInterval(() => touches.sweep(), 2000);
    return () => clearInterval(timer);
  });
  /// A path as something a person reads. The first segment is the part -- the
  /// rest ("mods.sodium.jar", "pack_meta.description_md") is an address for the
  /// marker, not for the sentence, which would only get longer without getting
  /// clearer.
  const fieldLabel = (path: string) => {
    const head = path.split('.')[0];
    const known = [
      'display_name', 'tagline', 'minecraft_version', 'java_major', 'loader',
      'tags', 'featured', 'version', 'mods', 'assets', 'pack_meta',
    ];
    return t(`pe.field.${known.includes(head) ? head : 'other'}` as never);
  };

  /// The recent changes, as sentences. Three at most: a longer list is not read,
  /// and the ones under it are the older ones.
  ///
  /// Grouped by what a reader is told, not by path: two mods edited in one
  /// window are two paths and one sentence, and the ungrouped list said
  /// "someone - a mod" twice in a row.
  const touchLines = $derived.by(() => {
    const byLabel = new Map<string, string[]>();
    for (const entry of touches.live) {
      const label = fieldLabel(entry.path);
      const who = byLabel.get(label) ?? [];
      for (const name of entry.who) if (!who.includes(name)) who.push(name);
      byLabel.set(label, who);
    }
    return [...byLabel.entries()].slice(0, 3).map(([label, people]) => ({
      label,
      who:
        people.length === 1
          ? people[0]
          : people.length === 2
            ? people.join(' & ')
            : t('pe.touchedByN', { n: String(people.length) }),
    }));
  });

  // The handshake claim and its drift. Loaded on demand: it asks a game server,
  // which is slower than everything else here and pointless for a pack that
  // names none.
  // Whether a claim can be derived at all for this pack's loader (#148). A
  // NeoForge or Fabric server puts no mod list in its ping, so there is nothing
  // to copy -- and that is knowable from the config, before anyone presses.
  const canSpoof = $derived(advertisesModList(cfg?.loader?.name ?? ''));
  let spoof = $state<SpoofReport | null>(null);
  let spoofBusy = $state(false);

  async function checkSpoof() {
    spoofBusy = true;
    try {
      spoof = await api.packSpoof(packId);
    } catch (e) {
      fail(e);
    } finally {
      spoofBusy = false;
    }
  }

  /// Rewrites the shipped claim from what the server says now. Confirmed first:
  /// it replaces a file the pack ships and touches the config, which is a
  /// different weight of action from looking.
  async function generateSpoof() {
    const ok = await dialogs.confirm(t('pe.spoof.confirm'), { title: t('pe.spoof.title') });
    if (!ok) return;
    spoofBusy = true;
    try {
      spoof = await api.generatePackSpoof(packId);
      // the config gained (or re-pointed) an asset row, and the mirror settled
      // the document to do it -- so this editor rejoins rather than carrying on
      // against a session that is gone
      await load();
      toasts.push({ kind: 'info', text: t('pe.spoof.written') });
    } catch (e) {
      fail(e);
    } finally {
      spoofBusy = false;
    }
  }

  let revertVersions = $state<string[]>([]);
  let revertPick = $state('');
  const revertOptions = $derived(revertVersions.map((v) => ({ value: v, label: v })));

  /// Put a config on screen. `lastSig` is set with it, so adopting something --
  /// a load, a revert, someone else's merged change -- is never mistaken for
  /// this editor having typed it.
  function adopt(c: PackConfig) {
    if (!c.pack_meta) {
      c.pack_meta = { icon_url: null, banner_url: null, gallery_urls: [], description_md: null };
    }
    cfg = c;
    tagsStr = (c.tags ?? []).join(', ');
    cardGalleryStr = (c.pack_meta.gallery_urls ?? []).join('\n');
    lastSig = sig();
  }

  /// Someone else's merged change. Adopting it is the same act as any other
  /// adoption; the difference is that it came from a person, so what moved and
  /// who moved it is worth a moment on screen.
  function adoptRemote(next: PackConfig, by: string) {
    const before = cfg ? ($state.snapshot(cfg) as unknown as Record<string, unknown>) : null;
    adopt(next);
    if (before) touches.record(changedPaths(before as never, next as never), by);
  }

  async function load() {
    loading = true;
    try {
      const { config: c, rev: r } = await api.packConfig(packId);
      adopt(c);
      rev = r;
      // Join the pack's document and take what it holds: someone may be editing
      // right now, and their work is newer than the file this just read.
      session?.close();
      session = await openPackSession(packId, c, adoptRemote);
      adopt(session.read());
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) {
        cfg = null; // offer to create
        rev = null;
      } else {
        fail(e);
      }
    }
    // best-effort: an unbuilt pack has no versions to revert to
    try {
      revertVersions = (await api.manifestVersions(packId)).builds.map((b) => b.version_number);
    } catch {
      revertVersions = [];
    }
    loading = false;
  }
  load();

  async function revertTo(version: string) {
    if (!version || !cfg) return;
    const ok = await dialogs.confirm(t('pe.revertConfirm', { version }), { danger: true });
    if (!ok) return;
    try {
      const { config: c, rev: r } = await api.revertPackConfig(packId, version);
      adopt(c);
      rev = r;
      // The mirror drops the document on a revert, so this editor rejoins the
      // one it seeds from the reverted config rather than keeping a seat at a
      // table that is gone.
      session?.close();
      session = await openPackSession(packId, c, adoptRemote);
      adopt(session.read());
      // a revert replaces the server's config outright, so whatever this editor
      // was in conflict with is gone
      clearConflict();
      if (previewOpen) previewToken++;
    } catch (e) {
      fail(e);
    }
  }

  function createBlank() {
    cfg = {
      pack_id: packId,
      display_name: packId,
      tagline: '',
      minecraft_version: '1.12.2',
      loader: { name: 'forge', version: '' },
      java_major: 8,
      tags: [],
      featured: false,
      mods: [],
      assets: [],
      pack_meta: { icon_url: null, banner_url: null, gallery_urls: [], description_md: null },
      // ownership + publication are server-authoritative; these are placeholders
      // the backend overwrites on create (owner = the creator) / preserves on edit.
      owner: 0,
      tier: 'official',
      visibility: 'published',
    };
    tagsStr = '';
    cardGalleryStr = '';
  }

  // content signature; the debounced autosave fires only when it changes. A JSON
  // array keeps the parts unambiguous (no separator a field value could forge).
  function sig(): string {
    return cfg ? JSON.stringify([$state.snapshot(cfg), tagsStr, cardGalleryStr]) : '';
  }

  // True while the editor holds edits the server has not accepted. Drives the
  // banner, the beforeunload guard and the close confirmation -- all three read
  // one fact rather than each deciding for itself.
  const unsaved = $derived(saveState === 'error' || saveState === 'conflict');

  // A tab close / reload with a rejected save pending would drop the edits
  // silently; the browser's own confirmation is the only thing that can stop it.
  $effect(() => {
    if (!unsaved) return;
    const guard = (e: BeforeUnloadEvent) => e.preventDefault();
    window.addEventListener('beforeunload', guard);
    return () => window.removeEventListener('beforeunload', guard);
  });

  // Leaving is no longer one button: the editor is a location, so back, the
  // trackpad gesture and the rail all close it. The question therefore lives on
  // the route, which asks it whichever way the exit came -- rather than on the
  // Close handler, which a gesture would walk straight past.
  $effect(() => {
    if (!unsaved) return;
    route.setLeaveGuard(() => dialogs.confirm(t('pe.unsavedLeave'), { danger: true }));
    return () => route.setLeaveGuard(null);
  });

  // Subscribe while the editor is open. The subscription is also the presence:
  // the mirror counts whoever is listening, so a closed tab or a dropped
  // connection is a departure without anything having to say goodbye.
  $effect(() => {
    const src = new EventSource(api.packEventsUrl(packId));
    src.addEventListener('pack', (e) => {
      let event: PackEvent;
      try {
        event = JSON.parse((e as MessageEvent).data) as PackEvent;
      } catch {
        return; // a frame we cannot read is not worth acting on
      }
      if (event.kind === 'present') {
        alsoHere = event.editors.filter((name) => name !== me.login);
        return;
      }
      // Someone else's keystrokes. Merged into this editor's copy, which calls
      // back with the result -- the difference between watching a colleague work
      // and being interrupted by a reload.
      if (event.kind === 'doc') {
        session?.receive(event.update, event.by);
        return;
      }
      // The history moved. Whoever declared it did so for everyone in the pack,
      // so the "changes since the last commit" count here corrects itself
      // instead of staying stale until a reload.
      if (event.kind === 'committed') {
        historyTick += 1;
        if (event.by !== me.login) {
          toasts.push({ kind: 'info', text: t('pe.committedBy', { who: event.by }) });
        }
        return;
      }
      streamRev = event.rev;
      // With a document open, a save is the mirror reporting that it wrote what
      // everyone here already has. Take the revision and stay put: re-reading
      // would replace the screen with a file that says the same thing, and take
      // the caret with it.
      if (session) {
        rev = event.rev;
        saveState = 'saved';
        // The fill appends libraries as it resolves them, and those rows exist
        // on disk without anyone having typed them -- so they are not in the
        // document and would stay invisible here until a reload. One read after
        // a write, not one per keystroke.
        void api
          .packConfig(packId)
          .then(({ config }) => mergePulled(config))
          .catch(() => {});
        return;
      }
      // Our own save coming back: the revision is the one we now hold.
      if (event.rev === rev) return;
      // Someone else saved. With nothing of our own in flight the honest move is
      // to show their work rather than a stale screen; with unsaved edits it
      // stays a decision, which is the machinery the 409 already drives.
      if (saveState === 'saving' || unsaved || sig() !== lastSig) {
        toasts.push({
          kind: 'info',
          text: t('pe.movedBy', { who: event.by }),
          detail: t('pe.movedHint'),
        });
        return;
      }
      void load();
      toasts.push({ kind: 'info', text: t('pe.movedBy', { who: event.by }) });
    });
    return () => src.close();
  });

  // The seat is given up with the editor: the mirror keeps the document for
  // whoever is still in the pack, and this copy stops sending into it.
  $effect(() => () => session?.close());

  // Every change goes to the writer. With a document open that is a merge, sent
  // as it is typed -- the delay that used to be here was there so a sentence was
  // one save rather than forty, and the mirror now does that waiting itself,
  // where it can also wait for the other editors. Without one the pack has no
  // stored config yet, so the first write is still a conditional save: it is
  // what creates the pack.
  $effect(() => {
    if (!cfg || conflict) return;
    const s = sig();
    if (s === lastSig) return;
    lastSig = s;
    if (session) {
      session.push(composed());
      saveState = 'saving'; // until the mirror says it wrote
      return;
    }
    saveState = 'saving';
    clearTimeout(saveTimer);
    saveTimer = setTimeout(() => doSave(s), 700);
  });

  // a cleared text input holds "" -- normalize to null so an empty card field is
  // omitted from the published summary rather than serialized as ""
  const blankToNull = (v: string | null | undefined) => (v && v.trim() ? v.trim() : null);

  // Retry the last failed save. The error is a banner rather than a tooltip: a
  // rejected save means the config on screen is NOT the config on disk, which is
  // the one state in this editor worth interrupting for.
  async function retrySave() {
    if (!cfg) return;
    await doSave(sig());
  }

  /// What is on screen, as a config: the two comma/newline text boxes become
  /// lists and the blank card fields become absent. Both writers send this, so
  /// the shape cannot differ between saving a pack and merging into one.
  function composed(): PackConfig {
    const snap = $state.snapshot(cfg!) as PackConfig;
    return {
      ...snap,
      tags: tagsStr
        .split(',')
        .map((x) => x.trim())
        .filter(Boolean),
      pack_meta: {
        icon_url: blankToNull(snap.pack_meta.icon_url),
        banner_url: blankToNull(snap.pack_meta.banner_url),
        description_md: blankToNull(snap.pack_meta.description_md),
        gallery_urls: cardGalleryStr
          .split('\n')
          .map((x) => x.trim())
          .filter(Boolean),
      },
    };
  }

  async function doSave(s: string) {
    if (!cfg) return;
    const payload = composed();
    try {
      const saved = await api.savePackConfig(packId, payload, rev);
      rev = saved.rev;
      // The pack exists now, so the document does too: from here on edits are
      // merged rather than saved against a base.
      if (!session) session = await openPackSession(packId, saved.config, adoptRemote);
      // The mirror answers with the config it stored, dependencies and all. That
      // answer used to be discarded, so a pulled library existed on disk and was
      // invisible here until a reload. Only the pulled rows are taken: they are
      // server-managed, and anything else would overwrite what was typed during
      // the save.
      mergePulled(saved.config);
      lastSig = s;
      saveState = 'saved';
      toasts.dismiss(saveToast);
      saveToast = null;
      if (previewOpen) previewToken++; // auto-refresh the preview
    } catch (e) {
      // 409: the stored config moved on since this editor read it -- someone
      // else saved the same pack. The edits on screen are intact and unsaved;
      // which version survives is the operator's call, not a retry's.
      const stale = e instanceof ApiError && e.status === 409;
      saveState = stale ? 'conflict' : 'error';
      conflict = stale;
      saveErr = detailOf(e);
      saveToast = toasts.replace(saveToast, {
        kind: 'error',
        text: stale ? t('pe.conflict') : t('pe.saveFailed'),
        detail: saveErr,
        sticky: true,
        action: stale
          ? { label: t('pe.conflictResolve'), run: resolveConflict }
          : { label: t('pe.saveRetry'), run: retrySave },
      });
    }
  }

  function clearConflict() {
    conflict = false;
    if (saveState === 'conflict') saveState = 'idle';
    toasts.dismiss(saveToast);
    saveToast = null;
  }

  // The two ways out of a refused save, both explicit: take the other version
  // and lose the edits on screen, or save on top of it. Overwriting re-reads the
  // current revision first and saves against that, so it stays a conditional
  // write -- a third save landing in this very window is caught too, rather than
  // being the one thing the fix does not cover.
  async function resolveConflict() {
    const choice = await dialogs.choose(t('pe.conflictAsk'), {
      title: t('pe.conflictTitle'),
      options: [
        { value: 'reload', label: t('pe.conflictReload') },
        { value: 'overwrite', label: t('pe.conflictOverwrite'), danger: true },
      ],
    });
    if (choice === 'reload') {
      clearConflict();
      await load();
    } else if (choice === 'overwrite') {
      try {
        rev = (await api.packConfig(packId)).rev;
      } catch (e) {
        fail(e);
        return;
      }
      clearConflict();
      await doSave(sig());
    }
  }

  async function onValidate(e: Event) {
    const input = e.target as HTMLInputElement;
    const file = input.files?.[0];
    if (!file) return;
    validating = true;
    valReport = null;
    try {
      valReport = await api.validatePack(packId, file);
      reportTab = 'validate';
    } catch (x) {
      fail(x);
    } finally {
      validating = false;
      input.value = '';
    }
  }

  // Resolve reads the SAVED config, so flush a pending autosave first -- the
  // report must reflect what is on screen, not the last debounced save.
  async function onResolve() {
    if (!cfg) return;
    resolving = true;
    try {
      const s = sig();
      if (s !== lastSig) {
        clearTimeout(saveTimer);
        await doSave(s);
        if (saveState === 'error') {
          // the save notice already carries the reason; a report over a config
          // the server refused would describe a state that does not exist
          resReport = null;
          return;
        }
      }
      resReport = await api.resolvePack(packId);
      reportTab = 'resolve';
    } catch (x) {
      fail(x);
      resReport = null;
    } finally {
      resolving = false;
    }
  }

  async function onBootstrap(e: Event) {
    const input = e.target as HTMLInputElement;
    const file = input.files?.[0];
    if (!file) return;
    bootBusy = true;
    bootJobId = null;
    try {
      const { job_id } = await api.bootstrapPack(
        packId,
        {
          minecraft_version: bootMc.trim(),
          loader_version: bootLoader.trim(),
          display_name: bootName.trim() || undefined,
        },
        file,
      );
      bootJobId = job_id;
    } catch (x) {
      fail(x);
      bootBusy = false;
    } finally {
      input.value = '';
    }
  }

  function onBootDone(status: JobStatus) {
    bootBusy = false;
    if (status === 'done') {
      bootstrapMode = false;
      load();
    }
  }

  /// An icon or banner was just uploaded into the pack's own static tree: put
  /// its path on the card, which is the half that makes it visible.
  ///
  /// The two used to be unconnected, on the same tab. Uploading a picture left
  /// the card's field empty, and filling it in meant knowing the file is served
  /// at `/v1/packs/<pack_id>/static/<path>` -- with the id percent-encoded for a
  /// community pack, which is `u/<uid>/<name>`. Nothing said so anywhere, so the
  /// one-action path was to paste a link to somebody else's CDN, and the pack's
  /// own picture sat unused in its own tree. The build resolves this path
  /// against the mirror, so what a launcher reads is a URL either way.
  function setCardImage(target: 'icon' | 'banner', relPath: string) {
    if (!cfg) return;
    if (target === 'icon') cfg.pack_meta.icon_url = relPath;
    else cfg.pack_meta.banner_url = relPath;
    toasts.push({ kind: 'ok', text: t(target === 'icon' ? 'be.iconSet' : 'be.bannerSet') });
  }

  // ── mods ──
  function blankSource(type: SourceDecl['type']): SourceDecl {
    if (type === 'modrinth') return { type, project_id: '', version_id: '' };
    if (type === 'smrt_cache') return { type, sha1: '' };
    return { type, rel_path: '' };
  }

  // Switching a row's source type used to blank the reference outright, so one
  // stray click on the dropdown lost a project id with no undo and an autosave
  // 700ms behind it. Each row remembers what it had per type, so switching back
  // restores it.
  //
  // Keyed by the row rather than by its position: the list re-sorts itself
  // whenever a mod is added or removed, and removing one shifts every row after
  // it down. A position-keyed memory therefore handed a row whatever the mod
  // that used to sit at that index had been pinned to -- restoring another
  // mod's project id into this one, which is worse than not remembering at all.
  // A weak map also lets a removed row's memory go with it.
  const priorSource = new WeakMap<
    DeclaredMod,
    Partial<Record<SourceDecl['type'], SourceDecl>>
  >();

  function changeSourceType(i: number, type: SourceDecl['type']) {
    const row = cfg!.mods[i];
    const kept = priorSource.get(row) ?? {};
    kept[row.source.type] = $state.snapshot(row.source) as SourceDecl;
    priorSource.set(row, kept);
    row.source = kept[type] ?? blankSource(type);
  }

  function removeMod(i: number) {
    cfg!.mods = cfg!.mods.filter((_, j) => j !== i);
  }

  // Sticky sort: the list stays ordered as mods are added (an added mod slots
  // into place instead of landing at the end). Defaults to A-Z so a freshly
  // opened pack reads in order and an added mod lands where it belongs.
  let sortDir = $state<'asc' | 'desc' | null>('asc');

  // Re-sort only on a structural change (a mod added/removed -> length changes)
  // or a direction change. The sort itself runs untracked, so editing a mod's
  // filename does NOT re-trigger this -- the row won't jump out from under the
  // cursor mid-edit. Reassigns cfg.mods so autosave + the per-mod display table
  // (same list) follow.
  $effect(() => {
    const dir = sortDir;
    if (!cfg || !dir) return;
    void cfg.mods.length; // dependency: structural changes only
    untrack(() => {
      if (!cfg) return;
      const sign = dir === 'asc' ? 1 : -1;
      const sorted = [...cfg.mods].sort(
        (a, b) => a.filename.localeCompare(b.filename, undefined, { sensitivity: 'base' }) * sign,
      );
      if (sorted.some((m, i) => m !== cfg!.mods[i])) cfg.mods = sorted;
    });
  });

  async function onDropJars(files: File[]) {
    if (!cfg) return;
    dropBusy = true;
    try {
      for (const file of files) {
        if (!file.name.endsWith('.jar')) continue;
        const sha1 = await api.uploadCacheJar(file);
        // same identity check as every other add path: dropping a jar the pack
        // already ships must not create a second row of it
        if (!appendMod({ filename: file.name, source: { type: 'smrt_cache', sha1 } })) {
          toasts.push({ kind: 'error', text: t('pe.dupMod', { name: file.name }) });
        }
      }
    } catch (x) {
      fail(x);
    } finally {
      dropBusy = false;
    }
  }

  // Server-managed rows: the fill owns them, so they are merged in rather than
  // edited here. Without this they only appeared after a reload.
  function mergePulled(saved: PackConfig) {
    if (!cfg) return;
    const here = new Set(cfg.mods.map((m) => sourceKey(m.source)));
    const added = (saved.mods ?? []).filter((m) => m.pulled && !here.has(sourceKey(m.source)));
    if (added.length) cfg.mods = [...cfg.mods, ...added];
  }

  // What a mod brings with it, asked at the moment it is added rather than
  // discovered on the next save (#53). Silent when it brings nothing: a notice
  // for the common case is noise, and most mods pull nothing.
  async function announcePulled(name: string) {
    if (!cfg) return;
    try {
      const pulled = await api.previewDependencies(packId, $state.snapshot(cfg));
      if (!pulled.length) return;
      toasts.push({
        kind: 'info',
        text: t('pe.pulls', { name, n: pulled.length }),
        detail: pulled.map((p) => p.filename).join(', '),
      });
    } catch {
      // an advisory that could not be computed (an outage, a slow registry) is
      // not worth a failure notice: the save still reports what it pulled
    }
  }

  // a stable identity for a declared source, so the same mod isn't added twice
  // (cache by sha1, Modrinth by project, static by path). The pinned version is
  // deliberately not part of it: the same project at another version is still the
  // same mod, and changing versions is a re-pin of the row, not a second row.
  function sourceKey(s: SourceDecl): string {
    if (s.type === 'smrt_cache') return `c:${s.sha1}`;
    if (s.type === 'modrinth') return `m:${s.project_id}`;
    return `s:${s.rel_path}`;
  }

  // What the pickers must not offer again. When re-pointing a row, that row's own
  // identity is excluded -- re-pinning it to another version of the same mod is
  // the normal way to update.
  function presentKeys(exceptRow: number | null): string[] {
    if (!cfg) return [];
    return cfg.mods.filter((_, i) => i !== exceptRow).map((m) => sourceKey(m.source));
  }

  // a pick from the mirror carries the resolved source (cache when the mirror
  // holds the bytes, else Modrinth) plus the build's install flags when known
  type MirrorSel = {
    filename: string;
    source: SourceDecl;
    default_enabled?: boolean;
  };

  // append a declared mod unless an identical source is already present; returns
  // whether it was added
  function appendMod(sel: MirrorSel): boolean {
    if (!cfg) return false;
    const key = sourceKey(sel.source);
    if (cfg.mods.some((m) => sourceKey(m.source) === key)) return false;
    cfg.mods = [
      ...cfg.mods,
      {
        filename: sel.filename,
        default_enabled: sel.default_enabled ?? true,
        source: sel.source,
        pulled: false,
      },
    ];
    announcePulled(sel.filename);
    return true;
  }

  // single pick from the mirror: add a new row or re-point the row being edited,
  // then close
  function onMirrorPick(sel: MirrorSel) {
    if (!cfg || !pick) return;
    if (pick.row === null) {
      appendMod(sel);
    } else {
      const m = cfg.mods[pick.row];
      m.source = sel.source;
      if (!m.filename) m.filename = sel.filename;
    }
    pick = null;
  }

  // cherry-pick one mod from a build without closing -- keep adding from it
  function onMirrorAddOne(sel: MirrorSel) {
    appendMod(sel);
  }

  // re-add a whole build's mod set; preserves each mod's required/default flags,
  // skips artifacts already present, then closes
  function onMirrorAddMany(items: MirrorSel[]) {
    if (!cfg) return;
    for (const it of items) appendMod(it);
    pick = null;
  }

  // a GitHub ingest always lands a fresh jar in the cache -> a cache source
  function onGithubPick(sel: { sha1: string; filename: string }) {
    onMirrorPick({ filename: sel.filename, source: { type: 'smrt_cache', sha1: sel.sha1 } });
  }

  // pull an asset from a build (Builds tab) into this pack, deduped by dest; the
  // asset already carries its resolved source, so it is appended as-is
  function onMirrorAddAsset(a: DeclaredAsset) {
    appendAsset(a);
  }

  function onModrinthPick(sel: { project_id: string; slug: string; version_id: string }) {
    if (!cfg || !pick) return;
    // the picker greys out what the pack already ships; this is the last word on
    // it, so a stale list can't land a second row of the same mod
    if (presentKeys(pick.row).includes(`m:${sel.project_id}`)) {
      toasts.push({ kind: 'error', text: t('pe.dupMod', { name: sel.slug }) });
      pick = null;
      suggestQuery = '';
      return;
    }
    if (pick.row === null) {
      cfg.mods = [
        ...cfg.mods,
        {
          filename: `${sel.slug}.jar`,
          default_enabled: true,
          source: { type: 'modrinth', project_id: sel.project_id, version_id: sel.version_id },
          pulled: false,
        },
      ];
      announcePulled(`${sel.slug}.jar`);
    } else {
      const m = cfg.mods[pick.row];
      m.source = { type: 'modrinth', project_id: sel.project_id, version_id: sel.version_id };
      if (!m.filename) m.filename = `${sel.slug}.jar`;
    }
    pick = null;
    suggestQuery = '';
  }

  // one row per dest: two assets writing the same file is a pack that installs
  // one of them at random, so every add path funnels through here
  function appendAsset(a: DeclaredAsset): boolean {
    if (!cfg) return false;
    const assets = cfg.assets ?? [];
    if (a.dest && assets.some((x) => x.dest === a.dest)) return false;
    cfg.assets = [...assets, a];
    return true;
  }

  function addAsset() {
    appendAsset({ dest: '', required: true, source: { type: 'smrt_static', rel_path: '' } });
  }
  function removeAsset(i: number) {
    cfg!.assets = (cfg!.assets ?? []).filter((_, j) => j !== i);
  }

  function onAssetModrinthPick(sel: { project_id: string; slug: string; version_id: string }) {
    if (!cfg || !assetPick) return;
    const dest = `${assetPick.folder}/${sel.slug}.zip`;
    if (!appendAsset({
      dest,
      required: true,
      source: { type: 'modrinth', project_id: sel.project_id, version_id: sel.version_id },
    })) {
      toasts.push({ kind: 'error', text: t('pe.dupAsset', { dest }) });
    }
    assetPick = null;
  }

  async function onDropAssets(files: File[]) {
    if (!cfg) return;
    assetDropBusy = true;
    try {
      for (const file of files) {
        const rel = assetPath('assets', file.name);
        await api.uploadStatic(packId, rel, file);
        if (!appendAsset({
          dest: file.name,
          required: true,
          source: { type: 'smrt_static', rel_path: rel },
        })) {
          toasts.push({ kind: 'error', text: t('pe.dupAsset', { dest: file.name }) });
        }
      }
    } catch (x) {
      fail(x);
    } finally {
      assetDropBusy = false;
    }
  }

  const loaderOptions = $derived.by(() => {
    const cur = cfg?.loader.name?.trim();
    const names = cur && !KNOWN_LOADERS.includes(cur) ? [cur, ...KNOWN_LOADERS] : KNOWN_LOADERS;
    return names.map((l) => ({ value: l, label: l }));
  });

  // Which report the dock is showing; null closes it. Both reports share one
  // dock rather than each getting its own floating window to fight over.
  type ReportTab = 'resolve' | 'validate';
  let reportTab = $state<ReportTab | null>(null);
  const reportTabs = $derived(
    [
      resReport ? { value: 'resolve', label: t('resolve.resolve') } : null,
      valReport ? { value: 'validate', label: t('pe.validate') } : null,
    ].filter((x): x is { value: string; label: string } => x !== null),
  );

  const tabItems = $derived([
    { value: 'config', label: t('pe.tab.config') },
    { value: 'branding', label: t('pe.tab.branding') },
    { value: 'graph', label: t('pe.tab.graph') },
    { value: 'build', label: t('pe.tab.build') },
    { value: 'threads', label: t('pe.tab.threads') },
    { value: 'access', label: t('pe.tab.access') },
  ]);
</script>

<!-- The editor arrives rather than being present. `|global` is load-bearing:
     these elements belong to this component, and the block that creates them is
     the caller's `{#if}` -- a local transition does not play then (#114). Both
     halves take the same duration, so they read as one surface, and that
     duration comes from the stylesheet's own token, which reduced motion
     zeroes. -->
<div class="hd" in:arrive|global>
  <!-- What the others just touched (#115 follow-on). A field over the last few
       seconds, not a person: a paragraph several people are writing has no
       owner, so the set is named and the count stands in past two. It decays,
       because "5 people over an hour" is not the fact anyone needs. -->
  {#if touchLines.length}
    <div class="touched" aria-live="polite">
      <span class="faint">{t('pe.touchedTitle')}</span>
      {#each touchLines as line (line.label)}
        <span class="touch">{line.who} &middot; {line.label}</span>
      {/each}
    </div>
  {/if}
  <h2 class="ttl mono">{packId}<span class="faint">/{t('pe.edit')}</span></h2>
  <!-- The tabs scroll inside their own strip rather than pushing the actions
       onto a second row: six of them plus a revert picker outgrow a narrow
       window, and Preview and Close are what the header is for. -->
  <div class="tabs">
    <TabStrip value={tab} tabs={tabItems} ariaLabel={t('pe.edit')} onChange={(v) => (tab = v as Tab)} />
  </div>
  <!-- One group, pinned right: a wrapping header must move the actions
       together, not strand Close on a row of its own. -->
  <div class="actions">
    {#if !loading && cfg && tab === 'config' && revertVersions.length}
      <span class="revertsel">
        <Select
          compact
          full
          bind:value={revertPick}
          options={revertOptions}
          placeholder={t('pe.revertPick')}
          title={t('pe.revertTo')}
          ariaLabel={t('pe.revertTo')}
          onChange={(v) => {
            if (v) revertTo(v);
            revertPick = '';
          }}
        />
      </span>
    {/if}
    {#if !loading && cfg && tab === 'config'}
      <span class="savestate" class:err={unsaved} title={saveErr}>
        {#if saveState === 'saving'}{t('pe.saving')}
        {:else if saveState === 'saved'}{t('pe.saved')}
        {:else if saveState === 'conflict'}{t('pe.conflictShort')}
        {:else if saveState === 'error'}{t('pe.saveError')}{/if}
      </span>
    {/if}
    {#if alsoHere.length}
      <span class="alsohere" title={t('pe.alsoHereHint')}>
        {t('pe.alsoHere', { who: alsoHere.join(', ') })}
      </span>
    {/if}
    {#if saveState === 'conflict'}
      <!-- the notice carries the same action, but it can be dismissed; a refused
           save must not become unreachable because a toast was closed -->
      <button class="sm danger" onclick={resolveConflict}>{t('pe.conflictResolve')}</button>
    {/if}
    {#if !loading && cfg}
      <button class="pv" class:active={logOpen} onclick={() => (logOpen = !logOpen)}>
        {t('hist.title')}
      </button>
      <button class="pv" class:active={previewOpen} onclick={() => (previewOpen = !previewOpen)}>
        {previewOpen ? t('pe.hidePreview') : t('pe.preview')}
      </button>
    {/if}
    <button onclick={onClose}>{t('common.close')}</button>
  </div>
</div>



<div class="body" class:split={previewOpen} in:arrive|global>
  <div class="editcol">
    {#if loading}
      <div class="muted mono">{t('common.loading')}</div>
    {:else if route.thread !== null}
      <!-- A discussion is a place of its own, over the pack it belongs to. -->
      <ThreadPage threadId={route.thread} onChanged={() => (threadTick += 1)} />
    {:else if route.commit}
      <!-- A checkpoint is a place of its own (ADR 0005): it has an address, and
           the editor it was opened from is still underneath when it closes. -->
      <CommitPage
        {packId}
        commitId={route.commit}
        building={buildBusy}
        onBuildCommit={(id) => {
          buildFrom = id;
          tab = 'build';
          route.closeCommit();
        }}
        onChanged={() => (historyTick += 1)}
      />
    {:else if tab === 'config'}
      {#if !cfg}
        <div class="panel empty">
          <p class="muted">{t('pe.noConfig', { id: packId })}</p>
          <div class="opts">
            <button class="primary" onclick={createBlank}>{t('pe.createBlank')}</button>
            <button onclick={() => (bootstrapMode = !bootstrapMode)}>{t('pe.bootstrap')}</button>
          </div>
          {#if bootstrapMode}
            <div class="bootform">
              <div class="brow">
                <Field label={t('pe.mcVersion')}><input bind:value={bootMc} placeholder="1.12.2" /></Field>
                <Field label={t('pe.loaderVersion')}><input bind:value={bootLoader} placeholder="14.23.5.2922" /></Field>
                <Field label={t('pe.displayName')}><input bind:value={bootName} placeholder={packId} aria-label={packId} /></Field>
              </div>
              <label class="upbtn">
                {bootBusy ? t('pe.bootWorking') : t('pe.bootChoose')}
                <input
                  type="file"
                  accept=".zip"
                  onchange={onBootstrap}
                  disabled={bootBusy || !bootMc.trim() || !bootLoader.trim()}
                  hidden
                />
              </label>
              {#if bootJobId}{#key bootJobId}<JobLog jobId={bootJobId} onDone={onBootDone} />{/key}{/if}
            </div>
          {/if}
        </div>
      {:else}

        <Section title={t('pe.basics')}>
          <div class="meta">
            <Field label={t('pe.displayName')} error={say(requiredError(cfg.display_name))}>
              <input bind:value={cfg.display_name} />
            </Field>
            <!-- A list that is not a cage: the datalist offers the releases a
                 pack is normally built against, and the field still takes
                 anything -- a snapshot, or a version released an hour ago that
                 the mirror has not heard of. Unknown is said, not refused. -->
            <Field label={t('pe.mcVersion')} error={say(requiredError(cfg.minecraft_version))}>
              <input bind:value={cfg.minecraft_version} list="mc-versions" />
              <datalist id="mc-versions">
                {#each mcReleases as v (v)}<option value={v}></option>{/each}
              </datalist>
            </Field>
            <Field label={t('pe.loaderName')}>
              <Select full bind:value={cfg.loader.name} options={loaderOptions} ariaLabel={t('pe.loaderName')} />
            </Field>
            <!-- Same shape as the Minecraft field: offers what the mirror
                 knows, accepts anything. A build published an hour ago, or a
                 private one, has to stay typeable. -->
            <Field label={t('pe.loaderVersion')}>
              <input bind:value={cfg.loader.version} list="loader-builds" />
              <datalist id="loader-builds">
                {#each loaderBuilds as b (b.version)}
                  <option value={b.version}
                    >{b.recommended ? t('pe.loaderRecommended') : b.latest ? t('pe.loaderLatest') : ''}</option
                  >
                {/each}
              </datalist>
            </Field>
            <!-- A list, not a number box: the set is closed, and a typed one
                 only announced itself as a launcher that would not start. The
                 hint fires when the choice disagrees with what this pack most
                 likely needs -- it says so and changes nothing, because an
                 archival pack pinned to an old toolchain is a real thing. -->
            <Field label={t('pe.java')} error={say(javaError(cfg.java_major))}>
              <Select
                full
                value={String(cfg.java_major)}
                options={javaOptions}
                ariaLabel={t('pe.java')}
                onChange={(v) => {
                  const n = Number.parseInt(v, 10);
                  if (Number.isFinite(n) && n > 0) cfg!.java_major = n;
                }}
              />
            </Field>
            {#if mcHint}
              <p class="javahint muted">{mcHint}</p>
            {/if}
            {#if javaHint}
              <p class="javahint muted">{javaHint}</p>
            {/if}
            <label class="chk"><input type="checkbox" bind:checked={cfg.featured} /> {t('pe.featured')}</label>
            <Field label={t('pe.tagline')} wide><input bind:value={cfg.tagline} /></Field>
            <Field label={t('pe.tags')} hint={t('pe.tagsHint')} wide><input bind:value={tagsStr} /></Field>
          </div>
        </Section>

        <Section title={t('pe.mods')} count={cfg.mods.length}>
          {#snippet actions()}
            <button class="sm" class:active={sortDir === 'asc'} onclick={() => (sortDir = 'asc')} title={t('pe.sortHint')}>{t('pe.sortAsc')}</button>
            <button class="sm" class:active={sortDir === 'desc'} onclick={() => (sortDir = 'desc')} title={t('pe.sortHint')}>{t('pe.sortDesc')}</button>
            <button class="sm" onclick={() => (pick = { src: 'search', row: null })}>{t('pe.addMod')}</button>
            <button class="sm" onclick={() => (pick = { src: 'cache', row: null })}>{t('pe.fromBuild')}</button>
            <button class="sm" onclick={() => (pick = { src: 'github', row: null })}>{t('pe.fromGithub')}</button>
            {#if packId.startsWith('u/')}
              <label class="sm valbtn">
                {t('pe.uploadJar')}
                <input type="file" accept=".jar" onchange={onUploadJar} hidden />
              </label>
            {/if}
            <button class="sm" onclick={onResolve} disabled={resolving} title={t('resolve.hint')}>
              {resolving ? t('resolve.resolving') : t('resolve.resolve')}
            </button>
            <label class="sm valbtn">
              {validating ? t('pe.validating') : t('pe.validate')}
              <input type="file" accept=".zip" onchange={onValidate} disabled={validating} hidden />
            </label>
          {/snippet}

          <DropZone
            label={dropBusy ? t('pe.uploading') : t('pe.dropJars')}
            accept=".jar"
            busy={dropBusy}
            onFiles={onDropJars}
          />

          <div class="mods">
            {#each cfg.mods as m, i (m)}
              <div class="modrow row-in" use:stagger={i} animate:flip={{ duration: 200 }}>
                <ModIcon name={m.filename} iconUrl={m.display?.icon_url} source={m.source} size={24} mono />
                <!-- no room for a caption in this row, so the verdict is the
                     control's own state and its title, which is where a dense
                     row can say something without growing -->
                <input
                  class="fn mono"
                  class:bad={!!filenameError(m.filename)}
                  bind:value={m.filename}
                  placeholder={t('pe.filename')}
                  aria-label={t('pe.filename')}
                  aria-invalid={!!filenameError(m.filename)}
                  title={say(filenameError(m.filename)) ?? ''}
                />
                <span class="srcsel">
                  <Select
                    compact
                    full
                    value={m.source.type}
                    options={MOD_SOURCE_OPTIONS}
                    ariaLabel={t('pe.source')}
                    onChange={(v) => changeSourceType(i, v as SourceDecl['type'])}
                  />
                </span>
                <div class="ref">
                  {#if m.source.type === 'smrt_cache'}
                    <button class="sm" onclick={() => (pick = { src: 'cache', row: i })}>{t('pe.choose')}</button>
                    <span class="refval mono faint">{m.source.sha1 ? m.source.sha1.slice(0, 12) : t('pe.unset')}</span>
                  {:else if m.source.type === 'modrinth'}
                    <button class="sm" onclick={() => (pick = { src: 'modrinth', row: i })}>{t('pe.choose')}</button>
                    <span class="refval mono faint">{m.source.project_id || t('pe.unset')}</span>
                  {:else}
                    <input class="mono" bind:value={m.source.rel_path} placeholder="rel_path" aria-label={t('pe.relPath')} />
                  {/if}
                </div>
                <label class="ck" title={t('pe.defHint')}><input type="checkbox" bind:checked={m.default_enabled} /> {t('pe.def')}</label>
                {#if m.source.type === 'smrt_cache'}
                  <input class="slug mono" bind:value={m.slug} placeholder={t('pe.slug')} aria-label={t('pe.slug')} title={t('pe.slugHint')} />
                {:else}
                  <!-- A Modrinth mod is already keyed across builds by its project
                       id, so a slug on it changes nothing; the column says what the
                       key actually is instead of offering a field that does nothing. -->
                  <span class="slug keyed faint mono" title={t('pe.keyedByProjectHint')}>{t('pe.keyedByProject')}</span>
                {/if}
                <button class="danger sm del" onclick={() => removeMod(i)} aria-label={t('common.delete')}>x</button>
              </div>
            {/each}
            {#if cfg.mods.length === 0}
              <div class="muted empty-row">{t('pe.noMods')}</div>
            {/if}
          </div>
        </Section>

        <Section title={t('pe.assets')} count={(cfg.assets ?? []).length}>
          {#snippet actions()}
            <button class="sm" onclick={() => (assetPick = { folder: 'resourcepacks', projectType: 'resourcepack' })}>{t('pe.asset.resourcepack')}</button>
            <button class="sm" onclick={() => (assetPick = { folder: 'shaderpacks', projectType: 'shader' })}>{t('pe.asset.shader')}</button>
            <button class="sm" onclick={addAsset}>{t('pe.addAsset')}</button>
          {/snippet}
          <DropZone
            label={assetDropBusy ? t('pe.uploading') : t('pe.dropAssets')}
            busy={assetDropBusy}
            onFiles={onDropAssets}
          />
          <div class="panel scroll flushtable">
            <table>
              <thead>
                <tr>
                  <th>{t('pe.dest')}</th>
                  <th style="width:120px">{t('pe.source')}</th>
                  <th>{t('pe.ref')}</th>
                  <th style="width:60px">{t('pe.req')}</th>
                  <th style="width:44px"></th>
                </tr>
              </thead>
              <tbody>
                {#each cfg.assets ?? [] as a, i}
                  <tr>
                    <td>
                      <input
                        class="mono"
                        class:bad={!!relPathError(a.dest)}
                        bind:value={a.dest}
                        aria-label={t('pe.dest')}
                        aria-invalid={!!relPathError(a.dest)}
                        title={say(relPathError(a.dest)) ?? ''}
                      />
                    </td>
                    <td>
                      <Select
                        compact
                        full
                        value={a.source.type}
                        options={ASSET_SOURCE_OPTIONS}
                        ariaLabel={t('pe.source')}
                        onChange={(v) => (cfg!.assets![i].source = blankSource(v as SourceDecl['type']))}
                      />
                    </td>
                    <td>
                      {#if a.source.type === 'modrinth'}
                        <input class="mono" bind:value={a.source.project_id} placeholder="project_id" aria-label={t('pe.projectId')} />
                        <input class="mono" bind:value={a.source.version_id} placeholder="version_id" aria-label={t('pe.versionId')} />
                      {:else if a.source.type === 'smrt_cache'}
                        <input class="mono" bind:value={a.source.sha1} placeholder="sha1" aria-label={t('pe.sha1')} />
                      {:else}
                        <input class="mono" bind:value={a.source.rel_path} placeholder="rel_path" aria-label={t('pe.relPath')} />
                      {/if}
                    </td>
                    <td class="ctr"><input type="checkbox" bind:checked={a.required} aria-label={t('pe.reqLabel')} /></td>
                    <td class="ctr"><button class="danger sm" onclick={() => removeAsset(i)} aria-label={t('common.delete')}>x</button></td>
                  </tr>
                {/each}
                {#if (cfg.assets ?? []).length === 0}
                  <tr><td colspan="5" class="muted">{t('pe.noAssets')}</td></tr>
                {/if}
              </tbody>
            </table>
          </div>
        </Section>

        <!-- The handshake claim (#110). A 1.12.2 server refuses a client whose
             mod list is not the one it expects, and the file that answers that
             was typed by hand and went stale in silence. Here it is derived,
             and the difference is visible before a player finds it. -->
        <Section title={t('pe.spoof.title')}>
          <p class="muted hint">{t('pe.spoof.hint')}</p>
          {#if !canSpoof}
            <p class="muted">{t('pe.spoof.notAdvertised', { loader: cfg.loader.name })}</p>
            <p class="muted hint">{t('pe.spoof.notAdvertisedWhy')}</p>
          {/if}
          {#if spoof}
            {#if spoof.unasked}
              <p class="muted">{spoof.unasked}</p>
            {:else}
              <p class="muted">
                {t('pe.spoof.asked', {
                  where: spoof.asked ?? '',
                  n: String(spoof.current?.mods.length ?? 0),
                })}
              </p>
            {/if}
            {#if spoof.drift.length}
              <div class="gate">
                <h4>{t('pe.spoof.drifted')}</h4>
                <ul>
                  {#each spoof.drift as line (line)}<li>{line}</li>{/each}
                </ul>
              </div>
            {:else if spoof.shipped && spoof.current}
              <p class="ok">{t('pe.spoof.matches')}</p>
            {:else if !spoof.shipped}
              <p class="muted">{t('pe.spoof.none')}</p>
            {/if}
          {/if}
          <div class="spoofbar">
            <button onclick={checkSpoof} disabled={spoofBusy || !canSpoof}>
              {t('pe.spoof.check')}
            </button>
            <button
              class="primary"
              onclick={generateSpoof}
              disabled={spoofBusy || !canSpoof || !spoof?.current}
            >
              {t('pe.spoof.generate')}
            </button>
          </div>
        </Section>

        <Section title={t('pe.card.title')}>
          <div class="card">
            <p class="cardhint muted">{t('pe.card.hint')}</p>
            <Field label={t('pe.card.icon')} wide error={say(cardImageError(cfg.pack_meta.icon_url ?? ''))}>
              <input class="mono" bind:value={cfg.pack_meta.icon_url} placeholder="_pack/icon.png" />
            </Field>
            <Field label={t('pe.card.banner')} wide error={say(cardImageError(cfg.pack_meta.banner_url ?? ''))}>
              <input class="mono" bind:value={cfg.pack_meta.banner_url} placeholder="_pack/banner.png" />
            </Field>
            <Field label={t('pe.card.gallery')} wide><textarea class="mono" rows="3" bind:value={cardGalleryStr}></textarea></Field>
            <Field label={t('pe.card.description')} wide><textarea class="mono" rows="5" bind:value={cfg.pack_meta.description_md}></textarea></Field>
          </div>
        </Section>
      {/if}
    {:else if tab === 'branding'}
      <BrandingEditor {packId} onBranding={setCardImage} />
      {#if cfg}
        <div class="dzone">
          <div class="dztitle mono">{t('pe.dangerZone')}</div>
          <div class="dzrow">
            <span class="dztext muted">{t('pe.deleteExplain')}</span>
            <button class="danger" onclick={deletePack}>{t('common.delete')}</button>
          </div>
        </div>
      {/if}
    {:else if tab === 'graph'}
      <PackGraph {packId} />
    {:else if tab === 'threads'}
      <PackThreads {packId} tick={threadTick} forkOf={cfg?.fork_of ?? null} />
    {:else if tab === 'access'}
      <!-- Granting is the owner's act; everyone who can open the pack may read
           who else is in it, which is what makes the list worth having. -->
      <PackAccess {packId} canGrant={canOwn} canModerate={canEdit} />
    {:else if tab === 'build'}
      <BuildConsole
        onHistoryMoved={() => (historyTick += 1)}
        {packId}
        {historyTick}
        {buildFrom}
        bind:jobId={buildJobId}
        bind:busy={buildBusy}
        onBuildStarted={() => (buildFrom = null)}
      />
    {/if}
  </div>
  {#if previewOpen}
    <div class="previewcol">
      {#key previewToken}<PackPreview {packId} />{/key}
    </div>
  {/if}
</div>

{#snippet validateReport()}
  {#if valReport}
  <div class="valrep">
    <div class="valhead">
      <span style="color:var(--ok)">{t('pe.valMatched', { n: valReport.matched })}</span>
      <span style={valReport.missing_in_config.length ? 'color:var(--danger)' : 'opacity:.62'}>
        {t('pe.valMissing', { n: valReport.missing_in_config.length })}
      </span>
      <span class="faint">{t('pe.valExtra', { n: valReport.extra_in_config.length })}</span>
      <span class="faint">{t('pe.valArchiveMods', { n: valReport.archive_mod_count })}</span>
    </div>
    {#if valReport.missing_in_config.length}
      <div class="vallist">
        <div class="vl-h" style="color:var(--danger)">{t('pe.valMissingH')}</div>
        {#each valReport.missing_in_config as m}<div class="mono vl-row">{m}</div>{/each}
      </div>
    {/if}
    {#if valReport.extra_in_config.length}
      <div class="vallist">
        <div class="vl-h faint">{t('pe.valExtraH')}</div>
        {#each valReport.extra_in_config as m}<div class="mono vl-row">{m}</div>{/each}
      </div>
    {/if}
  </div>
  {/if}
{/snippet}

{#if reportTab && cfg}
  <FloatDock
    id="pack-report"
    title={t('pe.reports')}
    subtitle={packId}
    width={520}
    onClose={() => (reportTab = null)}
  >
    {#snippet header()}
      {#if reportTabs.length > 1}
        <TabStrip
          value={reportTab ?? "resolve"}
          tabs={reportTabs}
          ariaLabel={t('pe.reports')}
          onChange={(v) => (reportTab = v as ReportTab)}
        />
      {/if}
    {/snippet}
    {#if reportTab === 'resolve' && resReport}
      <ResolvePanel
        report={resReport}
        onSuggest={(sel) => {
          suggestQuery = sel.replace(/^modrinth:/, '');
          pick = { src: 'search', row: null };
        }}
      />
    {:else if reportTab === 'validate'}
      {@render validateReport()}
    {/if}
  </FloatDock>
{/if}

{#if logOpen}
  <!-- A tool, not a place: it opens over whatever is being worked on and the
       editor underneath never reflows. Its own dock id, so it remembers where it
       was left and can sit beside the report dock rather than fight it. -->
  <FloatDock
    id="pack-log"
    title={t('hist.title')}
    subtitle={packId}
    width={560}
    onClose={() => (logOpen = false)}
  >
    <PackLog
      {packId}
      {log}
      busy={buildBusy}
      hasMore={!!logNext}
      failed={logFailed}
      loadingMore={logBusy}
      onMore={moreLog}
      onChanged={() => (historyTick += 1)}
      onBuildCommit={(id) => {
        buildFrom = id;
        tab = 'build';
        logOpen = false;
      }}
    />
  </FloatDock>
{/if}

{#if pick?.src === 'search' && cfg}
  <ModPicker
    {packId}
    mc={cfg.minecraft_version}
    loader={cfg.loader.name}
    present={presentKeys(pick.row)}
    initialQuery={suggestQuery}
    onClose={() => {
      pick = null;
      suggestQuery = '';
    }}
    onPick={(sel) => {
      onMirrorPick(sel);
      suggestQuery = '';
    }}
  />
{/if}
{#if pick?.src === 'cache' && cfg}
  <MirrorPicker
    mc={cfg.minecraft_version}
    loader={cfg.loader.name}
    allowMany={pick.row === null}
    present={presentKeys(pick.row)}
    onClose={() => (pick = null)}
    onPick={onMirrorPick}
    onAddOne={onMirrorAddOne}
    onAddMany={onMirrorAddMany}
    onAddAsset={onMirrorAddAsset}
  />
{/if}
{#if pick?.src === 'modrinth' && cfg}
  <ModrinthPicker
    mc={cfg.minecraft_version}
    loader={cfg.loader.name}
    initialQuery={suggestQuery}
    present={presentKeys(pick.row)}
    onClose={() => {
      pick = null;
      suggestQuery = '';
    }}
    onPick={onModrinthPick}
  />
{/if}
{#if pick?.src === 'github' && cfg}
  <GithubPicker onClose={() => (pick = null)} onPick={onGithubPick} />
{/if}
{#if assetPick && cfg}
  <ModrinthPicker
    mc={cfg.minecraft_version}
    projectType={assetPick.projectType}
    onClose={() => (assetPick = null)}
    onPick={onAssetModrinthPick}
  />
{/if}

<style>
  .hd {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-3) var(--space-4);
    margin-bottom: var(--space-4);
  }
  .ttl {
    font-size: var(--fs-lg);
  }
  .tabs {
    flex: 0 1 auto;
    min-width: 0;
    overflow-x: auto;
    scrollbar-width: thin;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    margin-left: auto;
    flex: 0 0 auto;
  }
  .alsohere {
    font-size: var(--fs-xs);
    color: var(--accent-hue);
    white-space: nowrap;
  }
  .savestate {
    font-size: var(--fs-sm);
    color: var(--fg-dim);
    min-width: 78px;
    text-align: right;
  }
  .revertsel {
    display: inline-flex;
    max-width: 180px;
  }
  .savestate.err {
    color: var(--danger);
  }
  .empty {
    padding: var(--space-6);
    text-align: center;
  }
  .opts {
    display: flex;
    justify-content: center;
    gap: var(--space-3);
    margin-top: var(--space-3);
  }
  .meta {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: var(--space-3) var(--space-4);
  }
  .card {
    display: grid;
    gap: var(--space-3) var(--space-4);
  }
  .spoofbar {
    display: flex;
    gap: var(--space-3);
    margin-top: var(--space-3);
  }
  .gate {
    border: 1px solid var(--danger);
    border-left-width: 3px;
    padding: 10px 14px;
    margin: 10px 0 0;
    max-width: 720px;
  }
  .gate h4 {
    margin: 0;
    font-size: var(--fs-sm);
    color: var(--danger);
  }
  .gate ul {
    margin: 8px 0 0;
    padding-left: 18px;
    font-size: var(--fs-sm);
    line-height: 1.6;
  }
  .ok {
    color: var(--ok);
    font-size: var(--fs-sm);
  }
  .touched {
    flex-basis: 100%;
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: var(--space-2) var(--space-3);
    font-size: var(--fs-sm);
    order: 99; /* under the title row, whatever else the header holds */
  }
  .touch {
    padding: 1px 8px;
    border: 1px solid var(--seam-bright);
    border-radius: 999px;
    color: var(--fg-dim);
    /* arrives rather than blinks, and stands still under reduced motion --
       the duration token is zeroed there, like everything else */
    animation: row-in var(--dur-enter) var(--ease-out) backwards;
  }
  .javahint,
  .cardhint {
    grid-column: 1 / -1;
    font-size: var(--fs-sm);
    margin: -4px 0 0;
  }
  .cardhint {
    margin: 0 0 var(--space-2);
    line-height: 1.5;
  }
  .card textarea {
    resize: vertical;
    width: 100%;
  }
  .chk {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--fs-md);
    align-self: end;
    padding-bottom: 8px;
  }
  .pv.active {
    border-color: var(--accent);
    color: var(--accent-strong);
  }
  button.sm {
    padding: 4px 10px;
    font-size: var(--fs-sm);
  }
  button.sm.active {
    border-color: var(--accent);
    color: var(--accent-strong);
  }

  /* section spacing */
  .body :global(.section) {
    margin-bottom: var(--space-4);
  }

  /* mods */
  .mods {
    margin-top: var(--space-3);
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }
  .modrow {
    display: grid;
    grid-template-columns: 24px minmax(120px, 1.4fr) 116px minmax(120px, 1.2fr) auto minmax(90px, 1fr) 30px;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2);
    border: 1px solid var(--seam);
    border-radius: var(--radius-sm);
    background: var(--panel-2);
  }
  .modrow input {
    padding: 5px 7px;
    font-size: var(--fs-sm);
  }
  /* a value the mirror would refuse, marked where the row has no space to
     explain -- the sentence is on the control's title */
  input.bad {
    border-color: var(--danger);
  }
  /* curator slug in the 7th grid column: the stable optional-toggle key for
     smrt_cache mods (ADR 0002) */
  .modrow .slug {
    min-width: 0;
    opacity: 0.85;
  }
  /* the non-editable half of that column: a statement, not a disabled input */
  .modrow .keyed {
    font-size: var(--fs-xs);
    align-self: center;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* the source-type Select wrapper occupies the grid's 3rd column; the trigger
     (full) fills it, and min-width:0 lets it shrink in the narrow flex reflow */
  .srcsel {
    display: flex;
    min-width: 0;
  }
  .ref {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
  }
  .refval {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--fs-xs);
  }
  .ck {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: var(--fs-xs);
    color: var(--fg-dim);
    white-space: nowrap;
  }
  .del {
    padding: 4px 8px;
  }
  .empty-row {
    padding: var(--space-3);
    font-size: var(--fs-md);
  }
  .flushtable {
    margin-top: var(--space-3);
  }
  td.ctr {
    text-align: center;
  }
  td input {
    padding: 5px 7px;
    font-size: var(--fs-sm);
  }
  /* A file-input label styled as a button: the global `button` rules do not reach
     a <label>, and the `.sm` class only styles `button.sm`, so without this the
     control rendered as bare text next to real buttons. Matches `button.sm`. */
  .valbtn {
    display: inline-flex;
    align-items: center;
    font-family: var(--sans);
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--fg);
    background: var(--panel-2);
    border: 1px solid var(--seam-bright);
    border-radius: var(--radius-sm);
    padding: 4px 10px;
    cursor: pointer;
    transition:
      border-color var(--dur-state) var(--ease-out),
      background var(--dur-state) var(--ease-out);
  }
  .valbtn:hover {
    background: var(--panel-3);
  }
  .valrep {
    font-size: var(--fs-sm);
  }
  .valhead {
    display: flex;
    gap: var(--space-4);
    flex-wrap: wrap;
    font-size: var(--fs-sm);
  }
  .vallist {
    margin-top: var(--space-3);
  }
  .vl-h {
    font-size: var(--fs-xs);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    margin-bottom: 6px;
  }
  .vl-row {
    font-size: var(--fs-sm);
    padding: 2px 0;
  }
  .bootform {
    margin-top: var(--space-4);
    text-align: left;
  }
  .brow {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: var(--space-3);
    margin-bottom: var(--space-3);
  }
  .upbtn {
    display: inline-block;
    font-size: var(--fs-md);
    color: var(--fg);
    background: var(--panel-2);
    border: 1px solid var(--seam-bright);
    border-radius: var(--radius-sm);
    padding: 8px 14px;
    cursor: pointer;
  }
  .upbtn:hover {
    border-color: var(--accent);
  }
  .body.split {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    gap: var(--space-5);
    align-items: start;
  }
  .editcol {
    min-width: 0;
  }
  .previewcol {
    position: sticky;
    top: 12px;
    max-height: calc(100vh - 96px);
    overflow: auto;
    min-width: 0;
  }
  .dzone {
    margin-top: var(--space-6);
    border: 1px solid color-mix(in srgb, var(--danger) 40%, var(--seam));
    border-radius: var(--radius-md);
    overflow: hidden;
  }
  .dztitle {
    font-size: var(--fs-xs);
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--danger);
    padding: var(--space-3) var(--space-4);
    background: var(--danger-soft);
    border-bottom: 1px solid color-mix(in srgb, var(--danger) 30%, var(--seam));
  }
  .dzrow {
    display: flex;
    align-items: center;
    gap: var(--space-4);
    padding: var(--space-4);
  }
  .dztext {
    flex: 1;
    font-size: var(--fs-sm);
  }

  /* ---- responsive reflow ---- */
  /* multi-column forms collapse; the editor/preview split stacks. The desktop
     rules above are left untouched, so wide layouts are unchanged. */
  @container view (max-width: 768px) {
    .meta,
    .brow {
      grid-template-columns: repeat(2, 1fr);
    }
    .body.split {
      grid-template-columns: 1fr;
    }
    .previewcol {
      position: static;
      max-height: none;
    }
  }
  @container view (max-width: 560px) {
    .meta,
    .brow {
      grid-template-columns: 1fr;
    }
  }

  /* mod row: the 8-column desktop grid becomes a stacked flex card on narrow
     viewports. Every control is preserved -- only the arrangement changes. */
  @container view (min-width: 561px) and (max-width: 768px) {
    .modrow {
      display: flex;
      flex-wrap: wrap;
      gap: var(--space-2);
    }
    .modrow .fn {
      flex: 1 1 45%;
      min-width: 120px;
      width: auto;
    }
    .modrow .srcsel {
      flex: 0 0 auto;
      width: auto;
    }
    .modrow .ref {
      flex: 1 1 45%;
      min-width: 120px;
    }
    .modrow .ck {
      flex: 0 0 auto;
    }
    .modrow .slug {
      flex: 1 1 240px;
      min-width: 120px;
      width: auto;
    }
    .modrow .del {
      flex: 0 0 auto;
    }
  }
  @container view (max-width: 560px) {
    .modrow {
      display: flex;
      flex-wrap: wrap;
      gap: var(--space-2);
    }
    .modrow .fn {
      flex: 1 1 auto;
      min-width: 0;
      width: auto;
    }
    .modrow .srcsel,
    .modrow .ref {
      flex: 1 1 100%;
      width: auto;
    }
    .modrow .ck {
      flex: 0 0 auto;
    }
    .modrow .slug {
      flex: 1 1 auto;
      min-width: 120px;
      width: auto;
    }
    .modrow .del {
      flex: 0 0 auto;
    }
  }
</style>
