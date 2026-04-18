<script lang="ts">
	import {
		estimateDownload,
		enqueueDownload,
		listDownloads,
		deleteDownload,
		type Download,
		type ResolutionEstimate,
	} from "$lib/api.gen";
	import { Button, PopoverMenu, type PopoverMenuEntry } from "glow";

	let {
		mediaType,
		tmdbId,
		title,
		posterPath,
		season = 0,
		episode = 0,
	}: {
		mediaType: string;
		tmdbId: number;
		title: string;
		posterPath?: string;
		season?: number;
		episode?: number;
	} = $props();

	let download = $state<Download | null>(null);
	let estimates = $state<ResolutionEstimate[]>([]);
	let dropdownOpen = $state(false);
	let loadingEstimates = $state(false);

	// Check download status on mount
	$effect(() => {
		listDownloads()
			.then((res) => {
				download =
					res.data.find(
						(d) =>
							d.media_type === mediaType &&
							d.tmdb_id === tmdbId &&
							d.season === season &&
							d.episode === episode,
					) ?? null;
			})
			.catch(() => {});
	});

	// Poll while downloading
	$effect(() => {
		if (download?.status !== "downloading" && download?.status !== "queued")
			return;
		const interval = setInterval(() => {
			listDownloads()
				.then((res) => {
					download =
						res.data.find(
							(d) =>
								d.media_type === mediaType &&
								d.tmdb_id === tmdbId &&
								d.season === season &&
								d.episode === episode,
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
			estimateDownload(mediaType, tmdbId)
				.then((res) => { estimates = res.data; })
				.catch(() => { estimates = []; })
				.finally(() => { loadingEstimates = false; });
		}
	});

	async function pickResolution(resolution: string) {
		dropdownOpen = false;
		await enqueueDownload({
			media_type: mediaType,
			tmdb_id: tmdbId,
			title,
			poster_path: posterPath,
			season,
			episode,
			resolution,
		});
		download = {
			status: "queued",
			resolution,
			downloaded_bytes: 0,
		} as Download;
	}

	function handleClick() {
		if (download) {
			deleteDownload(download.id);
			download = null;
		}
	}

	const progressPct = $derived(
		download && download.total_bytes && download.total_bytes > 0
			? Math.round((download.downloaded_bytes / download.total_bytes) * 100)
			: null,
	);

	const icon = $derived(
		download?.status === "completed"
			? "HardDriveDownload"
			: download?.status === "downloading" || download?.status === "queued"
				? "LoaderCircle"
				: "Download",
	);

	const menuItems = $derived<PopoverMenuEntry[]>(
		loadingEstimates
			? [{ kind: "item", label: "Loading...", disabled: true, onclick: () => {} }]
			: estimates.length === 0
				? [{ kind: "item", label: "No streams found", disabled: true, onclick: () => {} }]
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
		label={download.status === "downloading" && progressPct != null ? `${progressPct}%` : undefined}
		loading={download.status === "downloading" || download.status === "queued"}
		onclick={handleClick}
	/>
{:else}
	<PopoverMenu items={menuItems} align="right" bind:open={dropdownOpen}>
		{#snippet trigger()}
			<Button variant="ghost" icon="Download" />
		{/snippet}
	</PopoverMenu>
{/if}
