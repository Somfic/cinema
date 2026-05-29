import { movieDetails, tvDetails, startStream, type MediaType } from './api.gen';

export function imageUrl(path: string, size: string = 'original'): string {
	return `/api/image/${size}${path}`;
}

export async function getDetails(type: MediaType, id: number) {
	if (type === 'movie') return movieDetails(id);
	return tvDetails(id);
}

export async function playStream(infoHash: string, fileIdx: number) {
	const res = await startStream(infoHash, fileIdx);
	return { url: res.data.url, local: res.data.local };
}

export function formatBytes(bytes: number): string {
	if (bytes >= 1_099_511_627_776) return `${(bytes / 1_099_511_627_776).toFixed(2)} TB`;
	if (bytes >= 1_073_741_824) return `${(bytes / 1_073_741_824).toFixed(1)} GB`;
	if (bytes >= 1_048_576) return `${(bytes / 1_048_576).toFixed(0)} MB`;
	if (bytes >= 1024) return `${(bytes / 1024).toFixed(0)} KB`;
	return `${bytes} B`;
}
