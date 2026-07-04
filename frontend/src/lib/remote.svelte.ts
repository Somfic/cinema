// TV-remote layer. Lets a phone pair with a large-screen "TV" client and drive
// playback on it. Everything rides the existing WebSocket: the server keeps a
// presence roster (broadcast on `remote_presence`) and proxies `remote_*`
// topics between clients. This store is the single source of truth on the
// frontend — components read its reactive fields and call its methods.
//
// Topics:
//   remote_presence        — server → all: the full client roster
//   remote_cmd_<tvId>      — phone → TV: transport/navigation commands
//   remote_state_<tvId>    — TV → phone: now-playing state for the remote UI

import { browser } from "$app/environment";
import { goto } from "$app/navigation";
import { toast } from "glow";
import type { ClientPresence, Stream } from "$lib/schema";
import { announce, conn, listen, onOpen, publish, type UnlistenFn } from "$lib/schema/rpc";

export type Mode = "browser" | "tv" | "remote";
export type DeviceKind = "phone" | "tablet" | "desktop";

/** A command sent phone → TV. The TV applies it to its active player or routes
 *  navigation commands (`play_media`, `back`) itself. */
export interface RemoteCommand {
	kind:
	| "play_pause"
	| "play"
	| "pause"
	| "seek"
	| "seek_by"
	| "volume"
	| "mute"
	| "fullscreen"
	| "back"
	| "navigate"
	| "play_media"
	| "disconnect"
	| "select_stream"
	| "select_audio"
	| "select_subtitle"
	| "subtitle_off"
	| "set_subtitle_offset"
	| "set_transcoding";
	seconds?: number;
	volume?: number;
	href?: string;
	hash?: string;
	audioId?: number;
	subtitleUrl?: string;
	offset?: number;
	enabled?: boolean;
	onlyAudio?: boolean;
}

export interface RemoteAudioTrack {
	id: number;
	name: string;
	lang?: string;
}

export interface RemoteSubtitleTrack {
	id: string;
	language: string;
	url: string;
}

export interface RemoteStreamStats {
	progress_bytes: number;
	total_bytes: number;
	download_speed_mbps: number;
	peers: number;
	finished: boolean;
}

/** Now-playing snapshot the TV publishes so the remote can mirror it — enough
 *  to reproduce the full player control bar on the phone. */
export interface TvState {
	title?: string;
	/** Logo/title-treatment image URL, shown in place of the text title. */
	titleImage?: string;
	topline?: string;
	poster?: string;
	currentTime: number;
	duration: number;
	buffered?: number;
	loading?: boolean;
	paused: boolean;
	volume: number;
	muted: boolean;
	/** Whether a video is actually loaded (vs. an idle TV on a menu). */
	playing: boolean;
	streams?: Stream[];
	activeStreamHash?: string;
	audioTracks?: RemoteAudioTrack[];
	activeAudioTrack?: number;
	subtitleTracks?: RemoteSubtitleTrack[];
	activeTrackUrl?: string;
	subtitlesActive?: boolean;
	subtitleOffset?: number;
	transcoding?: { enabled: boolean; onlyAudio: boolean };
	streamStats?: RemoteStreamStats | null;
	pieceMap?: number[];
}

/** Imperative handle the active player registers so commands can reach it. */
export interface PlayerControls {
	play(): void;
	pause(): void;
	togglePlay(): void;
	seekTo(time: number): void;
	seekBy(delta: number): void;
	setVolume(value: number): void;
	toggleMute(): void;
	toggleFullscreen(): void;
	selectStream(hash: string): void;
	selectAudio(id: number): void;
	selectSubtitle(url: string): void;
	subtitleOff(): void;
	setSubtitleOffset(delta: number): void;
	setTranscoding(enabled: boolean, onlyAudio: boolean): void;
	exit(): void;
}

/** Everything needed to reproduce a playback on the TV via the play route. */
export interface CastTarget {
	type: string;
	id: number;
	infoHash: string;
	fileIdx: number;
	season?: number | null;
	episode?: number | null;
}

