<script lang="ts">
  import { api, ApiError } from '../lib/api';
  import { notifyFail } from '../lib/toasts.svelte';
  import { dialogs } from '../lib/dialogs.svelte';
  import { t } from '../lib/i18n.svelte';
  import { assetPath, isPackFile } from '../lib/packassets';
  import DropZone from './ui/DropZone.svelte';
  import ImageCropper from './ImageCropper.svelte';

  let {
    packId,
    onBranding,
  }: {
    packId: string;
    /// An icon or banner just landed in the pack's own static tree, at this
    /// path. The card's field is what makes it visible, and having uploaded the
    /// picture is the whole of what somebody meant to do -- so whoever owns the
    /// card is told, rather than leaving them to work out that the file is at
    /// `/v1/packs/<id>/static/<path>` and type it.
    onBranding?: (target: 'icon' | 'banner', relPath: string) => void;
  } = $props();

  // where a dropped file lands: the two branding images get stable names so the
  // pack-card URL stays put; everything else keeps its own filename
  type Dest = 'icon' | 'banner' | 'asset';
  let dest = $state<Dest>('asset');
  let files = $state<string[]>([]);
  let busy = $state(false);
  // a raster icon/banner drop opens the cropper before upload
  let crop = $state<{ file: File; aspect: number; target: 'icon' | 'banner' } | null>(null);
  const RASTER = /^image\/(png|jpeg|webp|gif)$/;
  // some OSes report an empty file.type for a valid image; fall back to the name
  const isRaster = (f: File) => RASTER.test(f.type) || /\.(png|jpe?g|webp|gif)$/i.test(f.name);

  async function load() {
    try {
      files = (await api.packStatic(packId)).files;
    } catch (e) {
      if (!(e instanceof ApiError && e.status === 404)) {
        notifyFail(e);
      }
      files = [];
    }
  }
  load();

  const ext = (name: string) => name.split('.').pop()?.toLowerCase() || 'png';

  function destFor(file: File): string {
    if (dest === 'icon') return assetPath(`icon.${ext(file.name)}`);
    if (dest === 'banner') return assetPath(`banner.${ext(file.name)}`);
    return assetPath(file.name);
  }

  async function uploadOne(relPath: string, data: File | Blob) {
    busy = true;
    try {
      const name = relPath.split('/').pop() || 'file';
      const f = data instanceof File ? data : new File([data], name, { type: data.type });
      await api.uploadStatic(packId, relPath, f);
      await load();
    } catch (x) {
      notifyFail(x);
    } finally {
      busy = false;
    }
  }

  // icon/banner must resolve to one file: drop any prior extension first so a
  // re-upload in another format can't leave a stale image the pack card points at
  async function putBranding(target: 'icon' | 'banner', relPath: string, data: File | Blob) {
    for (const f of files) {
      if (isPackFile(f, target) && f !== relPath) {
        try {
          await api.deleteStatic(packId, f);
        } catch {
          // best-effort cleanup; the upload below is what matters
        }
      }
    }
    await uploadOne(relPath, data);
    onBranding?.(target, relPath);
  }

  async function onDrop(dropped: File[]) {
    if (dest === 'asset') {
      // an asset drop may carry many; no crop
      busy = true;
      try {
        for (const file of dropped) await api.uploadStatic(packId, destFor(file), file);
        await load();
      } catch (x) {
        notifyFail(x);
      } finally {
        busy = false;
      }
      return;
    }
    // icon / banner: crop a raster image first; svg / non-raster goes as-is
    const file = dropped[0];
    if (!file) return;
    if (isRaster(file)) {
      crop = { file, aspect: dest === 'icon' ? 1 : 3, target: dest };
    } else {
      await putBranding(dest, destFor(file), file);
    }
  }

  function onCropApply(blob: Blob, outExt: string) {
    if (!crop) return;
    const target = crop.target;
    const relPath = assetPath(`${target}.${outExt}`);
    crop = null;
    putBranding(target, relPath, blob);
  }

  async function del(f: string) {
    const ok = await dialogs.confirm(t('be.deleteMsg', { file: f }), {
      title: t('be.deleteTitle'),
      danger: true,
    });
    if (!ok) return;
    try {
      await api.deleteStatic(packId, f);
      await load();
    } catch (x) {
      notifyFail(x);
    }
  }

  const isImage = (f: string) => /\.(png|jpe?g|gif|webp|svg)$/i.test(f);

  const modes: Dest[] = ['icon', 'banner', 'asset'];
  const modeKey: Record<Dest, Parameters<typeof t>[0]> = {
    icon: 'be.icon',
    banner: 'be.banner',
    asset: 'be.asset',
  };
  const dropLabel = $derived(
    busy
      ? t('pe.uploading')
      : dest === 'icon'
        ? t('be.dropIcon')
        : dest === 'banner'
          ? t('be.dropBanner')
          : t('be.dropAsset'),
  );
