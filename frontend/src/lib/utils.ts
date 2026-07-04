import type { MediaType } from "$lib/schema";
import { api } from "$lib/api";

export function imageUrl(path: string, size: string = "original"): string {
	// URL contract lives in the draad `#[raw]` schema; the catch-all carries
	// `{size}{path}` (path already begins with `/`).
	return api.urls.image(`${size}${path}`);
}

export async function getDetails(type: MediaType, id: number) {
	if (type === "movie") return api.media.movieDetails(id);
	return api.media.tvDetails(id);
}

export async function playStream(infoHash: string, fileIdx: number) {
	const res = await api.streams.start(infoHash, fileIdx);
	return { url: res.url, local: res.local };
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
