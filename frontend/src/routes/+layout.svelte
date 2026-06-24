<script lang="ts">
	import "glow/styles";
	import { toast, ToastContainer } from "glow";
	import { page } from "$app/state";
	import { goto } from "$app/navigation";
	import TopBar from "$lib/components/TopBar.svelte";
	import RemotePrompt from "$lib/components/RemotePrompt.svelte";
	import { remote } from "$lib/remote.svelte";
	import { api } from "$lib/api";

	let { children } = $props();

	// Connect, register this client and start tracking presence (browser-only).
	$effect(() => {
		remote.init();
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
</script>

<div class="app" class:tv-mode={remote.mode === "tv"}>
	{#if remote.mode !== "tv" && page.url.pathname !== "/remote"}
		<TopBar />
	{/if}
	<main>
		{@render children()}
	</main>
	<RemotePrompt />
	<ToastContainer position="top-center" />
</div>

<style>
	.app {
		display: flex;
		flex-direction: column;
		height: 100vh;
	}

	main {
		flex: 1;
		min-height: 0;
		overflow-x: clip;
	}

	@font-face {
		font-family: "Subtitle";
		src: url("/fonts/Helvetica Neue 67 Medium Condensed.otf")
			format("opentype");
		font-display: swap;
	}

	:global(:root) {
		--subtitle-font: "Subtitle", system-ui, sans-serif;
	}
</style>
