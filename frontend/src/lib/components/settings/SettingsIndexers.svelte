<script lang="ts">
	import {
		ChevronDown,
		CircleCheck,
		CircleX,
		GripVertical,
		Plus,
		Rss,
		Trash2
	} from 'lucide-svelte';

	import {
		deleteIndexerMutation,
		getIndexersQuery,
		reorderIndexersMutation,
		saveIndexerMutation,
		testIndexerMutation
	} from '$lib/queries/downloads/IndexerQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import type { IndexerSettings, IndexerTestResult } from '$lib/types';

	const INDEXER_KEY_MASK = 'indexer****';
	// The common audio categories; 3000 (Audio) expands to all subcats on most indexers.
	const AUDIO_CATEGORIES = [
		{ id: 3000, label: 'Audio' },
		{ id: 3010, label: 'MP3' },
		{ id: 3040, label: 'Lossless' }
	];
	const NEW = '__new__';

	const indexersQuery = getIndexersQuery();
	const save = saveIndexerMutation();
	const remove = deleteIndexerMutation();
	const reorder = reorderIndexersMutation();
	const test = testIndexerMutation();

	const indexers = $derived(indexersQuery.data ?? []);

	let editingId = $state<string | null>(null);
	let draft = $state<IndexerSettings | null>(null);
	let showKey = $state(false);
	let testResults = $state<Record<string, IndexerTestResult>>({});
	let dragId = $state<string | null>(null);

	function blankIndexer(): IndexerSettings {
		return {
			id: '',
			type: 'newznab',
			name: '',
			url: '',
			api_key: '',
			categories: [3000, 3010, 3040],
			enabled: true,
			priority: indexers.length + 1
		};
	}

	function startAdd() {
		draft = blankIndexer();
		editingId = NEW;
		showKey = false;
	}

	function startEdit(indexer: IndexerSettings) {
		if (editingId === indexer.id) {
			editingId = null;
			draft = null;
			return;
		}
		draft = { ...indexer, categories: [...indexer.categories] };
		editingId = indexer.id;
		showKey = false;
	}

	function cancelEdit() {
		editingId = null;
		draft = null;
	}

	function toggleCategory(id: number) {
		if (!draft) return;
		draft.categories = draft.categories.includes(id)
			? draft.categories.filter((c) => c !== id)
			: [...draft.categories, id].sort((a, b) => a - b);
	}

	async function saveDraft() {
		if (!draft) return;
		if (!draft.url.trim()) {
			toastStore.show({ message: 'An indexer URL is required', type: 'error' });
			return;
		}
		try {
			await save.mutateAsync(draft);
			toastStore.show({ message: 'Indexer saved', type: 'success' });
			cancelEdit();
		} catch {
			toastStore.show({ message: 'Could not save the indexer', type: 'error' });
		}
	}

	async function runTest() {
		if (!draft) return;
		const key = editingId ?? NEW;
		try {
			const result = await test.mutateAsync(draft);
			testResults = { ...testResults, [key]: result };
		} catch {
			testResults = {
				...testResults,
				[key]: {
					valid: false,
					message: "Couldn't reach the indexer",
					supports_audio_search: false,
					category_count: 0
				}
			};
		}
	}

	async function applySuggestion(url: string) {
		if (!draft) return;
		draft.url = url;
		await runTest();
	}

	async function removeIndexer(indexer: IndexerSettings) {
		try {
			await remove.mutateAsync(indexer.id);
			toastStore.show({ message: 'Indexer removed', type: 'success' });
			if (editingId === indexer.id) cancelEdit();
		} catch {
			toastStore.show({ message: 'Could not remove the indexer', type: 'error' });
		}
	}

	async function toggleEnabled(indexer: IndexerSettings) {
		try {
			await save.mutateAsync({ ...indexer, api_key: INDEXER_KEY_MASK, enabled: !indexer.enabled });
		} catch {
			toastStore.show({ message: 'Could not update the indexer', type: 'error' });
		}
	}

	function persistOrder(ids: string[]) {
		reorder.mutate(ids);
	}

	function move(index: number, delta: number) {
		const ids = indexers.map((i) => i.id);
		const target = index + delta;
		if (target < 0 || target >= ids.length) return;
		[ids[index], ids[target]] = [ids[target], ids[index]];
		persistOrder(ids);
	}

	function onDrop(targetIndex: number) {
		if (!dragId) return;
		const ids = indexers.map((i) => i.id);
		const from = ids.indexOf(dragId);
		if (from === -1 || from === targetIndex) {
			dragId = null;
			return;
		}
		ids.splice(targetIndex, 0, ids.splice(from, 1)[0]);
		dragId = null;
		persistOrder(ids);
	}

	function host(url: string): string {
		try {
			return new URL(url).host;
		} catch {
			return url;
		}
	}
</script>

