<script lang="ts">
	import { Icon, Media } from "glow";
	import { api } from "$lib/api";

	let {
		image,
		trailerKeys = [],
		title,
		year,
		active = true,
		label,
		action,
		remaining,
		progress,
		loading = false,
		onclick,
	}: {
		image?: string;
		trailerKeys?: string[];
		title?: string | null;
		year?: string | null;
		active?: boolean;
		label?: string;
		action: string;
		remaining?: string;
		progress?: number;
		loading?: boolean;
		onclick: () => void;
	} = $props();

	let muted = $state(false);
	let trailerPlaying = $state(false);
	let aspect = $state<number>();
	// Which trailer in the list is currently playing.
	let index = $state(0);

	const trailerKey = $derived(trailerKeys[index]);
	const trailerSrc = $derived.by(() => {
		if (!trailerKey) return undefined;
		const params = new URLSearchParams();
		if (title) params.set("title", title);
		if (year) params.set("year", year);
		const qs = params.toString();
		return api.urls.trailer(trailerKey) + (qs ? `?${qs}` : "");
	});
	const loop = $derived(trailerKeys.length <= 1);

	$effect(() => {
		trailerKeys;
		index = 0;
		trailerPlaying = false;
	});

	$effect(() => {
		trailerKey;
		aspect = undefined;
	});

	function advance() {
		if (trailerKeys.length > 1) {
			index = (index + 1) % trailerKeys.length;
		}
	}

	function onReady(video: HTMLVideoElement) {
		video.addEventListener("ended", advance, { once: true });
	}

	async function onPlaying() {
		trailerPlaying = true;
		if (!trailerKey || aspect != null) return;
		try {
			const meta = await api.trailer.meta(trailerKey);
			if (meta?.aspect) aspect = meta.aspect;
		} catch {
			/* leave at default 16:9 */
		}
	}

	function activate(e: KeyboardEvent) {
		if (e.key === "Enter" || e.key === " ") {
			e.preventDefault();
			onclick();
		}
	}

	function toggleMute(e: MouseEvent) {
		e.stopPropagation();
		muted = !muted;
	}
</script>

<div
	class="card"
	role="button"
	tabindex="0"
	style="aspect-ratio: {aspect ?? '16 / 9'}"
	{onclick}
	onkeydown={activate}
>
	<div class="bg">
		{#key trailerKey}
			<Media
				src={trailerSrc ?? image}
				fallback={trailerSrc ? image : undefined}
				type={trailerSrc ? "video" : "image"}
				fit="cover"
				autoplay
				{active}
				{loop}
				{muted}
				onVideoReady={onReady}
				onVideoPlaying={onPlaying}
			/>
		{/key}
	</div>
	<div class="overlay"></div>
	<div class="play-icon" class:spinning={loading}>
		<Icon name={loading ? "LoaderCircle" : "Play"} size={36} />
	</div>
	{#if trailerPlaying}
		<button
			class="mute"
			onclick={toggleMute}
			aria-label={muted ? "Unmute trailer" : "Mute trailer"}
		>
			<Icon name={muted ? "VolumeX" : "Volume2"} size={18} />
		</button>
	{/if}
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
</div>

<style>
	.card {
		position: relative;
		width: 100%;
		/* aspect-ratio is set inline from the detected trailer content ratio,
		   defaulting to 16/9. */
		border-radius: 10px;
		overflow: hidden;
		background: var(--bg-surface, #1a1a2e);
		border: none;
		cursor: pointer;
		padding: 0;
		color: inherit;
		transition:
			border-color 0.2s,
			aspect-ratio 0.4s ease;
	}

	.card:hover {
		border-color: rgba(255, 255, 255, 0.2);
	}

	.bg {
		position: absolute;
		inset: 0;
	}

	.overlay {
		position: absolute;
		inset: 0;
		background: linear-gradient(transparent 20%, rgba(0, 0, 0, 0.8));
		z-index: 1;
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

	.mute {
		position: absolute;
		top: 0.6rem;
		right: 0.6rem;
		z-index: 2;
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 0.4rem;
		border: none;
		border-radius: 50%;
		background: rgba(0, 0, 0, 0.5);
		color: white;
		cursor: pointer;
		opacity: 0;
		transition: opacity 0.2s;
	}

	.card:hover .mute {
		opacity: 1;
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
