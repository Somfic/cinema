// What a playback client can decode, for the server to plan around: it plays
// a file as is when the client supports it, and otherwise re-encodes only the
// streams the client can't decode.

import type { ClientCapabilities } from "$lib/schema";

/** Codecs this browser's media stack decodes (via MSE, which is what hls.js
 *  feeds) and containers its <video> plays from a plain URL. */
export function browserCapabilities(): ClientCapabilities {
	const w = window as unknown as {
		ManagedMediaSource?: typeof MediaSource;
		MediaSource?: typeof MediaSource;
	};
	const source = w.ManagedMediaSource ?? w.MediaSource;
	const probe = document.createElement("video");
	const mse = (type: string) => !!source?.isTypeSupported?.(type);
	// Safari without MSE plays HLS itself; fall back to asking the element.
	const decodes = (type: string) => mse(type) || probe.canPlayType(type) !== "";

	const video: [string, string][] = [
		["h264", 'video/mp4; codecs="avc1.640028"'],
		["hevc", 'video/mp4; codecs="hvc1.1.6.L150.B0"'],
		["av1", 'video/mp4; codecs="av01.0.08M.08"'],
		["vp9", 'video/mp4; codecs="vp09.00.40.08"'],
	];
	const audio: [string, string][] = [
		["aac", 'audio/mp4; codecs="mp4a.40.2"'],
		["ac3", 'audio/mp4; codecs="ac-3"'],
		["eac3", 'audio/mp4; codecs="ec-3"'],
		["opus", 'audio/mp4; codecs="opus"'],
		["flac", 'audio/mp4; codecs="flac"'],
	];
	const containers: [string, string][] = [
		["mp4", "video/mp4"],
		["webm", "video/webm"],
	];

	return {
		video_codecs: video.filter(([, t]) => decodes(t)).map(([c]) => c),
		audio_codecs: audio.filter(([, t]) => decodes(t)).map(([c]) => c),
		containers: containers
			.filter(([, t]) => probe.canPlayType(t) !== "")
			.map(([c]) => c),
		max_height: null,
	};
}

/** What the Chromecast decodes. The sender SDK can't ask the device, so
 *  beyond the H.264/AAC every receiver plays this comes from settings. */
export function castCapabilities(extra: {
	hevc: boolean;
	dolby: boolean;
	uhd: boolean;
}): ClientCapabilities {
	return {
		video_codecs: ["h264", ...(extra.hevc ? ["hevc"] : [])],
		audio_codecs: ["aac", ...(extra.dolby ? ["ac3", "eac3"] : [])],
		containers: ["mp4", "webm"],
		max_height: extra.uhd ? null : 1080,
	};
}
