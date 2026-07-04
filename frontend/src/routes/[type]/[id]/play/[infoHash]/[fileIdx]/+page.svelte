<script lang="ts">
	import { onDestroy } from "svelte";
	import { page } from "$app/state";
	import { goto } from "$app/navigation";
	import {
		type MediaItem,
		type Stream,
		type MediaType,
		type TranscodingOption,
	} from "$lib/schema";
	import { api } from "$lib/api";
	import { getDetails, imageUrl } from "$lib/utils";
	import VideoPlayer from "$lib/components/VideoPlayer.svelte";
	import { remote, type PlayerControls } from "$lib/remote.svelte";
	import { PlaybackSession } from "$lib/playback.svelte";

	let item = $state<MediaItem | null>(null);
	let error = $state<string | null>(null);
	// Alternative streams/sources for this title, so a paired remote can offer
	// the same source menu the inline player has.
	let streams = $state<Stream[]>([]);
	// Resume position (seconds) applied once on first load.
	let startTime = $state(0);

	const mediaType = $derived(page.params.type as MediaType);
	const mediaId = $derived(Number(page.params.id));
	const infoHash = $derived(page.params.infoHash);
	const fileIdx = $derived(Number(page.params.fileIdx));

	// Parse season/episode from query params for TV.
	const season = $derived(
		page.url.searchParams.get("s")
			? Number(page.url.searchParams.get("s"))
			: null,
	);
	const episode = $derived(
		page.url.searchParams.get("e")
			? Number(page.url.searchParams.get("e"))
			: null,
	);

	const session = new PlaybackSession({
		item: () => item,
		season: () => season,
		episode: () => episode,
		currentStream: () => ({ info_hash: infoHash as string, file_idx: fileIdx }),
		onError: (msg) => (error = msg),
	});

	const backdropUrls = $derived(
		item?.backdrops?.map((b) => imageUrl(b, "original")) ?? [],
	);

	// Backdrop shown behind the player while it loads — the episode still for a
	// TV episode, otherwise the movie/show backdrop.
	const playerBackdrop = $derived.by(() => {
		if (mediaType === "tv" && season !== null && episode !== null) {
			const still = item?.seasons
				?.find((s) => s.season_number === season)
				?.episodes?.find((e) => e.episode_number === episode)?.stills?.[0];
			if (still) return imageUrl(still, "original");
		}
		return backdropUrls[0];
	});

	// Direct (raw, untranscoded) stream URL for the "open in external player"
	// control — a desktop player handles any codec, so it's offered regardless
	// of whether the browser can play the file inline.
	const externalUrl = $derived(api.urls.stream(infoHash as string, fileIdx));

	const episodeName = $derived(() => {
		if (!item || mediaType !== "tv" || season === null || episode === null)
			return null;
		const s = item.seasons?.find((s) => s.season_number === season);
		return s?.episodes?.find((e) => e.episode_number === episode)?.name ?? null;
	});

	const playerTitle = $derived(
		mediaType === "tv" && item
			? (episodeName() ?? `Episode ${episode}`)
			: item?.title,
	);
	const playerTopline = $derived(
		mediaType === "tv" && item && season !== null && episode !== null
			? `S${season} E${episode} · ${item.title}`
			: undefined,
	);

	$effect(() => {
		const type = mediaType;
		const id = mediaId;
		const s = season;
		const e = episode;
		const hash = infoHash as string;
		const idx = fileIdx;

		const detailsPromise = getDetails(type, id)
			.then((res) => {
				item = res;
			})
			.catch((e) => {
				error = e.message;
			});

		// Fetch watch history first so the session starts with the saved
		// transcoding mode and seek position already applied.
		const historyPromise = api.watch
			.history()
			.then((items) => {
				const entry = items.find(
					(w) =>
						w.media_type === type &&
						w.tmdb_id === id &&
						(type === "movie" || (w.season === s && w.episode === e)) &&
						w.progress > 0,
				);
				if (
					entry &&
					entry.duration > 0 &&
					entry.progress < entry.duration - 30
				) {
					startTime = entry.progress;
					return {
						startAt: entry.progress,
						transcoding: entry.transcoding as TranscodingOption,
					};
				}
				return undefined;
			})
			.catch(
				() =>
					undefined as
						| { startAt: number; transcoding: TranscodingOption }
						| undefined,
			);

		const streamPromise = historyPromise.then((resumeOpts) =>
			session
				.start({ info_hash: hash, file_idx: idx }, resumeOpts)
				.catch((e: Error) => {
					error = e.message;
				}),
		);

		Promise.all([detailsPromise, streamPromise]).then(() => {
			session.loadSubtitles();
		});
	});

	// Fetch the alternative streams for this title (for the remote's source menu).
	$effect(() => {
		(async () => {
			try {
				if (mediaType === "movie") {
					streams = await api.streams.movie(mediaId);
				} else if (season !== null && episode !== null) {
					streams = await api.streams.tv(mediaId, season, episode);
				}
			} catch {
				streams = [];
			}
		})();
	});

	// Once playback reaches the resume point, stop re-applying it so a later
	// transcode/source switch resumes from the live position instead.
	$effect(() => {
		if (startTime > 0 && playerTime >= startTime - 0.5) startTime = 0;
	});

	// Save periodically while playing.
	$effect(() => {
		if (!session.streamUrl) return;
		const interval = setInterval(
			() => session.saveProgress(playerTime, playerDuration),
			30_000,
		);
		return () => clearInterval(interval);
	});

	// Switch source/quality by re-navigating the play route to the new stream.
	function selectStream(hash: string) {
		const stream = streams.find((s) => s.info_hash === hash);
		if (!stream) return;
		const qs = new URLSearchParams();
		if (season !== null) qs.set("s", String(season));
		if (episode !== null) qs.set("e", String(episode));
		const q = qs.toString();
		goto(
			`/${mediaType}/${mediaId}/play/${stream.info_hash}/${stream.file_idx}${q ? `?${q}` : ""}`,
		);
	}

	let playerTime = $state(0);
	let playerPaused = $state(true);
	let playerVolume = $state(1);
	let playerDuration = $state(0);
	let playerBuffered = $state(0);
	let playerSubtitleOffset = $state(-0.25);
	let playerLoading = $state(true);
	// Loose type: the bound instance exposes the VideoPlayer's `export function`s.
	// eslint-disable-next-line @typescript-eslint/no-explicit-any
	let playerRef = $state<any>(undefined);

	// Integer-second tick: throttles the TV→remote state feed to ~1 Hz instead
	// of firing on every `timeupdate`.
	const tvSecond = $derived(Math.floor(playerTime));

	// TV mode: register the player so a paired remote can drive it, and stream
	// now-playing state back to the remote.
	$effect(() => {
		if (remote.mode !== "tv" || !playerRef) return;
		const controls: PlayerControls = {
			play: () => playerRef.play(),
			pause: () => playerRef.pause(),
			togglePlay: () => playerRef.togglePlay(),
			seekTo: (t) => playerRef.seekTo(t),
			seekBy: (d) => playerRef.seekBy(d),
			setVolume: (v) => playerRef.setVolumeValue(v),
			toggleMute: () => playerRef.toggleMute(),
			toggleFullscreen: () => playerRef.toggleFullscreen(),
			selectStream: (hash) => selectStream(hash),
			selectAudio: (id) => session.switchAudio(id, playerTime),
			selectSubtitle: (url) => {
				const track = session.subtitleTracks.find((t) => t.url === url);
				if (track) session.selectSubtitleTrack(track);
			},
			subtitleOff: () => session.disableSubtitles(),
			setSubtitleOffset: (delta) => {
				playerSubtitleOffset += delta;
			},
			setTranscoding: (enabled, onlyAudio) => {
				// Update the state the player binds to (the inline menu does this
				// via binding; the command path must do it explicitly) so the
				// published TvState reflects the toggle and the remote doesn't
				// snap back.
				session.transcoding.enabled = enabled;
				session.transcoding.onlyAudio = onlyAudio;
				session.toggleTranscoding(enabled, onlyAudio, playerTime);
			},
			exit: () => close(),
		};
		remote.registerPlayer(controls);
		return () => remote.clearPlayer();
	});

	$effect(() => {
		if (remote.mode !== "tv") return;
		remote.publishState({
			title: playerTitle ?? undefined,
			titleImage: item?.logo_path
				? imageUrl(item.logo_path, "original")
				: undefined,
			topline: playerTopline ?? undefined,
			poster: backdropUrls[0],
			currentTime: tvSecond,
			duration: playerDuration,
			buffered: playerBuffered,
			loading: playerLoading,
			paused: playerPaused,
			volume: playerVolume,
			muted: playerVolume === 0,
			playing: !!session.streamUrl,
			streams,
			activeStreamHash: infoHash,
			audioTracks: session.fileAudioTracks.map((t) => ({
				id: t.stream_index,
				name: t.name,
				lang: t.language ?? undefined,
			})),
			activeAudioTrack: session.activeAudioIdx,
			subtitleTracks: session.subtitleTracks.map((t) => ({
				id: t.id,
				language: t.language,
				url: t.url,
			})),
			activeTrackUrl: session.activeTrackUrl,
			subtitlesActive: session.activeCues.length > 0,
			subtitleOffset: playerSubtitleOffset,
			transcoding: {
				enabled: session.transcoding.enabled,
				onlyAudio: session.transcoding.onlyAudio,
			},
			streamStats: session.streamStats,
			pieceMap: session.pieceMap,
		});
	});

	// Leaving playback: tell the paired remote the TV is idle again so it
	// resumes mirroring the phone's browsing (a stale playing:true would
	// otherwise keep navigation suppressed).
	onDestroy(() => {
		session.saveProgress(playerTime, playerDuration);
		session.stop();
		if (remote.mode === "tv") {
			remote.publishState({
				currentTime: 0,
				duration: 0,
				paused: true,
				volume: playerVolume,
				muted: false,
				playing: false,
			});
		}
		remote.clearPlayer();
	});

	function close() {
		goto(`/${mediaType}/${mediaId}`);
	}
