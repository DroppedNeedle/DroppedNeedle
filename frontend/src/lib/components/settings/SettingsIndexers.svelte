<script lang="ts">
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import {
		ChevronDown,
		CircleCheck,
		CircleX,
		GripVertical,
		Plus,
		Radar,
		Rss,
		Trash2
	} from 'lucide-svelte';

	import {
		deleteIndexerMutation,
		getIndexersQuery,
		getSearchBackendQuery,
		reorderIndexersMutation,
		saveIndexerMutation,
		saveSearchBackendMutation,
		testIndexerMutation
	} from '$lib/queries/downloads/IndexerQueries.svelte';
	import { getProwlarrConfigQuery } from '$lib/queries/downloads/ProwlarrQueries.svelte';
	import { getPluginSourcesQuery } from '$lib/queries/plugins/PluginSourceQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import type {
		IndexerSettings,
		IndexerTestResult,
		UsenetSearchBackendName
	} from '$lib/queries/downloads/IndexerQueries.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import TextField from '$lib/ui/TextField.svelte';

	import PluginHealthBadge from './PluginHealthBadge.svelte';
	import SettingsProwlarr from './SettingsProwlarr.svelte';
	import SettingsSectionCollapse from './SettingsSectionCollapse.svelte';

	// The common audio categories; 3000 (Audio) expands to all subcats on most indexers.
	const AUDIO_CATEGORIES = [
		{ id: 3000, label: 'Audio' },
		{ id: 3010, label: 'MP3' },
		{ id: 3040, label: 'Lossless' }
	];
	const NEW = '__new__';

	const indexersQuery = getIndexersQuery();
	const backendQuery = getSearchBackendQuery();
	const prowlarrQuery = getProwlarrConfigQuery();
	const save = saveIndexerMutation();
	const remove = deleteIndexerMutation();
	const reorder = reorderIndexersMutation();
	const test = testIndexerMutation();
	const saveBackend = saveSearchBackendMutation();
	const indexers = $derived(indexersQuery.data ?? []);
	const backend = $derived<UsenetSearchBackendName>(backendQuery.data?.backend ?? 'indexers');
	const enabledCount = $derived(indexers.filter((i) => i.enabled).length);
	const prowlarrStatus = $derived.by(() => {
		const config = prowlarrQuery.data;
		if (!config?.url) return 'Not configured yet';
		return config.enabled ? 'Ready' : 'Configured but disabled';
	});
	// Accordion: the active backend's section opens; flipping the radio opens it.
	let indexersOpen = $state(true);
	let prowlarrOpen = $state(false);
	let seededOpen = false;
	$effect(() => {
		const selected = backendQuery.data?.backend;
		if (selected && !seededOpen) {
			indexersOpen = selected === 'indexers';
			prowlarrOpen = selected === 'prowlarr';
			seededOpen = true;
		}
	});

	async function selectBackend(next: UsenetSearchBackendName) {
		if (next === backend) return;
		try {
			await saveBackend.mutateAsync(next);
			indexersOpen = next === 'indexers';
			prowlarrOpen = next === 'prowlarr';
		} catch {
			toastStore.show({ message: 'Could not switch search backend', type: 'error' });
		}
	}
	const pluginSourcesQuery = getPluginSourcesQuery();
	const pluginIndexers = $derived(
		(pluginSourcesQuery.data?.sources ?? []).filter((source) => source.has_indexer)
	);
	let editingId = $state<string | null>(null);
	let draft = $state<IndexerSettings | null>(null);
	let testResults = $state<Record<string, IndexerTestResult>>({});
	let dragId = $state<string | null>(null);

	function blankIndexer(): IndexerSettings {
		return {
			id: '',
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
	}

	function startEdit(indexer: IndexerSettings) {
		if (editingId === indexer.id) {
			editingId = null;
			draft = null;
			return;
		}
		draft = { ...indexer, categories: [...indexer.categories] };
		editingId = indexer.id;
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
			await save.mutateAsync({ ...indexer, api_key: SECRET_MASK, enabled: !indexer.enabled });
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
		<h2 class="text-lg font-semibold">Indexers / Prowlarr</h2>
		<p class="max-w-prose text-sm text-base-content/70">
			Usenet search sources. Pick one backend: your prioritized Newznab list, or a single Prowlarr
			connection. The other side stays saved for later.
		</p>
	</header>

	<Card variant="bordered" padding="md">
		<div class="flex flex-col gap-3">
			<div>
				<h3 class="font-semibold">Search via</h3>
				<p class="text-sm text-base-content/60">Only the selected backend is searched.</p>
			</div>
			<div class="grid gap-2 sm:grid-cols-2">
				<label
					class="flex cursor-pointer items-start gap-3 rounded-box border p-3"
					class:border-accent={backend === 'indexers'}
					class:border-base-300={backend !== 'indexers'}
				>
					<input
						type="radio"
						name="usenet-search-backend"
						class="radio radio-sm mt-1"
						checked={backend === 'indexers'}
						onchange={() => selectBackend('indexers')}
					/>
					<span>
						<span class="font-medium">Individual indexers</span>
						<span class="block text-sm text-base-content/60">
							Search the prioritized Newznab list below. {enabledCount} enabled.
						</span>
					</span>
				</label>
				<label
					class="flex cursor-pointer items-start gap-3 rounded-box border p-3"
					class:border-accent={backend === 'prowlarr'}
					class:border-base-300={backend !== 'prowlarr'}
				>
					<input
						type="radio"
						name="usenet-search-backend"
						class="radio radio-sm mt-1"
						checked={backend === 'prowlarr'}
						onchange={() => selectBackend('prowlarr')}
					/>
					<span>
						<span class="font-medium">Prowlarr</span>
						<span class="block text-sm text-base-content/60">
							Search everything through one Prowlarr connection. {prowlarrStatus}.
						</span>
					</span>
				</label>
			</div>
		</div>
	</Card>

	<SettingsSectionCollapse
		title="Individual indexers"
		description="Your prioritized Newznab list - higher is searched first"
		icon={Rss}
		bind:isOpen={indexersOpen}
		name="usenet-backend-sections"
	>
		{#if indexers.length === 0 && editingId !== NEW}
			<div
				class="flex flex-col items-center rounded-box border border-dashed border-base-300 bg-base-200/40 p-10 text-center"
			>
				<div class="grid size-14 place-items-center rounded-2xl bg-base-300/60">
					<Rss class="size-7 text-accent" aria-hidden="true" />
				</div>
				<p class="mt-4 font-semibold">No indexers yet</p>
				<p class="mx-auto mt-1 max-w-md text-sm text-base-content/70">
					Add a Newznab indexer (its URL + your API key) to search Usenet. DroppedNeedle bundles
					none - bring your own. Run Prowlarr instead? Select it above and configure it below.
				</p>
				<Button variant="primary" size="sm" icon={Plus} class="mt-5" onclick={startAdd}>
					Add indexer
				</Button>
			</div>
		{:else}
			<ul class="space-y-3">
				{#each indexers as indexer, index (indexer.id)}
					{@const isOpen = editingId === indexer.id}
					{@const result = testResults[indexer.id]}
					<li ondragover={(e) => e.preventDefault()} ondrop={() => onDrop(index)} role="listitem">
						<Card
							variant="bordered"
							padding="none"
							class="indexer-card {indexer.enabled ? 'is-active' : ''}"
						>
							<div>
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
											<h3 class="truncate text-lg font-bold">
												{indexer.name || host(indexer.url)}
											</h3>
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
										<span class="text-sm font-medium"
											>{indexer.enabled ? 'Enabled' : 'Disabled'}</span
										>
										<input
											type="checkbox"
											class="toggle toggle-sm toggle-primary"
											checked={indexer.enabled}
											onchange={() => toggleEnabled(indexer)}
											aria-label={indexer.enabled ? 'Disable indexer' : 'Enable indexer'}
										/>
									</label>
									<IconButton
										label={isOpen ? 'Collapse' : 'Expand'}
										icon={ChevronDown}
										size="sm"
										shape="square"
										class={isOpen ? 'rotate-180' : ''}
										aria-expanded={isOpen}
										onclick={() => startEdit(indexer)}
									/>
								</div>

								{#if isOpen && draft}
									<div class="space-y-5 border-t border-base-300 p-5">
										{@render editForm()}
									</div>
								{/if}
							</div>
						</Card>
					</li>
				{/each}
			</ul>

			{#if editingId === NEW && draft}
				<Card variant="bordered" padding="none">
					<div>
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
				</Card>
			{:else}
				<Button size="sm" icon={Plus} onclick={startAdd}>Add indexer</Button>
			{/if}
			{#if pluginIndexers.length > 0}
				<Card variant="bordered" padding="sm">
					<div>
						<p class="text-xs font-semibold uppercase tracking-wide text-base-content/50">
							Plugin indexers
						</p>
						<p class="mt-1 text-xs text-base-content/60">
							Plugins provide these. Configure them under Plugins; you cannot edit them here. A
							plugin indexer for Usenet appears with Usenet results; any other target gets its own
							group in review.
						</p>
						<ul class="mt-2 space-y-1.5">
							{#each pluginIndexers as source (source.key)}
								<li class="flex flex-wrap items-center gap-1.5 text-sm">
									<span class="font-medium">{source.display_name || source.key}</span>
									<span class="badge badge-ghost badge-sm" title="Indexer target">
										targets {source.target_source || source.key}
									</span>
									{#if !source.configured && source.health !== 'unknown'}
										<span class="badge badge-ghost badge-sm">Not configured</span>
									{/if}
									<PluginHealthBadge health={source.health} />
									{#if source.health_message}
										<span class="basis-full text-xs text-base-content/60">
											{source.health_message}
										</span>
									{/if}
								</li>
							{/each}
						</ul>
					</div>
				</Card>
			{/if}
		{/if}
	</SettingsSectionCollapse>

	<SettingsSectionCollapse
		title="Prowlarr"
		description="One connection that searches all of its indexers at once"
		icon={Radar}
		bind:isOpen={prowlarrOpen}
		name="usenet-backend-sections"
	>
		{#if prowlarrOpen}
			<SettingsProwlarr />
		{/if}
	</SettingsSectionCollapse>
</section>

{#snippet editForm()}
	{@const result = testResults[editingId ?? NEW]}
	<div class="space-y-3">
		<TextField label="Name" placeholder="My Indexer" bind:value={draft!.name} />
		<TextField label="URL" mono placeholder="https://indexer.example/api" bind:value={draft!.url} />
		<SecretField
			label="API key"
			mask={editingId !== NEW ? SECRET_MASK : undefined}
			placeholder="your indexer API key"
			bind:value={draft!.api_key}
		/>
		<fieldset class="fieldset">
			<legend class="fieldset-legend">Audio categories</legend>
			<div class="flex flex-wrap gap-2">
				{#each AUDIO_CATEGORIES as cat (cat.id)}
					<label class="label cursor-pointer gap-2 rounded-btn border border-base-300 px-3 py-1">
						<input
							type="checkbox"
							class="checkbox checkbox-sm"
							checked={draft!.categories.includes(cat.id)}
							onchange={() => toggleCategory(cat.id)}
						/>
						<span>{cat.label}</span>
					</label>
				{/each}
			</div>
		</fieldset>

		<div class="flex flex-wrap items-center gap-3">
			<Button size="sm" onclick={runTest} disabled={test.isPending}>
				{test.isPending ? 'Testing…' : 'Test'}
			</Button>
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
				<Button
					variant="outline"
					size="sm"
					onclick={() => applySuggestion(result.suggested_url!)}
					disabled={test.isPending}
				>
					Use <span class="font-mono">{result.suggested_url}</span>
				</Button>
			{/if}
			<div class="flex-1"></div>
			{#if editingId !== NEW}
				<Button
					variant="danger-ghost"
					size="sm"
					icon={Trash2}
					onclick={() => draft && removeIndexer(draft as IndexerSettings)}
				>
					Remove
				</Button>
			{/if}
			<Button variant="ghost" size="sm" onclick={cancelEdit}>Cancel</Button>
			<Button variant="primary" size="sm" onclick={saveDraft} disabled={save.isPending}>Save</Button
			>
		</div>
	</div>
{/snippet}

<style>
	:global(.indexer-card) {
		transition:
			box-shadow 0.4s ease,
			border-color 0.4s ease;
	}
	:global(.indexer-card.is-active) {
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
