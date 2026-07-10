// App-global store for downloads, pretranscodings and the live HLS count.
// One shared instance so every consumer (top-bar popover, cache page,
// player) sees the same live view over a single set of socket
// subscriptions. Call `downloadManager.init()` once from the root layout.

import { browser } from "$app/environment";
import { api } from "$lib/api";
import type { Download, Pretranscoding } from "$lib/schema";
import { toast } from "glow";

type LiveDownload = Download & { download_speed_mbps?: number | null };

class DownloadManager {
	downloads = $state<LiveDownload[]>([]);
	// Grouped by parent download id
	pretranscodings = $state<Record<number, Pretranscoding[]>>({});
	liveHlsCount = $state(0);

	activeDownloadsCount = $derived(
		this.downloads.filter(
			(d) => d.status === "Queued" || d.status === "Downloading",
		).length,
	);
	activePretranscodingCount = $derived(
		Object.values(this.pretranscodings)
			.flat()
			.filter((p) => p.status === "Queued" || p.status === "Transcoding").length,
	);

	#started = false;
	#loading = false;
	// Guard against a race between an in-flight refresh() and a Removed event
	// that would otherwise reintroduce a just-deleted row.
	#recentlyRemovedDownloads = new Set<number>();
	#recentlyRemovedPretranscodings = new Set<number>();

