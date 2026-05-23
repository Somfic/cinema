<script lang="ts">
	import "glow/styles";
	import TopBar from "$lib/components/TopBar.svelte";

	let { children } = $props();
</script>

<div class="app">
	<TopBar />
	<main>
		{@render children()}
	</main>
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
		font-family: 'Subtitle';
		src: url('/fonts/Helvetica Neue 67 Medium Condensed.otf') format('opentype');
		font-display: swap;
	}

	:global(:root) {
		--subtitle-font: 'Subtitle', system-ui, sans-serif;
	}
</style>
