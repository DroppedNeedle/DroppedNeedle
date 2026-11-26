<script lang="ts">
	import DiscoverTabs from '$lib/features/discover/components/DiscoverTabs.svelte';
	import { untrack } from 'svelte';
	import { page } from '$app/state';
	import { prefersReducedMotion } from 'svelte/motion';
	import { CalendarDays, Search } from 'lucide-svelte';
	import PastWeekShelf from '$lib/features/discover/components/PastWeekShelf.svelte';
	import {
		getPastWeeksQuery,
		type PickAnswer,
		type WeeksRecord
	} from '$lib/features/discover/queries/weeks.svelte';
	import { dayMonth } from '$lib/features/discover/state/weekWords';
	import Button from '$lib/ui/Button.svelte';
	import CarouselSkeleton from '$lib/ui/CarouselSkeleton.svelte';
	import EmptyState from '$lib/ui/EmptyState.svelte';
	import Input from '$lib/ui/Input.svelte';
	import ListPageHeader from '$lib/ui/ListPageHeader.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import StatRow from '$lib/ui/StatRow.svelte';
	import Tabs, { type TabItem } from '$lib/ui/Tabs.svelte';

	type Show = 'all' | PickAnswer;
	const TABS: TabItem<Show>[] = [
		{ id: 'all', label: 'All' },
		{ id: 'kept', label: 'Kept' },
		{ id: 'later', label: 'Later' },
		{ id: 'passed', label: 'Passed' },
		{ id: 'open', label: 'Never answered' }
	];

	let show = $state<Show>('all');
	let typed = $state('');
	let search = $state('');
	let typing: ReturnType<typeof setTimeout> | undefined;
	function onsearch(value: string) {
		typed = value;
		clearTimeout(typing);
		typing = setTimeout(() => (search = value.trim()), 300);
	}

	const list = getPastWeeksQuery(() => ({ state: show === 'all' ? null : show, search }));
	const weeks = $derived(list.data?.pages.flatMap((page) => page.items) ?? []);
	// The record comes with the first page of the list as it is first read.
	let record = $state<WeeksRecord | null>(null);
	$effect(() => {
		const first = list.data?.pages[0]?.record;
		if (first) record = first;
	});
	const narrowed = $derived(show !== 'all' || search !== '');

	const stats = $derived(
		record
			? [
					{ label: 'Picked', value: record.picked },
					{ label: 'Kept', value: record.answers.kept },
					{ label: 'You knew', value: record.answers.known },
					{ label: 'Later', value: record.answers.later },
					{ label: 'Passed', value: record.answers.passed },
					{ label: 'Open', value: record.answers.open }
				]
			: []
	);

	// A link to one week ("Found on Discover, week 47") reads on until that
	// week is listed, then brings it into view once.
	const wanted = $derived(Number(page.url.searchParams.get('week')) || null);
	let shown = false;
	$effect(() => {
		if (!wanted || shown || narrowed) return;
		const listed = weeks.some((week) => week.week_start_at === wanted);
		untrack(() => {
			if (listed) {
				shown = true;
				requestAnimationFrame(() =>
					document.getElementById(`week-${wanted}`)?.scrollIntoView({
						behavior: prefersReducedMotion.current ? 'auto' : 'smooth',
						block: 'start'
					})
				);
			} else if (list.hasNextPage && !list.isFetchingNextPage) {
				void list.fetchNextPage();
			}
		});
	});
</script>

<svelte:head>
	<title>Past weeks - DroppedNeedle</title>
</svelte:head>

