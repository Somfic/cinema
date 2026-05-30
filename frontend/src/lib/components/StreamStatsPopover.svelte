<script lang="ts">
	import { formatBytes } from "$lib/utils";
	import { Button, Data, Popover } from "glow";

	interface StreamStats {
		progress_bytes: number;
		total_bytes: number;
		download_speed_mbps: number;
		peers: number;
		finished: boolean;
	}

	let { streamStats }: { streamStats?: StreamStats | null } = $props();

	let open = $state(false);

	const torrentPercent = $derived(
		streamStats && streamStats.total_bytes > 0
			? Math.round((streamStats.progress_bytes / streamStats.total_bytes) * 100)
			: 0,
	);
</script>

{#if streamStats}
	<Popover align="right" bind:open>
		{#snippet trigger()}
			<Button variant="ghost" icon="Info" />
		{/snippet}
		{#snippet children()}
			<div class="stats-popover">
				<Data
					variant="inline"
					properties={[
						{ label: "Progress", value: `${torrentPercent}%` },
						{
							label: "Downloaded",
							value: `${formatBytes(streamStats.progress_bytes)} / ${formatBytes(streamStats.total_bytes)}`,
						},
						{
							label: "Speed",
							value: `${streamStats.download_speed_mbps.toFixed(1)} MB/s`,
						},
						{ label: "Peers", value: streamStats.peers },
						{
							label: "Status",
							value: streamStats.finished ? "Complete" : "Downloading",
						},
					]}
				/>
			</div>
		{/snippet}
	</Popover>
{/if}

<style>
	.stats-popover {
		min-width: 18rem;
		white-space: nowrap;
	}
</style>
