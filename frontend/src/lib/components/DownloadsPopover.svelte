<script lang="ts">
	import type { Download } from "$lib/schema";
	import { downloadManager } from "$lib/downloads.svelte";
	import {
		DOWNLOAD_STATUS_LABEL,
		formatBytes,
		pretranscodePercent,
		progress,
	} from "$lib/utils";
	import { Button, Popover, Text } from "glow";
	import { slide } from "svelte/transition";
	import TranscodePanel from "./TranscodePanel.svelte";

	let expandedId = $state<number | null>(null);

	const badgeCount = $derived(
		downloadManager.activeDownloadsCount +
			downloadManager.activePretranscodingCount +
			downloadManager.liveHlsCount,
	);

	function toggleTranscode(id: number) {
		expandedId = expandedId === id ? null : id;
	}

	function displayTitle(d: Download): string {
		const title = d.meta?.media_item?.title;
		if (title) return title;
		if (d.name) return d.name;
		return d.info_hash.slice(0, 10) + "…";
	}

	function episodeLabel(d: Download): string | null {
		if (d.meta?.season == null) return null;
		return `S${d.meta.season}E${d.meta.episode}`;
	}
</script>

<Popover align="right">
	{#snippet trigger()}
		<div class="trigger-wrap">
			<Button icon="Download" variant="ghost" />
			{#if badgeCount > 0}
				<span class="badge">{badgeCount}</span>
			{/if}
		</div>
	{/snippet}
	{#snippet children()}
		<div class="popover-panel">
			<div class="panel-header">
				<Text size="lg" weight="semibold">Downloads</Text>
				{#if downloadManager.liveHlsCount > 0}
					<Button
						label={`Kill ${downloadManager.liveHlsCount} HLS`}
						variant="danger"
						onclick={() => downloadManager.killAllLive()}
						class="kill-hls"
					/>
				{/if}
			</div>
			{#if downloadManager.downloads.length === 0}
				<div class="empty">
					<Text size="sm" variant="muted">No downloads yet</Text>
				</div>
			{:else}
				<div class="download-list">
					{#each downloadManager.downloads as d (d.id)}
						{@const pct = progress(d)}
						{@const ep = episodeLabel(d)}
						{@const activePt = downloadManager.activePretranscoding(d.id)}
						{@const ptPct = activePt ? pretranscodePercent(activePt) : null}
						<div class="download-row">
							<div class="row-main">
								<div class="row-info">
									<div class="row-title-line">
										<Text size="sm" weight="semibold">{displayTitle(d)}</Text>
										<div class="row-chips">
											{#if ep}
												<span class="chip">{ep}</span>
											{/if}
											{#if d.meta?.resolution}
												<span class="chip">{d.meta.resolution}</span>
											{/if}
											<span
												class="chip chip--status"
												class:chip--downloading={d.status === "Downloading"}
												class:chip--queued={d.status === "Queued"}
												class:chip--paused={d.status === "Paused"}
												class:chip--done={d.status === "Completed"}
												class:chip--failed={d.status === "Failed"}
											>
												{DOWNLOAD_STATUS_LABEL[d.status] ?? d.status}
											</span>
										</div>
									</div>
									{#if d.total_bytes && d.status !== "Queued"}
										<div class="progress-bar-wrap">
											<div
												class="progress-bar"
												class:progress-bar--done={d.status === "Completed"}
												style="width: {pct}%"
											></div>
										</div>
										<div
											style="display: flex; align-items: center; justify-content: space-between;"
										>
											<Text size="xs" variant="muted">
												{formatBytes(d.downloaded_bytes)} / {formatBytes(
													d.total_bytes,
												)}
											</Text>
											<Text size="xs" variant="muted">
												{#if d.status === "Downloading" && d.download_speed_mbps}
													{d.download_speed_mbps.toFixed(1)} MB/s
												{/if}
											</Text>
											<Text size="xs" variant="muted">
												{pct}%
											</Text>
										</div>
									{/if}
									{#if activePt}
										<div class="pretranscode-line">
											<div class="progress-bar-wrap progress-bar-wrap--pt">
												<div
													class="progress-bar progress-bar--pt"
													style={ptPct != null
														? `width: ${ptPct}%`
														: "width: 100%; opacity: 0.4"}
												></div>
											</div>
											<Text size="xs" variant="muted">
												{activePt.status === "Queued"
													? "Pretranscode queued"
													: activePt.status === "Paused"
														? `Pretranscode paused${ptPct != null ? ` · ${ptPct}%` : ""}`
														: ptPct != null
															? `Pretranscoding · ${ptPct}%`
															: "Pretranscoding · waiting for pieces"}
											</Text>
										</div>
									{/if}
								</div>
								<div class="row-actions">
									<Button
										icon="Cpu"
										variant="ghost"
										selected={expandedId === d.id}
										onclick={() => toggleTranscode(d.id)}
									/>
									{#if d.status === "Downloading" || d.status === "Queued"}
										<Button
											icon="Pause"
											variant="ghost"
											onclick={() => downloadManager.pause(d.id)}
										/>
									{/if}
									{#if d.status === "Paused" || d.status === "Failed" || d.status === "Cancelled"}
										<Button
											icon="Play"
											variant="ghost"
											onclick={() => downloadManager.resume(d.id)}
										/>
									{/if}
									{#if d.status !== "Completed" && d.status !== "Cancelled"}
										<Button
											icon="X"
											variant="ghost"
											onclick={() => downloadManager.cancel(d.id)}
										/>
									{/if}
									{#if d.status === "Completed" || d.status === "Cancelled"}
										<Button
											icon="Trash"
											variant="danger"
											onclick={() => downloadManager.remove(d.id)}
										/>
									{/if}
								</div>
							</div>
							{#if expandedId === d.id}
								<div
									class="transcode-slot"
									transition:slide|local={{ duration: 200 }}
								>
									<TranscodePanel
										download={d}
										pretranscodings={downloadManager.pretranscodings[d.id] ??
											[]}
										active
									/>
								</div>
							{/if}
						</div>
					{/each}
				</div>
			{/if}
		</div>
	{/snippet}
</Popover>

<style lang="scss">
	@use "glow/styles/theme" as *;

	.trigger-wrap {
		position: relative;
		display: inline-flex;
	}

	.badge {
		position: absolute;
		top: 2px;
		right: 2px;
		min-width: 1rem;
		height: 1rem;
		border-radius: 0.5rem;
		background: $primary;
		color: white;
		font-size: 0.65rem;
		font-weight: 700;
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 0 0.2rem;
		pointer-events: none;
	}

	.popover-panel {
		width: 22rem;
		max-height: 70vh;
		display: flex;
		flex-direction: column;
	}

	.panel-header {
		padding: 0.5rem 0.75rem;
		border-bottom: $border;
		flex-shrink: 0;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;

		:global(.kill-hls) {
			font-size: 0.75rem;
		}
	}

	.empty {
		padding: 1.5rem;
		display: flex;
		align-items: center;
		justify-content: center;
	}

	.download-list {
		overflow-y: auto;
		flex: 1;
	}

	.download-row {
		display: flex;
		flex-direction: column;
		padding: 0.6rem 0.75rem;
		border-bottom: $border;

		&:last-child {
			border-bottom: none;
		}
	}

	.row-main {
		display: flex;
		align-items: center;
		gap: 0.5rem;
	}

	.transcode-slot {
		margin-top: 0.5rem;
	}

	.row-info {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
	}

	.row-title-line {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		flex-wrap: wrap;
	}

	.row-chips {
		display: flex;
		gap: 0.25rem;
		flex-wrap: wrap;
	}

	.chip {
		font-size: 0.65rem;
		padding: 0.1rem 0.35rem;
		border-radius: 4px;
		background: rgba(255, 255, 255, 0.08);
		white-space: nowrap;
	}

	.chip--status {
		background: rgba(255, 255, 255, 0.06);
	}

	.chip--downloading {
		background: rgba(59, 130, 246, 0.2);
		color: rgb(100, 170, 255);
	}

	.chip--queued {
		background: rgba(255, 255, 255, 0.1);
	}

	.chip--paused {
		background: rgba(251, 191, 36, 0.2);
		color: rgb(251, 191, 36);
	}

	.chip--done {
		background: rgba(34, 197, 94, 0.2);
		color: rgb(34, 197, 94);
	}

	.chip--failed {
		background: rgba(239, 68, 68, 0.2);
		color: rgb(239, 68, 68);
	}

	.progress-bar-wrap {
		height: 3px;
		border-radius: 2px;
		background: rgba(255, 255, 255, 0.1);
		overflow: hidden;
	}

	.progress-bar {
		height: 100%;
		background: $primary;
		border-radius: 2px;
		transition: width 0.5s;
	}

	.progress-bar--done {
		background: rgb(34, 197, 94);
	}

	.pretranscode-line {
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
		margin-top: 0.15rem;
	}

	.progress-bar-wrap--pt {
		height: 2px;
	}

	.progress-bar--pt {
		background: rgb(139, 92, 246);
	}

	.row-actions {
		display: flex;
		gap: 0.3rem;
		flex-shrink: 0;
	}
</style>