<ListPageHeader title="Past weeks" ticket="DN / CC 3.3" lede="Every set, kept as it was.">
	{#snippet below()}
		<DiscoverTabs current="weeks" />
	{/snippet}
</ListPageHeader>

<div class="dn-weeks-page">
	{#if record && record.picked > 0}
		<section class="dn-weeks-page__record" aria-label="What your sets came to">
			<span class="dn-weeks-page__caps">So far</span>
			<StatRow {stats} label="Your picks so far" phoneRows class="dn-weeks-page__stats" />
			{#if record.counted_from_at}
				<span class="dn-weeks-page__caps dn-weeks-page__note">
					Still played a month on: counted from {dayMonth(record.counted_from_at)}
				</span>
			{/if}
		</section>
	{/if}

	<Tabs
		tabs={TABS}
		selected={show}
		onselect={(id) => (show = id)}
		label="Show"
		idPrefix="past-weeks"
		scroll
		class="dn-weeks-page__filters"
	>
		{#snippet aside()}
			<Input
				type="search"
				label="Search every week"
				placeholder="Search every week"
				icon={Search}
				value={typed}
				oninput={(event) => onsearch(event.currentTarget.value)}
				class="dn-weeks-page__find"
			/>
		{/snippet}
		<QueryBoundary query={list} what="your past weeks" isEmpty={() => weeks.length === 0}>
			{#snippet loading()}
				<CarouselSkeleton />
			{/snippet}
			{#snippet empty()}
				<EmptyState
					icon={CalendarDays}
					title={narrowed ? 'No picks match' : 'No past weeks yet'}
					description={narrowed ? 'Try another filter or search.' : 'Past weeks will be kept here.'}
				/>
			{/snippet}
			{#snippet children(data)}
				<div class="dn-weeks-page__weeks">
					{#each data.pages.flatMap((one) => one.items) as week (week.week_start_at)}
						<div id="week-{week.week_start_at}" class="scroll-mt-24">
							<PastWeekShelf {week} />
						</div>
					{/each}
				</div>
				{#if list.hasNextPage}
					<div class="dn-weeks-page__more">
						<Button
							variant="outline"
							size="sm"
							loading={list.isFetchingNextPage}
							onclick={() => list.fetchNextPage()}
						>
							Show more weeks
						</Button>
					</div>
				{:else if !narrowed}
					<p class="dn-weeks-page__end">That is every week.</p>
				{/if}
			{/snippet}
		</QueryBoundary>
	</Tabs>
</div>

<style>
	.dn-weeks-page {
		max-width: 88rem;
		padding: 0.5rem 2rem 4rem;
	}
	.dn-weeks-page__record {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.75rem 1.5rem;
		margin-bottom: 2rem;
		padding: 1rem 1.25rem;
		border: 1px solid var(--dn-line);
		border-radius: 10px;
	}
	.dn-weeks-page__caps {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.16em;
		text-transform: uppercase;
	}
	.dn-weeks-page__note {
		margin-left: auto;
		color: var(--dn-muted);
	}
	.dn-weeks-page__record :global(.dn-weeks-page__stats) {
		flex: 0 1 auto;
	}
	.dn-weeks-page :global(.dn-weeks-page__filters) {
		margin-bottom: 2.5rem;
		justify-content: space-between;
	}
	.dn-weeks-page :global(.dn-weeks-page__find) {
		width: min(100%, 19rem);
	}
	.dn-weeks-page__weeks {
		display: flex;
		flex-direction: column;
		gap: 3rem;
	}
	.dn-weeks-page__more {
		display: flex;
		justify-content: center;
		margin-top: 2rem;
	}
	.dn-weeks-page__end {
		margin: 2.5rem 0 0;
		text-align: center;
		font-family: var(--font-display);
		font-size: 1.5rem;
		text-transform: uppercase;
	}
	@media (max-width: 40rem) {
		.dn-weeks-page {
			padding: 0.5rem 1rem 3rem;
		}
		.dn-weeks-page__note {
			margin-left: 0;
		}
		.dn-weeks-page__record :global(.dn-weeks-page__stats) {
			flex-basis: 100%;
		}
		.dn-weeks-page :global(.dn-weeks-page__find) {
			width: 100%;
		}
	}
</style>
