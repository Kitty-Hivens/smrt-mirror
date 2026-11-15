<script lang="ts">
  import { DropdownMenu } from 'bits-ui';
  import { t, i18n, LOCALES } from '../lib/i18n.svelte';
  import { theme, type ThemeChoice } from '../lib/theme.svelte';
  import { inbox } from '../lib/inbox.svelte';
  import { route } from '../lib/route.svelte';
  import Avatar from './Avatar.svelte';
  import Glyph from './ui/Glyph.svelte';

  // The account and the panel's preferences, in the corner where every service
  // keeps them. They used to be two sections of the rail, which put "dark or
  // light" on the same footing as the registry. A guest has no account and still
  // has preferences, so the corner holds a gear for them instead of a face.
  let {
    me,
    onLogout,
  }: {
    me: { uid: number; login: string; role: string } | null;
    onLogout: () => void;
  } = $props();

  const THEMES: { value: ThemeChoice; key: 'set.themeSystem' | 'set.themeDark' | 'set.themeLight' }[] = [
    { value: 'system', key: 'set.themeSystem' },
    { value: 'dark', key: 'set.themeDark' },
    { value: 'light', key: 'set.themeLight' },
  ];
  const LOCALE_LABEL: Record<string, string> = { ru: 'Русский', en: 'English' };
</script>

