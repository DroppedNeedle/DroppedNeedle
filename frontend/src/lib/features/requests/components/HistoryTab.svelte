<script lang="ts">
	import { SvelteMap } from 'svelte/reactivity';
	import { fade } from '$lib/ui/motion';
	import { Clock } from 'lucide-svelte';
	import Pagination from '$lib/ui/Pagination.svelte';
	import RequestCard from '$lib/features/requests/components/RequestCard.svelte';
	import { getRequestHistoryQuery } from '$lib/features/requests/queries/RequestQueries.svelte';
	import {
		createClearHistoryMutation,
		createRetryRequestMutation
	} from '$lib/features/requests/queries/RequestMutations.svelte';
	import type { RequestItem, RequestKind } from '$lib/features/requests/queries/types';
	import { getWantedWatchesQuery } from '$lib/features/requests/queries/wanted.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import RequestsAlert from './RequestsAlert.svelte';
	import RequestsEmpty from './RequestsEmpty.svelte';
	import RequestSkeleton from './RequestSkeleton.svelte';
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
		<div class="dn-toolbar">
			<select
				class="select dn-toolbar__select"
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
				{#if authStore.isAdmin}
					<option value="reimportable">Can reimport</option>
				{/if}
			</select>

			<select
				class="select dn-toolbar__select"
				aria-label="Sort order"
				value={historySort ?? ''}
				onchange={handleSortChange}
			>
				<option value="">Newest first</option>
				<option value="oldest">Oldest first</option>
				<option value="status">By status</option>
			</select>

			{#if historyTotalPages > 1}
				<div class="ml-auto">
					<Pagination
						current={historyPage}
						total={historyTotalPages}
						onchange={handleHistoryPageChange}
					/>
				</div>
			{/if}
		</div>

		{#if historyError}
			<RequestsAlert
				message={historyError}
				tone="error"
				onretry={() => void historyQuery.refetch()}
			/>
		{/if}

		{#if historyLoading && historyItems.length === 0}
			<RequestSkeleton count={5} />
		{:else if historyItems.length === 0}
			<RequestsEmpty icon={Clock} tone="muted" title="No history yet">
				Completed and failed requests will appear here.
			</RequestsEmpty>
		{:else}
			<div class="flex flex-col gap-4">
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
				<div class="mt-8 flex justify-center">
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
