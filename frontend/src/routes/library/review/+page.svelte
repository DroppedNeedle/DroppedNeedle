<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import ReviewQueue from '$lib/components/library/ReviewQueue.svelte';
	import UnconfirmedAlbums from '$lib/components/library/UnconfirmedAlbums.svelte';
	import { ReviewFilters } from '$lib/components/library/review/reviewFilters.svelte';
	import { getPendingReviewCountQuery } from '$lib/queries/library/LibraryReviewQueries.svelte';

	// The address holds the open list: ?view=unmatched for albums nothing
	// fits, and the review list's view, filters and order (?review=<id>
	// opens one review first).
	const params = page.url.searchParams;
	const initialView = params.get('view') === 'unmatched' ? 'unmatched' : 'unconfirmed';
	let tab = $state<'unconfirmed' | 'reviews'>(
		ReviewFilters.inAddress(params) ? 'reviews' : 'unconfirmed'
	);
	const pending = getPendingReviewCountQuery();

	function address(change: (url: URL) => void): void {
		const url = new URL(page.url);
		change(url);
		void goto(url, { replaceState: true, keepFocus: true, noScroll: true });
	}

	function showUnconfirmed(): void {
		tab = 'unconfirmed';
		address((url) => ReviewFilters.clearFrom(url));
	}

	function showReviews(): void {
		tab = 'reviews';
		address((url) => url.searchParams.delete('view'));
	}
</script>

<svelte:head><title>Unconfirmed matches · DroppedNeedle</title></svelte:head>

<div class="min-h-[calc(100vh-200px)]">
	<PageHeader
		subtitle="Albums whose edition is a best guess, that match nothing yet, or that need a decision"
		backHref="/library"
		backLabel="Back to library"
	>
		{#snippet title()}Unconfirmed matches{/snippet}
	</PageHeader>
	<div class="mx-auto max-w-[96rem] space-y-4 px-4 pb-12 sm:px-6 lg:px-8">
		<div role="tablist" class="tabs tabs-border w-fit">
			<button
				role="tab"
				class="tab {tab === 'unconfirmed' ? 'tab-active' : ''}"
				aria-selected={tab === 'unconfirmed'}
				onclick={showUnconfirmed}>Best guesses and no match</button
			>
			<button
				role="tab"
				class="tab gap-2 {tab === 'reviews' ? 'tab-active' : ''}"
				aria-selected={tab === 'reviews'}
				onclick={showReviews}
				>Reviews
				{#if pending.data}<span class="badge badge-warning badge-sm">{pending.data}</span>{/if}
			</button>
		</div>
		{#if tab === 'reviews'}
			<ReviewQueue />
		{:else}
			<UnconfirmedAlbums
				{initialView}
				onview={(view) =>
					address((url) =>
						view === 'unmatched'
							? url.searchParams.set('view', view)
							: url.searchParams.delete('view')
					)}
			/>
		{/if}
	</div>
</div>
