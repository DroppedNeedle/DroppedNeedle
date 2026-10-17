<script lang="ts">
	import { page } from '$app/state';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import ReviewQueue from '$lib/components/library/ReviewQueue.svelte';
	import UnconfirmedAlbums from '$lib/components/library/UnconfirmedAlbums.svelte';
	import { getPendingReviewCountQuery } from '$lib/queries/library/LibraryReviewQueries.svelte';
	import type { LibraryReviewState } from '$lib/queries/library/LibraryQueryKeyFactory';

	// Links open a list directly: ?view=unmatched for albums nothing fits,
	// and for reviews ?review=<id>, ?state=needs_review (the old name for
	// pending) and ?reason=<code>.
	const params = page.url.searchParams;
	const focusReviewId = params.get('review');
	const rawState = params.get('state');
	const initialState: LibraryReviewState =
		rawState === 'excluded' || rawState === 'all' ? rawState : 'pending';
	const initialReason = params.get('reason') ?? '';
	const initialView = params.get('view') === 'unmatched' ? 'unmatched' : 'unconfirmed';
	let tab = $state<'unconfirmed' | 'reviews'>(
		focusReviewId || rawState || initialReason ? 'reviews' : 'unconfirmed'
	);
	const pending = getPendingReviewCountQuery();
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
				onclick={() => (tab = 'unconfirmed')}>Best guesses and no match</button
			>
			<button
				role="tab"
				class="tab gap-2 {tab === 'reviews' ? 'tab-active' : ''}"
				aria-selected={tab === 'reviews'}
				onclick={() => (tab = 'reviews')}
				>Needs a decision
				{#if pending.data}<span class="badge badge-warning badge-sm">{pending.data}</span>{/if}
			</button>
		</div>
		{#if tab === 'reviews'}
			<ReviewQueue {focusReviewId} {initialState} {initialReason} />
		{:else}
			<UnconfirmedAlbums {initialView} />
		{/if}
	</div>
</div>