const LABEL_KEY = "remote.label";
const MODE_KEY = "remote.mode";
const PAIRED_KEY = "remote.paired";

// Identity (`selfId`) is now the transport's server-assigned `client_id`
// (see `conn.id`): per-window, persisted by draad in sessionStorage, and
// stable across reconnects. It's `null` until the first WS welcome, so the
// mode-restore + announce below wait for `onOpen`.

/** Best-effort friendly device name from the user agent, e.g. "Chrome · macOS". */
function defaultLabel(): string {
	if (!browser) return "Device";
	const ua = navigator.userAgent;
	const browserName =
		/Edg\//.test(ua) ? "Edge"
			: /OPR\//.test(ua) ? "Opera"
				: /Firefox\//.test(ua) ? "Firefox"
					: /Chrome\//.test(ua) ? "Chrome"
						: /Safari\//.test(ua) ? "Safari"
							: "Browser";
	const os =
		/iPhone|iPad|iPod/.test(ua) ? "iOS"
			: /Android/.test(ua) ? "Android"
				: /Mac OS X/.test(ua) ? "macOS"
					: /Windows/.test(ua) ? "Windows"
						: /Linux/.test(ua) ? "Linux"
							: "";
	return os ? `${browserName} · ${os}` : browserName;
}

class RemoteStore {
	selfId = $state("");
	selfLabel = $state("");
	width = $state(browser ? window.innerWidth : 1280);
	mode = $state<Mode>("browser");
	peers = $state<ClientPresence[]>([]);
	pairedId = $state<string | null>(null);
	/** Latest now-playing state from the paired TV (remote side only). */
	tvState = $state<TvState | null>(null);
	/** Whether the bottom "connect to a TV" sheet is open. */
	castDrawerOpen = $state(false);

	// Non-reactive internals.
	_player: PlayerControls | null = null;
	_cmdUnsub: UnlistenFn | null = null;
	_stateUnsub: UnlistenFn | null = null;
	_started = false;
	_resizeTimer: ReturnType<typeof setTimeout> | undefined;
	_lastNav: string | null = null;

	kind = $derived<DeviceKind>(
		this.width < 768 ? "phone" : this.width < 1024 ? "tablet" : "desktop",
	);

	selfPresence = $derived<ClientPresence>({
		id: this.selfId,
		label: this.selfLabel,
		kind: this.kind,
		width: this.width,
		mode: this.mode,
		paired_to: this.pairedId,
	});

	/** Peers larger than this client. Single-user assumption: the smaller of two
	 *  clients is the remote, the larger is the TV — so a client offers to
	 *  control any peer bigger than itself. Ties break by id so two equal-width
	 *  windows still resolve to one TV + one remote (handy for testing). */
	castTargets = $derived(
		this.peers.filter(
			(p) =>
				p.id !== this.selfId &&
				p.mode !== "remote" &&
				(p.width > this.width ||
					(p.width === this.width && p.id > this.selfId)),
		),
	);

	/** The phone currently controlling this TV (TV side), if any. */
	controller = $derived(
		this.peers.find((p) => p.paired_to === this.selfId && p.mode === "remote") ??
		null,
	);

	/** The TV this remote is paired with (remote side). */
	pairedPeer = $derived(
		this.pairedId ? (this.peers.find((p) => p.id === this.pairedId) ?? null) : null,
	);

