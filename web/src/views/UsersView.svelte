<script lang="ts">
  import { api } from '../lib/api';
  import { notifyFail } from '../lib/toasts.svelte';
  import { dialogs } from '../lib/dialogs.svelte';
  import { t } from '../lib/i18n.svelte';
  import { nameOf } from '../lib/people';
  import type { UserRow } from '../lib/types';
  import Avatar from './Avatar.svelte';

  // low -> high; debug is the compat-authoring rung above admin (#39)
  const ROLES = ['member', 'admin', 'debug'] as const;

  let users = $state<UserRow[]>([]);
  let meUid = $state<number | null>(null);
  let loading = $state(true);

  async function load() {
    loading = true;
    try {
      const [u, me] = await Promise.all([api.listUsers(), api.me()]);
      users = u;
      meUid = me?.uid ?? null;
    } catch (e) {
      notifyFail(e);
    } finally {
      loading = false;
    }
  }
  load();

  async function setRole(u: UserRow, role: string) {
    try {
      await api.setUserRole(u.github_uid, role);
      await load();
    } catch (e) {
      notifyFail(e);
    }
  }

  /// Stop an account putting anything on the mirror, or let it back. The
  /// operators' answer to what a pack's own block cannot reach -- somebody whose
  /// pack was itself the offence.
  async function suspend(u: UserRow) {
    const reason = await dialogs.prompt(t('users.suspendAsk', { who: u.login }), {
      title: t('users.suspend'),
      placeholder: t('users.suspendReason'),
    });
    if (reason == null) return;
    try {
      await api.suspendAccount(u.github_uid, reason.trim() || undefined);
      await load();
    } catch (e) {
      notifyFail(e);
    }
  }

  async function lift(u: UserRow) {
    if (!(await dialogs.confirm(t('users.liftAsk', { who: u.login }), { title: t('users.lift') })))
      return;
    try {
      await api.liftSuspension(u.github_uid);
      await load();
    } catch (e) {
      notifyFail(e);
    }
  }

  function when(at: number): string {
    const d = new Date(at * 1000);
    return Number.isNaN(d.getTime()) ? String(at) : d.toLocaleDateString();
  }

  // last-login timestamps are unix seconds; 0 marks a row that never logged in
  function seen(unix: number): string {
    if (!unix) return t('users.never');
    return new Date(unix * 1000).toISOString().slice(0, 10);
  }
</script>

<div class="view">

  <div class="panel ulist">
    {#each users as u (u.github_uid)}
      <div class="urow">
        <Avatar uid={u.github_uid} login={u.login} size={32} />
        <div class="uinfo">
          <div class="uname">
            {u.login}{#if u.github_uid === meUid}<span class="me mono">{t('users.you')}</span>{/if}
          </div>
          <div class="umeta muted mono">
            <span>uid {u.github_uid}</span>
            <span>{t('users.lastLogin')} {seen(u.last_login_at)}</span>
          </div>
          {#if u.suspension}
            <div class="stopped">
              {u.suspension.reason
                ? t('users.suspendedWhy', {
                    reason: u.suspension.reason,
                    by: nameOf(u.suspension.by_uid, u.suspension.by_login),
                    at: when(u.suspension.at),
                  })
                : t('users.suspendedBy', {
                    by: nameOf(u.suspension.by_uid, u.suspension.by_login),
                    at: when(u.suspension.at),
                  })}
            </div>
          {/if}
        </div>
        <span class="chip role-{u.role}">{u.role}</span>
        {#if u.github_uid !== meUid}
          <div class="roleset">
            {#each ROLES.filter((r) => r !== u.role) as r}
              <button class="link" onclick={() => setRole(u, r)}>{t(`users.make.${r}`)}</button>
            {/each}
            {#if u.suspension}
              <button class="link" onclick={() => lift(u)}>{t('users.lift')}</button>
            {:else if u.role === 'member'}
              <button class="link danger" onclick={() => suspend(u)}>{t('users.suspend')}</button>
            {/if}
          </div>
        {/if}
      </div>
    {/each}
    {#if users.length === 0 && !loading}
      <div class="empty muted">{t('users.empty')}</div>
    {/if}
  </div>
</div>

<style>
  .view {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
  .err {
    color: var(--danger);
    background: var(--danger-soft);
    border: 1px solid color-mix(in srgb, var(--danger) 40%, transparent);
    border-radius: var(--radius-sm);
    padding: var(--space-3) var(--space-4);
    font-size: var(--fs-sm);
  }
  .ulist {
    overflow: hidden;
  }
  .urow {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto auto;
    align-items: center;
    gap: var(--space-2) var(--space-3);
    padding: var(--space-3);
    border-bottom: 1px solid var(--seam);
  }
  .urow:last-child {
    border-bottom: none;
  }
  .uinfo {
    min-width: 0;
  }
  .uname {
    font-size: var(--fs-lg);
    font-weight: 600;
  }
  .me {
    margin-left: 6px;
    font-size: var(--fs-xs);
    color: var(--fg-faint);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    font-weight: 400;
  }
  .stopped {
    margin-top: 4px;
    padding-left: 8px;
    border-left: 2px solid var(--danger);
    font-size: var(--fs-sm);
    color: var(--fg-dim);
  }
  .umeta {
    font-size: var(--fs-xs);
    margin-top: 2px;
  }
  /* each fact wraps as a whole, so a date never breaks across lines */
  .umeta span {
    white-space: nowrap;
  }
  /* the separator opens the second fact, so a wrap carries it to the new line
     instead of leaving it hanging at the end of the first */
  .umeta span + span::before {
    content: '\00B7\00A0';
  }
  .chip {
    font-size: var(--fs-xs);
    padding: 1px 8px;
    border: 1px solid var(--seam);
    border-radius: 999px;
    color: var(--fg-dim);
    flex-shrink: 0;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    font-family: var(--mono);
  }
  .chip.role-admin {
    color: var(--info);
    border-color: color-mix(in srgb, var(--info) 45%, var(--seam));
    background: var(--info-soft);
  }
  .chip.role-debug {
    color: var(--warn);
    border-color: color-mix(in srgb, var(--warn) 45%, var(--seam));
    background: var(--warn-soft);
  }
  .roleset {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
  }
  /* A narrow column moves the actions onto a line of their own under the
     name, rather than pushing the last of them off the edge. */
  @container view (max-width: 600px) {
    .urow {
      grid-template-columns: auto minmax(0, 1fr) auto;
    }
    .roleset {
      grid-column: 2 / -1;
      justify-content: flex-start;
      margin-left: -8px;
    }
  }
  .link {
    background: transparent;
    border: none;
    border-radius: 0;
    color: var(--fg-dim);
    padding: 4px 8px;
    font-size: var(--fs-xs);
    flex-shrink: 0;
  }
  .link:hover {
    color: var(--fg);
  }
  .empty {
    padding: var(--space-4);
    font-size: var(--fs-sm);
  }
</style>
