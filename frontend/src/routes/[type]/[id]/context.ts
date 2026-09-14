import { getContext, setContext } from "svelte";
import type {
	MediaItem,
	Season,
	Episode,
	WatchHistoryItem,
} from "$lib/schema";

/**
 * What the title's layout owns and its three route children read.
 *
 * The layout holds every piece of state that has to survive a move between
 * info → season → episode: the fetched title, the extracted backdrop palette,
 * the playback session. Those routes are siblings, so passing this down as
 * props is not an option, and re-fetching per route would re-flash the loader
 * and tear down the WebGL context behind the glow.
 */
export type TitleContext = {
	readonly item: MediaItem | null;
	readonly season: Season | null;
	readonly episode: Episode | null;
	readonly resumeEntry: WatchHistoryItem | null;
	readonly loadingStreams: boolean;
	/** True while the player overlay is up, so heroes can pause their trailers. */
	readonly playing: boolean;
	/** Play a movie from its first available stream. */
	playMovie: () => void;
	/** Play the episode the URL currently names, resuming if there is progress. */
	playEpisode: () => void;
	/** Resume whatever the watch history last left unfinished. */
	resume: () => void;
	/** Navigate within the title: no argument goes back to the info hero. */
	go: (season?: number, episode?: number) => void;
	/** The same, without a history entry — for scroll-driven season changes. */
	replace: (season: number) => void;
};

const KEY = Symbol("title");

export function setTitleContext(ctx: TitleContext) {
	setContext(KEY, ctx);
}

export function getTitleContext(): TitleContext {
	return getContext<TitleContext>(KEY);
}
