<script lang="ts">
	import {
		type MediaItem,
		type SearchResult,
		type WatchHistoryItem,
	} from "$lib/schema";
	import { api } from "$lib/api";
	import { imageUrl } from "$lib/utils";
	import {
		Button,
		Icon,
		Card,
		Text,
		Avatar,
		Modal,
		useModal,
		Data,
		Media,
	} from "glow";
	import type { IconName, DataItem } from "glow";
	import PlayCard from "./PlayCard.svelte";
	import DownloadButton from "./DownloadButton.svelte";

	// Map TMDB genre names to glow (lucide) icons
	const GENRE_ICONS: Record<string, IconName> = {
		action: "Bomb",
		adventure: "Compass",
		animation: "Palette",
		comedy: "Laugh",
		crime: "Siren",
		documentary: "Video",
		drama: "Drama",
		family: "Users",
		fantasy: "Sparkles",
		history: "Landmark",
		horror: "Ghost",
		music: "Music",
		mystery: "HatGlasses",
		romance: "Heart",
		"science fiction": "Rocket",
		"tv movie": "Tv",
		thriller: "HeartPulse",
		war: "Shield",
		western: "Mountain",
		"action & adventure": "Bomb",
		kids: "Baby",
		news: "Newspaper",
		reality: "Camera",
		"sci-fi & fantasy": "Rocket",
		soap: "Heart",
		talk: "Mic",
		"war & politics": "Shield",
	};

	const genreIcon = (name: string): IconName =>
		GENRE_ICONS[name.toLowerCase()] ?? "Tag";

	let {
		item,
		loadingStreams = false,
		onwatch,
		onselectseason,
		onselectepisode,
		similarItems = [],
		resumeEntry,
		onresume,
		playing = false,
		tvMode = false,
	}: {
		item: MediaItem;
		loadingStreams?: boolean;
		onwatch?: () => void;
		onselectseason?: (seasonNumber: number) => void;
		onselectepisode?: (season: number, episode: number) => void;
		similarItems?: SearchResult[];
		resumeEntry?: WatchHistoryItem | null;
		onresume?: () => void;
		playing?: boolean;
		tvMode?: boolean;
	} = $props();

	const infoModal = useModal();

	const trailerKeys = $derived.by(() => {
		const shuffle = <T,>(arr: T[]): T[] => {
			const a = [...arr];
			for (let i = a.length - 1; i > 0; i--) {
				const j = Math.floor(Math.random() * (i + 1));
				[a[i], a[j]] = [a[j], a[i]];
			}
			return a;
		};
		const yt = (item.videos ?? []).filter((v) => v.site === "YouTube");
		const keysOfType = (type: string) =>
			shuffle(yt.filter((v) => v.video_type === type)).map((v) => v.key);
		return [...keysOfType("Trailer"), ...keysOfType("Teaser")];
	});

	const releaseYear = $derived(item.release_date?.slice(0, 4));
	const runtimeLabel = $derived(
		item.runtime
			? `${Math.floor(item.runtime / 60) > 0 ? `${Math.floor(item.runtime / 60)}h ` : ""}${item.runtime % 60}min`
			: undefined,
	);

	// Live-updating countdown to the next episode's air date.
	let now = $state(Date.now());
	$effect(() => {
		const id = setInterval(() => (now = Date.now()), 60_000);
		return () => clearInterval(id);
	});
	const nextEpisodeLabel = $derived.by(() => {
		const next = item.next_episode;
		if (!next?.air_date) return undefined;
		// TMDB returns YYYY-MM-DD; treat as midnight UTC.
		const airMs = Date.parse(`${next.air_date}T00:00:00Z`);
		if (Number.isNaN(airMs)) return undefined;
		const diffMs = airMs - now;
		const prefix = `S${next.season_number}E${next.episode_number}`;
		if (diffMs <= 0) return `${prefix} aired`;
		const days = Math.floor(diffMs / 86_400_000);
		const hours = Math.floor((diffMs % 86_400_000) / 3_600_000);
		if (days >= 1) return `${prefix} in ${days}d ${hours}h`;
		const minutes = Math.max(1, Math.floor(diffMs / 60_000));
		if (hours >= 1) return `${prefix} in ${hours}h ${minutes % 60}m`;
		return `${prefix} in ${minutes}m`;
	});

	let onWatchlist = $state(false);
	let isFavorite = $state(false);
	let isWatched = $state(false);
	let watchlistLoading = $state(false);
	let favoriteLoading = $state(false);
	let watchedLoading = $state(false);

	$effect(() => {
		api.collections
			.contains("watchlist", item.media_type, item.id)
			.then((res) => {
				onWatchlist = res.in_collection;
			})
			.catch(() => {});
		api.collections
			.contains("favorites", item.media_type, item.id)
			.then((res) => {
				isFavorite = res.in_collection;
			})
			.catch(() => {});
		api.collections
			.contains("watched", item.media_type, item.id)
			.then((res) => {
				isWatched = res.in_collection;
			})
			.catch(() => {});
	});

	async function toggleCollection(
		name: string,
		current: boolean,
		setLoading: (v: boolean) => void,
		setState: (v: boolean) => void,
	) {
		setLoading(true);
		try {
			if (current) {
				await api.collections.remove(name, item.media_type, item.id);
				setState(false);
			} else {
				await api.collections.add({
					collection: name,
					media_type: item.media_type,
					tmdb_id: item.id,
					title: item.title,
					poster_path: item.poster_path ?? null,
				});
				setState(true);
			}
		} catch {
		} finally {
			setLoading(false);
		}
	}

	// ── Derived PlayCard props ──
	const movieBackdrop = $derived(
		item.backdrops?.[1]
			? imageUrl(item.backdrops[1], "w780")
			: item.backdrops?.[0]
				? imageUrl(item.backdrops[0], "w780")
				: undefined,
	);

	const movieResume = $derived(
		resumeEntry?.info_hash && item.media_type === "movie",
	);
	const movieRemainingMin = $derived(
		resumeEntry
			? Math.ceil((resumeEntry.duration - resumeEntry.progress) / 60)
			: 0,
	);
	const movieProgress = $derived(
		resumeEntry && resumeEntry.duration > 0
			? (resumeEntry.progress / resumeEntry.duration) * 100
			: 0,
	);

	const tvResume = $derived(resumeEntry && resumeEntry.season > 0);
	const resumeEpisode = $derived(
		tvResume
			? item.seasons
					?.find((s) => s.season_number === resumeEntry!.season)
					?.episodes?.find(
						(e) => e.episode_number === resumeEntry!.episode,
					)
			: null,
	);
	const firstSeason = $derived(item.seasons?.[0]);
	const firstEpisode = $derived(firstSeason?.episodes?.[0]);

	const tvImage = $derived(
		tvResume && resumeEpisode?.stills?.[0]
			? imageUrl(resumeEpisode.stills[0], "w780")
			: firstEpisode?.stills?.[0]
				? imageUrl(firstEpisode.stills[0], "w780")
				: undefined,
	);
	const tvLabel = $derived(
		tvResume
			? `S${resumeEntry!.season} E${resumeEntry!.episode}`
			: firstSeason && firstEpisode
				? `S${firstSeason.season_number} E${firstEpisode.episode_number}`
				: undefined,
	);
	const tvAction = $derived(
		tvResume
			? (resumeEpisode?.name ?? "Continue")
			: (firstEpisode?.name ?? "Start Watching"),
	);
	const tvRemaining = $derived(
		tvResume
			? `${Math.ceil((resumeEntry!.duration - resumeEntry!.progress) / 60)} min left`
			: undefined,
	);
	const tvProgress = $derived(
		tvResume && resumeEntry!.duration > 0
			? (resumeEntry!.progress / resumeEntry!.duration) * 100
			: 0,
	);
	const tvOnclick = $derived(
		tvResume && onselectepisode
			? () => onselectepisode!(resumeEntry!.season, resumeEntry!.episode)
			: firstSeason && firstEpisode && onselectepisode
				? () =>
						onselectepisode!(
							firstSeason!.season_number,
							firstEpisode!.episode_number,
						)
				: undefined,
	);

	const logoSrc = $derived(
		item.logo_path ? imageUrl(item.logo_path, "original") : null,
	);
