<!--
	Past weeks under Discover: the newest week before the one on the counter,
	as a shelf, and a way to every week. Before there is one, a quiet line
	says where they will be kept.
-->
<script lang="ts">
	import CarouselSkeleton from '$lib/ui/CarouselSkeleton.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { getPastWeeksQuery, type PastWeek } from '$lib/features/discover/queries/weeks.svelte';
	import PastWeekShelf from './PastWeekShelf.svelte';

	interface Props {
		/** When the week on the counter starts; that week is not a past one. */
		current: number | null;
		/** Read only once the counter's week is known. */
		enabled?: boolean;
	}

	let { current, enabled = true }: Props = $props();

	const weeks = getPastWeeksQuery(undefined, () => enabled);
	const query = $derived({
		data: weeks.data
			? (weeks.data.pages
					.flatMap((page) => page.items)
					.find((week: PastWeek) => week.week_start_at !== current) ?? null)
			: undefined,
		isPending: weeks.isPending,
		isError: weeks.isError,
		error: weeks.error,
		refetch: () => weeks.refetch()
	});
</script>

<section class="dn-past-weeks" aria-label="Past weeks">
	<div class="dn-past-weeks__head">
		<SectionHead title="Past weeks" href={withBasePath('/discover/weeks')} moreLabel="All weeks" />
		<p class="dn-past-weeks__lede">Every week is kept, with what you did</p>
	</div>
	<QueryBoundary {query} what="your past weeks">
		{#snippet loading()}
			<CarouselSkeleton />
		{/snippet}
		{#snippet empty()}
			<p class="dn-past-weeks__none">Past weeks will be kept here.</p>
		{/snippet}
		{#snippet children(week)}
			{#if week}<PastWeekShelf {week} />{/if}
		{/snippet}
	</QueryBoundary>
</section>

<style>
	.dn-past-weeks__head {
		margin-bottom: 1.25rem;
	}
	.dn-past-weeks__lede {
		margin: 0.25rem 0 0;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.16em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.dn-past-weeks__none {
		margin: 0;
		color: var(--dn-muted);
	}
</style>
