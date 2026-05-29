import { api, type MediaType } from "$lib/schema";

export function imageUrl(path: string, size: string = "original"): string {
	return `/api/image/${size}${path}`;
}

export async function getDetails(type: MediaType, id: number) {
	if (type === "movie") return api.media.movieDetails(id);
	return api.media.tvDetails(id);
}

export async function playStream(infoHash: string, fileIdx: number) {
	const res = await api.streams.start(infoHash, fileIdx);
	return { url: res.url, local: res.local };
}