<section class="space-y-4">
	<header class="space-y-1">
		<h2 class="text-lg font-semibold">Indexers</h2>
		<p class="max-w-prose text-sm text-base-content/70">
			Newznab search sources for Usenet. DroppedNeedle ships none - add your own. A Prowlarr
			"Generic Newznab" endpoint works here too. Higher in the list is searched first.
		</p>
	</header>

	{#if indexers.length === 0 && editingId !== NEW}
		<div
			class="flex flex-col items-center rounded-box border border-dashed border-base-300 bg-base-200/40 p-10 text-center"
		>
			<div class="grid size-14 place-items-center rounded-2xl bg-base-300/60">
				<Rss class="size-7 text-accent" aria-hidden="true" />
			</div>
			<p class="mt-4 font-semibold">No indexers yet</p>
			<p class="mx-auto mt-1 max-w-md text-sm text-base-content/70">
				Add a Newznab indexer (its URL + your API key) to search Usenet. DroppedNeedle bundles none
				- bring your own. A Prowlarr "Generic Newznab" endpoint works here too.
			</p>
			<button type="button" class="btn btn-primary btn-sm mt-5" onclick={startAdd}>
				<Plus class="size-4" aria-hidden="true" /> Add indexer
			</button>
		</div>
	{:else}
		<ul class="space-y-3">
			{#each indexers as indexer, index (indexer.id)}
				{@const isOpen = editingId === indexer.id}
				{@const result = testResults[indexer.id]}
				<li
					class="indexer-card card border border-base-300 bg-base-200"
					class:is-active={indexer.enabled}
					ondragover={(e) => e.preventDefault()}
					ondrop={() => onDrop(index)}
					role="listitem"
				>
					<div class="card-body gap-0 p-0">
						<div class="flex flex-wrap items-center gap-3 p-4">
							<button
								type="button"
								class="cursor-grab text-base-content/40 hover:text-base-content"
								aria-label="Drag to reorder"
								draggable="true"
								ondragstart={() => (dragId = indexer.id)}
								ondragend={() => (dragId = null)}
								onkeydown={(e) => {
									if (e.key === 'ArrowUp') {
										e.preventDefault();
										move(index, -1);
									} else if (e.key === 'ArrowDown') {
										e.preventDefault();
										move(index, 1);
									}
								}}
							>
								<GripVertical class="size-4" aria-hidden="true" />
							</button>
							<span class="badge badge-ghost badge-sm tabular-nums">{index + 1}</span>
							<div class="grid size-12 place-items-center rounded-2xl bg-base-300/60">
								<Rss class="size-6 text-accent" aria-hidden="true" />
							</div>
							<button
								type="button"
								class="min-w-0 flex-1 text-left"
								onclick={() => startEdit(indexer)}
								aria-expanded={isOpen}
							>
								<div class="flex items-center gap-2">
									<h3 class="truncate text-lg font-bold">{indexer.name || host(indexer.url)}</h3>
									{#if result?.valid}
										<span class="badge badge-ghost badge-sm">
											{result.supports_audio_search ? 'music search' : 'text search'}
										</span>
									{/if}
								</div>
								<div class="flex items-center gap-2 text-sm text-base-content/70">
									<span
										class="orb"
										class:is-connected={indexer.enabled}
										role="status"
										aria-label={indexer.enabled ? 'Enabled' : 'Disabled'}
									></span>
									<span class="truncate">{host(indexer.url)}</span>
								</div>
							</button>
							<label class="flex cursor-pointer items-center gap-2">
								<span class="text-sm font-medium">{indexer.enabled ? 'Enabled' : 'Disabled'}</span>
								<input
									type="checkbox"
									class="toggle toggle-accent"
									checked={indexer.enabled}
									onchange={() => toggleEnabled(indexer)}
									aria-label={indexer.enabled ? 'Disable indexer' : 'Enable indexer'}
								/>
							</label>
							<button
								type="button"
								class="btn btn-ghost btn-sm btn-square"
								onclick={() => startEdit(indexer)}
								aria-label={isOpen ? 'Collapse' : 'Expand'}
							>
								<ChevronDown
									class={isOpen
										? 'size-5 rotate-180 transition-transform'
										: 'size-5 transition-transform'}
									aria-hidden="true"
								/>
							</button>
						</div>

						{#if isOpen && draft}
							<div class="space-y-5 border-t border-base-300 p-5">
								{@render editForm()}
							</div>
						{/if}
					</div>
				</li>
			{/each}
		</ul>

		{#if editingId === NEW && draft}
			<div class="card border border-accent/40 bg-base-200">
				<div class="card-body gap-0 p-0">
					<div class="flex items-center gap-3 p-4">
						<div class="grid size-12 place-items-center rounded-2xl bg-base-300/60">
							<Rss class="size-6 text-accent" aria-hidden="true" />
						</div>
						<h3 class="text-lg font-bold">New indexer</h3>
					</div>
					<div class="space-y-5 border-t border-base-300 p-5">
						{@render editForm()}
					</div>
				</div>
			</div>
		{:else}
			<button type="button" class="btn btn-sm" onclick={startAdd}>
				<Plus class="size-4" aria-hidden="true" /> Add indexer
			</button>
		{/if}
	{/if}
</section>

{#snippet editForm()}
	{@const result = testResults[editingId ?? NEW]}
	<div class="space-y-3">
		<div class="form-control">
			<label class="label" for="indexer-name"><span class="label-text">Name</span></label>
			<input
				id="indexer-name"
				class="input input-bordered input-sm w-full"
				placeholder="My Indexer"
				bind:value={draft!.name}
			/>
		</div>
		<div class="form-control">
			<label class="label" for="indexer-url"><span class="label-text">URL</span></label>
			<input
				id="indexer-url"
				class="input input-bordered input-sm w-full font-mono text-sm"
				placeholder="https://indexer.example/api"
				bind:value={draft!.url}
			/>
		</div>
		<div class="form-control">
			<label class="label" for="indexer-key"><span class="label-text">API key</span></label>
			<div class="join w-full">
				<input
					id="indexer-key"
					type={showKey ? 'text' : 'password'}
					class="input input-bordered input-sm join-item flex-1 font-mono text-sm"
					placeholder="your indexer API key"
					bind:value={draft!.api_key}
				/>
				<button type="button" class="btn btn-sm join-item" onclick={() => (showKey = !showKey)}>
					{showKey ? 'Hide' : 'Show'}
				</button>
			</div>
		</div>
		<div class="form-control">
			<span class="label-text">Audio categories</span>
			<div class="mt-1 flex flex-wrap gap-2">
				{#each AUDIO_CATEGORIES as cat (cat.id)}
					<label class="label cursor-pointer gap-2 rounded-btn border border-base-300 px-3 py-1">
						<input
							type="checkbox"
							class="checkbox checkbox-sm"
							checked={draft!.categories.includes(cat.id)}
							onchange={() => toggleCategory(cat.id)}
						/>
						<span class="label-text">{cat.label}</span>
					</label>
				{/each}
			</div>
		</div>

		<div class="flex flex-wrap items-center gap-3">
			<button type="button" class="btn btn-sm" onclick={runTest} disabled={test.isPending}>
				{test.isPending ? 'Testing…' : 'Test'}
			</button>
			{#if result}
				<span
					class="flex items-center gap-1.5 text-sm"
					class:text-success={result.valid}
					class:text-error={!result.valid}
				>
					{#if result.valid}
						<CircleCheck class="size-4" aria-hidden="true" />
					{:else}
						<CircleX class="size-4" aria-hidden="true" />
					{/if}
					{result.message}
				</span>
			{/if}
			{#if result?.suggested_url}
				<button
					type="button"
					class="btn btn-outline btn-sm font-mono"
					onclick={() => applySuggestion(result.suggested_url!)}
					disabled={test.isPending}
				>
					Use {result.suggested_url}
				</button>
			{/if}
			<div class="flex-1"></div>
			{#if editingId !== NEW}
				<button
					type="button"
					class="btn btn-ghost btn-sm text-error"
					onclick={() => draft && removeIndexer(draft as IndexerSettings)}
				>
					<Trash2 class="size-4" aria-hidden="true" /> Remove
				</button>
			{/if}
			<button type="button" class="btn btn-ghost btn-sm" onclick={cancelEdit}>Cancel</button>
			<button
				type="button"
				class="btn btn-primary btn-sm"
				onclick={saveDraft}
				disabled={save.isPending}
			>
				Save
			</button>
		</div>
	</div>
{/snippet}

<style>
	.indexer-card {
		transition:
			box-shadow 0.4s ease,
			border-color 0.4s ease;
	}
	.indexer-card.is-active {
		border-color: oklch(from var(--color-accent) l c h / 0.55);
		box-shadow:
			0 0 0 1px oklch(from var(--color-accent) l c h / 0.3),
			0 0 44px oklch(from var(--color-accent) l c h / 0.18);
	}
	.orb {
		display: inline-block;
		width: 0.7rem;
		height: 0.7rem;
		border-radius: 9999px;
		background: oklch(from var(--color-base-content) l c h / 0.3);
		transition: background 0.3s ease;
	}
	.orb.is-connected {
		background: var(--color-accent);
		animation: orb-pulse 2.4s ease-in-out infinite;
	}
	@keyframes orb-pulse {
		0%,
		100% {
			box-shadow: 0 0 5px oklch(from var(--color-accent) l c h / 0.5);
		}
		50% {
			box-shadow: 0 0 14px oklch(from var(--color-accent) l c h / 0.95);
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.orb.is-connected {
			animation: none;
		}
	}
</style>
