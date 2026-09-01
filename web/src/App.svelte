<script lang="ts">
  import { api, setUnauthorizedHandler } from './lib/api';
  import { t } from './lib/i18n.svelte';
  import { mirror } from './lib/mirror.svelte';
  import { route } from './lib/route.svelte';
  import { isOperator } from './lib/roles';
  import { lazy } from './lib/lazy';
  import { terms } from './lib/terms.svelte';
  import Login from './views/Login.svelte';
  import AppShell from './views/AppShell.svelte';
  import Browse from './views/Browse.svelte';
  import PublicBrowse from './views/PublicBrowse.svelte';
  import ModPage from './views/ModPage.svelte';
  import ThreadPage from './views/ThreadPage.svelte';
  import Profile from './views/Profile.svelte';
  import MyPacks from './views/MyPacks.svelte';
  import Settings from './views/Settings.svelte';
  import Skeleton from './views/ui/Skeleton.svelte';
  import Toaster from './views/ui/Toaster.svelte';
  import DialogHost from './views/DialogHost.svelte';

  type Me = {
    uid: number;
    login: string;
    role: string;
    accepted_terms: boolean;
    suspension?: { reason?: string; by_uid: number; by_login?: string; at: number };
  };
  // undefined = still checking; null = a guest (not signed in); object = identity
  let me = $state<Me | null | undefined>(undefined);
  let showLogin = $state(false);

  $effect(() => {
    api.me().then((m) => {
      me = m;
      if (m) terms.init(m.accepted_terms);
    });
  });

  // The change stream needs a session, so it opens with one and closes with it.
  // A guest listening would be a reconnect loop against a 401.
  $effect(() => {
    if (me) {
      mirror.connect();
      return () => mirror.disconnect();
    }
  });

  // A 401 on an authed call (expired session) drops back to the guest view.
  setUnauthorizedHandler(() => {
    me = null;
  });

  async function logout() {
    await api.logout();
    me = null;
  }
</script>

{#if me === undefined}
  <div class="boot"><span class="muted mono">{t('app.checkingSession')}</span></div>
{:else if showLogin}
  <Login onClose={() => (showLogin = false)} />
{:else}
  <AppShell me={me ?? null} onSignIn={() => (showLogin = true)} onLogout={logout}>
    {#if route.thread != null && route.pack == null}
      <!-- A discussion with no editor under it: the catalog's way in, and the
           only one a guest has. Above the section switch because it is a place
           of its own, the way a mod page is. -->
      <ThreadPage threadId={route.thread} />
    {:else if route.mod != null}
      <ModPage modRef={route.mod} me={me ?? null} onBack={() => route.closeMod()} />
    {:else if route.section === 'mods' && me}
      <!-- read-only for a member, full authoring for an operator; the view gates
           its own write surface, so one component serves both -->
      {#await lazy(() => import('./views/ModManager.svelte'))}
        <!-- the section's own shape while its code is on the wire, so the wait
             for the chunk and the wait for the data read as one -->
        <div class="panel"><Skeleton rows={6} height={73} gap={0} shape="row" lead={32} /></div>
      {:then { default: ModManager }}
        <ModManager />
      {:catch}
        <div class="muted mono">{t('app.partGone')}</div>
      {/await}
    {:else if route.section === 'graph' && me}
      <!-- read-only for a member, full (with debug authoring) for an operator; the
           view gates its own write affordances, so one component serves both.
           lazy: Svelte Flow + dagre are ~200KB, loaded only when the graph opens -->
      {#await lazy(() => import('./views/GraphView.svelte'))}
        <Skeleton rows={1} height={420} />
      {:then { default: GraphView }}
        <GraphView />
      {:catch}
        <div class="muted mono">{t('app.partGone')}</div>
      {/await}
    {:else if route.section === 'settings'}
      <!-- before any role check: preferences belong to whoever is looking -->
      <Settings />
    {:else if route.section === 'profile' && me}
      <Profile {me} />
    {:else if route.section === 'mypacks' && me}
      <MyPacks {me} />
    {:else if me && isOperator(me.role) && route.section !== 'browse'}
      <Browse {me} />
    {:else}
      <PublicBrowse me={me ?? null} onSignIn={() => (showLogin = true)} />
    {/if}
  </AppShell>
{/if}

<DialogHost />
<Toaster />

<style>
  .boot {
    display: grid;
    place-items: center;
    height: 100%;
  }
</style>