</script>

<p class="muted hint">{t('be.hint')}</p>

<div class="modes" role="group" aria-label={t('be.dropAs')}>
  <span class="ml">{t('be.dropAs')}</span>
  {#each modes as m}
    <button class="mode" class:active={dest === m} aria-pressed={dest === m} onclick={() => (dest = m)}>
      {t(modeKey[m])}
    </button>
  {/each}
</div>

<DropZone
  label={dropLabel}
  accept={dest === 'asset' ? undefined : 'image/*'}
  multiple={dest === 'asset'}
  {busy}
  onFiles={onDrop}
/>
<div class="formats faint">{t('be.formats')}</div>


<div class="grid">
  {#each files as f}
    <div class="card panel">
      {#if isImage(f)}
        <img src={api.staticUrl(packId, f)} alt={f} />
      {:else}
        <div class="ext mono">.{f.split('.').pop()}</div>
      {/if}
      <div class="meta">
        <div class="fn mono" title={f}>{f}</div>
        <div class="row2">
          <a class="mono" href={api.staticUrl(packId, f)} target="_blank" rel="noreferrer">{t('be.open')}</a>
          <button class="danger sm" onclick={() => del(f)}>{t('common.delete')}</button>
        </div>
      </div>
    </div>
  {/each}
  {#if files.length === 0}<div class="muted">{t('be.empty')}</div>{/if}
</div>

{#if crop}
  <ImageCropper
    file={crop.file}
    aspect={crop.aspect}
    maxOut={crop.target === 'icon' ? 512 : 1500}
    title={crop.target === 'icon' ? t('crop.titleIcon') : t('crop.titleBanner')}
    onApply={onCropApply}
    onCancel={() => (crop = null)}
  />
{/if}

<style>
  .hint {
    font-size: var(--fs-sm);
    margin: 0 0 var(--space-3);
    max-width: 720px;
    line-height: 1.5;
  }
  .modes {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    margin-bottom: var(--space-3);
  }
  .ml {
    font-size: var(--fs-sm);
    color: var(--fg-dim);
    margin-right: var(--space-1);
  }
  .mode {
    padding: 5px 12px;
    font-size: var(--fs-sm);
    color: var(--fg-dim);
    background: transparent;
  }
  .mode.active {
    background: var(--accent-soft);
    color: var(--accent-strong);
    border-color: var(--seam-bright);
  }
  .formats {
    font-size: var(--fs-xs);
    margin: var(--space-2) 0 var(--space-4);
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(160px, 1fr));
    gap: var(--space-3);
  }
  .card {
    overflow: hidden;
  }
  .card img {
    width: 100%;
    height: 110px;
    object-fit: contain;
    background: var(--bg);
    display: block;
  }
  .ext {
    height: 110px;
    display: grid;
    place-items: center;
    color: var(--fg-faint);
    background: var(--bg);
    font-size: var(--fs-xl);
  }
  .meta {
    padding: 8px 10px;
  }
  .fn {
    font-size: var(--fs-xs);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .row2 {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-top: 6px;
  }
  button.danger.sm {
    padding: 3px 9px;
    font-size: var(--fs-xs);
  }
</style>
