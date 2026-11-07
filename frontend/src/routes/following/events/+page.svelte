<script lang="ts">
	import { onMount } from 'svelte';
	import { CalendarClock, Settings, Users } from 'lucide-svelte';
	import EventCityManager from '$lib/features/following/components/EventCityManager.svelte';
	import EventRow from '$lib/features/following/components/EventRow.svelte';
	import EventsSetup from '$lib/features/following/components/EventsSetup.svelte';
	import {
		getConcertsQuery,
		getEventCitiesQuery
	} from '$lib/features/following/queries/FollowQueries.svelte';
	import {
		createMarkConcertsSeenMutation,
		createReplaceEventCitiesMutation
	} from '$lib/features/following/queries/FollowMutations.svelte';
	import type { CitySearchResult, Concert } from '$lib/features/following/queries/types';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import ListPageHeader from '$lib/ui/ListPageHeader.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';

	const concertsQuery = getConcertsQuery();
	const citiesQuery = getEventCitiesQuery();

	const cities = $derived(citiesQuery.data?.items ?? []);
	const concerts = $derived(concertsQuery.data?.items ?? []);
	const configured = $derived(concertsQuery.data?.configured ?? true);
	const loading = $derived(concertsQuery.isPending || citiesQuery.isPending);

	// visiting this page is what clears the sidebar concerts badge
	const markSeen = createMarkConcertsSeenMutation();
	onMount(() => markSeen.mutate(undefined));

	const replaceCities = createReplaceEventCitiesMutation();
	function addFirstCity(picked: CitySearchResult) {
		replaceCities.mutate([
			{
				city_name: picked.name,
				latitude: picked.latitude,
				longitude: picked.longitude,
				radius_km: 32, // 20 mi default
				country_code: picked.country_code
			}
		]);
	}

	function parseLocalDate(value: string): Date {
		const [year, month, day] = value.split('-').map(Number);
		return new Date(year, (month || 1) - 1, day || 1);
	}

	interface MonthGroup {
		label: string;
		items: Concert[];
	}

	const monthGroups = $derived.by((): MonthGroup[] => {
		const groups: MonthGroup[] = [];
		for (const concert of concerts) {
			const label = parseLocalDate(concert.local_date).toLocaleDateString(undefined, {
				month: 'long',
				year: 'numeric'
			});
			const last = groups.at(-1);
			if (last && last.label === label) {
				last.items.push(concert);
			} else {
				groups.push({ label, items: [concert] });
			}
		}
		return groups;
	});
</script>

<svelte:head>
	<title>Upcoming Events - DroppedNeedle</title>
</svelte:head>

<ListPageHeader
	title="Upcoming Events"
	ticket="DN / CC 3.2"
	backHref={withBasePath('/following')}
	backLabel="Back to Following"
	backStyle="link"
/>

