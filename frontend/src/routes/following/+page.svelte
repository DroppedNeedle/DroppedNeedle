<script lang="ts">
	import { resolve } from '$app/paths';
	import { Heart, ArrowRight, CalendarClock, Check, Disc3, ExternalLink } from 'lucide-svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import ArtistImage from '$lib/components/ArtistImage.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import {
		getConcertsQuery,
		getFollowedArtistsQuery,
		getRecentReleasesQuery
	} from '$lib/queries/following/FollowQueries.svelte';
	import type { Concert } from '$lib/queries/following/types';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { KM_PER_MILE } from '$lib/constants';

	const RELEASE_STRIP = 7; // 1 large + 6 small covers
	const RELEASE_WINDOW_DAYS = 30;
	const GIG_ROWS = 4;
	const AVATAR_ROW = 12;

	const artistsQuery = getFollowedArtistsQuery();
	const recentReleasesQuery = getRecentReleasesQuery(
		() => RELEASE_WINDOW_DAYS,
		() => RELEASE_STRIP
	);
	const concertsQuery = getConcertsQuery();

	const artists = $derived(artistsQuery.data ?? []);
	const releases = $derived((recentReleasesQuery.data?.items ?? []).slice(0, RELEASE_STRIP));
	const releaseTotal = $derived(recentReleasesQuery.data?.total ?? 0);
	const concerts = $derived((concertsQuery.data?.items ?? []).slice(0, GIG_ROWS));
	const concertTotal = $derived(concertsQuery.data?.total ?? 0);
	const eventsConfigured = $derived(concertsQuery.data?.configured ?? true);
	const loading = $derived(
		artistsQuery.isPending || recentReleasesQuery.isPending || concertsQuery.isPending
	);

	function parseLocalDate(value: string): Date {
		const [year, month, day] = value.split('-').map(Number);
		return new Date(year, (month || 1) - 1, day || 1);
	}

	function dayOfMonth(value: string): string {
		return String(parseLocalDate(value).getDate());
	}

	function monthShort(value: string): string {
		return parseLocalDate(value).toLocaleDateString(undefined, { month: 'short' });
	}

	function gigLine(concert: Concert): string {
		const distance =
			concert.distance_km != null ? `${Math.round(concert.distance_km / KM_PER_MILE)} mi` : null;
		return [concert.venue_name, concert.city, distance].filter(Boolean).join(' · ');
	}
</script>

<svelte:head>
	<title>Following - DroppedNeedle</title>
</svelte:head>

