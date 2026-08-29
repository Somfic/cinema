<script lang="ts" module>
	import type { Stream } from "$lib/schema";
	import type { IconProp } from "glow";

	const STATUS_ICON: Record<string, IconProp> = {
		Queued: { name: "Hourglass", color: "var(--glow-color-warning)" },
		Downloading: { name: "ArrowDownToLine", color: "var(--glow-color-info)" },
		Paused: { name: "Pause", color: "var(--glow-color-warning)" },
		Completed: { name: "Check", color: "var(--glow-color-success)" },
	};

	/** Download state of a stream, as a row icon — undefined when it has none. */
	export function streamStatusIcon(stream: Stream): IconProp | undefined {
		return stream.download ? STATUS_ICON[stream.download.status] : undefined;
	}
</script>

<script lang="ts">
	import type { Snippet } from "svelte";
	import { DOWNLOAD_STATUS_LABEL } from "$lib/utils";
	import { PopoverMenu, type PopoverMenuEntry } from "glow";

	let {
		streams,
		trigger,
		onselect,
		activeStreamHash = null,
		loading = false,
		limit,
		icon = streamStatusIcon,
		disabled,
		extra = [],
		align = "right",
	}: {
		streams: Stream[];
		/** The element the menu hangs off — a Button, usually. */
		trigger: Snippet;
		onselect: (stream: Stream) => void;
		/** Ticks the stream that is currently playing. */
		activeStreamHash?: string | null;
		loading?: boolean;
		/** Cap the number of sources listed per resolution. */
		limit?: number;
		/** Leading icon per row; defaults to the stream's download state. */
		icon?: (stream: Stream) => IconProp | undefined;
		disabled?: (stream: Stream) => boolean;
		/** Appended below the sources — the player hangs its transcoding picker here. */
		extra?: PopoverMenuEntry[];
		align?: "left" | "right" | "stretch";
	} = $props();

	const RESOLUTION_ORDER: Record<string, number> = {
		"4K": 4,
		"2160p": 4,
		"1080p": 3,
		"720p": 2,
		"480p": 1,
	};

	let open = $state(false);
	let pickedResolution = $state<string | null>(null);

	// Quality is a filter, not a choice that outlives the menu.
	$effect(() => {
		if (!open) pickedResolution = null;
	});

	/** Streams bucketed by resolution, best first. */
	const groups = $derived.by(() => {
		const buckets = new Map<string, Stream[]>();
		for (const stream of streams) {
			const res = stream.resolution ?? "Unknown";
			const bucket = buckets.get(res);
			if (bucket) bucket.push(stream);
			else buckets.set(res, [stream]);
		}
		return [...buckets]
			.map(([resolution, streams]) => ({ resolution, streams }))
			.sort(
				(a, b) =>
					(RESOLUTION_ORDER[b.resolution] ?? 0) -
					(RESOLUTION_ORDER[a.resolution] ?? 0),
			);
	});

	// Defaults to whatever is playing, else the best quality on offer.
	const activeGroup = $derived(
		groups.find((g) => g.resolution === pickedResolution) ??
			groups.find((g) =>
				g.streams.some((s) => s.info_hash === activeStreamHash),
			) ??
			groups[0] ??
			null,
	);

	function description(stream: Stream): string | undefined {
		const parts = [
			stream.codec,
			stream.audio,
			stream.source_type,
			stream.hdr ? "HDR" : null,
			stream.imax ? "IMAX" : null,
			stream.seeders != null ? `${stream.seeders} seeds` : null,
			stream.download
				? (DOWNLOAD_STATUS_LABEL[stream.download.status] ??
					stream.download.status)
				: null,
		].filter(Boolean);
		return parts.length > 0 ? parts.join(" · ") : undefined;
	}

	const items = $derived<PopoverMenuEntry[]>([
		...(loading || groups.length === 0
			? [
					{
						kind: "item" as const,
						label: loading ? "Finding sources…" : "No sources found",
						disabled: true,
						onclick: () => {},
					},
				]
			: [
					...(groups.length > 1
						? [
								{ kind: "header" as const, label: "Quality" },
								...groups.map((group) => ({
									kind: "item" as const,
									label: group.resolution,
									selected: group === activeGroup,
									onclick: () => {
										pickedResolution = group.resolution;
										// PopoverMenu closes on any item click; filtering the
										// list isn't a pick, so take the close back.
										open = true;
									},
								})),
							]
						: []),
					{ kind: "header" as const, label: "Sources" },
					...(activeGroup?.streams ?? [])
						.slice(0, limit)
						.map((stream) => ({
							kind: "item" as const,
							label: stream.source,
							description: description(stream),
							// MenuItem splits `shortcut` on spaces into one <Kbd> each, so keep
							// "3.67 GB" as a single token.
							shortcut: stream.size_display?.replace(/ /g, "\u00a0"),
							icon: icon?.(stream),
							disabled: disabled?.(stream) ?? false,
							selected: stream.info_hash === activeStreamHash,
							onclick: () => onselect(stream),
						})),
				]),
		...extra,
	]);
</script>

<PopoverMenu {items} {align} {trigger} bind:open />
