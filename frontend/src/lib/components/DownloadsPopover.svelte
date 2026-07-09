<script lang="ts">
	import type { Download, Pretranscoding } from "$lib/schema";
	import { api } from "$lib/api";
	import {
		DOWNLOAD_STATUS_LABEL,
		formatBytes,
		pretranscodePercent,
		progress,
	} from "$lib/utils";
	import { Button, Popover, Text, toast } from "glow";
	import { onDestroy, onMount } from "svelte";
	import { slide } from "svelte/transition";
	import TranscodePanel from "./TranscodePanel.svelte";

	let expandedId = $state<number | null>(null);

	function toggleTranscode(id: number) {
		expandedId = expandedId === id ? null : id;
	}

	let downloads = $state<
		Array<Download & { download_speed_mbps?: number | null }>
	>([]);

	// All pretranscodings, grouped by their parent download id. Updated
	// live from the pretranscodingsEvents socket subscription below.
	let pretranscodings = $state<Record<number, Pretranscoding[]>>({});

	const activeCount = $derived(
		downloads.filter((d) => d.status === "Queued" || d.status === "Downloading")
			.length,
	);

	let loading = $state(false);
	const recentlyRemovedDownloads = new Set<number>();
	const recentlyRemovedPretranscodings = new Set<number>();

	async function load() {
		if (loading) {
			return;
		}

		try {
			loading = true;
			recentlyRemovedDownloads.clear();
			recentlyRemovedPretranscodings.clear();

			const [ds, pts] = await Promise.all([
				api.downloads.list(),
				api.transcodings.list(),
			]);
			downloads = ds.filter((it) => !recentlyRemovedDownloads.has(it.id));

			const grouped: Record<number, Pretranscoding[]> = {};
			for (const pt of pts) {
				if (recentlyRemovedPretranscodings.has(pt.id)) {
					continue;
				}
				const parent = downloads.find((d) => d.id === pt.download_id);
				if (!parent) continue;
				(grouped[parent.id] ??= []).push(pt);
			}
			pretranscodings = grouped;
		} catch {
			// silently ignore
		} finally {
			loading = false;
			recentlyRemovedDownloads.clear();
			recentlyRemovedPretranscodings.clear();
		}
	}

	function activePretranscoding(id: number): Pretranscoding | undefined {
		return pretranscodings[id]?.find(
			(pt) => pt.status === "Transcoding" || pt.status === "Queued",
		);
	}

	let unsub: (() => void) | undefined;

	onMount(() => {
		load();

		const unsubOnDownloadProgress = api.downloadsEvents.onProgress((p) => {
			const idx = downloads.findIndex((d) => d.id === p.download_id);
			if (idx === -1) {
				load();
				return;
			}
			downloads[idx] = {
				...downloads[idx],
				downloaded_bytes: p.downloaded_bytes,
				total_bytes: p.total_bytes,
				download_speed_mbps: p.download_speed_mbps,
				status: p.status,
			};
		});
		const unsubOnDownloadStatusUpdate = api.downloadsEvents.onStatusUpdate(
			(statusUpdate) => {
				const idx = downloads.findIndex(
					(d) => d.id === statusUpdate.download_id,
				);
				if (idx === -1) {
					load();
					return;
				}
				downloads[idx] = {
					...downloads[idx],
					status: statusUpdate.new_status,
				};
			},
		);
		const unsubOnDownloadRemove = api.downloadsEvents.onRemoved((id) => {
			recentlyRemovedDownloads.add(id);

			downloads = downloads.filter((it) => it.id === id);
			delete pretranscodings[id];
		});

		const unsubOnPretranscodingProgress = api.transcodingsEvents.onProgress(
			(p) => {
				const list = pretranscodings[p.download_id];
				if (!list) {
					// New row: refetch the full list so its metadata lands in state.
					load();
					return;
				}
				const idx = list.findIndex((x) => x.id === p.pretranscoding_id);
				if (idx === -1) {
					load();
					return;
				}
				list[idx] = {
					...list[idx],
					transcoded_ms: p.transcoded_ms,
					total_ms: p.total_ms ?? list[idx].total_ms,
					status: p.status,
				};
			},
		);
		const unsubOnPretranscodingStatusUpdate =
			api.transcodingsEvents.onStatusUpdate((s) => {
				const list = pretranscodings[s.download_id];
				if (!list) {
					load();
					return;
				}
				const idx = list.findIndex((x) => x.id === s.pretranscoding_id);
				if (idx === -1) {
					load();
					return;
				}
				list[idx] = { ...list[idx], status: s.new_status };
			});
		const unsubOnPretranscodingRemove = api.transcodingsEvents.onRemoved(
			(p) => {
				recentlyRemovedPretranscodings.add(p.pretranscoding_id);

				const list = pretranscodings[p.download_id];
				if (!list) {
					// A pretranscoding has been removed, but we don't have a download entry for it, so nothing to do
					return;
				}

				pretranscodings[p.download_id] = list.filter(
					(it) => it.id !== p.pretranscoding_id,
				);
			},
		);

		unsub = () => {
			unsubOnDownloadProgress();
			unsubOnDownloadStatusUpdate();
			unsubOnDownloadRemove();

			unsubOnPretranscodingProgress();
			unsubOnPretranscodingStatusUpdate();
			unsubOnPretranscodingRemove();
		};
	});

	onDestroy(() => unsub?.());

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

	async function pause(d: Download) {
		try {
			await api.downloads.pause(d.id);
			const idx = downloads.findIndex((x) => x.id === d.id);
			if (idx !== -1) downloads[idx] = { ...downloads[idx], status: "Paused" };
		} catch (err: unknown) {
			toast.error(
				`Pause failed: ${err instanceof Error ? err.message : String(err)}`,
			);
		}
	}

	async function resume(d: Download) {
		try {
			await api.downloads.resume(d.id);
			const idx = downloads.findIndex((x) => x.id === d.id);
			if (idx !== -1) downloads[idx] = { ...downloads[idx], status: "Queued" };
		} catch (err: unknown) {
			toast.error(
				`Resume failed: ${err instanceof Error ? err.message : String(err)}`,
			);
		}
	}

	async function cancel(d: Download) {
		try {
			await api.downloads.cancel(d.id);
			const idx = downloads.findIndex((x) => x.id === d.id);
			if (idx !== -1)
				downloads[idx] = { ...downloads[idx], status: "Cancelled" };
		} catch (err: unknown) {
			toast.error(
				`Cancel failed: ${err instanceof Error ? err.message : String(err)}`,
			);
		}
	}

	async function remove(d: Download) {
		try {
			await api.downloads.remove(d.id);
			downloads = downloads.filter((x) => x.id !== d.id);
			delete pretranscodings[d.id];
		} catch (err: unknown) {
			toast.error(
				`Remove failed: ${err instanceof Error ? err.message : String(err)}`,
			);
		}
	}
