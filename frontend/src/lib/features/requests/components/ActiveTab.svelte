<script lang="ts">
	import { SvelteSet } from 'svelte/reactivity';
	import { fade, fly } from '$lib/ui/motion';
	import { CircleCheck, CircleX } from 'lucide-svelte';
	import RequestCard from '$lib/features/requests/components/RequestCard.svelte';
	import type { RequestKind } from '$lib/features/requests/queries/types';
	import { getActiveRequestsQuery } from '$lib/features/requests/queries/RequestQueries.svelte';
	import {
		createBatchCancelRequestsMutation,
		createCancelRequestMutation
	} from '$lib/features/requests/queries/RequestMutations.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import Button from '$lib/ui/Button.svelte';
	import RequestsAlert from './RequestsAlert.svelte';
	import RequestsEmpty from './RequestsEmpty.svelte';
	import RequestSkeleton from './RequestSkeleton.svelte';
	import { itemKind, itemRequestKey, requestKey } from './requestItems';

	interface Props {
		active: boolean;
	}

	let { active }: Props = $props();

	const activeListQuery = getActiveRequestsQuery(() => active);
	const activeItems = $derived(activeListQuery.data?.items ?? []);
	const activeLoading = $derived(activeListQuery.isPending);
	const activeError = $derived(activeListQuery.isError ? "Couldn't load active requests" : null);

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

	const allSelected = $derived(
		activeItems.length > 0 && selectedList.length === activeItems.length
	);

	function toggleAll() {
		selectedKeys = allSelected ? new Set() : new Set(activeItems.map(itemRequestKey));
	}

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
			<RequestsAlert message={activeError} onretry={() => void activeListQuery.refetch()} />
		{/if}

		{#if activeLoading && activeItems.length === 0}
			<RequestSkeleton count={3} />
		{:else if activeItems.length === 0}
			<RequestsEmpty icon={CircleCheck} tone="success" title="All clear">
				No active downloads. Search for albums and request them to see progress here.
			</RequestsEmpty>
		{:else}
			<div class="rq-select">
				<label class="rq-select__all">
					<input
						type="checkbox"
						class="checkbox"
						checked={allSelected}
						indeterminate={selectedList.length > 0 && !allSelected}
						onchange={toggleAll}
						aria-label="Select all"
					/>
					<span class="rq-select__count">{selectedList.length} selected</span>
				</label>
				<span class="rq-select__rule" aria-hidden="true"></span>
				<Button
					variant="outline"
					size="display-sm"
					icon={CircleX}
					disabled={selectedList.length === 0 || batchCancelMutation.isPending}
					onclick={() => void handleBatchCancel()}
				>
					Cancel selected
				</Button>
				<Button
					variant="outline"
					size="display-sm"
					disabled={selectedList.length === 0}
					onclick={() => (selectedKeys = new Set())}
				>
					Clear
				</Button>
			</div>
			<div class="flex flex-col gap-4">
				{#each activeItems as item, i (itemRequestKey(item))}
					<div in:fly={{ y: 12, duration: 200, delay: i * 30 }}>
						<RequestCard
							{item}
							mode="active"
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

<style>
	/* the batch row: a tick for every row, the count, then the batch buttons */
	.rq-select {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.75rem 1.25rem;
		margin-bottom: 1.25rem;
		padding-left: 1.625rem;
	}
	.rq-select__all {
		display: inline-flex;
		align-items: center;
		gap: 1rem;
		cursor: pointer;
	}
	.rq-select__count {
		font-family: var(--font-mono);
		font-size: 0.875rem;
		font-weight: 600;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		white-space: nowrap;
	}
	.rq-select__rule {
		width: 1px;
		height: 1.75rem;
		background-color: var(--dn-line);
	}
	@media (max-width: 767px) {
		.rq-select {
			gap: 0.5rem 0.75rem;
			padding-left: 0.875rem;
		}
		.rq-select__all {
			gap: 0.625rem;
		}
		.rq-select__count {
			font-size: 0.75rem;
			letter-spacing: 0.06em;
		}
		.rq-select__rule {
			display: none;
		}
		.rq-select :global(.btn) {
			--size: 2.25rem;
			--fontsize: 0.875rem;
			--btn-p: 0.625rem;
			gap: 0.375rem;
		}
	}
</style>
