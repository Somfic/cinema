<script lang="ts">
	import EpisodeDetail from "$lib/components/EpisodeDetail.svelte";
	import { getTitleContext } from "../../context";

	const title = getTitleContext();
</script>

{#if title.item && title.season && title.episode}
	<div class="detail">
		<EpisodeDetail
			season={title.season}
			episode={title.episode}
			showTitle={title.item.title}
			tmdbId={title.item.tmdb_id}
			resumeEntry={title.resumeEntry}
			loadingStreams={title.loadingStreams}
			onselectepisode={(s, e) => title.go(s, e)}
			onplay={title.playEpisode}
		/>
	</div>
{/if}

<style>
	.detail {
		display: flex;
		height: 100%;
		overflow: hidden;
		/* Clears the back button, which sits in this corner over the episode
		   strip's season label. */
		padding-top: 3.25rem;
	}

	@media (max-width: 768px) {
		.detail {
			flex-direction: column;
		}
	}
</style>
