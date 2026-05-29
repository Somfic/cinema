<script lang="ts">
	import type { Season } from "$lib/schema";
	import { imageUrl } from "$lib/utils";
	import { Card, Text } from "glow";

	let {
		seasons,
		onscrollseason,
		onselectepisode,
	}: {
		seasons: Season[];
		onscrollseason: (seasonNumber: number) => void;
		onselectepisode: (season: number, episode: number) => void;
	} = $props();

	const selectableSeasons = $derived(seasons.filter((s) => s.episodes.length > 0));
	const cols = $derived(selectableSeasons.length <= 4 ? selectableSeasons.length : Math.ceil(selectableSeasons.length / 2));
</script>

<div class="browser">
	<div class="season-grid" style="grid-template-columns: repeat({cols}, 180px)">
		{#each selectableSeasons as season}
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
		{/each}
	</div>
</div>

<style>
	.browser {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		width: 100%;
		height: 100%;
		padding: 2rem;
		gap: 1rem;
		background: rgba(0, 0, 0, 0.35);
		backdrop-filter: blur(16px);
	}

	.season-grid {
		display: grid;
		gap: 1rem;
		justify-content: center;
		overflow-y: auto;
		width: 100%;
	}

	@media (max-width: 768px) {
		.browser {
			padding: 1rem;
		}

		.season-grid {
			grid-template-columns: repeat(auto-fill, minmax(130px, 1fr));
		}
	}
</style>
