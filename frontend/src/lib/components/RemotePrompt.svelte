<script lang="ts">
	import { goto } from "$app/navigation";
	import { Drawer, Icon, type ButtonAction } from "glow";
	import { remote } from "$lib/remote.svelte";

	const tv = $derived(remote.tvState);
	const hasMedia = $derived(!!tv?.playing && (tv?.duration ?? 0) > 0);

	const title = $derived(
		remote.mode === "remote"
			? `Connected to ${remote.pairedPeer?.label ?? "TV"}`
			: "Cast to a TV",
	);

	function openControls() {
		remote.closeCast();
		goto("/remote");
	}

	function disconnect() {
		remote.disconnect();
		remote.closeCast();
	}

	// Footer action: only show Disconnect while connected.
	const actions = $derived<ButtonAction[] | undefined>(
		remote.mode === "remote"
			? [{ label: "Disconnect", icon: "Power", variant: "ghost", onclick: disconnect }]
			: undefined,
	);
</script>

<Drawer
	bind:open={remote.castDrawerOpen}
	side="bottom"
	{title}
	icon={remote.mode === "remote" ? "Tv" : "Cast"}
	{actions}
>
	{#if remote.mode === "remote"}
		{#if hasMedia}
			<button class="now" onclick={openControls}>
				<div class="now-text">
					<span class="now-title">{tv?.title ?? "Now playing"}</span>
					{#if tv?.topline}<span class="now-sub">{tv.topline}</span>{/if}
				</div>
				<Icon name="SlidersHorizontal" size={18} />
			</button>
		{:else}
			<div class="idle">
				<Icon name="Cast" size={28} />
				<p>Connected.</p>
				<p class="idle-sub">Browse and tap play — it'll start on the TV.</p>
			</div>
		{/if}
	{:else if remote.castTargets.length > 0}
		<div class="targets">
			{#each remote.castTargets as target (target.id)}
				<button class="target" onclick={() => remote.becomeRemote(target.id)}>
					<Icon name="Tv" size={20} />
					<span class="label">{target.label}</span>
					<Icon name="ChevronRight" size={18} />
				</button>
			{/each}
		</div>
	{:else}
		<div class="idle">
			<Icon name="TvMinimal" size={28} />
			<p>No screens found.</p>
			<p class="idle-sub">
				Open this app on a bigger screen on the same network.
			</p>
		</div>
	{/if}
</Drawer>

<style lang="scss">
	@use "glow/styles/theme" as *;

	.now {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		width: 100%;
		padding: 0.75rem 0.5rem;
		background: $bg-surface-element;
		border: none;
		border-radius: 0.5rem;
		color: inherit;
		cursor: pointer;
		text-align: left;
	}

	.now-text {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
	}

	.now-title {
		font-weight: 600;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.now-sub {
		font-size: 0.8rem;
		opacity: 0.6;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.targets {
		display: flex;
		flex-direction: column;
		gap: 0.375rem;
	}

	.target {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		width: 100%;
		padding: 0.75rem 0.5rem;
		background: none;
		border: none;
		border-radius: 0.5rem;
		color: inherit;
		cursor: pointer;
		text-align: left;
	}

	.target:hover,
	.target:active {
		background: $bg-surface-element;
	}

	.target .label {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: 0.95rem;
	}

	.idle {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.3rem;
		padding: 1.25rem 0 0.75rem;
		text-align: center;
		opacity: 0.75;
	}

	.idle p {
		margin: 0;
	}

	.idle-sub {
		font-size: 0.8rem;
		opacity: 0.7;
	}
</style>
