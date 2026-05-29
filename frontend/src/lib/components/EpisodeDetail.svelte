<script lang="ts">
	import type { Season, Episode, WatchHistoryItem } from "$lib/schema";
	import { imageUrl } from "$lib/utils";
	import { Text } from "glow";
	import PlayCard from "./PlayCard.svelte";
	import DownloadButton from "./DownloadButton.svelte";

	let {
		season,
		episode,
		showTitle,
		tmdbId,
		resumeEntry,
		loadingStreams = false,
		onselectepisode,
		onplay,
	}: {
		season: Season;
		episode: Episode;
		showTitle: string;
		tmdbId: number;
		resumeEntry?: WatchHistoryItem | null;
		loadingStreams?: boolean;
		onselectepisode: (season: number, episode: number) => void;
		onplay?: () => void;
	} = $props();

	const canResume = $derived(
		resumeEntry?.season === season.season_number &&
			resumeEntry?.episode === episode.episode_number &&
			resumeEntry?.info_hash &&
			resumeEntry?.progress > 0,
	);

	const remainingMin = $derived(
		canResume && resumeEntry
			? Math.ceil((resumeEntry.duration - resumeEntry.progress) / 60)
			: null,
	);

	const progressPct = $derived(
		canResume && resumeEntry
			? (resumeEntry.progress / resumeEntry.duration) * 100
			: 0,
	);
</script>

<div class="episode-strip">
	<Text size="sm" weight="semibold">{season.name}</Text>
	<div class="episode-list">
		{#each season.episodes as ep}
			<button
				class="episode-row"
				class:active={ep.episode_number === episode.episode_number}
				onclick={() =>
					onselectepisode(season.season_number, ep.episode_number)}
			>
				<img
					class="ep-thumb"
					src={ep.stills[0] ? imageUrl(ep.stills[0], "w185") : ""}
					alt=""
				/>
				<div class="ep-info">
					<Text size="xs" variant="muted"
						>Episode {ep.episode_number}</Text
					>
					<Text size="sm" weight="semibold">{ep.name}</Text>
				</div>
			</button>
		{/each}
	</div>
</div>

<div class="sidebar">
	<div class="top">
		<h2 class="ep-title">{episode.name}</h2>
		<div class="meta">
			<Text as="span" variant="secondary" size="sm">
				S{season.season_number} E{episode.episode_number}
			</Text>
			<Text as="span" variant="secondary" size="sm">{showTitle}</Text>
		</div>

		{#if episode.overview}
			<div class="overview">
				<Text size="sm">{episode.overview}</Text>
			</div>
		{/if}
	</div>
	<div class="bottom">
		{#if onplay}
			<PlayCard
				image={episode.stills[1]
					? imageUrl(episode.stills[1], "w780")
					: episode.stills[0]
						? imageUrl(episode.stills[0], "w780")
						: undefined}
				label="S{season.season_number} E{episode.episode_number}"
				action={canResume ? "Continue" : (episode.name ?? "Play")}
				remaining={remainingMin
					? `${remainingMin} min left`
					: undefined}
				progress={progressPct}
				loading={loadingStreams}
				onclick={onplay}
			/>
		{/if}
	</div>

	<!-- <DownloadButton
		mediaType="tv"
		{tmdbId}
		title={showTitle}
		season={season.season_number}
		episode={episode.episode_number}
	/> -->
</div>

<style>
	.episode-strip {
		width: 340px;
		flex-shrink: 0;
		padding: 1rem;
		padding-top: 2rem;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		overflow-y: auto;
	}

	.episode-list {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.episode-row {
		display: flex;
		gap: 0.75rem;
		align-items: center;
		padding: 0.4rem;
		border-radius: 8px;
		cursor: pointer;
		background: transparent;
		border: none;
		text-align: left;
		color: inherit;
		transition: background 0.15s;
	}

	.episode-row:hover {
		background: rgba(255, 255, 255, 0.05);
	}

	.episode-row.active {
		background: rgba(255, 255, 255, 0.08);
	}

	.ep-thumb {
		width: 100px;
		flex-shrink: 0;
		aspect-ratio: 16/9;
		object-fit: cover;
		border-radius: 6px;
		background: var(--bg-surface, #1a1a2e);
	}

	.ep-info {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	.overview {
		margin-top: 1rem;
	}

	.sidebar {
		width: 35vw;
		height: 100%;
		overflow-y: auto;
		padding: 2rem;
		padding-top: 3.7rem;
		display: flex;
		flex-direction: column;
		justify-content: space-between;
		gap: 0.75rem;
	}

	.ep-title {
		font-size: 1.5rem;
		font-weight: 700;
		color: white;
		margin: 0;
	}

	.meta {
		display: flex;
		gap: 0.75rem;
		align-items: center;
	}

	@media (max-width: 768px) {
		.episode-strip {
			width: 100%;
			height: auto;
			max-height: 30vh;
			padding: 0.5rem;
			padding-top: 0.5rem;
			flex: 0 0 auto;
		}

		.ep-thumb {
			width: 80px;
		}

		.sidebar {
			width: 100%;
			flex: 1 1 auto;
			min-height: 0;
			padding: 1rem;
			padding-top: 1rem;
			justify-content: flex-end;
		}

		.ep-title {
			font-size: 1.2rem;
		}
	}
</style>
