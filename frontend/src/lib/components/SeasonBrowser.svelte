<script lang="ts">
	import type { Season } from "$lib/schema";
	import { imageUrl } from "$lib/utils";
	import { Card } from "glow";

	let {
		seasons,
		onscrollseason,
		onselectepisode,
	}: {
		seasons: Season[];
		onscrollseason: (seasonNumber: number) => void;
		onselectepisode: (season: number, episode: number) => void;
	} = $props();

	const selectableSeasons = $derived(
		seasons.filter((s) => s.episodes.length > 0),
	);
	// Desktop layout used a 1-row grid for ≤4 seasons and a 2-row grid
	// otherwise (one even split). Preserved here for parity with the
	// original behaviour.
	const cols = $derived(
		selectableSeasons.length <= 4
			? selectableSeasons.length
			: Math.ceil(selectableSeasons.length / 2),
	);
</script>

<!--
	Layout: the outer `.browser` is the sole scroll container. Inside, `.cards`
	just flow-lays the posters; each one sits in a `.card-slot` so we control
	the card's *outer width* via CSS while glow's Card handles its own 2:3
	aspect ratio internally. No height: 100% on the card, no flex/grid track
	height computation — every prior bug came from one of those two.

	Desktop: flex-wrap row of 180px slots, centered.
	Mobile: flex column of 100%-width slots — scrolls vertically inside .browser.
-->
<div class="browser">
	<div class="cards" style="--cols: {cols}">
		{#each selectableSeasons as season}
			<div class="card-slot">
				<Card
					media={{
						src: season.poster_path
							? imageUrl(season.poster_path, "w780")
							: "",
						aspectRatio: "2/3",
					}}
					mediaLayout="overlay"
					onclick={() => {
						onscrollseason(season.season_number);
						onselectepisode(season.season_number, 1);
					}}
				/>
			</div>
		{/each}
	</div>
</div>

<style>
	.browser {
		width: 100%;
		height: 100%;
		overflow-y: auto;
		padding: 2rem;
		box-sizing: border-box;
		background: rgba(0, 0, 0, 0.35);
		backdrop-filter: blur(16px);
	}

	/* Vertical + horizontal centring is desktop-only. On mobile the
	   browser stays a plain block scroll container so `.cards` (and the
	   `width: 100%` slots inside it) actually fill the viewport. */
	@media (min-width: 769px) {
		.browser {
			display: flex;
			flex-direction: column;
			align-items: center;
			justify-content: center;
		}
	}

	/* Desktop: fixed N-column grid driven by `--cols`. Matches the
	   pre-mobile-refactor layout (≤4 seasons → one row, otherwise an
	   even two-row split). */
	.cards {
		display: grid;
		grid-template-columns: repeat(var(--cols), 180px);
		gap: 1rem;
		justify-content: center;
		align-items: flex-start;
	}

	.card-slot {
		width: 180px;
	}

	/* glow's `.card` sets `height: 100%`, which would stretch each slot
	   vertically and break the 2:3 aspect ratio. */
	.card-slot :global(.card) {
		height: auto;
	}

	@media (max-width: 768px) {
		.browser {
			/* Wider horizontal inset so the 2:3 posters don't take up the
			   entire screen height each. */
			padding: 1rem 4rem;
		}

		/* Drop the grid for a plain flex column — the grid track-height
		   computation with an aspect-ratio-driven child collapses every
		   row to the same height, stacking all cards at y=0. */
		.cards {
			display: flex;
			flex-direction: column;
		}

		.card-slot {
			width: 100%;
		}
	}
</style>
