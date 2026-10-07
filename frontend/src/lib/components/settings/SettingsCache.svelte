<script lang="ts">
	import { ApiError } from '$lib/api/client';
	import {
		createClearCacheMutation,
		createPrecacheRunMutation,
		getCacheStatsQuery,
		type CacheClearBody
	} from '$lib/queries/settings/AdminCacheQueries.svelte';
	import { syncStatus } from '$lib/stores/syncStatus.svelte';

	const statsQuery = getCacheStatsQuery();
	const clearMutation = createClearCacheMutation();
	const precacheMutation = createPrecacheRunMutation();

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

	function formatBytes(bytes: number): string {
		if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
		if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
		return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
	}

	const PROMPTS: Record<string, string> = {
		all: 'Clear every cached provider response and image? They are fetched again on demand.',
		covers: 'Delete the cached album covers and artist images? They are fetched again on demand.'
	};

	async function clear(body: CacheClearBody, prompt: string) {
		if (!confirm(prompt)) return;
		try {
			const result = await clearMutation.mutateAsync(body);
			showMessage(result.message, 'success');
		} catch (error) {
			showMessage(error instanceof ApiError ? error.message : "Couldn't clear the cache", 'error');
		}
	}

	function clearSource(source: string) {
		return clear({ scope: 'source', source }, `Clear the cached ${source} responses?`);
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

<div class="card bg-base-200">
	<div class="card-body">
		<h2 class="card-title text-2xl mb-4">Cache management</h2>
		<p class="text-base-content/70 mb-6">
			Provider responses (MusicBrainz, Last.fm, ListenBrainz and the rest) are kept in memory, and
			album covers and artist images on disk. Anything cleared is fetched again on demand.
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
					<div class="stat-title">Cached provider responses</div>
					<div class="stat-value text-primary">{stats.entries}</div>
				</div>
				<div class="stat">
					<div class="stat-title">Cached images</div>
					<div class="stat-value text-primary">{stats.cover_images}</div>
					<div class="stat-desc">{formatBytes(stats.cover_bytes)} on disk</div>
				</div>
			</div>

			<div class="space-y-2 mb-6">
				<h3 class="text-xl font-semibold">Library images</h3>
				<p class="text-sm text-base-content/70">
					Fetch artist images, album covers and page details for your whole library now, so pages
					load fast later. It runs in the background and skips anything already cached.
				</p>
				<button
					class="btn btn-primary btn-sm"
					onclick={refreshImages}
					disabled={precacheMutation.isPending || syncStatus.isActive}
				>
					{#if precacheMutation.isPending}
						<span class="loading loading-spinner loading-sm"></span>
					{/if}
					{syncStatus.isActive ? 'Refresh running' : 'Refresh library images'}
				</button>
			</div>

			<div class="space-y-4">
				<h3 class="text-xl font-semibold">Clear cache</h3>
				<div class="flex flex-wrap gap-2">
					{#each stats.sources ?? [] as source (source)}
						<button
							class="btn btn-outline btn-sm"
							onclick={() => clearSource(source)}
							disabled={clearMutation.isPending}
						>
							Clear {source}
						</button>
					{/each}
					<button
						class="btn btn-outline btn-sm"
						onclick={() => clear({ scope: 'covers' }, PROMPTS.covers)}
						disabled={clearMutation.isPending}
					>
						Clear images
					</button>
					<button
						class="btn btn-error btn-sm"
						onclick={() => clear({ scope: 'all' }, PROMPTS.all)}
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