<div class="dn-gigs">
	{#if loading}
		<div class="flex max-w-5xl flex-col gap-3" aria-hidden="true">
			<div class="mb-5 flex items-center gap-3">
				<div class="skeleton h-4 w-28"></div>
				<div class="skeleton h-11 w-40 rounded-full"></div>
			</div>
			{#each Array(6) as _, i (i)}
				<div class="dn-gigs__skeleton">
					<div class="skeleton h-full w-[6.25rem] rounded-none"></div>
					<div class="skeleton size-14 rounded-full"></div>
					<div class="flex flex-1 flex-col gap-2">
						<div class="skeleton h-4 w-1/3"></div>
						<div class="skeleton h-3 w-1/2"></div>
					</div>
				</div>
			{/each}
		</div>
	{:else if !configured}
		<Card variant="paper" padding="none" class="dn-gigs__notice">
			<div class="dn-gigs__notice-body">
				<span class="dn-gigs__notice-icon" aria-hidden="true">
					<CalendarClock class="size-7" strokeWidth={1.75} />
				</span>
				<div class="min-w-0 flex-1">
					<h2 class="dn-gigs__notice-title">Events aren't set up yet</h2>
					<p class="dn-gigs__notice-text">
						Concert listings need an events source (Ticketmaster or Skiddle).
						{#if authStore.isAdmin}
							Add an API key in Settings to turn this on.
						{:else}
							Ask your admin to add one in Settings.
						{/if}
					</p>
				</div>
				{#if authStore.isAdmin}
					<Button
						variant="primary"
						size="display-sm"
						icon={Settings}
						href={withBasePath('/settings?tab=events')}
					>
						Open Settings
					</Button>
				{/if}
			</div>
		</Card>
	{:else if cities.length === 0}
		<EventsSetup onpick={addFirstCity} />
	{:else}
		<EventCityManager {cities} />

		{#if concerts.length === 0}
			<Card variant="paper" padding="none" class="dn-gigs__notice mt-10">
				<div class="dn-gigs__notice-body">
					<span class="dn-gigs__notice-icon" aria-hidden="true">
						<CalendarClock class="size-7" strokeWidth={1.75} />
					</span>
					<div class="min-w-0 flex-1">
						<h2 class="dn-gigs__notice-title">No gigs found in your cities yet</h2>
						<p class="dn-gigs__notice-text">
							When an artist you follow announces a show near one of your cities, it shows up here.
						</p>
					</div>
					<Button
						variant="paper"
						size="display-sm"
						icon={Users}
						href={withBasePath('/following/artists')}
					>
						Your Artists
					</Button>
				</div>
			</Card>
		{:else}
			{#each monthGroups as group (group.label)}
				<section class="dn-gigs__month" aria-label={group.label}>
					<SectionHead title={group.label} />
					<ul class="flex flex-col gap-3">
						{#each group.items as concert (concert.source + concert.source_event_id + concert.artist_mbid)}
							<li><EventRow {concert} /></li>
						{/each}
					</ul>
				</section>
			{/each}
		{/if}
	{/if}
</div>

<style>
	.dn-gigs {
		padding: 0.5rem 2rem 4rem;
	}

	.dn-gigs__month {
		max-width: 64rem;
		margin-top: 2.75rem;
	}
	.dn-gigs__month :global(.dn-section-head) {
		margin-bottom: 1rem;
	}

	.dn-gigs__skeleton {
		display: flex;
		height: 6.5rem;
		align-items: center;
		gap: 1.25rem;
		overflow: hidden;
		padding-right: 1.25rem;
		border: 1px solid var(--dn-line);
		border-radius: 0.625rem;
		background-color: var(--color-base-200);
	}

	.dn-gigs :global(.dn-gigs__notice) {
		max-width: 48rem;
	}
	.dn-gigs__notice-body {
		display: flex;
		align-items: center;
		gap: 1.25rem;
		padding: 1.5rem 1.75rem;
	}
	.dn-gigs__notice-icon {
		display: grid;
		width: 3.5rem;
		height: 3.5rem;
		flex: none;
		place-items: center;
		border-radius: 9999px;
		background-color: var(--dn-paper-tile);
		color: var(--dn-action);
	}
	.dn-gigs__notice-title {
		font-family: var(--font-display);
		font-size: 1.75rem;
		font-weight: 400;
		line-height: 1.05;
		text-transform: uppercase;
		font-synthesis: none;
	}
	.dn-gigs__notice-text {
		margin-top: 0.375rem;
		font-size: 0.9375rem;
		color: var(--dn-muted);
	}

	@media (max-width: 1023px) {
		.dn-gigs {
			padding-inline: 1.5rem;
		}
	}
	@media (max-width: 767px) {
		.dn-gigs {
			padding: 0.5rem 1rem 3rem;
		}
		.dn-gigs__notice-body {
			display: grid;
			grid-template-columns: auto minmax(0, 1fr);
			align-items: start;
			gap: 1rem;
			padding: 1.25rem;
		}
		.dn-gigs__notice-body > :global(.btn) {
			grid-column: 1 / -1;
			justify-self: start;
		}
		.dn-gigs__notice-icon {
			width: 2.75rem;
			height: 2.75rem;
		}
		.dn-gigs__notice-title {
			font-size: 1.375rem;
		}
	}
</style>
