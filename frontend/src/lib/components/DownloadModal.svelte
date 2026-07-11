<script lang="ts">
	import type { MediaType, Stream } from "$lib/schema";
	import { api } from "$lib/api";
	import { Button, Icon, Modal, Spinner, Text, toast, tooltip } from "glow";

	let {
		open = $bindable(false),
		tmdbId,
		mediaType,
		season = null,
		episode = null,
	}: {
		open: boolean;
		tmdbId: number;
		mediaType: MediaType;
		season?: number | null;
		episode?: number | null;
	} = $props();

	let streams = $state<Stream[]>([]);
	let loading = $state(false);
	let enqueueing = $state<string | null>(null); // info_hash+file_idx of in-flight enqueue

	const RESOLUTION_ORDER: Record<string, number> = {
		"4K": 4,
		"2160p": 4,
		"1080p": 3,
		"720p": 2,
		"480p": 1,
	};

	const resolutions = $derived.by(() => {
		const seen = new Set<string>();
		const result: string[] = [];
		for (const s of streams) {
			const res = s.resolution ?? "Unknown";
			if (!seen.has(res)) {
				seen.add(res);
				result.push(res);
			}
		}
		return result.sort(
			(a, b) => (RESOLUTION_ORDER[b] ?? 0) - (RESOLUTION_ORDER[a] ?? 0),
		);
	});

	function streamsForResolution(res: string) {
		return streams.filter((s) => (s.resolution ?? "Unknown") === res);
	}

	$effect(() => {
		if (!open) {
			streams = [];
			enqueueing = null;
			loading = false;
			return;
		}
		loading = true;
		let ignore = false;
		const fetch =
			mediaType === "tv" && season != null && episode != null
				? api.streams.tv(tmdbId, season, episode)
				: api.streams.movie(tmdbId);
		fetch
			.then((result) => {
				if (!ignore) {
					streams = result;
				}
			})
			.catch((err) => {
				toast.error(`Failed to load streams: ${err.message}`);
				open = false;
			})
			.finally(() => {
				loading = false;
			});

		return () => {
			ignore = true;
		};
	});

	const RETRIABLE_STATUSES = new Set(["Failed", "Cancelled"]);

	function canEnqueue(stream: Stream): boolean {
		return (
			stream.download == null || RETRIABLE_STATUSES.has(stream.download.status)
		);
	}

	async function enqueue(stream: Stream) {
		const key = `${stream.info_hash}:${stream.file_idx}`;
		if (enqueueing === key) return;
		enqueueing = key;
		try {
			await api.downloads.enqueue({
				info_hash: stream.info_hash,
				file_idx: stream.file_idx,
			});
			toast.success("Download enqueued");
			open = false;
		} catch (err: unknown) {
			const msg = err instanceof Error ? err.message : String(err);
			toast.error(`Download failed: ${msg}`);
		} finally {
			enqueueing = null;
		}
	}
</script>

<Modal bind:open title="Download" size="large">
	<div class="download-modal">
		{#if loading}
			<div class="loading">
				<Spinner />
				<Text variant="muted" size="sm">Finding sources…</Text>
			</div>
		{:else if streams.length === 0}
			<div class="empty">
				<Text variant="muted" size="sm">No sources found</Text>
			</div>
		{:else}
			<div class="groups">
				{#each resolutions as res}
					<div class="resolution-group">
						<div class="res-header">
							<Text size="xs" weight="semibold" variant="secondary">{res}</Text>
						</div>
						<div class="stream-list">
							{#each streamsForResolution(res) as stream}
								{@const key = `${stream.info_hash}:${stream.file_idx}`}
								<div class="stream-row">
									<div class="stream-main">
										<Text size="sm" weight="semibold">{stream.source}</Text>
										<div class="chips">
											{#if stream.codec}
												<span class="chip">{stream.codec}</span>
											{/if}
											{#if stream.audio}
												<span class="chip">{stream.audio}</span>
											{/if}
											{#if stream.source_type}
												<span class="chip">{stream.source_type}</span>
											{/if}
											{#if stream.hdr}
												<span class="chip chip--hdr">HDR</span>
											{/if}
											{#if stream.imax}
												<span class="chip chip--imax">IMAX</span>
											{/if}
										</div>
									</div>
									<div style="display: flex; align-items: center; gap: 1rem">
										<div class="stream-meta">
											{#if stream.size_display}
												<Text size="xs" variant="muted">
													{stream.size_display}
												</Text>
											{/if}
											{#if stream.seeders != null}
												<Text size="xs" variant="muted">
													{stream.seeders} seeds
												</Text>
											{/if}
										</div>
										{#if canEnqueue(stream)}
											<Button
												variant="ghost"
												icon="Download"
												loading={enqueueing === key}
												disabled={enqueueing !== null}
												onclick={() => enqueue(stream)}
											></Button>
										{:else if stream.download?.status === "Queued"}
											<span
												style="padding: 0.5rem;"
												use:tooltip={{ content: "Queued", position: "bottom" }}
											>
												<Icon name="Hourglass" color="rgb(251, 191, 36)" />
											</span>
										{:else if stream.download?.status === "Downloading"}
											<span
												style="padding: 0.5rem;"
												use:tooltip={{
													content: "Downloading",
													position: "bottom",
												}}
											>
												<Spinner />
											</span>
										{:else if stream.download?.status === "Paused"}
											<span
												style="padding: 0.5rem;"
												use:tooltip={{ content: "Paused", position: "bottom" }}
											>
												<Icon name="Pause" color="rgb(251, 191, 36)" />
											</span>
										{:else if stream.download?.status === "Completed"}
											<span
												style="padding: 0.5rem;"
												use:tooltip={{
													content: "Completed",
													position: "bottom",
												}}
											>
												<Icon name="Check" color="rgb(74, 222, 128)" />
											</span>
										{/if}
									</div>
								</div>
							{/each}
						</div>
					</div>
				{/each}
			</div>
		{/if}
	</div>
</Modal>

<style lang="scss">
	@use "glow/styles/theme" as *;

	.download-modal {
		min-height: 8rem;
	}

	.loading,
	.empty {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 0.75rem;
		padding: 2rem;
	}

	.groups {
		display: flex;
		flex-direction: column;
		gap: 1.25rem;
	}

	.resolution-group {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.res-header {
		padding: 0 0.25rem 0.25rem;
		border-bottom: $border;
	}

	.stream-list {
		display: flex;
		flex-direction: column;
	}

	.stream-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 1rem;
		padding: 0.6rem 0.5rem;
		border-radius: 8px;
		background: transparent;
		border: none;
		color: inherit;
		text-align: left;
		transition: background 0.15s;

		&:hover:not(:disabled) {
			background: rgba(255, 255, 255, 0.05);
		}

		&:disabled {
			opacity: 0.6;
			cursor: default;
		}
	}

	.stream-main {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		min-width: 0;
	}

	.chips {
		display: flex;
		flex-wrap: wrap;
		gap: 0.3rem;
	}

	.chip {
		font-size: 0.7rem;
		padding: 0.1rem 0.4rem;
		border-radius: 4px;
		background: rgba(255, 255, 255, 0.08);
		white-space: nowrap;
	}

	.chip--hdr {
		background: rgba(251, 191, 36, 0.2);
		color: rgb(251, 191, 36);
	}

	.chip--imax {
		background: rgba(59, 130, 246, 0.2);
		color: rgb(59, 130, 246);
	}

	.stream-meta {
		display: flex;
		flex-direction: column;
		align-items: flex-end;
		gap: 0.2rem;
		flex-shrink: 0;
	}
</style>
