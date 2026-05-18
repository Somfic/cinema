<script lang="ts">
	import {
		search,
		watchHistory as fetchWatchHistory,
		getCollection as fetchCollection,
		listCollectionDefs,
		addToCollection,
		removeFromCollection,
		reorderCollection,
		type SearchResult,
		type WatchHistoryItem,
		type CollectionItem,
		type CollectionDef,
	} from "$lib/api.gen";
	import { imageUrl } from "$lib/utils";
	import { Heading, Input, Text, Icon, Modal, Button } from "glow";
	import { sortable } from "glow";
	import PosterCard from "$lib/components/PosterCard.svelte";
	import { page } from "$app/state";
	import { replaceState } from "$app/navigation";
	import { setFocusSearch } from "$lib/topbar.svelte";
	import { onDestroy } from "svelte";
	import { browser } from "$app/environment";
	import { fade } from "svelte/transition";

	let searchInput: HTMLInputElement | undefined;

	setFocusSearch(() => searchInput?.focus());
	onDestroy(() => setFocusSearch(null));

	let query = $state(page.url.searchParams.get("q") ?? "");
	let results = $state<SearchResult[]>([]);
	let loading = $state(false);
	let timeout: ReturnType<typeof setTimeout>;

	const CONTINUE_SLUG = "continue";

	let defs = $state<CollectionDef[]>([]);
	let historyItems = $state<WatchHistoryItem[]>([]);
	let collectionItems = $state<Record<string, CollectionItem[]>>({});

	type Row =
		| { def: CollectionDef; kind: "history" }
		| { def: CollectionDef; kind: "collection" };

	const rows = $derived<Row[]>(
		defs
			.filter((d) => !d.hidden)
			.map((d) =>
				d.slug === CONTINUE_SLUG
					? ({ def: d, kind: "history" } as const)
					: ({ def: d, kind: "collection" } as const),
			)
			// History row only shows with watch progress; collections always
			// show (even empty) so the "+" add placeholder is reachable.
			.filter((r) => r.kind !== "history" || historyItems.length > 0),
	);

	// Load browse data on mount
	$effect(() => {
		fetchWatchHistory()
			.then((res) => {
				historyItems = res.data;
			})
			.catch(() => {});
		listCollectionDefs()
			.then(async (res) => {
				defs = res.data;
				const entries = await Promise.all(
					res.data
						.filter((d) => d.slug !== CONTINUE_SLUG)
						.map(async (d) => {
							try {
								const r = await fetchCollection(d.slug);
								return [d.slug, r.data] as const;
							} catch {
								return [d.slug, [] as CollectionItem[]] as const;
							}
						}),
				);
				collectionItems = Object.fromEntries(entries);
			})
			.catch(() => {});
	});

	// ── Add-to-collection dialog ──
	let addOpen = $state(false);
	let addTarget = $state<CollectionDef | null>(null);
	let addQuery = $state("");
	let addResults = $state<SearchResult[]>([]);
	let addSearching = $state(false);
	let addItems = $state<CollectionItem[]>([]);
	let addTimer: ReturnType<typeof setTimeout>;

	function isAdded(mediaType: string, tmdbId: number): boolean {
		return addItems.some(
			(i) => i.media_type === mediaType && i.tmdb_id === tmdbId,
		);
	}

	async function openAdd(def: CollectionDef) {
		addTarget = def;
		addQuery = "";
		addResults = [];
		addItems = collectionItems[def.slug] ?? [];
		addOpen = true;
		const res = await fetchCollection(def.slug).catch(() => null);
		if (res && addTarget?.slug === def.slug) addItems = res.data;
	}

	function onAddSearch(v: string) {
		addQuery = v;
		clearTimeout(addTimer);
		if (addQuery.length < 2) {
			addResults = [];
			return;
		}
		addSearching = true;
		addTimer = setTimeout(async () => {
			try {
				const res = await search({ q: addQuery });
				addResults = res.data;
			} finally {
				addSearching = false;
			}
		}, 300);
	}

	async function refreshTarget(slug: string) {
		const res = await fetchCollection(slug).catch(() => null);
		if (res) {
			collectionItems = { ...collectionItems, [slug]: res.data };
			if (addTarget?.slug === slug) addItems = res.data;
		}
	}

	async function toggleResult(r: SearchResult) {
		if (!addTarget) return;
		const slug = addTarget.slug;
		if (isAdded(r.media_type, r.id)) {
			await removeFromCollection(slug, r.media_type, r.id).catch(
				() => {},
			);
		} else {
			await addToCollection({
				collection: slug,
				media_type: r.media_type,
				tmdb_id: r.id,
				title: r.title,
				poster_path: r.poster_path ?? undefined,
			}).catch(() => {});
		}
		await refreshTarget(slug);
	}

	async function removeAddedItem(item: CollectionItem) {
		if (!addTarget) return;
		const slug = addTarget.slug;
		await removeFromCollection(
			slug,
			item.media_type,
			item.tmdb_id,
		).catch(() => {});
		await refreshTarget(slug);
	}

	async function persistAddOrder() {
		if (!addTarget) return;
		const slug = addTarget.slug;
		// sortable mutates addItems in place; reflect the new order on the
		// home rows immediately, then persist.
		collectionItems = { ...collectionItems, [slug]: [...addItems] };
		await reorderCollection(slug, {
			items: addItems.map((i) => ({
				media_type: i.media_type,
				tmdb_id: i.tmdb_id,
			})),
		}).catch(() => {});
	}

	const browsing = $derived(query.length < 2 && results.length === 0);

	function updateQueryParam() {
		if (!browser) return;
		const u = new URL(page.url);
		if (query.length >= 2) {
			u.searchParams.set("q", query);
		} else {
			u.searchParams.delete("q");
		}
		replaceState(u, {});
	}

	function onInput() {
		clearTimeout(timeout);
		updateQueryParam();
		if (query.length < 2) {
			results = [];
			return;
		}
		loading = true;
		timeout = setTimeout(async () => {
			try {
				const res = await search({ q: query });
				results = res.data;
			} finally {
				loading = false;
			}
		}, 300);
	}

	// Run initial search if q param is present
	if (browser && query.length >= 2) {
		loading = true;
		search({ q: query }).then((res) => {
			results = res.data;
			loading = false;
		}).catch(() => { loading = false; });
	}
