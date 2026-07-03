<script lang="ts">
	import { type CacheEntry, type DiskStats } from "$lib/schema";
	import { api } from "$lib/api";
	import Spinner from "$lib/components/Spinner.svelte";
	import { imageUrl, formatBytes } from "$lib/utils";
	import { Heading, Button, Text, Pill, Modal } from "glow";

	type Category = "all" | "movies" | "tv" | "hls" | "orphan";

	let items = $state<CacheEntry[]>([]);
	let disk = $state<DiskStats | null>(null);
	let loading = $state(true);
	let filter = $state<Category>("all");

	let confirmTarget = $state<CacheEntry | null>(null);
	let confirmOpen = $state(false);

	let clearOpen = $state(false);

	function refresh() {
		api.cache
			.items()
			.then((r) => {
				items = r;
			})
			.catch(() => {});
		api.cache
			.disk()
			.then((r) => {
				disk = r;
			})
			.catch(() => {});
	}

	$effect(() => {
		loading = true;
		Promise.allSettled([
			api.cache.items().then((r) => (items = r)),
			api.cache.disk().then((r) => (disk = r)),
		]).finally(() => {
			loading = false;
		});
	});

	let filteredItems = $derived(
		[...items]
			.filter((it) => {
				switch (filter) {
					case "movies": {
						return it.download?.meta?.media_item?.media_type === "movie";
					}
					case "tv": {
						return it.download?.meta?.media_item?.media_type === "tv";
					}
					case "hls": {
						return it.kind === "hls";
					}
					case "orphan": {
						return it.kind === "orphan";
					}
					default: {
						return true;
					}
				}
			})
			.sort((a, b) => b.disk_bytes - a.disk_bytes),
	);

	let cinemaUsed = $derived(disk?.cinema_bytes ?? 0);
	let otherUsed = $derived(
		disk ? Math.max(0, disk.used_bytes - disk.cinema_bytes) : 0,
	);
	let freeSpace = $derived(disk?.free_bytes ?? 0);
	let totalSpace = $derived(disk?.total_bytes ?? 0);

	// Donut geometry — circumference of a circle r=50.
	const R = 50;
	const C = 2 * Math.PI * R;

	let donutSlices = $derived.by(() => {
		if (!disk || totalSpace === 0) return [];
		const slices: { color: string; bytes: number; label: string }[] = [
			{
				color: "var(--cache-cinema, #6ea8fe)",
				bytes: cinemaUsed,
				label: "Cinema",
			},
			{
				color: "var(--cache-other, #6c757d)",
				bytes: otherUsed,
				label: "Other",
			},
			{
				color: "var(--cache-free, #2c3138)",
				bytes: freeSpace,
				label: "Free",
			},
		];
		let offset = 0;
		return slices.map((s) => {
			const fraction = s.bytes / totalSpace;
			const dash = fraction * C;
			const out = {
				...s,
				dasharray: `${dash} ${C - dash}`,
				dashoffset: -offset,
			};
			offset += dash;
			return out;
		});
	});

	let breakdown = $derived.by(() => {
		if (!disk) return [];
		const total = Math.max(disk.cinema_bytes, 1);
		const segs: {
			key: Category;
			label: string;
			bytes: number;
			color: string;
		}[] = [
			{
				key: "movies",
				label: "Movies",
				bytes: disk.movies_bytes,
				color: "var(--cache-movies, #6ea8fe)",
			},
			{
				key: "tv",
				label: "TV",
				bytes: disk.tv_bytes,
				color: "var(--cache-tv, #20c997)",
			},
			{
				key: "hls",
				label: "HLS",
				bytes: disk.hls_bytes,
				color: "var(--cache-hls, #ffc107)",
			},
			{
				key: "orphan",
				label: "Orphan",
				bytes: disk.orphan_bytes,
				color: "var(--cache-orphan, #fd7e14)",
			},
		];
		return segs.map((s) => ({ ...s, percent: (s.bytes / total) * 100 }));
	});

	function openConfirm(item: CacheEntry) {
		confirmTarget = item;
		confirmOpen = true;
	}

	async function performDelete() {
		const target = confirmTarget;
		confirmOpen = false;
		confirmTarget = null;
		if (!target) return;
		try {
			if (target.kind === "orphan") {
				await api.cache.orphan(target.info_hash);
			} else if (target.download != null) {
				await api.downloads.remove(target.download.id);
			}
		} catch {
			/* ignored — UI just refetches */
		}
		refresh();
	}

	async function performClear() {
		clearOpen = false;
		try {
			await api.cache.clearAppCache();
		} catch {
			/* ignored */
		}
		refresh();
	}

	function rowTitle(it: CacheEntry): string {
		if (it.kind === "orphan") {
			return `Orphaned (${it.info_hash.slice(0, 12)}…)`;
		}
		const downloadMeta = it.download?.meta;
		let t = downloadMeta?.media_item?.title ?? "Untitled";
		if (
			downloadMeta?.media_item?.media_type === "tv" &&
			downloadMeta.season &&
			downloadMeta.episode
		) {
			const s = String(downloadMeta.season).padStart(2, "0");
			const e = String(downloadMeta.episode).padStart(2, "0");
			t = `${t} · S${s}E${e}`;
		}
		return t;
	}
