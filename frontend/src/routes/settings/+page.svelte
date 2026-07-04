<script lang="ts">
	import {
		type CollectionDef,
		type CollectionItem,
		type SearchResult,
		type YoutubeCookiesStatus,
	} from "$lib/schema";
	import { api } from "$lib/api";
	import { imageUrl } from "$lib/utils";
	import {
		Button,
		Card,
		Code,
		Field,
		FieldRow,
		FileUpload,
		Input,
		SettingsSection,
		SettingsShell,
		Tabs,
		Text,
		ToggleInput,
		toast,
		sortable,
	} from "glow";

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
		api.collections
			.listDefs()
			.then((list) => {
				defs = list;
			})
			.catch(() => {});
	}

	function selectCollection(def: CollectionDef) {
		selected = def;
		renameTitle = def.title;
		query = "";
		results = [];
		api.collections
			.get(def.slug)
			.then((list) => {
				items = list;
			})
			.catch(() => {
				items = [];
			});
	}

	async function createCollection() {
		const title = newTitle.trim();
		const slug = slugify(title);
		if (!title || !slug) return;
		await api.collections
			.createDef({
				slug,
				title,
				kind: newOrdered ? "ordered" : "manual",
			})
			.catch(() => {});
		newTitle = "";
		loadDefs();
	}

	async function renameCollection() {
		if (!selected || selected.system) return;
		const title = renameTitle.trim();
		if (!title || title === selected.title) return;
		await api.collections
			.createDef({
				slug: selected.slug,
				title,
				kind: selected.kind,
			})
			.catch(() => {});
		selected = { ...selected, title };
		loadDefs();
	}

	async function setVisibility(def: CollectionDef, visible: boolean) {
		await api.collections.setVisibility(def.slug, !visible).catch(() => {});
		loadDefs();
	}

	async function deleteCollection(def: CollectionDef) {
		await api.collections.deleteDef(def.slug).catch(() => {});
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
				results = await api.search.search(query);
			} finally {
				searching = false;
			}
		}, 300);
	}

	async function addItem(r: SearchResult) {
		if (!selected) return;
		await api.collections
			.add({
				collection: selected.slug,
				media_type: r.media_type,
				tmdb_id: r.id,
				title: r.title,
				poster_path: r.poster_path ?? null,
			})
			.catch(() => {});
		const list = await api.collections.get(selected.slug).catch(() => null);
		if (list) items = list;
	}

	async function removeItem(item: CollectionItem) {
		if (!selected) return;
		await api.collections
			.remove(selected.slug, item.media_type, item.tmdb_id)
			.catch(() => {});
		items = items.filter(
			(i) =>
				!(
					i.media_type === item.media_type &&
					i.tmdb_id === item.tmdb_id
				),
		);
	}

	function persistDefOrder() {
		api.collections.reorderDefs(defs.map((d) => d.slug)).catch(() => {});
	}

	function persistOrder() {
		if (!selected) return;
		api.collections
			.reorder(
				selected.slug,
				items.map((i) => ({
					media_type: i.media_type,
					tmdb_id: i.tmdb_id,
				})),
			)
			.catch(() => {});
	}

	let cookiesStatus = $state<YoutubeCookiesStatus | null>(null);
	let cookieFiles = $state<File[]>([]);
	let cookiesBusy = $state(false);

	async function refreshCookies() {
		try {
			cookiesStatus = await api.settings.youtubeCookiesStatus();
		} catch (e) {
			toast.error(`Failed to load cookies status: ${e}`);
		}
	}

	async function onCookieFiles(files: File[]) {
		const file = files[0];
		if (!file) return;
		cookiesBusy = true;
		try {
			const content = await file.text();
			cookiesStatus = await api.settings.setYoutubeCookies(content);
			toast.success("YouTube cookies saved");
		} catch (e) {
			toast.error(`Upload failed: ${e}`);
		} finally {
			cookiesBusy = false;
			cookieFiles = [];
		}
	}

	async function clearCookies() {
		cookiesBusy = true;
		try {
			cookiesStatus = await api.settings.clearYoutubeCookies();
			toast.success("Stored cookies removed");
		} catch (e) {
			toast.error(`Clear failed: ${e}`);
		} finally {
			cookiesBusy = false;
		}
	}

	function formatBytes(n: number): string {
		if (n < 1024) return `${n} B`;
		if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
		return `${(n / (1024 * 1024)).toFixed(2)} MiB`;
	}

	$effect(() => {
		loadDefs();
		refreshCookies();
	});
</script>

<svelte:head>
	<title>Settings</title>
</svelte:head>

