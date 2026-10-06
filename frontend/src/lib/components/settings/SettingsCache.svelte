<script lang="ts">
	import { ApiError } from '$lib/api/client';
	import {
		createClearCacheMutation,
		getCacheStatsQuery,
		type CacheClearBody
	} from '$lib/queries/settings/AdminCacheQueries.svelte';

	const statsQuery = getCacheStatsQuery();
	const clearMutation = createClearCacheMutation();

	let message = $state('');
	let messageType = $state<'success' | 'error'>('success');
	let clearTimer: ReturnType<typeof setTimeout> | null = null;

	const stats = $derived(statsQuery.data ?? null);
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

	async function clearCache(source: string | null) {
		const prompt = source
			? `Clear the cached ${source} responses?`
			: 'Clear every cached provider response? They are fetched again on demand.';
		if (!confirm(prompt)) return;
		const body: CacheClearBody = source ? { scope: 'source', source } : { scope: 'all' };
		try {
			const result = await clearMutation.mutateAsync(body);
			showMessage(result.message, 'success');
		} catch (error) {
			showMessage(error instanceof ApiError ? error.message : "Couldn't clear the cache", 'error');
		}
	}
</script>

<div class="card bg-base-200">
	<div class="card-body">
		<h2 class="card-title text-2xl mb-4">Cache management</h2>
		<p class="text-base-content/70 mb-6">
			Provider responses (MusicBrainz, Last.fm, ListenBrainz and the rest) are kept in memory and
			refetched on demand once cleared.
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
			<div class="stat mb-6 px-0">
				<div class="stat-title">Cached provider responses</div>
				<div class="stat-value text-primary">{stats.entries}</div>
			</div>

			<div class="space-y-4">
				<h3 class="text-xl font-semibold">Clear cache</h3>
				<div class="flex flex-wrap gap-2">
					{#each stats.sources ?? [] as source (source)}
						<button
							class="btn btn-outline btn-sm"
							onclick={() => clearCache(source)}
							disabled={clearMutation.isPending}
						>
							Clear {source}
						</button>
					{/each}
					<button
						class="btn btn-error btn-sm"
						onclick={() => clearCache(null)}
						disabled={clearMutation.isPending}
					>
						{#if clearMutation.isPending}
							<span class="loading loading-spinner loading-sm"></span>
						{/if}
						Clear everything
					</button>
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
	</div>
</div>
