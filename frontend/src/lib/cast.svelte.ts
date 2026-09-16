// Google Cast (Chromecast) sender layer. Loads the Cast framework SDK on
// demand, owns the single `CastContext`, and mirrors the receiver's playback
// state into reactive fields so the existing player UI can drive a Chromecast
// the same way it drives the local <video>.
//
// Only Chromium-based browsers ship the Cast SDK; everywhere else `supported`
// stays false and no cast affordance is shown.
//
// Playback contract: casting always runs off an HLS session (`/api/hls/...`),
// because a Chromecast can't decode most torrent containers/codecs directly.
// The play route enforces that before calling `load()`.

import { browser } from "$app/environment";

const SDK_SRC =
	"https://www.gstatic.com/cv/js/sender/v1/cast_sender.js?loadCastFramework=1";

/** A sideloaded caption track. `url` must be absolute and CORS-readable. */
export interface CastTextTrack {
	id: number;
	url: string;
	label: string;
	language: string;
}

export interface CastLoadRequest {
	/** Absolute media URL the receiver fetches itself. */
	url: string;
	contentType: string;
	title?: string;
	subtitle?: string;
	currentTime?: number;
	tracks?: CastTextTrack[];
	activeTrackId?: number | null;
}

/* eslint-disable @typescript-eslint/no-explicit-any */
type Any = any;

function sdk(): { chrome: Any; cast: Any } | null {
	const w = window as Any;
	if (!w.chrome?.cast || !w.cast?.framework) return null;
	return { chrome: w.chrome, cast: w.cast };
}

/**
 * Cast sender singleton. `init()` is idempotent and safe to call from every
 * component that wants casting; everything else no-ops until the SDK is up.
 */
class CastController {
	/** The Cast SDK loaded and initialised (Chromium only). */
	supported = $state(false);
	/** At least one receiver is on the network — gates the cast button. */
	deviceAvailable = $state(false);
	connected = $state(false);
	connecting = $state(false);
	deviceName = $state<string | null>(null);

	/** Receiver playback state, mirrored from the SDK's RemotePlayer. */
	mediaLoaded = $state(false);
	currentTime = $state(0);
	duration = $state(0);
	paused = $state(true);
	buffering = $state(false);
	volume = $state(1);
	muted = $state(false);
	error = $state<string | null>(null);
	/** Media URL currently handed to the receiver — the first thing to check
	 *  when a cast session connects but never starts playing. */
	loadedUrl = $state<string | null>(null);
	/** Receiver-side state: "IDLE" | "PLAYING" | "PAUSED" | "BUFFERING". */
	playerState = $state<string | null>(null);
	/** True while the receiver is showing a still poster rather than the
	 *  actual stream — see `loadPoster`. */
	showingPoster = $state(false);

	#initStarted = false;
	#player: Any = null;
	#controller: Any = null;
	/** Ids of the caption tracks on the currently loaded media. */
	#activeTrackIds: number[] = [];

	init(): void {
		if (!browser || this.#initStarted) return;
		this.#initStarted = true;

		// The SDK calls this global once it has decided whether casting is
		// available in this browser; it may already be loaded (client-side
		// navigation back into a player), in which case wire up immediately.
		(window as Any).__onGCastApiAvailable = (isAvailable: boolean) => {
			if (isAvailable) this.#setup();
		};
		if (sdk()) {
			this.#setup();
			return;
		}

		const script = document.createElement("script");
		script.src = SDK_SRC;
		script.async = true;
		document.head.appendChild(script);
	}

	#setup(): void {
		const s = sdk();
		if (!s || this.supported) return;
		const { chrome, cast } = s;

		const context = cast.framework.CastContext.getInstance();
		context.setOptions({
			receiverApplicationId: chrome.cast.media.DEFAULT_MEDIA_RECEIVER_APP_ID,
			// Rejoin a session this origin started (e.g. after a page reload)
			// instead of stranding a playing Chromecast.
			autoJoinPolicy: chrome.cast.AutoJoinPolicy.ORIGIN_SCOPED,
		});

		context.addEventListener(
			cast.framework.CastContextEventType.CAST_STATE_CHANGED,
			(e: Any) => this.#applyCastState(e.castState),
		);
		this.#applyCastState(context.getCastState());

		this.#player = new cast.framework.RemotePlayer();
		this.#controller = new cast.framework.RemotePlayerController(this.#player);
		this.#controller.addEventListener(
			cast.framework.RemotePlayerEventType.ANY_CHANGE,
			() => this.#sync(),
		);

