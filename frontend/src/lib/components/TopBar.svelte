<script lang="ts">
	import { page } from "$app/state";
	import { goto } from "$app/navigation";
	import { Button, ButtonGroup, Icon } from "glow";
	import { getGoBack, getFocusSearch } from "$lib/topbar.svelte";
	import { remote } from "$lib/remote.svelte";

	// Cast affordance (mobile only).
	function handleCast() {
		if (remote.mode === "remote") {
			// Already connected: jump to the controls if playing, else show status.
			if (remote.tvState?.playing) goto("/remote");
			else remote.openCast();
			return;
		}
		// Not connected: with a single screen, just start casting; otherwise let
		// the user pick from the drawer.
		if (remote.castTargets.length === 1) {
			remote.becomeRemote(remote.castTargets[0].id);
		} else {
			remote.openCast();
		}
	}

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
		<Button icon="Search" variant="ghost" onclick={handleHome} />
		{#if !isRoot}
			<Button icon="ArrowLeft" variant="ghost" onclick={handleBack} />
		{/if}
	</div>
	<div class="center">
		{#if remote.mode === "remote"}
			<button class="casting" onclick={handleCast}>
				<Icon name="Cast" size={14} />
				Casting to {remote.pairedPeer?.label ?? "TV"}
			</button>
		{/if}
	</div>
	<div class="right">
		{#if remote.castTargets.length > 0 || remote.mode === "remote"}
			<div class="cast-button">
				<Button
					icon="Cast"
					tooltip={remote.mode === "remote"
						? "Disconnect"
						: "Cast to TV"}
					onclick={handleCast}
				/>
			</div>
		{/if}
		<Button
			icon="Settings"
			variant="ghost"
			onclick={() => goto("/settings")}
		/>
	</div>
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
		gap: 0.5rem;
	}

	.center {
		flex: 2;
		display: flex;
		align-items: center;
		justify-content: center;
		min-width: 0;
	}

	.casting {
		display: flex;
		align-items: center;
		gap: 0.35rem;
		background: none;
		border: none;
		color: inherit;
		cursor: pointer;
		font-size: 0.8rem;
		opacity: 0.7;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.right {
		justify-content: flex-end;
	}

	.cast-button {
		display: none;
	}

	@media (max-width: 768px) {
		.cast-button {
			display: flex;
		}
	}
</style>
