<script lang="ts">
	import type { MediaType, Stream } from "$lib/schema";
	import { api } from "$lib/api";
	import StreamMenu, { streamStatusIcon } from "./StreamMenu.svelte";
	import { Button, toast } from "glow";

	let {
		tmdbId,
		mediaType,
		season = null,
		episode = null,
	}: {
		tmdbId: number;
		mediaType: MediaType;
		season?: number | null;
		episode?: number | null;
	} = $props();

	let streams = $state<Stream[]>([]);
	let loading = $state(false);
	let loadedKey: string | null = null;

	// An episode list renders one of these per row, so sources are only fetched
	// when the menu is actually opened.
	async function load() {
		const key = `${mediaType}:${tmdbId}:${season}:${episode}`;
		if (loadedKey === key) return;
		loadedKey = key;
		loading = true;
		try {
			streams =
				mediaType === "tv" && season != null && episode != null
					? await api.streams.tv(tmdbId, season, episode)
					: await api.streams.movie(tmdbId);
		} catch (err: unknown) {
			loadedKey = null;
			const msg = err instanceof Error ? err.message : String(err);
			toast.error(`Failed to load streams: ${msg}`);
		} finally {
			loading = false;
		}
	}

	const RETRIABLE_STATUSES = new Set(["Failed", "Cancelled"]);

	function alreadyDownloading(stream: Stream): boolean {
		return (
			stream.download != null &&
			!RETRIABLE_STATUSES.has(stream.download.status)
		);
	}

	async function enqueue(stream: Stream) {
		try {
			await api.downloads.enqueue({
				info_hash: stream.info_hash,
				file_idx: stream.file_idx,
			});
			toast.success("Download enqueued");
			loadedKey = null; // pick up the new status next time the menu opens
		} catch (err: unknown) {
			const msg = err instanceof Error ? err.message : String(err);
			toast.error(`Download failed: ${msg}`);
		}
	}
</script>

<StreamMenu
	{streams}
	{loading}
	onselect={enqueue}
	disabled={alreadyDownloading}
	icon={(stream) => streamStatusIcon(stream) ?? "Download"}
>
	{#snippet trigger()}
		<Button
			variant="ghost"
			icon="Download"
			tooltip="Download"
			{loading}
			onclick={load}
		/>
	{/snippet}
</StreamMenu>