		this.supported = true;
		this.#sync();
	}

	#applyCastState(state: string): void {
		const s = sdk();
		if (!s) return;
		const CastState = s.cast.framework.CastState;
		this.deviceAvailable = state !== CastState.NO_DEVICES_AVAILABLE;
		this.connecting = state === CastState.CONNECTING;
		this.connected = state === CastState.CONNECTED;
		if (!this.connected) {
			this.deviceName = null;
			this.mediaLoaded = false;
		} else {
			const session =
				s.cast.framework.CastContext.getInstance().getCurrentSession();
			this.deviceName =
				session?.getCastDevice?.()?.friendlyName ?? this.deviceName;
		}
	}

	#sync(): void {
		const p = this.#player;
		if (!p) return;
		// A loaded poster is media as far as the SDK is concerned, but not as
		// far as playback is concerned — the player still counts as loading.
		this.mediaLoaded = !!p.isMediaLoaded && !this.showingPoster;
		this.currentTime = p.currentTime ?? 0;
		this.duration = p.duration ?? 0;
		this.paused = !!p.isPaused;
		this.buffering = p.playerState === "BUFFERING";
		this.playerState = p.playerState ?? null;
		this.volume = p.volumeLevel ?? 1;
		this.muted = !!p.isMuted;
		if (p.isConnected !== undefined) this.connected = !!p.isConnected;
	}

	// ── Session ──

	/** Opens the browser's device picker and connects. */
	async requestSession(): Promise<void> {
		const s = sdk();
		if (!s) return;
		this.error = null;
		try {
			await s.cast.framework.CastContext.getInstance().requestSession();
		} catch (e: unknown) {
			// "cancel" is the user dismissing the picker — not an error.
			const code = (e as Any)?.code ?? e;
			if (code !== "cancel") this.error = String(code);
		}
	}

	/** Disconnects and stops playback on the receiver. */
	endSession(): void {
		const s = sdk();
		s?.cast.framework.CastContext.getInstance()
			.getCurrentSession()
			?.endSession(true);
		this.mediaLoaded = false;
	}

	// ── Media ──

	/**
	 * Shows a still image on the receiver. The default receiver only paints
	 * what it has been asked to load, so between connecting and the stream
	 * being ready there is nothing on the TV but its ambient backdrop. Loading
	 * the artwork as a photo fills that gap, and the real `load()` replaces it.
	 */
	async loadPoster(url: string, title?: string): Promise<void> {
		const s = sdk();
		if (!s) return;
		const { chrome } = s;
		const session = s.cast.framework.CastContext.getInstance().getCurrentSession();
		if (!session) return;

		const info = new chrome.cast.media.MediaInfo(url, "image/jpeg");
		info.streamType = chrome.cast.media.StreamType.NONE;
		const metadata = new chrome.cast.media.PhotoMediaMetadata();
		if (title) metadata.title = title;
		info.metadata = metadata;

		const request = new chrome.cast.media.LoadRequest(info);
		request.autoplay = true;
		this.showingPoster = true;
		try {
			await session.loadMedia(request);
		} catch (e: unknown) {
			// Cosmetic only — never let a failed poster block playback.
			this.showingPoster = false;
			console.warn("[cast] poster failed", e);
		}
	}

	/**
	 * Loads media on the receiver, replacing whatever it was playing.
	 * Resolves once the receiver has accepted the load request.
	 */
	async load(request: CastLoadRequest): Promise<void> {
		const s = sdk();
		if (!s) return;
		const { chrome } = s;
		const session =
			s.cast.framework.CastContext.getInstance().getCurrentSession();
		if (!session) return;

		const info = new chrome.cast.media.MediaInfo(
			request.url,
			request.contentType,
		);
		info.streamType = chrome.cast.media.StreamType.BUFFERED;

		// Deliberately no `images` here. The default receiver paints metadata
		// artwork as the background *behind* the video, so it shows through the
		// letterbox bars of anything that isn't exactly the panel's aspect ratio.
		// Artwork belongs to `loadPoster`, which covers the gap before playback
		// starts; once the stream is up the bars should just be black.
		const metadata = new chrome.cast.media.GenericMediaMetadata();
		if (request.title) metadata.title = request.title;
		if (request.subtitle) metadata.subtitle = request.subtitle;
		info.metadata = metadata;

		const tracks = request.tracks ?? [];
		info.tracks = tracks.map((t) => {
			const track = new chrome.cast.media.Track(
				t.id,
				chrome.cast.media.TrackType.TEXT,
			);
			track.trackContentId = t.url;
			track.trackContentType = "text/vtt";
			track.subtype = chrome.cast.media.TextTrackType.SUBTITLES;
			track.name = t.label;
			track.language = t.language;
			return track;
		});
		this.#activeTrackIds = tracks.map((t) => t.id);

		const style = new chrome.cast.media.TextTrackStyle();
		style.backgroundColor = "#00000000";
		style.foregroundColor = "#FFFFFFFF";
		style.edgeType = chrome.cast.media.TextTrackEdgeType.OUTLINE;
		style.edgeColor = "#000000FF";
		info.textTrackStyle = style;

		const load = new chrome.cast.media.LoadRequest(info);
		load.autoplay = true;
		load.currentTime = request.currentTime ?? 0;
		if (request.activeTrackId != null) {
			load.activeTrackIds = [request.activeTrackId];
		}

		this.error = null;
		this.showingPoster = false;
		this.loadedUrl = request.url;
		console.info("[cast] loading", {
			url: request.url,
			contentType: request.contentType,
			currentTime: request.currentTime,
			tracks: tracks.map((t) => t.url),
		});
		try {
			await session.loadMedia(load);
		} catch (e: unknown) {
			// Cast rejects with an error code string ("LOAD_FAILED",
			// "LOAD_CANCELLED", …); the receiver never says more than that, so
			// the URL logged above is what you check next.
			const reason = String((e as Any)?.details?.reason ?? (e as Any) ?? "");
			this.error = `Cast failed: ${reason}`;
			console.error("[cast] load failed", reason, "for", request.url, e);
			throw e;
		}

		// A load can be accepted and then fail on the receiver (unreachable
		// segments, a codec it can't decode) — that surfaces only as the media
		// going IDLE with a reason.
		const media = session.getMediaSession();
		media?.addUpdateListener(() => {
			const idle = media.idleReason;
			if (media.playerState === "IDLE" && idle && idle !== "FINISHED") {
				this.error = `Receiver stopped: ${idle}`;
				console.error("[cast] receiver idle:", idle, "media:", media.media);
			}
		});
	}

	/** Switches the active caption track, or turns captions off with `null`. */
	setTextTrack(id: number | null): void {
		const s = sdk();
		if (!s) return;
		const media = s.cast.framework.CastContext.getInstance()
			.getCurrentSession()
			?.getMediaSession();
		if (!media) return;
		const request = new s.chrome.cast.media.EditTracksInfoRequest(
			id == null ? [] : [id],
		);
		media.editTracksInfo(
			request,
			() => {},
			() => {},
		);
	}

	/** Whether the receiver knows about this caption track id. */
	hasTextTrack(id: number): boolean {
		return this.#activeTrackIds.includes(id);
	}

	// ── Transport ──

	togglePlay(): void {
		this.#controller?.playOrPause();
	}

	play(): void {
		if (this.paused) this.#controller?.playOrPause();
	}

	pause(): void {
		if (!this.paused) this.#controller?.playOrPause();
	}

	seekTo(time: number): void {
		if (!this.#player || !this.#controller) return;
		this.#player.currentTime = Math.max(0, time);
		this.#controller.seek();
		// The SDK only echoes the new position on its next tick; reflect it
		// immediately so the scrubber doesn't snap back under the cursor.
		this.currentTime = Math.max(0, time);
	}

	seekBy(delta: number): void {
		this.seekTo(this.currentTime + delta);
	}

	setVolume(value: number): void {
		if (!this.#player || !this.#controller) return;
		const v = Math.max(0, Math.min(1, value));
		this.#player.volumeLevel = v;
		this.#controller.setVolumeLevel();
		this.volume = v;
	}

	toggleMute(): void {
		this.#controller?.muteOrUnmute();
	}
}

export const cast = new CastController();

/** Resolves a same-origin path to an absolute URL the receiver can fetch. */
export function castAbsoluteUrl(path: string): string {
	if (/^https?:\/\//.test(path)) return path;
	return new URL(path, window.location.origin).href;
}

/**
 * True when this page is served from an address a Chromecast on the LAN can
 * reach back into. `localhost` resolves to the Chromecast itself, so a dev
 * server on localhost can hand off a URL the receiver can never load.
 */
export function castReachableOrigin(): boolean {
	const host = window.location.hostname;
	return host !== "localhost" && host !== "127.0.0.1" && host !== "::1";
}