	init(): void {
		if (!browser || this.#started) return;
		this.#started = true;
		this.refresh();
		this.#subscribe();
	}

	async refresh(): Promise<void> {
		if (this.#loading) return;
		try {
			this.#loading = true;
			this.#recentlyRemovedDownloads.clear();
			this.#recentlyRemovedPretranscodings.clear();

			const [ds, pretranscodings, lc] = await Promise.all([
				api.downloads.list(),
				api.transcodings.list(),
				api.hls.liveCount(),
			]);
			this.downloads = ds.filter(
				(it) => !this.#recentlyRemovedDownloads.has(it.id),
			);
			this.liveHlsCount = lc;

			const grouped: Record<number, Pretranscoding[]> = {};
			for (const pretranscoding of pretranscodings) {
				if (this.#recentlyRemovedPretranscodings.has(pretranscoding.id)) continue;
				const parent = this.downloads.find((d) => d.id === pretranscoding.download_id);
				if (!parent) continue;
				(grouped[parent.id] ??= []).push(pretranscoding);
			}
			this.pretranscodings = grouped;
		} catch {
			// Silently ignore: a failed hydrate leaves prior state in place.
		} finally {
			this.#loading = false;
			this.#recentlyRemovedDownloads.clear();
			this.#recentlyRemovedPretranscodings.clear();
		}
	}

	activePretranscoding(downloadId: number): Pretranscoding | undefined {
		return this.pretranscodings[downloadId]?.find(
			(pretranscoding) =>
				pretranscoding.status === "Transcoding" ||
				pretranscoding.status === "Queued" ||
				pretranscoding.status === "Paused",
		);
	}

	pretranscodingsForStream(
		infoHash: string,
		fileIdx: number,
	): Pretranscoding[] {
		return Object.values(this.pretranscodings)
			.flat()
			.filter(pretranscoding => pretranscoding.download_info_hash === infoHash && pretranscoding.download_file_idx === fileIdx);
	}

	hasCompletedPretranscoding(
		infoHash: string,
		fileIdx: number,
		onlyAudio: boolean,
		audio_index: number
	): boolean {
		return this.pretranscodingsForStream(infoHash, fileIdx).some(
			(pretranscoding) => pretranscoding.only_audio === onlyAudio
				&& pretranscoding.status === "Completed"
				&& pretranscoding.audio_index === audio_index,
		);
	}

	async pause(id: number): Promise<void> {
		try {
			await api.downloads.pause(id);
			const idx = this.downloads.findIndex((x) => x.id === id);
			if (idx !== -1)
				this.downloads[idx] = { ...this.downloads[idx], status: "Paused" };
		} catch (err: unknown) {
			toast.error(
				`Pause failed: ${err instanceof Error ? err.message : String(err)}`,
			);
		}
	}

	async resume(id: number): Promise<void> {
		try {
			await api.downloads.resume(id);
			const idx = this.downloads.findIndex((x) => x.id === id);
			if (idx !== -1)
				this.downloads[idx] = { ...this.downloads[idx], status: "Queued" };
		} catch (err: unknown) {
			toast.error(
				`Resume failed: ${err instanceof Error ? err.message : String(err)}`,
			);
		}
	}

	async cancel(id: number): Promise<void> {
		try {
			await api.downloads.cancel(id);
			const idx = this.downloads.findIndex((x) => x.id === id);
			if (idx !== -1)
				this.downloads[idx] = { ...this.downloads[idx], status: "Cancelled" };
		} catch (err: unknown) {
			toast.error(
				`Cancel failed: ${err instanceof Error ? err.message : String(err)}`,
			);
		}
	}

	async remove(id: number): Promise<void> {
		try {
			await api.downloads.remove(id);
			this.downloads = this.downloads.filter((x) => x.id !== id);
			delete this.pretranscodings[id];
		} catch (err: unknown) {
			toast.error(
				`Remove failed: ${err instanceof Error ? err.message : String(err)}`,
			);
		}
	}

	async killAllLive(): Promise<void> {
		try {
			await api.hls.stopAll();
		} catch (err: unknown) {
			toast.error(
				`Stop all failed: ${err instanceof Error ? err.message : String(err)}`,
			);
		}
	}

	#subscribe(): void {
		api.downloadsEvents.onProgress((p) => {
			const idx = this.downloads.findIndex((d) => d.id === p.download_id);
			if (idx === -1) {
				this.refresh();
				return;
			}
			this.downloads[idx] = {
				...this.downloads[idx],
				downloaded_bytes: p.downloaded_bytes,
				total_bytes: p.total_bytes,
				download_speed_mbps: p.download_speed_mbps,
				status: p.status,
			};
		});
		api.downloadsEvents.onStatusUpdate(
			(statusUpdate) => {
				const idx = this.downloads.findIndex(
					(d) => d.id === statusUpdate.download_id,
				);
				if (idx === -1) {
					this.refresh();
					return;
				}
				this.downloads[idx] = {
					...this.downloads[idx],
					status: statusUpdate.new_status,
				};
			},
		);
		api.downloadsEvents.onRemoved((id) => {
			this.#recentlyRemovedDownloads.add(id);
			this.downloads = this.downloads.filter((it) => it.id !== id);
			delete this.pretranscodings[id];
		});

		api.transcodingsEvents.onProgress(
			(p) => {
				const list = this.pretranscodings[p.download_id];
				if (!list) {
					// New row: refetch so its metadata lands in state.
					this.refresh();
					return;
				}
				const idx = list.findIndex((x) => x.id === p.pretranscoding_id);
				if (idx === -1) {
					this.refresh();
					return;
				}
				list[idx] = {
					...list[idx],
					transcoded_ms: p.transcoded_ms,
					total_ms: p.total_ms ?? list[idx].total_ms,
					status: p.status,
				};
			},
		);
		api.transcodingsEvents.onStatusUpdate((s) => {
			const list = this.pretranscodings[s.download_id];
			if (!list) {
				this.refresh();
				return;
			}
			const idx = list.findIndex((x) => x.id === s.pretranscoding_id);
			if (idx === -1) {
				this.refresh();
				return;
			}
			list[idx] = { ...list[idx], status: s.new_status };
		});
		api.transcodingsEvents.onRemoved(
			(p) => {
				this.#recentlyRemovedPretranscodings.add(p.pretranscoding_id);
				const list = this.pretranscodings[p.download_id];
				if (!list) return;
				this.pretranscodings[p.download_id] = list.filter(
					(it) => it.id !== p.pretranscoding_id,
				);
			},
		);

		api.hlsEvents.onLiveCount((n) => {
			this.liveHlsCount = n;
		});

	}
}

export const downloadManager = new DownloadManager();
