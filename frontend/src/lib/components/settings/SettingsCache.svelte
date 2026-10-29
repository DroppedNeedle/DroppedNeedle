<script lang="ts">
	import { ApiError } from '$lib/api/errors';
	import {
		createClearCacheMutation,
		createPrecacheRunMutation,
		getCacheStatsQuery,
		type CacheClearBody
	} from '$lib/queries/settings/AdminCacheQueries.svelte';
	import { syncStatus } from '$lib/stores/syncStatus.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import { formatBytes } from '$lib/utils/formatting';
	import { formatCount, providerLabel } from './diagnosticsDisplay';

	const statsQuery = getCacheStatsQuery();
	const clearMutation = createClearCacheMutation();
	const precacheMutation = createPrecacheRunMutation();

	let message = $state('');
	let messageType = $state<'success' | 'error'>('success');
	let clearTimer: ReturnType<typeof setTimeout> | null = null;

	const stats = $derived(statsQuery.data ?? null);
	const sources = $derived(
		[...(stats?.sources ?? [])].sort((a, b) =>
			providerLabel(a.source).localeCompare(providerLabel(b.source))
		)
	);
	const providerBytes = $derived(sources.reduce((sum, source) => sum + source.bytes, 0));
	const needsAdmin = $derived(
		statsQuery.error instanceof ApiError &&
			(statsQuery.error.status === 401 || statsQuery.error.status === 403)
	);

	export async function load() {
		await statsQuery.refetch();
	}

	function showMessage(text: string, type: 'success' | 'error') {
		message = text;
		messageType = type;
		if (clearTimer) clearTimeout(clearTimer);
		clearTimer = type === 'success' ? setTimeout(() => (message = ''), 5000) : null;
	}

	const PROMPTS = {
		provider_data: 'Clear all cached provider data? It is fetched again on demand.',
		covers: 'Clear the cached album covers and artist images? They are fetched again on demand.'
	} as const;

	// The clear waiting for the user to confirm it.
	let pending = $state<{ body: CacheClearBody; prompt: string } | null>(null);
	let confirming = $state(false);

	function clear(scope: keyof typeof PROMPTS) {
		pending = { body: { scope }, prompt: PROMPTS[scope] };
		confirming = true;
	}

	async function runClear() {
		if (!pending) return;
		const { body } = pending;
		try {
			const result = await clearMutation.mutateAsync(body);
			showMessage(result.message, 'success');
		} catch (error) {
			showMessage(error instanceof ApiError ? error.message : "Couldn't clear the cache", 'error');
		}
	}

	async function refreshImages() {
		try {
			await precacheMutation.mutateAsync();
			syncStatus.undismiss();
			syncStatus.checkStatus();
			showMessage(
				'Library image refresh started. Progress shows at the bottom of the page.',
				'success'
			);
		} catch (error) {
			showMessage(
				error instanceof ApiError && error.status === 409
					? 'A library image refresh is already running.'
					: error instanceof ApiError
						? error.message
						: "Couldn't start the refresh",
				'error'
			);
		}
	}
</script>

<Card>
	{#snippet header()}
		<CardTitle class="text-2xl mb-4">Cache management</CardTitle>
	{/snippet}
	<p class="text-base-content/70 mb-6">
		Provider data and images are kept on disk. Anything cleared is fetched again on demand.
	</p>

	{#if statsQuery.isPending}
		<div class="flex justify-center items-center py-12">
			<span class="loading loading-spinner loading-lg"></span>
		</div>
	{:else if needsAdmin}
		<div role="alert" class="alert alert-warning mt-4">
			<span>Admin access is required to view cache statistics.</span>
		</div>
	{:else if stats}
		<div class="stats stats-vertical sm:stats-horizontal bg-base-100 mb-6 w-full">
			<div class="stat">
				<div class="stat-title">Provider data</div>
				<div class="stat-value text-primary">{formatCount(stats.entries)}</div>
				<div class="stat-desc">{formatBytes(providerBytes)}</div>
			</div>
			<div class="stat">
				<div class="stat-title">Cached images</div>
				<div class="stat-value text-primary">{formatCount(stats.cover_images)}</div>
				<div class="stat-desc">{formatBytes(stats.cover_bytes)}</div>
			</div>
			<div class="stat">
				<div class="stat-title">Database</div>
				<div class="stat-value text-primary">{formatBytes(stats.database_bytes)}</div>
				<div class="stat-desc">Write-ahead log {formatBytes(stats.wal_bytes)}</div>
			</div>
		</div>

		{#if !stats.provider_data_on}
			<div role="alert" class="alert alert-warning mb-6">
				<span>The provider data cache is off. Answers are fetched each time.</span>
			</div>
		{/if}

		{#if sources.length > 0}
			<div class="overflow-x-auto mb-6">
				<table class="table table-sm" aria-label="Provider data by source">
					<thead>
						<tr>
							<th scope="col">Source</th>
							<th scope="col" class="text-right">Rows</th>
							<th scope="col" class="text-right">Size</th>
						</tr>
					</thead>
					<tbody>
						{#each sources as source (source.source)}
							<tr>
								<td>{providerLabel(source.source)}</td>
								<td class="text-right tabular-nums">{formatCount(source.rows)}</td>
								<td class="text-right tabular-nums">{formatBytes(source.bytes)}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}

		<div class="space-y-2 mb-6">
			<h3 class="text-xl font-semibold">Library images</h3>
			<p class="text-sm text-base-content/70">
				Fetch artist images, album covers and page details for your whole library now, so pages load
				fast later. It runs in the background and skips anything already cached.
			</p>
			<Button
				variant="primary"
				size="sm"
				onclick={refreshImages}
				loading={precacheMutation.isPending}
				disabled={syncStatus.isActive}
			>
				{syncStatus.isActive ? 'Refresh running' : 'Refresh library images'}
			</Button>
		</div>

		<div class="space-y-4">
			<h3 class="text-xl font-semibold">Clear cache</h3>
			<div class="flex flex-wrap gap-2">
				<Button
					variant="outline"
					size="sm"
					onclick={() => clear('provider_data')}
					disabled={clearMutation.isPending}
				>
					Clear provider data
				</Button>
				<Button
					variant="outline"
					size="sm"
					onclick={() => clear('covers')}
					disabled={clearMutation.isPending}
				>
					Clear images
				</Button>
			</div>
		</div>
	{:else}
		<div role="alert" class="alert alert-error mt-4">
			<span>Couldn't load cache stats</span>
		</div>
	{/if}

	{#if message}
		<div
			role="status"
			class="alert mt-4"
			class:alert-success={messageType === 'success'}
			class:alert-error={messageType === 'error'}
		>
			<span>{message}</span>
		</div>
	{/if}
</Card>

<ConfirmDialog
	bind:open={confirming}
	title="Clear cache"
	message={pending?.prompt}
	confirmLabel="Clear"
	tone="danger"
	pending={clearMutation.isPending}
	onconfirm={runClear}
/>
