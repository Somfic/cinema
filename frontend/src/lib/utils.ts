import { api } from "$lib/api";
import type { DownloadStatus, MediaType, PretranscodingStatus } from "./schema";

export function imageUrl(path: string, size: string = "original"): string {
	// URL contract lives in the draad `#[raw]` schema; the catch-all carries
	// `{size}{path}` (path already begins with `/`).
	return api.urls.image(`${size}${path}`);
}

export async function getDetails(type: MediaType, id: number) {
	if (type === "movie") return api.media.movieDetails(id);
	return api.media.tvDetails(id);
}

export function formatBytes(bytes: number): string {
	if (bytes >= 1_099_511_627_776) return `${(bytes / 1_099_511_627_776).toFixed(2)} TB`;
	if (bytes >= 1_073_741_824) return `${(bytes / 1_073_741_824).toFixed(1)} GB`;
	if (bytes >= 1_048_576) return `${(bytes / 1_048_576).toFixed(0)} MB`;
	if (bytes >= 1024) return `${(bytes / 1024).toFixed(0)} KB`;
	return `${bytes} B`;
}

export function round(n: number, precision: number = 0) {
	return Math.round((n + Number.EPSILON) * 10 ** precision) / (10 ** precision);
}

export function progress(download: { total_bytes: number | null, downloaded_bytes: number }, precision: number = 0) {
	if (!download.total_bytes) return null;
	return Math.min(100, round((download.downloaded_bytes / download.total_bytes) * 100, precision));
}

export const DOWNLOAD_STATUS_LABEL: Record<DownloadStatus, string> = {
	Queued: "Queued",
	Downloading: "Downloading",
	Paused: "Paused",
	Completed: "Downloaded",
	Failed: "Failed",
	Cancelled: "Cancelled",
};

export const PRETRANSCODING_STATUS_LABEL: Record<PretranscodingStatus, string> = {
	Queued: "Queued",
	Transcoding: "Transcoding",
	Paused: "Paused",
	Completed: "Cached",
	Failed: "Failed",
	Cancelled: "Cancelled",
};

export function pretranscodePercent(pt: {
	transcoded_ms: number;
	total_ms: number | null;
}): number | null {
	if (!pt.total_ms || pt.total_ms <= 0) return null;
	return Math.min(100, round((pt.transcoded_ms / pt.total_ms) * 100, 0));
}

// The backdrop color extractor emits colors as `"r, g, b"` strings (used directly
// in CSS `rgb()`). `Glow` needs hex stops, so convert here.
export function rgbToHex(rgb: string, scale = 1): string {
	const parts = rgb.split(",").map((s) => Number(s.trim()));
	const [r, g, b] = parts.map((n) =>
		Math.max(0, Math.min(255, Math.round((Number.isFinite(n) ? n : 0) * scale))),
	);
	const hex = (n: number) => n.toString(16).padStart(2, "0");
	return `#${hex(r)}${hex(g)}${hex(b)}`;
}