	/** Connect, register, and wire presence. Idempotent; call once from layout. */
	init() {
		if (!browser || this._started) return;
		this._started = true;

		const ss = window.sessionStorage;
		this.selfLabel = ss.getItem(LABEL_KEY) ?? defaultLabel();
		ss.setItem(LABEL_KEY, this.selfLabel);

		const savedMode = ss.getItem(MODE_KEY) as Mode | null;
		const savedPaired = ss.getItem(PAIRED_KEY);

		window.addEventListener("resize", () => {
			this.width = window.innerWidth;
			// Re-announce after resize settles so peers re-evaluate who's smaller.
			clearTimeout(this._resizeTimer);
			this._resizeTimer = setTimeout(() => this._announce(), 200);
		});

		// Subscribing opens the socket; the server then assigns our id and
		// `onOpen` fires (after the welcome frame), so `conn.id` is set below.
		listen<ClientPresence[]>("remote_presence", (roster) =>
			this._onPresence(roster),
		);

		// Everything keyed on `selfId` (mode restore, topic subscriptions,
		// announce) waits for the server-assigned id. Restore the saved role
		// once, then re-announce on every (re)connect.
		let restored = false;
		onOpen(() => {
			if (conn.id) this.selfId = conn.id;
			if (!restored) {
				restored = true;
				if (savedMode === "tv") {
					this.mode = "tv";
					this._startTv();
				} else if (savedMode === "remote" && savedPaired) {
					this.mode = "remote";
					this.pairedId = savedPaired;
					this._startRemote(savedPaired);
				}
			}
			this._announce();
		});
	}

	private _persist() {
		if (!browser) return;
		const ss = window.sessionStorage;
		ss.setItem(MODE_KEY, this.mode);
		if (this.pairedId) ss.setItem(PAIRED_KEY, this.pairedId);
		else ss.removeItem(PAIRED_KEY);
	}

	private _announce() {
		announce(this.selfPresence, "register");
	}

	private _onPresence(roster: ClientPresence[]) {
		this.peers = roster;
		// A phone paired to us → automatically flip this large screen into TV
		// mode (the user's described flow). We never auto-leave; the indicator
		// just hides when the controller disconnects.
		if (this.mode === "browser") {
			const controller = roster.find(
				(p) => p.paired_to === this.selfId && p.mode === "remote",
			);
			if (controller) this.becomeTv();
		}
	}

	// ── Remote (phone) side ──

	openCast() {
		this.castDrawerOpen = true;
	}

	closeCast() {
		this.castDrawerOpen = false;
	}

	becomeRemote(targetId: string) {
		this.mode = "remote";
		this.pairedId = targetId;
		this._lastNav = null;
		this.castDrawerOpen = false;
		this._persist();
		this._announce();
		this._startRemote(targetId);
		const label = this.peers.find((p) => p.id === targetId)?.label ?? "TV";
		toast.success(`Connected to ${label}`);
	}

	/** Deliberate disconnect: tell the TV to drop back to browser mode, then
	 *  detach. Distinct from a lost websocket, which leaves the TV in TV mode. */
	disconnect() {
		if (this.pairedId) this.send({ kind: "disconnect" });
		this.leaveRemote();
	}

	leaveRemote() {
		this._stateUnsub?.();
		this._stateUnsub = null;
		this.tvState = null;
		this.pairedId = null;
		this.mode = "browser";
		this._persist();
		this._announce();
	}

	private _startRemote(targetId: string) {
		this._stateUnsub?.();
		this.tvState = null;
		this._stateUnsub = listen<TvState>(
			`remote_state_${targetId}`,
			(s) => (this.tvState = s),
		);
	}

	/** Send a command to the paired TV. */
	send(cmd: RemoteCommand) {
		if (!this.pairedId) return;
		publish(`remote_cmd_${this.pairedId}`, cmd);
	}

	/** Mirror the phone's current page onto the TV (remote side, deduped). */
	sendNavigate(path: string) {
		if (this.mode !== "remote" || !this.pairedId) return;
		if (path === this._lastNav) return;
		this._lastNav = path;
		this.send({ kind: "navigate", href: path });
	}

	/** Launch a playback on the paired TV instead of playing locally, and jump
	 *  the phone to the full-screen controls. */
	cast(target: CastTarget) {
		let href = `/${target.type}/${target.id}/play/${target.infoHash}/${target.fileIdx}`;
		const qs = new URLSearchParams();
		if (target.season != null) qs.set("s", String(target.season));
		if (target.episode != null) qs.set("e", String(target.episode));
		const q = qs.toString();
		if (q) href += `?${q}`;
		this.send({ kind: "play_media", href });
		this.castDrawerOpen = false;
		goto("/remote");
	}

