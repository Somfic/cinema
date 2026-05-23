<script lang="ts">
	import {
		listCollectionDefs,
		createCollectionDef,
		deleteCollectionDef,
		setCollectionVisibility,
		reorderCollectionDefs,
		getCollection as fetchCollection,
		addToCollection,
		removeFromCollection,
		reorderCollection,
		search,
		type CollectionDef,
		type CollectionItem,
		type SearchResult,
	} from "$lib/api.gen";
	import { imageUrl } from "$lib/utils";
	import { Heading, Input, Button, Text, Card, ToggleInput } from "glow";
	import { sortable } from "glow";

	const CONTINUE_SLUG = "continue";

	let defs = $state<CollectionDef[]>([]);
	let selected = $state<CollectionDef | null>(null);
	let items = $state<CollectionItem[]>([]);

	let newTitle = $state("");
	let newOrdered = $state(true);
	let renameTitle = $state("");

	let query = $state("");
	let results = $state<SearchResult[]>([]);
	let searching = $state(false);
	let searchTimer: ReturnType<typeof setTimeout>;

	function slugify(s: string): string {
		return s
			.toLowerCase()
			.trim()
			.replace(/[^a-z0-9]+/g, "-")
			.replace(/^-+|-+$/g, "");
	}

	function loadDefs() {
		listCollectionDefs()
			.then((res) => {
				defs = res.data;
			})
			.catch(() => {});
	}

	$effect(() => {
		loadDefs();
	});

	function selectCollection(def: CollectionDef) {
		selected = def;
		renameTitle = def.title;
		query = "";
		results = [];
		fetchCollection(def.slug)
			.then((res) => {
				items = res.data;
			})
			.catch(() => {
				items = [];
			});
	}

	async function createCollection() {
		const title = newTitle.trim();
		const slug = slugify(title);
		if (!title || !slug) return;
		await createCollectionDef({
			slug,
			title,
			kind: newOrdered ? "ordered" : "manual",
		}).catch(() => {});
		newTitle = "";
		loadDefs();
	}

	async function renameCollection() {
		if (!selected || selected.system) return;
		const title = renameTitle.trim();
		if (!title || title === selected.title) return;
		await createCollectionDef({
			slug: selected.slug,
			title,
			kind: selected.kind,
		}).catch(() => {});
		selected = { ...selected, title };
		loadDefs();
	}

	async function setVisibility(def: CollectionDef, visible: boolean) {
		await setCollectionVisibility(def.slug, {
			hidden: !visible,
		}).catch(() => {});
		loadDefs();
	}

	async function deleteCollection(def: CollectionDef) {
		await deleteCollectionDef(def.slug).catch(() => {});
		if (selected?.slug === def.slug) {
			selected = null;
			items = [];
		}
		loadDefs();
	}

	function onSearchInput(v: string) {
		query = v;
		clearTimeout(searchTimer);
		if (query.length < 2) {
			results = [];
			return;
		}
		searching = true;
		searchTimer = setTimeout(async () => {
			try {
				const res = await search({ q: query });
				results = res.data;
			} finally {
				searching = false;
			}
		}, 300);
	}

	async function addItem(r: SearchResult) {
		if (!selected) return;
		await addToCollection({
			collection: selected.slug,
			media_type: r.media_type,
			tmdb_id: r.id,
			title: r.title,
			poster_path: r.poster_path ?? undefined,
		}).catch(() => {});
		const res = await fetchCollection(selected.slug).catch(() => null);
		if (res) items = res.data;
	}

	async function removeItem(item: CollectionItem) {
		if (!selected) return;
		await removeFromCollection(
			selected.slug,
			item.media_type,
			item.tmdb_id,
		).catch(() => {});
		items = items.filter(
			(i) =>
				!(
					i.media_type === item.media_type &&
					i.tmdb_id === item.tmdb_id
				),
		);
	}

	function persistDefOrder() {
		reorderCollectionDefs({
			slugs: defs.map((d) => d.slug),
		}).catch(() => {});
	}

	function persistOrder() {
		if (!selected) return;
		reorderCollection(selected.slug, {
			items: items.map((i) => ({
				media_type: i.media_type,
				tmdb_id: i.tmdb_id,
			})),
		}).catch(() => {});
	}
</script>

<svelte:head>
	<title>Collections</title>
</svelte:head>