<div class="mx-auto w-full max-w-5xl px-2 py-4 sm:px-4 sm:py-8 lg:px-8">
	<div class="mb-6 flex items-center gap-2">
		<Heart class="h-6 w-6 text-primary" aria-hidden="true" />
		<h1 class="text-2xl font-bold sm:text-3xl">Following</h1>
	</div>

	{#if loading}
		<div class="flex flex-col gap-8" aria-hidden="true">
			<div class="grid grid-cols-3 gap-3 sm:grid-cols-5">
				<div
					class="col-span-2 row-span-2 aspect-square animate-pulse rounded-2xl bg-base-200"
				></div>
				{#each Array(6) as _, i (i)}
					<div class="aspect-square animate-pulse rounded-2xl bg-base-200"></div>
				{/each}
			</div>
			{#each Array(3) as _, i (i)}
				<div class="h-16 animate-pulse rounded-2xl bg-base-200"></div>
			{/each}
		</div>
	{:else if artists.length === 0}
		<EmptyState
			icon={Heart}
			title="You're not following anyone yet"
			description="Follow artists you love and this page fills with their new releases and gigs near you."
			ctaLabel="Discover artists"
			ctaHref="/discover"
		/>
	{:else}
		<div class="flex flex-col gap-5">
			<!-- each section is one big click target: the header link stretches
			     over the whole card via ::after; inner links sit above on z-10 -->
			<section
				aria-label="New releases"
				class="group/card relative rounded-2xl border border-base-300 bg-base-200/40 p-4 transition-colors hover:border-primary sm:p-5"
			>
				<a
					href={resolve('/following/new-releases')}
					class="mb-3 flex items-baseline justify-between after:absolute after:inset-0 after:rounded-2xl after:content-['']"
				>
					<h2 class="text-sm font-semibold uppercase tracking-wider text-base-content/50">
						New releases
						<span class="text-base-content/35">· last {RELEASE_WINDOW_DAYS} days</span>
						{#if releaseTotal > 0}<span class="text-base-content/35">({releaseTotal})</span>{/if}
					</h2>
					<span
						class="flex items-center gap-1 text-sm font-medium text-base-content/60 transition-colors group-hover/card:text-primary"
					>
						See all
						<ArrowRight
							class="h-3.5 w-3.5 transition-transform group-hover/card:translate-x-0.5"
							aria-hidden="true"
						/>
					</span>
				</a>
				{#if releases.length > 0}
					{@const heroGrid = releases.length >= 3}
					<div class="relative z-10 grid grid-cols-3 gap-3 sm:grid-cols-5">
						{#each releases as release, index (release.release_group_mbid)}
							<a
								href={resolve(`/album/${release.release_group_mbid}`)}
								class="group flex flex-col gap-1.5 {heroGrid && index === 0
									? 'col-span-2 row-span-2'
									: ''}"
								aria-label="Open {release.title} by {release.artist_name}"
							>
								<div class="relative aspect-square w-full overflow-hidden rounded-2xl">
									<AlbumImage
										mbid={release.release_group_mbid}
										alt={release.title}
										size="full"
										requestSize={250}
										className="h-full w-full"
										rounded="xl"
									/>
									{#if release.in_library}
										<span
											class="badge badge-success badge-sm absolute top-2 right-2 gap-0.5 border-0 font-medium"
											title="In your library"
										>
											<Check class="h-3 w-3" aria-hidden="true" />
											<span class="sr-only">In your library</span>
										</span>
									{/if}
								</div>
								<div class="min-w-0">
									<p
										class="truncate font-medium group-hover:text-primary {heroGrid && index === 0
											? 'text-base'
											: 'text-sm'}"
									>
										{release.title}
									</p>
									<p class="truncate text-xs text-base-content/60">{release.artist_name}</p>
								</div>
							</a>
						{/each}
					</div>
				{:else}
					<p
						class="flex items-center gap-3 rounded-2xl border border-dashed border-base-300 p-4 text-sm text-base-content/60"
					>
						<Disc3 class="h-5 w-5 shrink-0" aria-hidden="true" />
						Nothing released in the last {RELEASE_WINDOW_DAYS} days - quiet month.
					</p>
				{/if}
			</section>

			<section
				aria-label="Coming up"
				class="group/card relative rounded-2xl border border-base-300 bg-base-200/40 p-4 transition-colors hover:border-primary sm:p-5"
			>
				<a
					href={resolve('/following/events')}
					class="mb-3 flex items-baseline justify-between after:absolute after:inset-0 after:rounded-2xl after:content-['']"
				>
					<h2 class="text-sm font-semibold uppercase tracking-wider text-base-content/50">
						Coming up
						{#if concertTotal > 0}<span class="text-base-content/35">({concertTotal})</span>{/if}
					</h2>
					<span
						class="flex items-center gap-1 text-sm font-medium text-base-content/60 transition-colors group-hover/card:text-primary"
					>
						See all
						<ArrowRight
							class="h-3.5 w-3.5 transition-transform group-hover/card:translate-x-0.5"
							aria-hidden="true"
						/>
					</span>
				</a>
				{#if concerts.length > 0}
					<ul class="relative z-10 flex flex-col gap-2">
						{#each concerts as concert (concert.source + concert.source_event_id + concert.artist_mbid)}
							<li
								class="flex items-center gap-3 rounded-2xl border border-base-300 bg-base-200 p-3"
							>
								<div
									class="flex w-11 shrink-0 flex-col items-center rounded-xl bg-base-300/60 py-1"
									aria-hidden="true"
								>
									<span class="text-base leading-tight font-bold text-primary">
										{dayOfMonth(concert.local_date)}
									</span>
									<span class="text-[10px] font-semibold uppercase text-base-content/50">
										{monthShort(concert.local_date)}
									</span>
								</div>
								<a href={resolve(`/artist/${concert.artist_mbid}`)} class="shrink-0">
									<ArtistImage
										mbid={concert.artist_mbid}
										alt={concert.artist_name}
										size="xs"
										rounded="full"
									/>
								</a>
								<div class="min-w-0 flex-1">
									<a
										href={resolve(`/artist/${concert.artist_mbid}`)}
										class="block truncate font-semibold hover:text-primary"
									>
										{concert.artist_name}
									</a>
									<p class="truncate text-sm text-base-content/60">{gigLine(concert)}</p>
								</div>
								{#if concert.ticket_url}
									<a
										href={concert.ticket_url}
										target="_blank"
										rel="noopener noreferrer"
										class="btn btn-ghost btn-xs shrink-0 gap-1 rounded-full"
									>
										Tickets <ExternalLink class="h-3 w-3" aria-hidden="true" />
									</a>
								{/if}
							</li>
						{/each}
					</ul>
				{:else if eventsConfigured}
					<!-- the whole card already navigates to /following/events -->
					<p
						class="flex items-center gap-3 rounded-2xl border border-dashed border-base-300 p-4 text-sm text-base-content/60"
					>
						<CalendarClock class="h-5 w-5 shrink-0" aria-hidden="true" />
						Pick your cities to see gigs near you
					</p>
				{:else if authStore.isAdmin}
					<a
						href={resolve('/settings?tab=events')}
						class="relative z-10 flex items-center gap-3 rounded-2xl border border-dashed border-base-300 p-4 text-sm text-base-content/60 transition-colors hover:border-primary hover:text-primary"
					>
						<CalendarClock class="h-5 w-5 shrink-0" aria-hidden="true" />
						Set up an events source to see gigs from artists you follow
					</a>
				{/if}
			</section>

			<section
				aria-label="Your artists"
				class="group/card relative rounded-2xl border border-base-300 bg-base-200/40 p-4 transition-colors hover:border-primary sm:p-5"
			>
				<a
					href={resolve('/following/artists')}
					class="mb-3 flex items-baseline justify-between after:absolute after:inset-0 after:rounded-2xl after:content-['']"
				>
					<h2 class="text-sm font-semibold uppercase tracking-wider text-base-content/50">
						Your artists <span class="text-base-content/35">({artists.length})</span>
					</h2>
					<span
						class="flex items-center gap-1 text-sm font-medium text-base-content/60 transition-colors group-hover/card:text-primary"
					>
						See all
						<ArrowRight
							class="h-3.5 w-3.5 transition-transform group-hover/card:translate-x-0.5"
							aria-hidden="true"
						/>
					</span>
				</a>
				<div class="relative z-10 flex flex-wrap items-center gap-2">
					{#each artists.slice(0, AVATAR_ROW) as artist (artist.mbid)}
						<a
							href={resolve(`/artist/${artist.mbid}`)}
							title={artist.name}
							aria-label="Open {artist.name}"
						>
							<ArtistImage mbid={artist.mbid} alt={artist.name} size="xs" rounded="full" />
						</a>
					{/each}
					{#if artists.length > AVATAR_ROW}
						<a
							href={resolve('/following/artists')}
							class="badge badge-lg border-base-300 bg-base-200 py-4 font-medium hover:border-primary hover:text-primary"
						>
							+{artists.length - AVATAR_ROW}
						</a>
					{/if}
				</div>
			</section>
		</div>
	{/if}
</div>