</script>

<div class="sidebar" class:tv={tvMode}>
	<div class="title-area">
		{#if logoSrc}
			<div class="logo">
				<Media src={logoSrc} alt={item.title} fit="contain" lazy={false} />
			</div>
		{:else}
			<h1 class="title">{item.title}</h1>
		{/if}
	</div>

	<div class="actions-row" class:tv={tvMode}>
		<div class="meta">
			{#if item.release_date}
				<Text as="span" variant="secondary" size="sm"
					>{item.release_date.slice(0, 4)}</Text
				>
			{/if}
			{#if item.runtime}
				<Text as="span" variant="secondary" size="sm"
					>{Math.floor(item.runtime / 60) > 0
						? `${Math.floor(item.runtime / 60)}h `
						: ""}{item.runtime % 60}min</Text
				>
			{/if}
			{#if item.seasons}
				<Text as="span" variant="secondary" size="sm">
					{item.seasons.length} season{item.seasons.length > 1
						? "s"
						: ""}
				</Text>
			{/if}
			{#if item.rating}
				<Text as="span" variant="secondary" size="sm"
					>★ {item.rating.toFixed(1)}</Text
				>
			{/if}
			{#if nextEpisodeLabel}
				<Text as="span" variant="secondary" size="sm">
					{nextEpisodeLabel}
				</Text>
			{/if}
		</div>
		{#if !tvMode}
			<Button
				variant="ghost"
				icon={isFavorite ? { name: "Heart", fill: true } : "Heart"}
				tooltip={isFavorite
					? "Remove from favorites"
					: "Add to favorites"}
				loading={favoriteLoading}
				onclick={() =>
					toggleCollection(
						"favorites",
						isFavorite,
						(v) => (favoriteLoading = v),
						(v) => (isFavorite = v),
					)}
			/>
			<Button
				variant="ghost"
				icon={onWatchlist
					? { name: "Bookmark", fill: true }
					: "Bookmark"}
				tooltip={onWatchlist
					? "Remove from watchlist"
					: "Add to watchlist"}
				loading={watchlistLoading}
				onclick={() =>
					toggleCollection(
						"watchlist",
						onWatchlist,
						(v) => (watchlistLoading = v),
						(v) => (onWatchlist = v),
					)}
			/>
			<Button
				variant="ghost"
				icon="Info"
				tooltip="More information"
				onclick={infoModal.show}
			/>
			{#if onselectseason && item.seasons?.length}
				<Button
					variant="ghost"
					icon="LayoutGrid"
					tooltip="Browse episodes"
					onclick={() =>
						onselectseason(item.seasons![0].season_number)}
				/>
			{/if}
		{/if}
		<!-- <DownloadButton
			mediaType={item.media_type}
			tmdbId={item.id}
			title={item.title}
			posterPath={item.poster_path ?? null}
		/> -->
	</div>

	{#if !tvMode && item.media_type === "movie" && (movieResume ? onresume : onwatch)}
		<PlayCard
			image={movieBackdrop}
			{trailerKeys}
			imdbId={item.imdb_id}
			active={!playing}
			label={item.title}
			action={movieResume
				? (item.tagline ?? "Continue")
				: (item.tagline ?? "Watch")}
			remaining={movieResume
				? `${movieRemainingMin} min left`
				: undefined}
			progress={movieResume ? movieProgress : 0}
			loading={!movieResume && loadingStreams}
			onclick={movieResume ? onresume! : onwatch!}
		/>
	{/if}

	{#if !tvMode && item.seasons?.length && tvOnclick}
		<PlayCard
			image={tvImage}
			{trailerKeys}
			imdbId={item.imdb_id}
			active={!playing}
			label={tvLabel}
			action={tvAction}
			remaining={tvRemaining}
			progress={tvProgress}
			onclick={tvOnclick}
		/>
	{/if}

	<!-- {#if similarItems.length > 0}
		<div class="similar-section">
			<Text weight="semibold" size="sm">Similar</Text>
			<div class="similar-grid">
				{#each similarItems.slice(0, 8) as sim}
					<Card
						media={{
							src: sim.poster_path
								? imageUrl(sim.poster_path, "w185")
								: "",
							aspectRatio: "2/3",
						}}
						mediaLayout="overlay"
						onclick={() =>
							(window.location.href = `/${sim.media_type}/${sim.id}`)}
					>
						{#snippet bottomLeft()}
							<Text size="xs" variant="on-image">{sim.title}</Text>
						{/snippet}
					</Card>
				{/each}
			</div>
		</div>
	{/if} -->
</div>

{#if item.genres?.length}
	<div class="genres-overlay">
		{#each item.genres as genre}
			<Button
				variant="ghost"
				icon={genreIcon(genre.name)}
				tooltip={genre.name}
			/>
		{/each}
	</div>
{/if}

{#snippet overviewVal()}
	<Text size="sm">{item.overview}</Text>
{/snippet}
{#snippet genresVal()}
	<div class="info-chips">
		{#each item.genres as genre}
			<Button
				variant="outlined"
				icon={genreIcon(genre.name)}
				label={genre.name}
			/>
		{/each}
	</div>
{/snippet}
{#snippet directorsVal()}
	<div class="info-people">
		{#each item.directors as person}
			<div class="person">
				<Avatar
					name={person.name}
					src={person.profile_path
						? imageUrl(person.profile_path, "w185")
						: undefined}
					size="md"
				/>
				<Text size="sm">{person.name}</Text>
			</div>
		{/each}
	</div>
{/snippet}
{#snippet castVal()}
	<div class="info-people">
		{#each item.cast as person}
			<div class="person">
				<Avatar
					name={person.name}
					src={person.profile_path
						? imageUrl(person.profile_path, "w185")
						: undefined}
					size="md"
				/>
				<div class="person-meta">
					<Text size="sm">{person.name}</Text>
					{#if person.character}
						<Text size="xs" variant="secondary"
							>{person.character}</Text
						>
					{/if}
				</div>
			</div>
		{/each}
	</div>
{/snippet}

<Modal bind:open={infoModal.open} title={item.title} size="large">
	<div class="info-dialog">
		<Data
			variant="inline"
			divided={false}
			properties={[
				releaseYear &&
					({
						label: "Year",
						icon: "Calendar",
						value: releaseYear,
					} satisfies DataItem),
				runtimeLabel &&
					({
						label: "Runtime",
						icon: "Clock",
						value: runtimeLabel,
					} satisfies DataItem),
				item.seasons?.length &&
					({
						label: "Seasons",
						icon: "Layers",
						value: `${item.seasons.length}`,
					} satisfies DataItem),
				item.rating &&
					({
						label: "Rating",
						icon: "Star",
						value: `${item.rating.toFixed(1)} / 10`,
					} satisfies DataItem),
				item.tagline &&
					({
						label: "Tagline",
						icon: "Quote",
						value: item.tagline,
						muted: true,
					} satisfies DataItem),
			].filter(Boolean) as DataItem[]}
		/>

		<Data
			variant="stacked"
			divided={false}
			properties={[
				item.overview &&
					({
						label: "Overview",
						icon: "FileText",
						render: overviewVal,
					} satisfies DataItem),
				item.genres?.length &&
					({
						label: "Genres",
						icon: "Tags",
						render: genresVal,
					} satisfies DataItem),
				item.directors?.length &&
					({
						label:
							item.directors.length > 1
								? "Directors"
								: "Director",
						icon: "Clapperboard",
						render: directorsVal,
					} satisfies DataItem),
				item.cast?.length &&
					({
						label: "Cast",
						icon: "Users",
						render: castVal,
					} satisfies DataItem),
			].filter(Boolean) as DataItem[]}
		/>
	</div>
</Modal>

<style>
	.sidebar {
		width: 40vw;
		min-height: 100%;
		padding: 2rem;
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	.title-area {
		flex: 1;
		display: flex;
		flex-direction: column;
		justify-content: center;
		gap: 0.75rem;
		min-height: 0;
	}

	/* Box for the <Media> logo — Media fills its parent and contains the logo
	   within. Media crossfades it in once loaded; 0.95 opacity lets the moving
	   glow behind subtly show through the logo. */
	.logo {
		width: 100%;
		height: 30vh;
		opacity: 0.95;
		filter: drop-shadow(0 2px 12px rgba(0, 0, 0, 0.6));
	}

	/* Contain the logo within the box, horizontally centered (right-aligned in TV
	   mode via the override below). */
	.logo :global(.media),
	.logo :global(img) {
		object-position: center center;
	}

	.title {
		font-size: 2.5rem;
		font-weight: 700;
		color: white;
		text-shadow: 0 2px 12px rgba(0, 0, 0, 0.5);
		margin: 0;
	}

	.meta {
		display: flex;
		gap: 0.75rem;
		align-items: center;
		margin-right: auto;
	}

	/* TV mode: no action buttons, so right-align the meta. */
	.actions-row.tv .meta {
		margin-right: 0;
		margin-left: auto;
	}

	/* TV mode: group the logo directly above the meta at the bottom of the
	   hero, both right-aligned (no centered logo, no big gap). */
	.sidebar.tv {
		justify-content: flex-end;
	}

	.sidebar.tv .title-area {
		flex: none;
		align-items: flex-end;
	}

	.sidebar.tv .logo :global(.media),
	.sidebar.tv .logo :global(img) {
		object-position: right center;
	}

	.sidebar.tv .actions-row {
		margin-top: 0;
	}

	.actions-row {
		display: flex;
		gap: 0.5rem;
		align-items: center;
		margin-top: auto;
		justify-content: flex-end;
	}

	.genres-overlay {
		position: absolute;
		left: 0;
		bottom: 0;
		padding: 2rem;
		display: flex;
		gap: 0.5rem;
		flex-wrap: wrap;
		max-width: 55vw;
		z-index: 2;
	}

	.info-dialog {
		display: flex;
		flex-direction: column;
		gap: 1.25rem;
	}

	.info-chips {
		display: flex;
		gap: 0.5rem;
		flex-wrap: wrap;
	}

	.info-people {
		display: flex;
		gap: 1rem 1.25rem;
		flex-wrap: wrap;
	}

	.person {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-shrink: 0;
		max-width: 14rem;
	}

	.person-meta {
		display: flex;
		flex-direction: column;
		min-width: 0;
	}

	.person-meta :global(span) {
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	@media (max-width: 768px) {
		.genres-overlay {
			top: 0;
			bottom: auto;
			padding: 0 1rem 1rem;
			flex-direction: column;
			max-width: none;
		}

		.sidebar {
			width: 100%;
			padding: 1rem;
			padding-top: 40vh;
			min-height: 100%;
			box-sizing: border-box;
			gap: 1rem;
		}

		.title-area {
			flex: 0;
			align-items: center;
			text-align: center;
		}

		.logo {
			width: 100%;
			height: 22vh;
		}

		.logo :global(.media),
		.logo :global(img) {
			object-position: center center;
		}

		.title {
			font-size: 2rem;
			width: 100%;
		}

		.actions-row {
			flex-wrap: wrap;
		}

		.meta {
			gap: 0.5rem;
		}
	}
</style>