	// ── TV side ──

	becomeTv() {
		this.mode = "tv";
		this.pairedId = null;
		this._persist();
		this._announce();
		this._startTv();
	}

	leaveTv() {
		this._cmdUnsub?.();
		this._cmdUnsub = null;
		this.mode = "browser";
		this._persist();
		this._announce();
	}

	private _startTv() {
		this._cmdUnsub?.();
		this._cmdUnsub = listen<RemoteCommand>(
			`remote_cmd_${this.selfId}`,
			(c) => this._handleCommand(c),
		);
	}

	private _handleCommand(cmd: RemoteCommand) {
		if (cmd.kind === "play_media") {
			if (cmd.href) goto(cmd.href);
			return;
		}
		if (cmd.kind === "navigate") {
			// While the TV is in the player, ignore browse-mirroring — a
			// reconnecting or browsing phone must not navigate the TV off what's
			// playing. Gate on the URL (robust even if the player instance is
			// momentarily torn down while buffering/transcoding). The phone hooks
			// back into the session via its own auto-nav to /remote.
			if (this._player || window.location.pathname.includes("/play/")) return;
			// Mirror the phone's browsing. Ignore no-op navigations so the page
			// doesn't reload while the remote idles on the same screen. Use goto
			// (not replaceState) so the target page re-reads season/episode from
			// the URL — replaceState wouldn't re-run that sync.
			const here = window.location.pathname + window.location.search;
			if (!cmd.href || cmd.href === here) return;
			goto(cmd.href, { replaceState: true, noScroll: true, keepFocus: true });
			return;
		}
		if (cmd.kind === "back") {
			// If a player is active, exit it explicitly (proper cleanup + return
			// to the title page); otherwise fall back to browser history.
			if (this._player) this._player.exit();
			else history.back();
			return;
		}
		if (cmd.kind === "disconnect") {
			// The remote deliberately ended the session — return to browser mode.
			// (A dropped websocket does NOT send this, so the TV stays put.)
			this.leaveTv();
			return;
		}
		const p = this._player;
		if (!p) return;
		switch (cmd.kind) {
			case "play_pause":
				p.togglePlay();
				break;
			case "play":
				p.play();
				break;
			case "pause":
				p.pause();
				break;
			case "seek":
				if (cmd.seconds != null) p.seekTo(cmd.seconds);
				break;
			case "seek_by":
				if (cmd.seconds != null) p.seekBy(cmd.seconds);
				break;
			case "volume":
				if (cmd.volume != null) p.setVolume(cmd.volume);
				break;
			case "mute":
				p.toggleMute();
				break;
			case "fullscreen":
				p.toggleFullscreen();
				break;
			case "select_stream":
				if (cmd.hash) p.selectStream(cmd.hash);
				break;
			case "select_audio":
				if (cmd.audioId != null) p.selectAudio(cmd.audioId);
				break;
			case "select_subtitle":
				if (cmd.subtitleUrl) p.selectSubtitle(cmd.subtitleUrl);
				break;
			case "subtitle_off":
				p.subtitleOff();
				break;
			case "set_subtitle_offset":
				if (cmd.offset != null) p.setSubtitleOffset(cmd.offset);
				break;
			case "set_transcoding":
				p.setTranscoding(!!cmd.enabled, !!cmd.onlyAudio);
				break;
		}
	}

	/** The active player registers here so TV-mode commands can drive it. */
	registerPlayer(controls: PlayerControls) {
		this._player = controls;
	}

	clearPlayer() {
		if (this._player) this._player = null;
	}

	/** Publish now-playing state to the paired remote (TV side). */
	publishState(state: TvState) {
		if (this.mode !== "tv") return;
		publish(`remote_state_${this.selfId}`, state);
	}
}

export const remote = new RemoteStore();