<div class="page">
	<SettingsShell title="Settings">
		<Tabs
			tabs={[
				{
					id: "collections",
					label: "Collections",
					icon: "List",
					content: collectionsTab,
				},
				{
					id: "trailers",
					label: "Cookies",
					icon: "Film",
					content: trailersTab,
				},
			]}
		/>
	</SettingsShell>
</div>

{#snippet collectionsTab()}
	<SettingsSection
		title="Your collections"
		description="Drag to reorder. System collections can be hidden but not deleted."
		variant="plain"
	>
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
					<button
						class="def-name"
						onclick={() => selectCollection(def)}
					>
						<Text size="sm">{def.title}</Text>
						<Text size="xs" variant="muted">
							{def.kind === "ordered"
								? "Fixed order"
								: "Manual"}{def.system ? " · System" : ""}
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
	</SettingsSection>

	<SettingsSection title="Create collection" variant="plain">
		<FieldRow>
			<Field label="Name" layout="vertical">
				<Input
					type="text"
					placeholder="e.g. Saturday night"
					value={newTitle}
					onChange={(v) => (newTitle = v)}
				/>
			</Field>
			<Field
				label="Fixed order"
				hint="When enabled, you decide the order - otherwise newest first."
				layout="horizontal"
				align="center"
			>
				<ToggleInput
					checked={newOrdered}
					onChange={(c) => (newOrdered = c)}
				/>
			</Field>
		</FieldRow>
		<div class="row-end">
			<Button
				label="Create"
				variant="primary"
				onclick={createCollection}
			/>
		</div>
	</SettingsSection>

	{#if selected}
		{#if selected.slug === CONTINUE_SLUG}
			<SettingsSection title={selected.title} variant="card">
				<Text size="sm" variant="muted">
					Auto-managed from playback - titles appear here as you watch
					and disappear when finished. Use the toggle in the list to
					show or hide this row, and drag it to reposition it among
					the other collections.
				</Text>
			</SettingsSection>
		{:else}
			<SettingsSection
				title={selected.title}
				description={selected.kind === "ordered"
					? "Drag to set the order."
					: "Ordered by date added."}
				variant="card"
			>
				{#if !selected.system}
					<Field label="Rename" layout="vertical">
						<div class="rename-row">
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
					</Field>
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
			</SettingsSection>

			<SettingsSection title="Add items" variant="card">
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
									<Text size="xs" variant="muted"
										>{r.title}</Text
									>
								{/snippet}
							</Card>
						{/each}
					</div>
				{/if}
			</SettingsSection>
		{/if}
	{/if}
{/snippet}

{#snippet trailersTab()}
	<SettingsSection
		title="YouTube cookies"
		description="Trailers are fetched via yt-dlp. On a server IP YouTube often blocks anonymous requests. The recommended fix is running the PO-token provider (docker compose — see the readme); uploading a cookies.txt exported from a signed-in browser also helps."
		variant="plain"
	>
		<Field label="Status" layout="horizontal" align="center">
			{#if !cookiesStatus}
				<Text size="sm" variant="muted">Loading…</Text>
			{:else if cookiesStatus.source === "env"}
				<Text size="sm">
					Using
					<Code>CINEMA_YTDLP_COOKIES</Code>
					({cookiesStatus.env_path}). Env var takes precedence over
					uploads.
				</Text>
			{:else if cookiesStatus.source === "file"}
				<Text size="sm">
					Stored cookies file ({cookiesStatus.size != null
						? formatBytes(Number(cookiesStatus.size))
						: "?"}).
				</Text>
			{:else}
				<Text size="sm" variant="muted">No cookies configured</Text>
			{/if}
		</Field>

		<Field
			label="Upload cookies.txt"
			layout="vertical"
			disabled={cookiesBusy || cookiesStatus?.source === "env"}
		>
			<FileUpload
				bind:files={cookieFiles}
				accept=".txt,text/plain"
				multiple={false}
				maxFiles={1}
				maxSize={1024 * 1024}
				emptyTitle="cookies.txt"
				hint="Plain text, up to 1 MiB"
				onChange={onCookieFiles}
				onError={(m) => toast.error(m)}
			/>
		</Field>

		{#if cookiesStatus?.source === "file"}
			<div class="row-end">
				<Button
					label="Remove stored cookies"
					variant="secondary"
					onclick={clearCookies}
					disabled={cookiesBusy}
				/>
			</div>
		{/if}
	</SettingsSection>
{/snippet}

<style>
	.page {
		max-width: 880px;
		margin: 2rem auto;
		padding: 0 1rem;
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

	.rename-row {
		display: flex;
		gap: 0.75rem;
		align-items: center;
		flex-wrap: wrap;
	}

	.row-end {
		display: flex;
		justify-content: flex-end;
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
