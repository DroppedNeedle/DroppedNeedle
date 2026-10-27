<script lang="ts" module>
	export interface ActiveSummary {
		count: number;
		downloading: number;
		searching: number;
		fetching: boolean;
	}
</script>

<script lang="ts">
	import { SvelteSet } from 'svelte/reactivity';
	import { fade, fly } from '$lib/ui/motion';
	import { CircleCheck, TriangleAlert } from 'lucide-svelte';
	import RequestCard from '$lib/components/RequestCard.svelte';
	import type { RequestKind } from '$lib/queries/requests/types';
	import { getRequestDownloadsQuery } from '$lib/queries/downloads/DownloadQueries.svelte';
	import { getActiveRequestsQuery } from '$lib/queries/requests/RequestQueries.svelte';
	import {
		createBatchCancelRequestsMutation,
		createCancelRequestMutation
	} from '$lib/queries/requests/RequestMutations.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import Button from '$lib/ui/Button.svelte';
	import RequestsEmpty from './RequestsEmpty.svelte';
	import { itemKind, itemRequestKey, liveRequestStatus, requestKey } from './requestItems';

	interface Props {
		active: boolean;
		onsummary?: (summary: ActiveSummary) => void;
	}

	let { active, onsummary }: Props = $props();

	const activeListQuery = getActiveRequestsQuery(() => active);
	const activeItems = $derived(activeListQuery.data?.items ?? []);
	const activeLoading = $derived(activeListQuery.isPending);
	const activeError = $derived(activeListQuery.isError ? "Couldn't load active requests" : null);

	const tasksQuery = getRequestDownloadsQuery(() => active && activeItems.some((i) => i.task_id));
	const tasksById = $derived(new Map((tasksQuery.data?.items ?? []).map((t) => [t.id, t])));
	const liveStatusOf = (item: (typeof activeItems)[number]) => liveRequestStatus(item, tasksById);

	$effect(() => {
		onsummary?.({
			count: activeListQuery.data?.count ?? activeItems.length,
			downloading: activeItems.filter((i) => liveStatusOf(i) === 'downloading').length,
			searching: activeItems.filter((i) => {
				const status = liveStatusOf(i);
				return status === 'pending' || status === 'queued';
			}).length,
			fetching: activeListQuery.isFetching
		});
	});

	// Batch-cancel selection.
	let selectedKeys = $state<Set<string>>(new Set());
	const selectedList = $derived(activeItems.filter((i) => selectedKeys.has(itemRequestKey(i))));

	function handleSelect(mbid: string, requestKind: RequestKind, selected: boolean) {
		const next = new SvelteSet(selectedKeys);
		if (selected) next.add(requestKey(mbid, requestKind));
		else next.delete(requestKey(mbid, requestKind));
		selectedKeys = next;
	}

	const cancelRequestMutation = createCancelRequestMutation();
	const batchCancelMutation = createBatchCancelRequestsMutation();

	async function handleBatchCancel() {
		const albumMbids = selectedList
			.filter((i) => itemKind(i) === 'album')
			.map((i) => i.musicbrainz_id);
		const trackMbids = selectedList
			.filter((i) => itemKind(i) === 'track')
			.map((i) => i.musicbrainz_id);
		try {
			if (albumMbids.length > 0) {
				await batchCancelMutation.mutateAsync({ mbids: albumMbids, kind: 'album' });
			}
			if (trackMbids.length > 0) {
				await batchCancelMutation.mutateAsync({ mbids: trackMbids, kind: 'track' });
			}
			selectedKeys = new Set();
		} catch {
			// the mutation already toasted the failure
		}
	}

	// Drop batch-cancel selections for rows that settled since the last poll.
	$effect(() => {
		const live = new Set(activeItems.map(itemRequestKey));
		if (selectedKeys.size > 0 && ![...selectedKeys].every((key) => live.has(key))) {
			selectedKeys = new Set([...selectedKeys].filter((key) => live.has(key)));
		}
	});

	async function handleCancel(mbid: string, requestKind: RequestKind) {
		try {
			await cancelRequestMutation.mutateAsync({ mbid, kind: requestKind });
		} catch {
			// the mutation already toasted the failure
		}
	}
</script>

{#if active}
	<div in:fade={{ duration: 150 }} aria-live="polite">
		{#if activeError}
			<div class="alert alert-warning mb-4">
				<TriangleAlert class="h-5 w-5" />
				<span>{activeError}</span>
				<Button size="sm" onclick={() => void activeListQuery.refetch()}>Retry</Button>
			</div>
		{/if}

		{#if activeLoading && activeItems.length === 0}
			<div class="flex flex-col gap-2.5">
				{#each Array(3) as _, i (`active-loading-${i}`)}
					<div
						class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4 bg-base-200 rounded-box animate-pulse"
						style:animation-delay="{i * 100}ms"
					>
						<div class="w-14 h-14 sm:w-18 sm:h-18 bg-base-300 rounded-lg"></div>
						<div class="flex-1">
							<div class="h-4 bg-base-300 rounded w-44 mb-2"></div>
							<div class="h-3 bg-base-300 rounded w-28 mb-1"></div>
							<div class="h-2.5 bg-base-300 rounded w-20"></div>
						</div>
						<div class="flex flex-col items-end gap-2">
							<div class="h-5 bg-base-300 rounded-full w-24"></div>
							<div class="h-1.5 bg-base-300 rounded w-36"></div>
						</div>
					</div>
				{/each}
			</div>
		{:else if activeItems.length === 0}
			<RequestsEmpty icon={CircleCheck} tone="success" title="All clear">
				No active downloads. Search for albums and request them to see progress here.
			</RequestsEmpty>
		{:else}
			{#if selectedList.length > 0}
				<div class="flex items-center gap-2 mb-3">
					<span class="text-xs text-base-content/60">
						{selectedList.length} selected
					</span>
					<Button
						variant="danger-ghost"
						size="sm"
						disabled={batchCancelMutation.isPending}
						onclick={() => void handleBatchCancel()}
					>
						Cancel selected
					</Button>
					<Button variant="ghost" size="sm" onclick={() => (selectedKeys = new Set())}>
						Clear
					</Button>
				</div>
			{/if}
			<div class="flex flex-col gap-2.5">
				{#each activeItems as item, i (itemRequestKey(item))}
					<div in:fly={{ y: 12, duration: 200, delay: i * 30 }}>
						<RequestCard
							{item}
							mode="active"
							liveStatus={liveStatusOf(item)}
							selectable
							selected={selectedKeys.has(itemRequestKey(item))}
							onselect={handleSelect}
							oncancel={authStore.isAdmin || item.user_id === authStore.user?.id
								? handleCancel
								: undefined}
						/>
					</div>
				{/each}
			</div>
		{/if}
	</div>
{/if}
