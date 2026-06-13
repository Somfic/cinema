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