</script>

<svelte:head>
	<title>Cache · Cinema</title>
</svelte:head>

<div class="page">
	{#if loading}
		<div class="loading-screen">
			<Spinner />
		</div>
	{:else}
		<header class="page-header">
			<Heading level={2}>Cache</Heading>
			<Button
				label="Clear app cache"
				icon="Eraser"
				variant="ghost"
				onclick={() => {
					clearOpen = true;
				}}
			/>
		</header>

		{#if disk}
			<section class="disk-panel">
				<div class="donut-wrap">
					<svg viewBox="-60 -60 120 120" class="donut">
						<circle
							cx="0"
							cy="0"
							r={R}
							fill="none"
							stroke="rgba(255,255,255,0.04)"
							stroke-width="14"
						/>
						{#each donutSlices as slice}
							<circle
								cx="0"
								cy="0"
								r={R}
								fill="none"
								stroke={slice.color}
								stroke-width="14"
								stroke-dasharray={slice.dasharray}
								stroke-dashoffset={slice.dashoffset}
								transform="rotate(-90)"
							/>
						{/each}
						<text x="0" y="-4" text-anchor="middle" class="donut-center-top">
							{formatBytes(freeSpace)}
						</text>
						<text x="0" y="14" text-anchor="middle" class="donut-center-sub">
							free of {formatBytes(totalSpace)}
						</text>
					</svg>
				</div>

				<div class="legend">
					{#each donutSlices as slice}
						<div class="legend-row">
							<span class="swatch" style="background:{slice.color}"></span>
							<Text size="sm">{slice.label}</Text>
							<span class="legend-spacer"></span>
							<Text size="sm">{formatBytes(slice.bytes)}</Text>
						</div>
					{/each}
				</div>
			</section>

			<section class="breakdown">
				<div class="breakdown-header">
					<Text size="sm">
						Cinema breakdown · {formatBytes(disk.cinema_bytes)}
					</Text>
					<Button
						disabled={filter === "all"}
						onclick={() => {
							filter = "all";
						}}
					>
						Show all
					</Button>
				</div>
				<div class="stacked-bar">
					{#each breakdown as seg}
						{#if seg.bytes > 0}
							<button
								type="button"
								class="seg"
								class:active={filter === seg.key}
								style="flex:{seg.bytes}; background:{seg.color}"
								onclick={() => (filter = filter === seg.key ? "all" : seg.key)}
								title="{seg.label} — {formatBytes(seg.bytes)}"
								aria-label="{seg.label} {formatBytes(seg.bytes)}"
							></button>
						{/if}
					{/each}
				</div>
				<div class="breakdown-legend">
					{#each breakdown as seg}
						<Button
							variant="outlined"
							onclick={() => {
								filter = filter === seg.key ? "all" : seg.key;
							}}
							selected={filter === seg.key}
						>
							<span class="swatch" style="background:{seg.color}"></span>
							<Text size="sm">{seg.label}</Text>
							<Text size="sm">{formatBytes(seg.bytes)}</Text>
						</Button>
					{/each}
				</div>
			</section>
		{/if}

		<section class="items">
			<Heading level={3}>
				{filter === "all"
					? "All items"
					: filter[0].toUpperCase() + filter.slice(1)}
				{#if !loading}
					<span class="count"
						>· {filteredItems.length} item{filteredItems.length === 1
							? ""
							: "s"}</span
					>
				{/if}
			</Heading>

			{#if filteredItems.length === 0}
				<Text size="sm">Nothing here.</Text>
			{:else}
				<ul class="list">
					{#each filteredItems as it (it.kind + "-" + (it.download?.id ?? it.info_hash))}
						<li class="row">
							<div class="thumb">
								{#if it.download?.meta?.media_item?.poster_path}
									<img
										src={imageUrl(
											it.download.meta.media_item.poster_path,
											"w200",
										)}
										alt=""
									/>
								{:else}
									<div class="thumb-placeholder"></div>
								{/if}
							</div>
							<div class="meta">
								<Text size="sm">{rowTitle(it)}</Text>
								<div class="pills">
									{#if it.download?.meta?.resolution}
										<Pill label={it.download.meta.resolution} />
									{/if}
									{#if it.download?.status && it.download.status !== "Completed"}
										<Pill label={it.download.status} />
									{/if}
									{#if it.kind === "orphan"}
										<Pill label="orphan" />
									{/if}
									{#if it.kind === "download" && it.download?.status !== "Completed" && it.download?.total_bytes && it.download.downloaded_bytes != null && it.download.total_bytes > 0}
										<Pill
											label={`${Math.round((it.download.downloaded_bytes / it.download.total_bytes) * 100)}%`}
										/>
									{/if}
								</div>
							</div>
							<div class="size">
								<Text size="sm">{formatBytes(it.disk_bytes)}</Text>
							</div>
							<Button
								icon="Trash"
								variant="ghost"
								onclick={() => openConfirm(it)}
							/>
						</li>
					{/each}
				</ul>
			{/if}
		</section>
	{/if}
</div>

<Modal
	bind:open={confirmOpen}
	title="Delete cached item?"
	size="small"
	actions={[
		{
			label: "Cancel",
			variant: "ghost",
			onclick: () => {
				confirmOpen = false;
			},
		},
		{ label: "Delete", variant: "primary", onclick: performDelete },
	]}
>
	{#if confirmTarget}
		<Text size="sm">
			Delete <strong>{rowTitle(confirmTarget)}</strong>?
		</Text>
		<Text size="sm">
			This will free {formatBytes(confirmTarget.disk_bytes)} and is not reversible.
		</Text>
	{/if}
</Modal>

<Modal
	bind:open={clearOpen}
	title="Clear app cache?"
	size="small"
	actions={[
		{
			label: "Cancel",
			variant: "ghost",
			onclick: () => {
				clearOpen = false;
			},
		},
		{ label: "Clear", variant: "primary", onclick: performClear },
	]}
>
	<Text size="sm">
		This stops every active transcoding session and removes transient cache
		directories. Downloads are not touched.
	</Text>
	{#if disk}
		<Text size="sm"
			>Will free up to {formatBytes(disk.hls_bytes)} from HLS sessions.</Text
		>
	{/if}
</Modal>

<style lang="scss">
	@use "glow/styles/theme" as *;

	.page {
		padding: 1.5rem;
		display: flex;
		flex-direction: column;
		gap: 1.5rem;
		max-width: 1100px;
		margin: 0 auto;
	}

	.loading-screen {
		position: fixed;
		inset: 0;
		display: flex;
		align-items: center;
		justify-content: center;
	}

	.page-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 1rem;
	}

	.disk-panel {
		display: flex;
		gap: 2rem;
		align-items: center;
		flex-wrap: wrap;
		background: rgba(255, 255, 255, 0.04);
		border: $border;
		border-radius: 0.75rem;
		padding: 1.25rem;
	}

	.donut-wrap {
		flex: 0 0 auto;
	}

	.donut {
		width: 200px;
		height: 200px;
		display: block;
	}

	.donut-center-top {
		font-size: 14px;
		font-weight: 600;
		fill: currentColor;
	}

	.donut-center-sub {
		font-size: 8px;
		fill: currentColor;
		opacity: 0.6;
	}

	.legend {
		flex: 1 1 220px;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		min-width: 200px;
	}

	.legend-row {
		display: flex;
		align-items: center;
		gap: 0.5rem;
	}

	.legend-spacer {
		flex: 1;
	}

	.swatch {
		display: inline-block;
		width: 0.75rem;
		height: 0.75rem;
		border-radius: 0.2rem;
	}

	.breakdown {
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
	}

	.breakdown-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 1rem;
	}

	.stacked-bar {
		display: flex;
		height: 1.25rem;
		border-radius: 0.4rem;
		overflow: hidden;
		background: rgba(255, 255, 255, 0.04);
	}

	.seg {
		border: none;
		padding: 0;
		cursor: pointer;
		transition: filter 120ms ease;
		min-width: 4px;

		&:hover {
			filter: brightness(1.15);
		}

		&.active {
			outline: 2px solid rgba(255, 255, 255, 0.7);
			outline-offset: -2px;
		}
	}

	.breakdown-legend {
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
	}

	.items {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	.count {
		opacity: 0.6;
		font-weight: 400;
	}

	.list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.row {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		padding: 0.5rem 0.75rem;
		background: rgba(255, 255, 255, 0.04);
		border: $border;
		border-radius: 0.5rem;
	}

	.thumb {
		width: 2.5rem;
		height: 3.75rem;
		flex: 0 0 auto;
		border-radius: 0.25rem;
		overflow: hidden;
		background: rgba(255, 255, 255, 0.06);

		img {
			width: 100%;
			height: 100%;
			object-fit: cover;
		}
	}

	.thumb-placeholder {
		width: 100%;
		height: 100%;
	}

	.meta {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.pills {
		display: flex;
		flex-wrap: wrap;
		gap: 0.25rem;
	}

	.size {
		flex: 0 0 auto;
		min-width: 5rem;
		text-align: right;
	}
</style>
