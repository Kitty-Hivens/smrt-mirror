<script lang="ts">
  import { Dialog } from 'bits-ui';
  import { api, ApiError } from '../lib/api';
  import { t } from '../lib/i18n.svelte';
  import type { SourceDecl } from '../lib/types';
  import Field from './ui/Field.svelte';

  let {
    onPick,
    onClose,
  }: {
    onPick: (sel: { filename: string; source: SourceDecl }) => void;
    onClose: () => void;
  } = $props();

  let repo = $state('');
  let tag = $state('');
  let asset = $state('');
  let busy = $state(false);
  let err = $state('');

  const ready = $derived(!!(repo.trim() && tag.trim() && asset.trim()));

  /// Name the asset and leave it where it is. A release asset has a derivable
  /// address, so the launcher downloads it from whoever published it: nothing
  /// is fetched here and no copy is kept on this mirror. Whether the asset is
  /// really there is answered by the first resolve, which is also where its
  /// content hash comes from.
  function pin() {
    if (!ready || busy) return;
    onPick({
      filename: asset.trim(),
      source: { type: 'github', repo: repo.trim(), tag: tag.trim(), asset: asset.trim() },
    });
  }

  /// The other half, and the reason the ingest route still exists: the bytes
  /// land in the mirror's cache and the row pins that copy. It is what a pack
  /// is left with once a release is deleted or taken down.
  async function copyIn() {
    if (!ready || busy) return;
    busy = true;
    err = '';
    try {
      const r = await api.ingestGithub(repo.trim(), tag.trim(), asset.trim());
      onPick({ filename: asset.trim(), source: { type: 'smrt_cache', sha1: r.sha1 } });
    } catch (e) {
      err = e instanceof ApiError ? `${e.status} ${e.body}` : String(e);
      busy = false;
    }
  }

  // escape / outside-click flip Bits' open to false; the parent unmounts us on close
  function onOpenChange(open: boolean) {
    if (!open) onClose();
  }
</script>

<Dialog.Root open {onOpenChange}>
  <Dialog.Overlay class="dlg-scrim" />
  <Dialog.Content class="ghp-dlg panel">
    <div class="hd row">
      <Dialog.Title level={3} class="ghp-h">{t('gh.title')}</Dialog.Title>
      <div class="sp"></div>
      <button onclick={onClose}>{t('common.close')}</button>
    </div>
    <p class="muted hint">{t('gh.hint')}</p>
    <div class="form">
      <Field label={t('gh.repo')}>
        <input class="mono" bind:value={repo} placeholder="Kitty-Hivens/open-smrt-network" />
      </Field>
      <Field label={t('gh.tag')}><input class="mono" bind:value={tag} placeholder="v1.0.0" /></Field>
      <Field label={t('gh.asset')}>
        <input class="mono" bind:value={asset} placeholder="open-smrt-network-1.0.0.jar" />
      </Field>
    </div>
    {#if err}<div class="err mono">{err}</div>{/if}
    <div class="row foot">
      <div class="sp"></div>
      <button onclick={copyIn} disabled={!ready || busy} title={t('gh.copyHint')}>
        {busy ? t('gh.copying') : t('gh.copy')}
      </button>
      <button class="primary" onclick={pin} disabled={!ready || busy} title={t('gh.pinHint')}>
        {t('gh.pin')}
      </button>
    </div>
  </Dialog.Content>
</Dialog.Root>

<style>
  /* Panel + title classes ride on Bits components, so they are global (no scope
     hash) and uniquely named to avoid colliding with the DialogHost .dlg/.overlay
     globals. The backdrop is the shared .dlg-scrim in app.css. */
  :global(.ghp-dlg) {
    position: fixed;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    z-index: 61;
    width: 520px;
    max-width: 92vw;
    padding: var(--space-4);
  }
  .hd {
    margin-bottom: var(--space-2);
  }
  :global(.ghp-h) {
    font-size: var(--fs-lg);
  }
  .sp {
    flex: 1;
  }
  .hint {
    font-size: var(--fs-sm);
    margin: 0 0 var(--space-4);
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }
  .err {
    color: var(--danger);
    font-size: var(--fs-sm);
    margin-top: var(--space-3);
  }
  .foot {
    margin-top: var(--space-4);
  }
  @media (max-width: 560px) {
    :global(.ghp-dlg) {
      padding: var(--space-3);
    }
  }
</style>
