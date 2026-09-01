<script lang="ts">
  // Who has been let into this pack (ADR 0006).
  //
  // The list holds grants and nothing else. The owner of a community namespace
  // and the mirror's admins reach the pack without a row, so the view says that
  // in words rather than inventing entries the store does not have -- a list
  // that quietly showed them would be a list nobody could trust to be complete.
  import { api } from '../lib/api';
  import { notifyFail, toasts } from '../lib/toasts.svelte';
  import { t } from '../lib/i18n.svelte';
  import Skeleton from './ui/Skeleton.svelte';
  import { dialogs } from '../lib/dialogs.svelte';
  import { nameOf as decidedBy } from '../lib/people';
  import type { PackBlock, PackGrant, PackLevel } from '../lib/types';

  let {
    packId,
    canGrant = false,
    canModerate = false,
  }: {
    packId: string;
    /// Hand out and take back access -- the owner's call.
    canGrant?: boolean;
    /// Moderate: see who was stopped from writing here, and let them back.
    canModerate?: boolean;
  } = $props();

  let rows = $state<PackGrant[]>([]);
  let blocks = $state<PackBlock[]>([]);
  let loading = $state(true);
  let failed = $state(false);
  let working = $state(false);

  // The form: a uid rather than a name, because a grant is keyed by the GitHub
  // account and a login can be changed by its owner at any time.
  let uid = $state('');
  let level = $state<PackLevel>('edit');

  const LEVELS: PackLevel[] = ['view', 'edit', 'own'];

  $effect(() => {
    void load(packId);
  });

  async function load(pack: string) {
    loading = true;
    try {
      rows = await api.packAccess(pack);
      failed = false;
    } catch (e) {
      failed = true;
      notifyFail(e);
    } finally {
      loading = false;
    }
    // A moderator who cannot see the list cannot undo a row in it. Read
    // separately: a pack with nobody blocked is the normal case, and failing to
    // read an empty list must not take the access list down with it.
    if (canModerate) {
      blocks = await api.packBlocks(pack).catch(() => []);
    }
  }

  async function unblock(row: PackBlock) {
    const who = row.login ?? String(row.github_uid);
    if (!(await dialogs.confirm(t('acc.unblockAsk', { who }), { title: t('acc.unblock') }))) return;
    working = true;
    try {
      await api.unblockFromPack(packId, row.github_uid);
      await load(packId);
    } catch (e) {
      notifyFail(e);
    } finally {
      working = false;
    }
  }

  async function grant() {
    const who = Number(uid.trim());
    if (!Number.isInteger(who) || who <= 0) {
      toasts.push({ kind: 'info', text: t('acc.needUid') });
      return;
    }
    working = true;
    try {
      await api.grantPackAccess(packId, who, level);
      uid = '';
      await load(packId);
    } catch (e) {
      notifyFail(e);
    } finally {
      working = false;
    }
  }

  async function revoke(row: PackGrant) {
    const who = row.login ?? String(row.github_uid);
    if (!(await dialogs.confirm(t('acc.revokeAsk', { who }), { title: t('acc.revoke'), danger: true }))) {
      return;
    }
    working = true;
    try {
      await api.revokePackAccess(packId, row.github_uid);
      await load(packId);
    } catch (e) {
      notifyFail(e);
    } finally {
      working = false;
    }
  }

  function when(at: number): string {
    const d = new Date(at * 1000);
    return Number.isNaN(d.getTime()) ? String(at) : d.toLocaleDateString();
  }
</script>

<div class="access">
  <p class="muted lead">{t('acc.lead')}</p>

  {#if loading && !rows.length}
    <Skeleton rows={2} height={34} />
  {:else if failed}
    <p class="muted">{t('acc.unreadable')}</p>
  {:else if !rows.length}
    <p class="muted empty">{t('acc.none')}</p>
  {:else}
    <ul class="rows">
      {#each rows as r (r.github_uid)}
        <li>
          <span class="who">{r.login ?? t('acc.unknownUser', { uid: r.github_uid })}</span>
          <span class="lvl" data-level={r.level}>{t(`acc.level.${r.level}`)}</span>
          <span class="muted meta">{t('acc.grantedBy', { by: decidedBy(r.granted_by, r.granted_by_login), at: when(r.granted_at) })}</span>
          {#if canGrant}
            <button class="link danger" onclick={() => revoke(r)} disabled={working}>
              {t('acc.revoke')}
            </button>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}

  {#if canModerate && blocks.length}
    <!-- The same question from the other side: the list above is who may reach
         this pack, this one is who may no longer write on it. -->
    <p class="muted lead blocked">{t('acc.blockedLead')}</p>
    <ul class="rows">
      {#each blocks as b (b.github_uid)}
        <li>
          <span class="who">{b.login ?? t('acc.unknownUser', { uid: b.github_uid })}</span>
          {#if b.reason}<span class="muted">{b.reason}</span>{/if}
          <span class="muted meta">{t('acc.blockedBy', { by: decidedBy(b.blocked_by, b.blocked_by_login), at: when(b.blocked_at) })}</span>
          <button class="link" onclick={() => unblock(b)} disabled={working}>
            {t('acc.unblock')}
          </button>
        </li>
      {/each}
    </ul>
  {/if}

  {#if canGrant}
    <div class="add">
      <input
        bind:value={uid}
        placeholder={t('acc.uidPlaceholder')}
        disabled={working}
        onkeydown={(e) => e.key === 'Enter' && grant()}
      />
      <select bind:value={level} disabled={working}>
        {#each LEVELS as l (l)}
          <option value={l}>{t(`acc.level.${l}`)}</option>
        {/each}
      </select>
      <button onclick={grant} disabled={working || !uid.trim()}>{t('acc.grant')}</button>
    </div>
    <p class="muted hint">{t('acc.hint')}</p>
  {/if}
</div>

<style>
  .access {
    max-width: 640px;
    padding: 4px 0;
  }
  .lead {
    font-size: var(--fs-sm);
    margin: 0 0 12px;
  }
  .blocked {
    margin-top: 4px;
  }
  .rows {
    list-style: none;
    margin: 0 0 14px;
    padding: 0;
    border-top: 1px solid var(--seam);
  }
  .rows li {
    display: flex;
    align-items: baseline;
    gap: 12px;
    padding: 8px 0;
    border-bottom: 1px solid var(--seam);
    font-size: var(--fs-sm);
  }
  .who {
    font-weight: 500;
  }
  .lvl {
    font-variant: small-caps;
    color: var(--fg-dim);
  }
  .lvl[data-level='own'] {
    color: var(--warn, var(--fg));
  }
  .meta {
    margin-left: auto;
  }
  .empty {
    font-size: var(--fs-sm);
  }
  .add {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .add input {
    font: inherit;
    width: 200px;
  }
  .hint {
    font-size: var(--fs-sm);
    margin: 8px 0 0;
  }
  @container view (max-width: 560px) {
    .rows li {
      flex-wrap: wrap;
    }
    .meta {
      margin-left: 0;
      flex: 1 1 100%;
    }
  }
</style>