<DropdownMenu.Root>
  <DropdownMenu.Trigger
    class={'acct-trg' + (me ? '' : ' guest')}
    aria-label={me ? t('acct.menu', { login: me.login }) : t('acct.settings')}
  >
    {#if me}
      <Avatar uid={me.uid} login={me.login} size={28} />
      {#if inbox.unread}<span class="acct-dot" aria-hidden="true">{inbox.unread}</span>{/if}
    {:else}
      <Glyph name="gear" size={17} />
    {/if}
  </DropdownMenu.Trigger>
  <DropdownMenu.Portal>
    <DropdownMenu.Content class="acct-menu" align="end" sideOffset={8}>
      {#if me}
        <div class="acct-who">
          <Avatar uid={me.uid} login={me.login} size={36} />
          <div class="acct-whotext">
            <div class="acct-login">{me.login}</div>
            <div class="acct-role mono">{me.role}</div>
          </div>
        </div>
        <DropdownMenu.Item class="acct-item" onSelect={() => route.go('profile')}>
          <Glyph name="user" size={15} />
          <span class="acct-lbl">{t('acct.profile')}</span>
          {#if inbox.unread}<span class="acct-count">{inbox.unread}</span>{/if}
        </DropdownMenu.Item>
        <DropdownMenu.Separator class="acct-sep" />
      {/if}

      <DropdownMenu.Group>
        <DropdownMenu.GroupHeading class="acct-head">{t('set.theme')}</DropdownMenu.GroupHeading>
        <DropdownMenu.RadioGroup value={theme.choice} onValueChange={(v) => theme.set(v as ThemeChoice)}>
          <div class="acct-seg">
            {#each THEMES as opt (opt.value)}
              <DropdownMenu.RadioItem class="acct-segitem" value={opt.value} closeOnSelect={false}>
                {t(opt.key)}
              </DropdownMenu.RadioItem>
            {/each}
          </div>
        </DropdownMenu.RadioGroup>
      </DropdownMenu.Group>

      <DropdownMenu.Group>
        <DropdownMenu.GroupHeading class="acct-head">{t('set.language')}</DropdownMenu.GroupHeading>
        <DropdownMenu.RadioGroup value={i18n.locale} onValueChange={(v) => i18n.set(v as (typeof LOCALES)[number])}>
          <div class="acct-seg">
            {#each LOCALES as loc (loc)}
              <DropdownMenu.RadioItem class="acct-segitem" value={loc} closeOnSelect={false}>
                {LOCALE_LABEL[loc] ?? loc}
              </DropdownMenu.RadioItem>
            {/each}
          </div>
        </DropdownMenu.RadioGroup>
      </DropdownMenu.Group>

      {#if me}
        <DropdownMenu.Separator class="acct-sep" />
        <DropdownMenu.Item class="acct-item" onSelect={onLogout}>
          <Glyph name="exit" size={15} />
          <span class="acct-lbl">{t('shell.signOut')}</span>
        </DropdownMenu.Item>
      {/if}
    </DropdownMenu.Content>
  </DropdownMenu.Portal>
</DropdownMenu.Root>

<style>
  /* Every part rides a Bits component, so a class passed to it gets no Svelte
     scope hash: these are global, with names unique to this menu. */
  :global(.acct-trg) {
    position: relative;
    display: inline-grid;
    place-items: center;
    width: 36px;
    height: 36px;
    min-height: 0;
    padding: 0;
    border-radius: 999px;
    border: 1px solid transparent;
    background: transparent;
    box-shadow: none;
    color: var(--fg-dim);
  }
  :global(.acct-trg:hover),
  :global(.acct-trg[data-state='open']) {
    background: var(--panel-2);
    border-color: var(--seam);
    color: var(--fg);
  }
  :global(.acct-trg.guest) {
    border-color: var(--seam);
  }
  /* what is waiting to be read, on the face it is waiting for */
  :global(.acct-dot) {
    position: absolute;
    top: -3px;
    right: -5px;
    min-width: 17px;
    height: 17px;
    padding: 0 4px;
    display: grid;
    place-items: center;
    border-radius: 999px;
    border: 2px solid var(--bg);
    background: var(--solid);
    color: var(--on-solid);
    font-size: 10px;
    font-weight: 700;
    line-height: 1;
  }
  :global(.acct-menu) {
    z-index: 60;
    width: 280px;
    max-width: calc(100vw - 24px);
    padding: var(--space-2);
    border: 1px solid var(--seam-bright);
    border-radius: var(--radius-md);
    background: var(--panel);
    box-shadow: var(--shadow-pop);
    transform-origin: var(--bits-dropdown-menu-content-transform-origin, top right);
    animation: acct-in var(--dur-enter) var(--ease-out);
  }
  @keyframes acct-in {
    from {
      opacity: 0;
      transform: translateY(-4px) scale(0.98);
    }
  }
  :global(.acct-who) {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-2) var(--space-2) var(--space-3);
  }
  :global(.acct-whotext) {
    min-width: 0;
  }
  :global(.acct-login) {
    font-weight: 650;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  :global(.acct-role) {
    font-size: var(--fs-xs);
    color: var(--fg-faint);
  }
  :global(.acct-item) {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: 8px var(--space-2);
    border-radius: var(--radius-sm);
    font-size: var(--fs-md);
    color: var(--fg);
    cursor: pointer;
    outline: none;
  }
  :global(.acct-item .glyph) {
    color: var(--fg-dim);
  }
  :global(.acct-item[data-highlighted]) {
    background: var(--panel-2);
  }
  :global(.acct-lbl) {
    flex: 1;
  }
  :global(.acct-count) {
    font-size: var(--fs-xs);
    font-weight: 700;
    padding: 1px 7px;
    border-radius: 999px;
    background: var(--solid);
    color: var(--on-solid);
  }
  :global(.acct-sep) {
    height: 1px;
    margin: var(--space-2) calc(-1 * var(--space-2));
    background: var(--seam);
  }
  :global(.acct-head) {
    padding: var(--space-2) var(--space-2) 6px;
    font-size: var(--fs-xs);
    color: var(--fg-faint);
  }
  /* the choices of one preference side by side, as one control */
  :global(.acct-seg) {
    display: flex;
    gap: 2px;
    padding: 3px;
    margin: 0 var(--space-1) var(--space-1);
    border: 1px solid var(--seam);
    border-radius: var(--radius-sm);
    background: var(--panel-2);
  }
  :global(.acct-segitem) {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    padding: 6px 4px;
    border-radius: calc(var(--radius-sm) - 2px);
    font-size: var(--fs-sm);
    color: var(--fg-dim);
    text-align: center;
    cursor: pointer;
    outline: none;
    transition:
      background var(--dur-state) var(--ease-out),
      color var(--dur-state) var(--ease-out);
  }
  :global(.acct-segitem[data-highlighted]) {
    color: var(--fg);
    box-shadow: inset 0 0 0 1px var(--seam-bright);
  }
  :global(.acct-segitem[data-state='checked']) {
    background: var(--panel);
    color: var(--fg);
    box-shadow: var(--shadow-1);
  }
</style>
