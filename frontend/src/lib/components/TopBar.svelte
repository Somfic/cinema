<script lang="ts">
	import { page } from "$app/state";
	import { goto } from "$app/navigation";
	import { Button, ButtonGroup } from "glow";
	import { getGoBack, getFocusSearch } from "$lib/topbar.svelte";

	const base = "";

	let isRoot = $derived(
		page.url.pathname === "/" || page.url.pathname === base + "/",
	);

	let parentPath = $derived(base);

	async function handleHome() {
		if (isRoot) {
			getFocusSearch()?.();
		} else {
			await goto(base);
			requestAnimationFrame(() => getFocusSearch()?.());
		}
	}

	function handleBack() {
		const goBack = getGoBack();
		if (goBack) {
			const handled = goBack();
			if (!handled) {
				history.back();
			}
		} else {
			history.back();
		}
	}
</script>

<header class="top-bar">
	<div class="left">
		<ButtonGroup>
			<Button icon="Search" variant="ghost" onclick={handleHome} />
			{#if !isRoot}
				<Button icon="ArrowLeft" variant="ghost" onclick={handleBack} />
			{/if}
		</ButtonGroup>
	</div>
	<div class="center"></div>
	<div class="right"></div>
</header>

<style lang="scss">
	@use "glow/src/lib/style/theme.scss" as *;

	.top-bar {
		display: flex;
		align-items: center;
		padding: 0.5rem 1rem;
		background-color: $bg-surface;
		border-bottom: $border;
		min-height: 3rem;
		position: relative;
		z-index: 6;
	}

	.left,
	.right {
		flex: 1;
		display: flex;
		align-items: center;
	}

	.center {
		flex: 2;
		display: flex;
		align-items: center;
		justify-content: center;
	}

	.right {
		justify-content: flex-end;
	}
</style>
