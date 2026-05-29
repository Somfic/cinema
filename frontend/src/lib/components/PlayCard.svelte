<script lang="ts">
	import { Icon } from "glow";

	let {
		image,
		label,
		action,
		remaining,
		progress,
		loading = false,
		onclick,
	}: {
		image?: string;
		label?: string;
		action: string;
		remaining?: string;
		progress?: number;
		loading?: boolean;
		onclick: () => void;
	} = $props();

	let bgA = $state<HTMLImageElement>(undefined!);
	let bgB = $state<HTMLImageElement>(undefined!);
	let useA = true;

	$effect(() => {
		if (!image || !bgA || !bgB) return;
		const incoming = useA ? bgA : bgB;
		const outgoing = useA ? bgB : bgA;
		const img = new Image();
		img.src = image;
		img.onload = () => {
			incoming.src = image;
			incoming.style.opacity = "1";
			outgoing.style.opacity = "0";
			useA = !useA;
		};
	});
</script>

<button class="card" {onclick}>
	<img class="bg" bind:this={bgA} alt="" />
	<img class="bg" bind:this={bgB} alt="" />
	<div class="overlay"></div>
	<div class="play-icon" class:spinning={loading}>
		<Icon name={loading ? "LoaderCircle" : "Play"} size={36} />
	</div>
	<div class="content">
		<div class="left">
			{#if label}
				<span class="label">{label}</span>
			{/if}
			<span class="action">{action}</span>
		</div>
		{#if remaining}
			<span class="remaining">{remaining}</span>
		{/if}
	</div>
	{#if progress != null && progress > 0}
		<div class="progress" style="width: {progress}%"></div>
	{/if}
</button>

<style>
	.card {
		position: relative;
		width: 100%;
		aspect-ratio: 16/9;
		border-radius: 10px;
		overflow: hidden;
		background: var(--bg-surface, #1a1a2e);
		border: none;
		cursor: pointer;
		padding: 0;
		color: inherit;
		transition: border-color 0.2s;
	}

	.card:hover {
		border-color: rgba(255, 255, 255, 0.2);
	}

	.bg {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		object-fit: cover;
		opacity: 0;
		transition: opacity 0.5s ease;
	}

	.overlay {
		position: absolute;
		inset: 0;
		background: linear-gradient(transparent 20%, rgba(0, 0, 0, 0.8));
	}

	.play-icon {
		position: absolute;
		inset: 0;
		display: flex;
		align-items: center;
		justify-content: center;
		z-index: 1;
		color: white;
		opacity: 0;
		transition: opacity 0.2s;
		background: rgba(0, 0, 0, 0.3);
	}

	.card:hover .play-icon,
	.play-icon.spinning {
		opacity: 1;
	}

	.spinning :global(svg) {
		animation: spin 1s linear infinite;
	}

	@keyframes spin {
		from {
			transform: rotate(0deg);
		}
		to {
			transform: rotate(360deg);
		}
	}

	.content {
		position: absolute;
		bottom: 0;
		left: 0;
		right: 0;
		padding: 0.6rem 0.8rem;
		display: flex;
		justify-content: space-between;
		align-items: flex-end;
		z-index: 1;
	}

	.left {
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	.label {
		font-size: 0.65rem;
		color: rgba(255, 255, 255, 0.5);
		text-transform: uppercase;
		letter-spacing: 0.04em;
		text-align: left;
	}

	.action {
		display: flex;
		align-items: center;
		justify-content: flex-start;
		gap: 4px;
		font-size: 0.85rem;
		font-weight: 600;
		color: white;
		text-align: left;
	}

	.remaining {
		font-size: 0.7rem;
		color: rgba(255, 255, 255, 0.5);
		white-space: nowrap;
	}

	.progress {
		position: absolute;
		bottom: 0;
		left: 0;
		height: 3px;
		background: var(--primary, #2563eb);
		z-index: 2;
	}
</style>