</script>

<svelte:head>
	<title>Cinema</title>
</svelte:head>

<div class="content">
	<Input
		type="text"
		placeholder="Search movies and TV shows..."
		value={query}
		icon={"Search"}
		{loading}
		inputRef={(el) => (searchInput = el)}
		onChange={(v) => {
			query = v;
			onInput();
		}}
	/>

	{#if browsing}
		{#each rows as r (r.def.slug)}
			<section>
				<Heading level={2}>{r.def.title}</Heading>
				<div class="row">
					{#if r.kind === "history"}
						{#each historyItems as item (item.media_type + item.tmdb_id)}
						{@const minLeft = Math.max(0, Math.ceil(
							(item.duration - item.progress) / 60,
						))}
						{@const pct =
							item.duration > 0
								? item.progress / item.duration
								: 0}
						<PosterCard
							expand
							posterPath={item.poster_path}
							mediaType={item.media_type}
							tmdbId={item.tmdb_id}
							progress={pct}
							onclick={() =>
								(window.location.href = `/${item.media_type}/${item.tmdb_id}`)}
						>
							{#snippet bottomLeft()}
								<Text size="xs" variant="muted">
									{item.media_type === "tv" && item.season > 0
										? `S${item.season} E${item.episode} · ${minLeft} min left`
										: `${minLeft} min left`}
								</Text>
							{/snippet}
						</PosterCard>
					{/each}
					{:else}
						{#each collectionItems[r.def.slug] ?? [] as item (item.media_type + item.tmdb_id)}
						<PosterCard
							expand
							posterPath={item.poster_path}
							mediaType={item.media_type}
							tmdbId={item.tmdb_id}
							onclick={() =>
								(window.location.href = `/${item.media_type}/${item.tmdb_id}`)}
						>
							{#snippet bottomLeft()}
								<Text size="xs" variant="muted"
									>{item.title}</Text
								>
							{/snippet}
						</PosterCard>
					{/each}
					<div class="poster">
						<button
							type="button"
							class="add-tile"
							aria-label={`Add to ${r.def.title}`}
							onclick={() => openAdd(r.def)}
						>
							<Icon name="Plus" />
						</button>
					</div>
					{/if}
				</div>
			</section>
		{/each}
	{:else if results.length > 0}
		<div class="grid">
			{#each results as item (item.id + item.media_type)}
				<div transition:fade={{ duration: 150 }}>
				<PosterCard
					posterPath={item.poster_path}
					mediaType={item.media_type}
					tmdbId={item.id}
					onclick={() =>
						(window.location.href = `/${item.media_type}/${item.id}`)}
				>
					{#snippet bottomLeft()}
						<Text size="xs" variant="muted">{item.title}</Text>
					{/snippet}
					{#snippet bottomRight()}
						<Text size="xs" variant="muted">
							{item.release_date
								? `${item.release_date.slice(0, 4)} · ${item.media_type === "movie" ? "Movie" : "TV"}`
								: item.media_type === "movie"
									? "Movie"
									: "TV"}
						</Text>
					{/snippet}
				</PosterCard>
				</div>
			{/each}
		</div>
	{/if}
</div>

<Modal
	bind:open={addOpen}
	title={addTarget ? `Add to ${addTarget.title}` : "Add"}
	size="large"
>
	<div class="add-dialog">
		<Input
			type="text"
			placeholder="Search movies and TV shows..."
			value={addQuery}
			icon={"Search"}
			loading={addSearching}
			onChange={onAddSearch}
		/>
		{#if addResults.length > 0}
			<div class="dlg-group">
				<Text size="sm" variant="muted">Search results</Text>
				<div class="grid">
					{#each addResults as r (r.media_type + r.id)}
						{@const added = isAdded(r.media_type, r.id)}
						<PosterCard
							posterPath={r.poster_path}
							mediaType={r.media_type}
							tmdbId={r.id}
							selected={added}
							onclick={() => toggleResult(r)}
						>
							{#snippet bottomLeft()}
								<Text size="xs" variant="muted">{r.title}</Text>
							{/snippet}
							{#snippet bottomRight()}
								{#if added}
									<Text size="xs" variant="muted">✓ Added</Text>
								{/if}
							{/snippet}
						</PosterCard>
					{/each}
				</div>
			</div>
		{/if}
		{#if addItems.length > 0}
			<div class="dlg-group">
				<Text size="sm" variant="muted">
					{addTarget?.kind === "ordered"
						? "In this collection — drag to reorder"
						: "In this collection"}
				</Text>
				<div
					class="list"
					use:sortable={{
						items: addItems,
						direction: "vertical",
						disabled: addTarget?.kind !== "ordered",
						handle: ".handle",
						onReorder: () => persistAddOrder(),
					}}
				>
					{#each addItems as item (item.media_type + item.tmdb_id)}
						<div class="list-item">
							{#if addTarget?.kind === "ordered"}
								<span class="handle">⠿</span>
							{/if}
							<img
								class="thumb"
								src={item.poster_path
									? imageUrl(item.poster_path, "w92")
									: ""}
								alt=""
							/>
							<Text size="sm">{item.title}</Text>
							<div class="spacer"></div>
							<Button
								icon="X"
								variant="ghost"
								onclick={() => removeAddedItem(item)}
							/>
						</div>
					{/each}
				</div>
			</div>
		{/if}
	</div>
</Modal>

<style>
	.content {
		padding: 2rem;
		display: flex;
		flex-direction: column;
		gap: 1.5rem;
	}

	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(180px, 1fr));
		gap: 1rem;
	}

	section {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	.row {
		display: flex;
		gap: 0.75rem;
		overflow-x: auto;
		padding-bottom: 0.25rem;
	}

	.row > .poster {
		width: 147px;
		height: 220px;
		flex-shrink: 0;
	}

	.add-tile {
		width: 100%;
		height: 100%;
		display: flex;
		align-items: center;
		justify-content: center;
		border: 2px dashed
			var(--glow-border-color, rgba(255, 255, 255, 0.25));
		border-radius: 12px;
		background: transparent;
		color: inherit;
		opacity: 0.5;
		cursor: pointer;
		transition:
			opacity 150ms ease,
			border-color 150ms ease;
	}

	.add-tile:hover {
		opacity: 1;
		border-color: var(--glow-primary, #8b6ded);
	}

	.add-dialog {
		display: flex;
		flex-direction: column;
		gap: 1.5rem;
	}

	.dlg-group {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.list {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.list-item {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		padding: 0.375rem 0.5rem;
		border-radius: 0.375rem;
		background: rgba(255, 255, 255, 0.04);
	}

	.list-item .handle {
		cursor: grab;
		user-select: none;
		opacity: 0.5;
	}

	.list-item .thumb {
		width: 32px;
		height: 48px;
		object-fit: cover;
		border-radius: 0.25rem;
		background: rgba(255, 255, 255, 0.06);
		flex-shrink: 0;
	}

	.list-item .spacer {
		flex: 1;
	}
</style>
