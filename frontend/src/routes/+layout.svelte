<script lang="ts">
  import { Root, Page, toast } from "glow";
  import type { SidebarItem } from "glow/layout";
  import { page } from "$app/state";
  import { goto } from "$app/navigation";
  import RemotePrompt from "$lib/components/RemotePrompt.svelte";
  import { remote } from "$lib/remote.svelte";
  import { downloadManager } from "$lib/downloads.svelte";
  import { api } from "$lib/api";

  let { children } = $props();

  // Connect, register this client and start tracking presence (browser-only).
  $effect(() => {
    remote.init();
    downloadManager.init();
    api.onError((err) => {
      toast.error(`API error: ${err.message}`);
    });
  });

  // Remote side: mirror the phone's current page onto the paired TV so it
  // follows along as you browse. Reads page.url reactively. Suppressed while
  // the TV is playing (the phone is on the full-screen /remote page then) and
  // for /remote itself, which is a phone-only control surface.
  $effect(() => {
    if (remote.mode !== "remote" || remote.tvState?.playing) return;
    if (page.url.pathname === "/remote") return;
    // Include the query string so the TV follows season/episode selection
    // (and shows the episode still as its backdrop).
    remote.sendNavigate(page.url.pathname + page.url.search);
  });

  // Remote side: when the TV starts playing, jump the phone to the full-screen
  // controls page; when it stops, leave it. Edge-triggered so the user can
  // still minimise (browse) while the TV keeps playing. Leaving is debounced
  // because the TV briefly reports not-playing while switching source or
  // transcoding — that shouldn't bounce the phone out of /remote.
  let wasPlaying = false;
  let leaveTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    const playing = remote.mode === "remote" && !!remote.tvState?.playing;
    if (playing) {
      clearTimeout(leaveTimer);
      if (!wasPlaying) {
        wasPlaying = true;
        remote.closeCast();
        if (page.url.pathname !== "/remote") goto("/remote");
      }
    } else if (wasPlaying) {
      clearTimeout(leaveTimer);
      leaveTimer = setTimeout(() => {
        wasPlaying = false;
        if (window.location.pathname === "/remote") history.back();
      }, 2500);
    }
  });
  // --- Navigation rail -----------------------------------------------------

  // Hidden on the TV (which is a lean-back surface driven by the phone) and on
  // /remote, which is a full-screen phone-only control surface.
  const showSidebar = $derived(
    remote.mode !== "tv" && page.url.pathname !== "/remote",
  );

  const topItems: SidebarItem[] = [
    { label: "Home", href: "/", icon: "House" },
    { label: "Cache", href: "/cache", icon: "HardDrive" },
  ];

  const bottomItems: SidebarItem[] = [
    { label: "Settings", href: "/settings", icon: "Settings" },
  ];

  const TITLES: Record<string, string> = {
    "/": "Cinema",
    "/cache": "Cache",
    "/settings": "Settings",
  };
  const title = $derived(TITLES[page.url.pathname] ?? "Cinema");

  const sidebarConfig = { title: "Cinema", topItems, bottomItems };
</script>

<Root toastPosition="top-center">
  {#if showSidebar}
    <Page {title} layout="full" {sidebarConfig}>
      {@render children()}
    </Page>
  {:else}
    <!-- The TV and /remote are lean-back, full-bleed surfaces with no
		     navigation of their own — `bare` is the shell-less mode for exactly
		     that, and it is what the glow examples use for the same shape. -->
    <Page {title} layout="bare">
      {@render children()}
    </Page>
  {/if}
</Root>
<RemotePrompt />

<style>
  @font-face {
    font-family: "Subtitle";
    src: url("/fonts/Helvetica Neue 67 Medium Condensed.otf") format("opentype");
    font-display: swap;
  }

  :global(:root) {
    --subtitle-font: "Subtitle", system-ui, sans-serif;
  }
</style>
