<script lang="ts">
	import { resolve } from '$app/paths';
	import { onMount } from 'svelte';
	import { ArrowLeft, CalendarClock, ExternalLink, Settings } from 'lucide-svelte';
	import ArtistImage from '$lib/components/ArtistImage.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import CitySearchInput from '$lib/components/following/CitySearchInput.svelte';
	import EventCityManager from '$lib/components/following/EventCityManager.svelte';
	import {
		getConcertsQuery,
		getEventCitiesQuery
	} from '$lib/queries/following/FollowQueries.svelte';
	import {
		createMarkConcertsSeenMutation,
		createReplaceEventCitiesMutation
	} from '$lib/queries/following/FollowMutations.svelte';
	import type { CitySearchResult, Concert } from '$lib/queries/following/types';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { KM_PER_MILE } from '$lib/constants';

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
				radius_km: 32, // 20 mi default (U6)
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

	function weekday(value: string): string {
		return parseLocalDate(value).toLocaleDateString(undefined, { weekday: 'short' });
	}

	function dayOfMonth(value: string): string {
		return String(parseLocalDate(value).getDate());
	}

	function monthShort(value: string): string {
		return parseLocalDate(value).toLocaleDateString(undefined, { month: 'short' });
	}

	function venueLine(concert: Concert): string {
		const distance =
			concert.distance_km != null ? `${Math.round(concert.distance_km / KM_PER_MILE)} mi` : null;
		return [concert.venue_name, concert.city, distance].filter(Boolean).join(' · ');
	}
</script>

<svelte:head>
	<title>Upcoming Events - DroppedNeedle</title>
</svelte:head>

<div class="mx-auto w-full max-w-4xl px-2 py-4 sm:px-4 sm:py-8 lg:px-8">
	<div class="mb-6 flex items-center gap-3">
		<a
			href={resolve('/following')}
			class="btn btn-ghost btn-sm btn-circle"
			aria-label="Back to Following"
		>
			<ArrowLeft class="h-5 w-5" />
		</a>
		<CalendarClock class="h-6 w-6 text-primary" aria-hidden="true" />
		<h1 class="text-2xl font-bold sm:text-3xl">Upcoming Events</h1>
	</div>

	{#if loading}
		<div class="flex flex-col gap-3" aria-hidden="true">
			<div class="h-8 w-40 animate-pulse rounded-lg bg-base-200"></div>
			{#each Array(6) as _, i (i)}
				<div class="flex items-center gap-4 rounded-2xl border border-base-300 bg-base-200 p-4">
					<div class="h-14 w-12 animate-pulse rounded-xl bg-base-300"></div>
					<div class="h-10 w-10 animate-pulse rounded-full bg-base-300"></div>
					<div class="flex-1 space-y-2">
						<div class="h-4 w-1/3 animate-pulse rounded bg-base-300"></div>
						<div class="h-3 w-1/2 animate-pulse rounded bg-base-300"></div>
					</div>
				</div>
			{/each}
		</div>
	{:else if !configured}
		<div class="rounded-2xl border border-base-300 bg-base-200 p-8 text-center">
			<CalendarClock class="mx-auto mb-3 h-10 w-10 text-base-content/40" aria-hidden="true" />
			<h2 class="mb-1 text-lg font-semibold">Events aren't set up yet</h2>
			<p class="mx-auto max-w-md text-sm text-base-content/60">
				Concert listings need an events source (Ticketmaster or Skiddle).
				{#if authStore.isAdmin}
					Add an API key in Settings to turn this on.
				{:else}
					Ask your admin to add one in Settings.
				{/if}
			</p>
			{#if authStore.isAdmin}
				<a href={resolve('/settings')} class="btn btn-primary btn-sm mt-4 gap-2 rounded-full">
					<Settings class="h-4 w-4" aria-hidden="true" /> Open Settings
				</a>
			{/if}
		</div>
	{:else if cities.length === 0}
		<!-- first run (U7): the empty screen IS the setup step -->
		<div class="mx-auto flex max-w-md flex-col items-center gap-4 py-16 text-center">
			<span
				class="flex h-16 w-16 items-center justify-center rounded-full bg-primary/15 text-primary"
			>
				<CalendarClock class="h-8 w-8" aria-hidden="true" />
			</span>
			<h2 class="text-xl font-semibold">Where do you go to gigs?</h2>
			<CitySearchInput onpick={addFirstCity} autofocus className="w-full" />
			<p class="text-sm text-base-content/60">
				Pick as many cities as you like - we'll show upcoming concerts from artists you follow near
				each one.
			</p>
		</div>
	{:else}
		<div class="mb-6 rounded-2xl border border-base-300 bg-base-200 p-4">
			<EventCityManager {cities} />
		</div>

		{#if concerts.length === 0}
			<EmptyState
				icon={CalendarClock}
				title="No gigs found in your cities yet"
				description="When an artist you follow announces a show near one of your cities, it shows up here."
				ctaLabel="Your Artists"
				ctaHref="/following/artists"
			/>
		{:else}
			{#each monthGroups as group (group.label)}
				<h2 class="mt-8 mb-3 text-sm font-semibold uppercase tracking-wider text-base-content/50">
					{group.label}
				</h2>
				<ul class="flex flex-col gap-2">
					{#each group.items as concert (concert.source + concert.source_event_id + concert.artist_mbid)}
						<li
							class="flex items-center gap-4 rounded-2xl border border-base-300 bg-base-200 p-3 sm:p-4"
						>
							<!-- calendar date block (U1) -->
							<div
								class="flex w-12 shrink-0 flex-col items-center rounded-xl bg-base-300/60 py-1.5"
								aria-hidden="true"
							>
								<span class="text-[10px] font-semibold uppercase text-base-content/50">
									{weekday(concert.local_date)}
								</span>
								<span class="text-lg leading-tight font-bold text-primary">
									{dayOfMonth(concert.local_date)}
								</span>
								<span class="text-[10px] font-semibold uppercase text-base-content/50">
									{monthShort(concert.local_date)}
								</span>
							</div>

							<a
								href={resolve(`/artist/${concert.artist_mbid}`)}
								class="shrink-0"
								aria-label="Open {concert.artist_name}"
							>
								<ArtistImage
									mbid={concert.artist_mbid}
									alt={concert.artist_name}
									size="xs"
									rounded="full"
								/>
							</a>

							<div class="min-w-0 flex-1">
								<div class="flex items-center gap-2">
									<a
										href={resolve(`/artist/${concert.artist_mbid}`)}
										class="truncate font-semibold hover:text-primary"
									>
										{concert.artist_name}
									</a>
									{#if concert.status === 'cancelled'}
										<span class="badge badge-error badge-sm">Cancelled</span>
									{:else if concert.status === 'rescheduled'}
										<span class="badge badge-warning badge-sm">Rescheduled</span>
									{/if}
								</div>
								{#if concert.event_name && concert.event_name !== concert.artist_name}
									<p class="truncate text-sm text-base-content/80">{concert.event_name}</p>
								{/if}
								<p
									class="truncate text-sm text-base-content/60"
									class:line-through={concert.status === 'cancelled'}
								>
									{venueLine(concert)}
								</p>
							</div>

							{#if concert.ticket_url}
								<a
									href={concert.ticket_url}
									target="_blank"
									rel="noopener noreferrer"
									class="btn btn-ghost btn-sm shrink-0 gap-1.5 rounded-full"
								>
									Tickets <ExternalLink class="h-3.5 w-3.5" aria-hidden="true" />
								</a>
							{/if}
						</li>
					{/each}
				</ul>
			{/each}
		{/if}
	{/if}
</div>
