<script lang="ts">
	import {
		type Download,
		type MediaType,
		type ResolutionEstimate,
	} from "$lib/schema";
	import { api } from "$lib/api";
	import { Button, PopoverMenu, type PopoverMenuEntry } from "glow";

	let {
		mediaType,
		tmdbId,
		season = 0,
		episode = 0,
	}: {
		mediaType: MediaType;
		tmdbId: number;
		season?: number;
		episode?: number;
	} = $props();

	let download = $state<Download | null>(null);
	let estimates = $state<ResolutionEstimate[]>([]);
	let dropdownOpen = $state(false);
	let loadingEstimates = $state(false);

	// Check download status on mount
	$effect(() => {
		api.downloads
			.list()
			.then((items) => {
				download =
					items.find(
						(d) =>
							d.meta?.media_item?.media_type === mediaType &&
							d.meta.media_item.tmdb_id === tmdbId &&
							d.meta.season === season &&
							d.meta.episode === episode,
					) ?? null;
			})
			.catch(() => {});
	});

	// Poll while downloading
	$effect(() => {
		if (download?.status !== "Downloading" && download?.status !== "Queued")
			return;
		const interval = setInterval(() => {
			api.downloads
				.list()
				.then((items) => {
					download =
						items.find(
							(d) =>
								d.meta?.media_item?.media_type === mediaType &&
								d.meta.media_item.tmdb_id === tmdbId &&
								d.meta.season === season &&
								d.meta.episode === episode,
						) ?? null;
				})
				.catch(() => {});
		}, 3000);
		return () => clearInterval(interval);
	});

	// Fetch estimates when dropdown opens
	$effect(() => {
		if (dropdownOpen && !download && estimates.length === 0) {
			loadingEstimates = true;
			api.downloads
				.estimate(mediaType, tmdbId)
				.then((items) => {
					estimates = items;
				})
				.catch(() => {
					estimates = [];
				})
				.finally(() => {
					loadingEstimates = false;
				});
		}
	});

	async function pickResolution(resolution: string) {
		dropdownOpen = false;
		const id = await api.downloads.enqueue({
			media_type: mediaType,
			tmdb_id: tmdbId,
			season,
			episode,
			resolution,
			info_hash: null,
			file_idx: null,
		});
		const downloads = await api.downloads.list();
		download = downloads.find((download) => download.id === id) ?? null;
	}

	function handleClick() {
		if (download) {
			api.downloads.remove(download.id);
			download = null;
		}
	}

	const progressPct = $derived(
		download && download.total_bytes && download.total_bytes > 0
			? Math.round((download.downloaded_bytes / download.total_bytes) * 100)
			: null,
	);

	const icon = $derived(
		download?.status === "Completed"
			? "HardDriveDownload"
			: download?.status === "Downloading" || download?.status === "Queued"
				? "LoaderCircle"
				: "Download",
	);

	const menuItems = $derived<PopoverMenuEntry[]>(
		loadingEstimates
			? [
					{
						kind: "item",
						label: "Loading...",
						disabled: true,
						onclick: () => {},
					},
				]
			: estimates.length === 0
				? [
						{
							kind: "item",
							label: "No streams found",
							disabled: true,
							onclick: () => {},
						},
					]
				: estimates.map((est) => ({
						kind: "item" as const,
						label: est.resolution,
						shortcut: est.size_display ?? undefined,
						onclick: () => pickResolution(est.resolution),
					})),
	);
</script>

{#if download}
	<Button
		variant="ghost"
		{icon}
		label={download.status === "Downloading" && progressPct != null
			? `${progressPct}%`
			: undefined}
		loading={download.status === "Downloading" || download.status === "Queued"}
		onclick={handleClick}
	/>
{:else}
	<PopoverMenu items={menuItems} align="right" bind:open={dropdownOpen}>
		{#snippet trigger()}
			<Button variant="ghost" icon="Download" />
		{/snippet}
	</PopoverMenu>
{/if}