<div class="content">
	<section>
		<Heading level={2}>Collections</Heading>
		<div
			class="defs"
			use:sortable={{
				items: defs,
				direction: "vertical",
				handle: ".def-handle",
				onReorder: () => persistDefOrder(),
			}}
		>
			{#each defs as def (def.slug)}
				<div class="def" class:active={selected?.slug === def.slug}>
					<span class="def-handle">⠿</span>
					<button class="def-name" onclick={() => selectCollection(def)}>
						<Text size="sm">{def.title}</Text>
						<Text size="xs" variant="muted">
							{def.kind === "ordered" ? "Fixed order" : "Manual"}{def.system
								? " · System"
								: ""}
						</Text>
					</button>
					{#if def.system}
						<ToggleInput
							label="Visible"
							checked={!def.hidden}
							onChange={(v) => setVisibility(def, v)}
						/>
					{:else}
						<Button
							icon="Trash"
							variant="ghost"
							onclick={() => deleteCollection(def)}
						/>
					{/if}
				</div>
			{/each}
		</div>
	</section>

	<section>
		<Heading level={3}>New collection</Heading>
		<div class="create">
			<Input
				type="text"
				placeholder="Collection name"
				value={newTitle}
				onChange={(v) => (newTitle = v)}
			/>
			<ToggleInput
				label="Fixed order"
				checked={newOrdered}
				onChange={(c) => (newOrdered = c)}
			/>
			<Button label="Create" variant="primary" onclick={createCollection} />
		</div>
	</section>

	{#if selected}
		{#if selected.slug === CONTINUE_SLUG}
			<section>
				<Heading level={3}>{selected.title}</Heading>
				<Text size="sm" variant="muted">
					Auto-managed from playback — titles appear here as you watch
					and disappear when finished. Use the toggle in the list to
					show or hide this row, and drag it to reposition it among the
					other collections.
				</Text>
			</section>
		{:else}
		<section>
			<Heading level={3}>
				{selected.title}
				<Text size="xs" variant="muted">
					{selected.kind === "ordered"
						? "— drag to set the order"
						: "— ordered by date added"}
				</Text>
			</Heading>

			{#if !selected.system}
				<div class="create">
					<Input
						type="text"
						placeholder="Collection name"
						value={renameTitle}
						onChange={(v) => (renameTitle = v)}
					/>
					<Button
						label="Rename"
						variant="secondary"
						onclick={renameCollection}
					/>
				</div>
			{/if}

			{#if items.length > 0}
				<div
					class="list"
					use:sortable={{
						items,
						direction: "vertical",
						disabled: selected.kind !== "ordered",
						onReorder: () => persistOrder(),
					}}
				>
					{#each items as item (item.media_type + item.tmdb_id)}
						<div class="list-item">
							{#if selected.kind === "ordered"}
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
								onclick={() => removeItem(item)}
							/>
						</div>
					{/each}
				</div>
			{:else}
				<Text size="sm" variant="muted">No items yet.</Text>
			{/if}
		</section>

		<section>
			<Heading level={3}>Add items</Heading>
			<Input
				type="text"
				placeholder="Search movies and TV shows..."
				value={query}
				icon={"Search"}
				loading={searching}
				onChange={onSearchInput}
			/>
			{#if results.length > 0}
				<div class="grid">
					{#each results as r (r.media_type + r.id)}
						<Card
							media={{
								src: r.poster_path
									? imageUrl(r.poster_path, "w342")
									: "",
								aspectRatio: "2/3",
							}}
							mediaLayout="overlay"
							onclick={() => addItem(r)}
						>
							{#snippet bottomLeft()}
								<Text size="xs" variant="muted">{r.title}</Text>
							{/snippet}
						</Card>
					{/each}
				</div>
			{/if}
		</section>
		{/if}
	{/if}
</div>

<style>
	.content {
		padding: 2rem;
		display: flex;
		flex-direction: column;
		gap: 1.5rem;
	}

	section {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	.defs {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.def {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding: 0.25rem 0.5rem;
		border-radius: 0.375rem;
	}

	.def.active {
		background: rgba(255, 255, 255, 0.06);
	}

	.def-handle {
		cursor: grab;
		user-select: none;
		opacity: 0.5;
	}

	.def-name {
		flex: 1;
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.1rem;
		background: none;
		border: none;
		padding: 0.25rem 0;
		cursor: pointer;
		color: inherit;
		text-align: left;
	}

	.create {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		flex-wrap: wrap;
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

	.handle {
		cursor: grab;
		user-select: none;
		opacity: 0.5;
	}

	.thumb {
		width: 32px;
		height: 48px;
		object-fit: cover;
		border-radius: 0.25rem;
		background: rgba(255, 255, 255, 0.06);
		flex-shrink: 0;
	}

	.spacer {
		flex: 1;
	}

	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
		gap: 1rem;
	}

	.grid :global(> *) {
		width: 100%;
	}
</style>
