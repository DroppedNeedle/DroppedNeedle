<script lang="ts">
	import { resolve } from '$app/paths';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { SvelteURLSearchParams } from 'svelte/reactivity';
	import LibraryReviewFilters from './LibraryReviewFilters.svelte';
	import LibraryReviewTable from './LibraryReviewTable.svelte';
	import LibraryReviewDetail from './LibraryReviewDetail.svelte';
	import LibraryBulkActionDialog from './LibraryBulkActionDialog.svelte';
	import { getLibraryReviewsQuery } from '$lib/queries/library/LibraryReviewQueries.svelte';
	import { getLibraryPolicyTreeQuery } from '$lib/queries/library/LibraryPolicyQueries.svelte';
	import type { LibraryReviewFilters as Filters } from '$lib/queries/library/LibraryReviewQueries.svelte';

	const filters = $derived<Filters>({
		cursor: page.url.searchParams.get('cursor') ?? undefined,
		state:
			page.url.searchParams.get('state') === 'all'
				? undefined
				: (page.url.searchParams.get('state') ?? 'needs_review'),
		reasonCode: page.url.searchParams.get('reason') ?? undefined,
		rootId: page.url.searchParams.get('root') ?? undefined,
		policy: page.url.searchParams.get('policy') ?? undefined,
		search: page.url.searchParams.get('q') ?? undefined,
		sort: page.url.searchParams.get('sort') ?? 'newest'
	});
	const query = getLibraryReviewsQuery(() => filters);
	const policyTree = getLibraryPolicyTreeQuery();
	const response = $derived(query.data?.pages[0]);
	const items = $derived(response?.items ?? []);
	let selectedIds = $state<string[]>([]);
	let allMatching = $state(false);
	const reviewId = $derived(page.url.searchParams.get('review'));
	const selected = $derived(items.filter((item) => selectedIds.includes(item.id)));
	const rootLabels = $derived(
		Object.fromEntries((policyTree.data?.roots ?? []).map((root) => [root.id, root.label]))
	);
	const filtered = $derived(
		Boolean(
			filters.search ||
			filters.reasonCode ||
			filters.rootId ||
			filters.policy ||
			filters.state !== 'needs_review'
		)
	);

	function reviewHref(params: SvelteURLSearchParams): string {
		return params.size
			? resolve(`/library/review?${params.toString()}`)
			: resolve('/library/review');
	}

	function updateUrl(next: Filters): void {
		const params = new SvelteURLSearchParams();
		if (next.cursor) params.set('cursor', next.cursor);
		params.set('state', next.state ?? 'all');
		if (next.reasonCode) params.set('reason', next.reasonCode);
		if (next.rootId) params.set('root', next.rootId);
		if (next.policy) params.set('policy', next.policy);
		if (next.search) params.set('q', next.search);
		if (next.sort && next.sort !== 'newest') params.set('sort', next.sort);
		void goto(reviewHref(params), {
			noScroll: true,
			keepFocus: true
		});
		selectedIds = [];
		allMatching = false;
	}

	function openReview(id: string): void {
		const params = new SvelteURLSearchParams(page.url.searchParams);
		params.set('review', id);
		void goto(reviewHref(params), { noScroll: true, keepFocus: true });
	}

	function closeReview(): void {
		const params = new SvelteURLSearchParams(page.url.searchParams);
		params.delete('review');
		void goto(reviewHref(params), {
			noScroll: true,
			keepFocus: true,
			replaceState: true
		});
	}
</script>

<LibraryReviewFilters {filters} roots={policyTree.data?.roots ?? []} onchange={updateUrl} />

{#if query.isLoading}
	<div class="mt-4 space-y-2">
		<div class="skeleton h-16"></div>
		<div class="skeleton h-16"></div>
		<div class="skeleton h-16"></div>
	</div>
{:else if query.isError}
	<div class="alert alert-error mt-4">Could not load identification reviews.</div>
{:else}
	<div class="mt-4">
		{#if items.length}
			<div class="mb-3 flex flex-wrap items-center gap-2 text-sm">
				<button
					class="btn btn-ghost btn-sm"
					onclick={() => {
						selectedIds = items.map((item) => item.id);
						allMatching = false;
					}}>Select current page</button
				>
				{#if (response?.filtered_total ?? 0) > items.length}
					<button
						class="btn btn-ghost btn-sm"
						onclick={() => {
							selectedIds = [];
							allMatching = true;
						}}>Select all {(response?.filtered_total ?? 0).toLocaleString()} matching</button
					>
				{/if}
				{#if allMatching}<span class="badge badge-primary badge-outline"
						>Full filtered result selected</span
					>{/if}
			</div>
		{/if}
		<LibraryReviewTable
			{items}
			{selectedIds}
			{filtered}
			state={filters.state}
			{rootLabels}
			onselectionchange={(ids) => {
				selectedIds = ids;
				allMatching = false;
			}}
			onreview={openReview}
		/>
	</div>
	{#if response}
		<div class="mt-4 flex items-center justify-between gap-3 text-sm">
			<span class="text-base-content/55"
				>{response.filtered_total.toLocaleString()} review items</span
			>
			<div class="join">
				{#if filters.cursor}<button
						class="btn btn-sm join-item"
						onclick={() => updateUrl({ ...filters, cursor: undefined })}>First page</button
					>{/if}{#if response.next_cursor}<button
						class="btn btn-sm join-item"
						onclick={() => updateUrl({ ...filters, cursor: response.next_cursor ?? undefined })}
						>Next page</button
					>{/if}
			</div>
		</div>
		<LibraryBulkActionDialog
			{selected}
			{allMatching}
			matchingCount={response.filtered_total}
			{filters}
			catalogRevision={response.catalog_revision}
			onclear={() => {
				selectedIds = [];
				allMatching = false;
			}}
		/>
	{/if}
{/if}

<LibraryReviewDetail {reviewId} onclose={closeReview} />
