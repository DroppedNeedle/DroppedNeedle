<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import ReviewQueue from '$lib/components/library/ReviewQueue.svelte';
	import UnconfirmedAlbums, {
		type WaitingView
	} from '$lib/components/library/UnconfirmedAlbums.svelte';
	import { ReviewFilters } from '$lib/components/library/review/reviewFilters.svelte';
	import { getLibraryStatsQuery } from '$lib/queries/library/LibraryQueries.svelte';
	import Tabs from '$lib/ui/Tabs.svelte';

	type Tab = WaitingView | 'reviews';

	// The address holds the open list: ?view=unmatched or ?view=looks_wrong
	// for those albums, the review list's view, filters and order (?review=<id>
	// opens one review first), and ?focus=<album id> for the row to come
	// back to after comparing an album's editions.
	const params = page.url.searchParams;
	const VIEWS: WaitingView[] = ['unconfirmed', 'unmatched', 'looks_wrong'];
	const asked = VIEWS.find((view) => view === params.get('view')) ?? 'unconfirmed';
	let tab = $state<Tab>(ReviewFilters.inAddress(params) ? 'reviews' : asked);
	const stats = getLibraryStatsQuery();

	const tabs = $derived([
		{ id: 'unconfirmed' as Tab, label: 'Best guess', count: stats.data?.guess_count },
		{ id: 'unmatched' as Tab, label: 'Not found', count: stats.data?.not_found_count },
		{ id: 'looks_wrong' as Tab, label: 'Looks wrong', count: stats.data?.looks_wrong_count },
		{ id: 'reviews' as Tab, label: 'Needs you', count: stats.data?.review_count }
	]);

	function show(next: Tab): void {
		tab = next;
		const url = new URL(page.url);
		url.searchParams.delete('focus');
		if (next === 'reviews') {
			url.searchParams.delete('view');
		} else {
			ReviewFilters.clearFrom(url);
			if (next === 'unconfirmed') url.searchParams.delete('view');
			else url.searchParams.set('view', next);
		}
		void goto(url, { replaceState: true, keepFocus: true, noScroll: true });
	}
</script>

<svelte:head><title>Check albums · DroppedNeedle</title></svelte:head>

<div class="min-h-[calc(100vh-200px)]">
	<PageHeader
		subtitle="We're not sure which edition these are. They still play. Files stay untouched until you say."
		backHref="/library"
		backLabel="Back to library"
	>
		{#snippet title()}Check albums{/snippet}
	</PageHeader>
	<div class="mx-auto max-w-[96rem] space-y-4 px-4 pb-12 sm:px-6 lg:px-8">
		<Tabs
			{tabs}
			selected={tab}
			onselect={show}
			label="Check albums"
			idPrefix="check-albums"
			look="border"
		>
			{#if tab === 'reviews'}
				<ReviewQueue />
			{:else}
				{#key tab}
					<UnconfirmedAlbums view={tab} focus={params.get('focus')} />
				{/key}
			{/if}
		</Tabs>
	</div>
</div>