</script>

<Popover align="right">
	{#snippet trigger()}
		<div class="trigger-wrap">
			<Button icon="Download" variant="ghost" />
			{#if activeCount > 0}
				<span class="badge">{activeCount}</span>
			{/if}
		</div>
	{/snippet}
	{#snippet children()}
		<div class="popover-panel">
			<div class="panel-header">
				<Text size="lg" weight="semibold">Downloads</Text>
			</div>
			{#if downloads.length === 0}
				<div class="empty">
					<Text size="sm" variant="muted">No downloads yet</Text>
				</div>
			{:else}
				<div class="download-list">
					{#each downloads as d (d.id)}
						{@const pct = progress(d)}
						{@const ep = episodeLabel(d)}
						{@const activePt = activePretranscoding(d.id)}
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
													: ptPct != null
														? `Pretranscoding · ${ptPct}%`
														: "Pretranscoding · waiting for pieces"}
											</Text>
										</div>
									{/if}
								</div>
								<div class="row-actions">
									{#if d.status === "Downloading" || d.status === "Queued"}
										<Button
											icon="Pause"
											variant="ghost"
											onclick={() => pause(d)}
										/>
									{/if}
									{#if d.status === "Paused" || d.status === "Failed" || d.status === "Cancelled"}
										<Button
											icon="Play"
											variant="ghost"
											onclick={() => resume(d)}
										/>
									{/if}
									{#if d.status !== "Completed" && d.status !== "Cancelled"}
										<Button
											icon="X"
											variant="ghost"
											onclick={() => cancel(d)}
										/>
									{/if}
									{#if d.status === "Completed" || d.status === "Cancelled"}
										<Button
											icon="Trash"
											variant="ghost"
											onclick={() => remove(d)}
										/>
									{/if}
									<Button
										icon="Cpu"
										variant="ghost"
										selected={expandedId === d.id}
										onclick={() => toggleTranscode(d.id)}
									/>
								</div>
							</div>
							{#if expandedId === d.id}
								<div
									class="transcode-slot"
									transition:slide|local={{ duration: 200 }}
								>
									<TranscodePanel
										download={d}
										pretranscodings={pretranscodings[d.id] ?? []}
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
		gap: 0.1rem;
		flex-shrink: 0;
	}
</style>
