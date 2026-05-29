<script lang="ts">
	import { Pill } from "glow";
	import type { Stream } from "$lib/schema";

	let {
		streams,
		onplay,
	}: {
		streams: Stream[];
		onplay: (stream: Stream) => void;
	} = $props();
</script>

<div class="streams">
	{#each streams as stream}
		<button class="stream-option" onclick={() => onplay(stream)}>
			<div class="stream-main">
				{#if stream.resolution}
					<span class="stream-res">{stream.resolution}</span>
				{/if}
				<div class="stream-badges">
					{#if stream.hdr}
						<Pill label="HDR" color="#d97706" />
					{/if}
					{#if stream.imax}
						<Pill label="IMAX" color="#dc2626" />
					{/if}
					{#if stream.codec}
						<Pill label={stream.codec} />
					{/if}
					{#if stream.audio}
						<Pill label={stream.audio} color="#7c3aed" />
					{/if}
					{#if stream.source_type}
						<Pill label={stream.source_type} color="#065f46" />
					{/if}
				</div>
			</div>
			<div class="stream-meta">
				{#if stream.seeders}
					<span class="stream-stat">
						<span class="stream-stat-icon">👤</span>
						{stream.seeders}
					</span>
				{/if}
				{#if stream.size_display}
					<span class="stream-stat">
						<span class="stream-stat-icon">💾</span>
						{stream.size_display}
					</span>
				{/if}
				<span class="stream-source">{stream.source}</span>
			</div>
		</button>
	{/each}
</div>

<style>
	.streams {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
	}

	.stream-option {
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
		background: rgba(255, 255, 255, 0.04);
		backdrop-filter: blur(8px);
		border: 1px solid rgba(255, 255, 255, 0.06);
		border-radius: 10px;
		padding: 0.65rem 0.85rem;
		cursor: pointer;
		text-align: left;
		color: inherit;
		transition:
			background 0.2s,
			border-color 0.2s,
			transform 0.15s;
	}

	.stream-option:hover {
		background: rgba(255, 255, 255, 0.08);
		border-color: rgba(255, 255, 255, 0.12);
		transform: translateY(-1px);
	}

	.stream-option:active {
		transform: translateY(0);
	}

	.stream-main {
		display: flex;
		align-items: center;
		gap: 0.6rem;
	}

	.stream-res {
		font-size: 0.95rem;
		font-weight: 700;
		color: white;
		min-width: 3.5em;
		letter-spacing: -0.01em;
	}

	.stream-badges {
		display: flex;
		gap: 0.25rem;
		flex-wrap: wrap;
		align-items: center;
	}

	.stream-meta {
		display: flex;
		gap: 0.75rem;
		align-items: center;
		padding-left: 4.1em;
	}

	.stream-stat {
		display: flex;
		align-items: center;
		gap: 0.25rem;
		font-size: 0.7rem;
		color: rgba(255, 255, 255, 0.4);
	}

	.stream-stat-icon {
		font-size: 0.65rem;
	}

	.stream-source {
		margin-left: auto;
		font-size: 0.65rem;
		color: rgba(255, 255, 255, 0.2);
		letter-spacing: 0.02em;
	}
</style>