</script>

<div class="player-page">
	{#if error}
		<div class="error-overlay">
			<p>{error}</p>
			<button onclick={close}>Go back</button>
		</div>
	{:else if session.streamUrl}
		<VideoPlayer
			src={session.streamUrl}
			subtitles={session.activeCues}
			title={playerTitle}
			topline={playerTopline}
			titleImage={item?.logo_path
				? imageUrl(item.logo_path, "original")
				: undefined}
			audioTracks={session.fileAudioTracks.map((t) => ({
				id: t.stream_index,
				name: t.name,
				lang: t.language ?? undefined,
			}))}
			activeAudioTrack={session.activeAudioIdx}
			onAudioSelect={(track) => session.switchAudio(track.id, playerTime)}
			chapters={session.fileChapters}
			knownDuration={session.hlsSessionId ? session.mediaDuration : 0}
			onSeekRestart={session.hlsSessionId
				? (t) => session.seekRestart(t)
				: undefined}
			streamStats={session.streamStats}
			pieceMap={session.pieceMap}
			bind:transcoding={session.transcoding}
			onTranscodingChange={(enabled, onlyAudio) =>
				session.toggleTranscoding(enabled, onlyAudio, playerTime)}
			subtitleTracks={session.subtitleTracks}
			loadingSubtitles={session.loadingSubtitles}
			activeTrackUrl={session.activeTrackUrl}
			onClose={close}
			onSubtitleSelect={(t) => session.selectSubtitleTrack(t)}
			onSubtitleOff={() => session.disableSubtitles()}
			backdrop={playerBackdrop}
			{externalUrl}
			onReveal={() => api.streams.reveal(infoHash as string, fileIdx)}
			{startTime}
			bind:this={playerRef}
			bind:currentTime={playerTime}
			bind:paused={playerPaused}
			bind:volume={playerVolume}
			bind:duration={playerDuration}
			bind:buffered={playerBuffered}
			bind:subtitleOffset={playerSubtitleOffset}
			bind:loading={playerLoading}
			tvMode={remote.mode === "tv"}
			autoplay
		/>
	{:else}
		<VideoPlayer
			src=""
			title={playerTitle}
			topline={playerTopline}
			titleImage={item?.logo_path
				? imageUrl(item.logo_path, "original")
				: undefined}
			backdrop={playerBackdrop}
			{externalUrl}
			tvMode={remote.mode === "tv"}
			onClose={close}
		/>
	{/if}
</div>

<style>
	.player-page {
		position: fixed;
		inset: 0;
		z-index: 100;
		background: #000;
		overflow: hidden;
	}

	.error-overlay {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		height: 100%;
		color: white;
		gap: 1rem;
	}

	.error-overlay button {
		padding: 0.5rem 1.5rem;
		background: rgba(255, 255, 255, 0.1);
		border: 1px solid rgba(255, 255, 255, 0.2);
		color: white;
		border-radius: 8px;
		cursor: pointer;
	}
</style>
