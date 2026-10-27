<script lang="ts">
	import { SvelteMap } from 'svelte/reactivity';
	import { fade } from '$lib/ui/motion';
	import { Clock } from 'lucide-svelte';
	import Pagination from '$lib/components/Pagination.svelte';
	import RequestCard from '$lib/components/RequestCard.svelte';
	import { getRequestHistoryQuery } from '$lib/queries/requests/RequestQueries.svelte';
	import {
		createClearHistoryMutation,
		createRetryRequestMutation
	} from '$lib/queries/requests/RequestMutations.svelte';
	import type { RequestItem, RequestKind } from '$lib/queries/requests/types';
	import { getWantedWatchesQuery } from '$lib/queries/wanted/WantedQuery.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import Button from '$lib/ui/Button.svelte';
	import RequestsEmpty from './RequestsEmpty.svelte';
	import { itemKind, itemRequestKey } from './requestItems';

	interface Props {
		active: boolean;
		oncount?: (count: number) => void;
	}

	let { active, oncount }: Props = $props();

	let historyPage = $state(1);
	const historyPageSize = 20;
	let historyFilter = $state<string | undefined>(undefined);
	let historySort = $state<string | undefined>(undefined);
	const historyQuery = getRequestHistoryQuery(
		() => ({
			page: historyPage,
			pageSize: historyPageSize,
			status: historyFilter,
			sort: historySort
		}),
		() => active
	);
	const historyItems = $derived<RequestItem[]>(historyQuery.data?.items ?? []);
	const historyTotalPages = $derived(historyQuery.data?.total_pages ?? 1);
	const historyLoading = $derived(historyQuery.isPending);
	const historyError = $derived(historyQuery.isError ? "Couldn't load request history" : null);

	$effect(() => oncount?.(historyQuery.data?.total ?? 0));

	// Failed rows that are still being worked on show a chip instead of reading as dead.
	const wantedQuery = getWantedWatchesQuery(() => active);
	const wantedStates = $derived.by(() => {
		const map = new SvelteMap<string, 'retrying' | 'watching'>();
		for (const entry of wantedQuery.data?.retrying ?? []) {
			map.set(entry.musicbrainz_id.toLowerCase(), 'retrying');
		}
		for (const watch of wantedQuery.data?.items ?? []) {
			if (watch.state === 'watching') {
				map.set(watch.musicbrainz_id.toLowerCase(), 'watching');
			}
		}
		return map;
	});

	const retryRequestMutation = createRetryRequestMutation();
	const clearHistoryMutation = createClearHistoryMutation();

	async function handleRetry(mbid: string, requestKind: RequestKind) {
		try {
			await retryRequestMutation.mutateAsync({ mbid, kind: requestKind });
		} catch {
			// the mutation already toasted the failure
		}
	}

	async function handleClear(mbid: string, requestKind: RequestKind) {
		try {
			await clearHistoryMutation.mutateAsync({ mbid, kind: requestKind });
		} catch {
			// the mutation already toasted the failure
		}
	}

	function handleRemoved() {
		void historyQuery.refetch();
	}

	function handleHistoryPageChange(page: number) {
		historyPage = page;
	}

	function handleFilterChange(e: Event) {
		const value = (e.target as HTMLSelectElement).value;
		historyFilter = value || undefined;
		historyPage = 1;
	}

	function handleSortChange(e: Event) {
		const value = (e.target as HTMLSelectElement).value;
		historySort = value || undefined;
		historyPage = 1;
	}
</script>

{#if active}
	<div in:fade={{ duration: 150 }}>
		<div class="flex flex-wrap items-center gap-2 mb-4">
			<select
				class="select select-sm"
				aria-label="Filter by status"
				value={historyFilter ?? ''}
				onchange={handleFilterChange}
			>
				<option value="">All statuses</option>
				<option value="imported">Imported</option>
				<option value="incomplete">Incomplete</option>
				<option value="failed">Failed</option>
				<option value="cancelled">Cancelled</option>
				<option value="rejected">Rejected</option>
			</select>

			<select
				class="select select-sm"
				aria-label="Sort order"
				value={historySort ?? ''}
				onchange={handleSortChange}
			>
				<option value="">Newest first</option>
				<option value="oldest">Oldest first</option>
				<option value="status">By status</option>
			</select>

			<div class="flex-1"></div>

			{#if historyTotalPages > 1}
				<Pagination
					current={historyPage}
					total={historyTotalPages}
					onchange={handleHistoryPageChange}
				/>
			{/if}
		</div>

		{#if historyError}
			<div class="alert alert-error mb-4">
				<span>{historyError}</span>
				<Button size="sm" onclick={() => void historyQuery.refetch()}>Retry</Button>
			</div>
		{/if}

		{#if historyLoading && historyItems.length === 0}
			<div class="flex flex-col gap-2.5">
				{#each Array(5) as _, i (`history-loading-${i}`)}
					<div
						class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4 bg-base-200 rounded-box animate-pulse"
						style:animation-delay="{i * 80}ms"
					>
						<div class="w-14 h-14 sm:w-18 sm:h-18 bg-base-300 rounded-lg"></div>
						<div class="flex-1">
							<div class="h-4 bg-base-300 rounded w-44 mb-2"></div>
							<div class="h-3 bg-base-300 rounded w-28"></div>
						</div>
						<div class="flex flex-col items-end gap-2">
							<div class="h-5 bg-base-300 rounded-full w-20"></div>
							<div class="h-3 bg-base-300 rounded w-28"></div>
						</div>
					</div>
				{/each}
			</div>
		{:else if historyItems.length === 0}
			<RequestsEmpty icon={Clock} tone="muted" title="No history yet">
				Completed and failed requests will appear here.
			</RequestsEmpty>
		{:else}
			<div class="flex flex-col gap-2.5">
				{#each historyItems as item (itemRequestKey(item))}
					<RequestCard
						{item}
						mode="history"
						watchState={itemKind(item) === 'album' &&
						['failed', 'incomplete', 'cancelled'].includes(item.status)
							? wantedStates.get(item.musicbrainz_id.toLowerCase())
							: undefined}
						onretry={authStore.isAdmin || item.user_id === authStore.user?.id
							? handleRetry
							: undefined}
						onclear={handleClear}
						onremoved={handleRemoved}
					/>
				{/each}
			</div>

			{#if historyTotalPages > 1}
				<div class="flex justify-center mt-6">
					<Pagination
						current={historyPage}
						total={historyTotalPages}
						onchange={handleHistoryPageChange}
					/>
				</div>
			{/if}
		{/if}
	</div>
{/if}
