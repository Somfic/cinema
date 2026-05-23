<script lang="ts">
	import { Card } from "glow";
	import type { Snippet } from "svelte";
	import { onDestroy } from "svelte";
	import { imageUrl } from "$lib/utils";
	import { movieDetails, tvDetails } from "$lib/api.gen";

	let {
		posterPath,
		mediaType,
		tmdbId,
		progress,
		selected,
		expand = false,
		onclick,
		bottomLeft,
		bottomRight,
	}: {
		posterPath?: string | null;
		mediaType: string;
		tmdbId: number;
		progress?: number;
		selected?: boolean;
		// When true the card lives in a horizontal row: it owns a fixed
		// height and grows from a 2:3 poster to a wider ~3:2 frame on hover.
		expand?: boolean;
		onclick: () => void;
		bottomLeft?: Snippet;
		bottomRight?: Snippet;
	} = $props();

	// Fixed row height; idle is the 2:3 poster, hover grows wider (3:2-ish).
	const ROW_H = 220;
	const POSTER_W = Math.round((ROW_H * 2) / 3);
	const HOVER_W = Math.round(ROW_H * 1.5);

	// Cache the raw backdrop list per title so we fetch each one once.
	const backdropCache = new Map<string, string[]>();

	let node: HTMLDivElement | undefined;
	let hovering = $state(false);
	let stills = $state<string[]>([]);
	let frame = $state(0);
	let watchdog: ReturnType<typeof setInterval> | undefined;
	// What's actually shown. Only ever set to a still after that image has
	// preloaded *and* we're still hovering, so a late load can never override
	// the poster after the pointer has left.
	let displaySrc = $state("");
	let timer: ReturnType<typeof setInterval> | undefined;
	let hoverToken = 0;

	const posterSrc = $derived(
		posterPath ? imageUrl(posterPath, "w342") : "",
	);

	// Whenever we're not hovering, the poster is the source of truth.
	$effect(() => {
		if (!hovering) displaySrc = posterSrc;
	});

	const media = $derived({
		src: displaySrc || posterSrc,
		// In expand mode our CSS box controls the shape; otherwise keep the
		// usual poster aspect so grids/dialogs are unaffected.
		...(expand ? {} : { aspectRatio: "2/3" }),
		progress,
	});

	function shuffled(list: string[]): string[] {
		const a = [...list];
		for (let i = a.length - 1; i > 0; i--) {
			const j = Math.floor(Math.random() * (i + 1));
			[a[i], a[j]] = [a[j], a[i]];
		}
		return a;
	}

	// Drop the top-voted backdrop (the polished key art the detail pages
	// use) and shuffle the rest — the slideshow gets candid stills only.
	function candid(list: string[]): string[] {
		return shuffled(list.length > 1 ? list.slice(1) : list);
	}

	async function getStills(): Promise<string[]> {
		const key = `${mediaType}:${tmdbId}`;
		const cached = backdropCache.get(key);
		if (cached) return candid(cached);
		try {
			const res =
				mediaType === "movie"
					? await movieDetails(tmdbId)
					: await tvDetails(tmdbId);
			const list = res.data.backdrops ?? [];
			backdropCache.set(key, list);
			return candid(list);
		} catch {
			return [];
		}
	}

	function show(src: string, token: number) {
		const img = new Image();
		img.onload = () => {
			if (hovering && token === hoverToken) displaySrc = src;
		};
		img.src = src;
	}

	async function onEnter() {
		hovering = true;
		const token = ++hoverToken;
		// Resizing the card shifts layout; the browser does not fire
		// mouseleave when the box moves out from under a still pointer, so
		// poll the real :hover state and self-correct a missed leave.
		if (!watchdog) {
			watchdog = setInterval(() => {
				if (node && !node.matches(":hover")) stop();
			}, 150);
		}
		const list = await getStills();
		if (!hovering || token !== hoverToken) return;
		stills = list;
		frame = 0;
		if (list.length > 0) {
			show(imageUrl(list[0], "w780"), token);
			if (!timer) {
				timer = setInterval(() => {
					if (!hovering || stills.length === 0) return;
					frame = (frame + 1) % stills.length;
					show(imageUrl(stills[frame], "w780"), token);
				}, 3000);
			}
		}
	}

	function stop() {
		hovering = false;
		hoverToken++;
		frame = 0;
		displaySrc = posterSrc;
		if (timer) {
			clearInterval(timer);
			timer = undefined;
		}
		if (watchdog) {
			clearInterval(watchdog);
			watchdog = undefined;
		}
	}

	onDestroy(stop);
</script>

<div
	role="presentation"
	bind:this={node}
	class="poster-card"
	class:expand
	style={expand
		? `height:${ROW_H}px;width:${hovering ? HOVER_W : POSTER_W}px`
		: undefined}
	onmouseenter={onEnter}
	onmouseleave={stop}
>
	<Card
		{media}
		mediaLayout="overlay"
		{selected}
		{onclick}
		{bottomLeft}
		{bottomRight}
	/>
</div>

<style>
	.poster-card {
		width: 100%;
		height: 100%;
	}

	.poster-card.expand {
		flex: 0 0 auto;
		position: relative;
		transition: width 240ms ease;
	}
</style>
