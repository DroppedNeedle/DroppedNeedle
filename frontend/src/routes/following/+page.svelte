<script lang="ts">
	import { Heart, ArrowRight, CalendarClock, Check, Disc3, ExternalLink } from 'lucide-svelte';
	import PageHero from '$lib/ui/PageHero.svelte';
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

<div class="min-h-[calc(100vh-200px)]">
	<PageHero
		title="Following"
		subtitle="New releases, live shows, and the artists you care about."
		eyebrow="Your artists"
		tint="var(--color-error)"
	>
		{#snippet icon()}
			<Heart class="h-7 w-7" />
		{/snippet}
	</PageHero>

	<div class="px-4 pb-12 sm:px-6 lg:px-8">
		{#if loading}
			<div
				class="grid grid-cols-1 gap-10 lg:grid-cols-[minmax(0,7fr)_minmax(0,3fr)] lg:gap-8"
				aria-hidden="true"
			>
				<div>
					<div class="mb-4 h-4 w-44 animate-pulse rounded bg-base-200"></div>
					<div class="grid grid-cols-3 gap-3 sm:grid-cols-5">
						<div
							class="col-span-2 row-span-2 aspect-square animate-pulse rounded-2xl bg-base-200"
						></div>
						{#each Array(6) as _, i (i)}
							<div class="aspect-square animate-pulse rounded-2xl bg-base-200"></div>
						{/each}
					</div>
				</div>
				<div class="flex flex-col gap-3">
					<div class="mb-1 h-4 w-32 animate-pulse rounded bg-base-200"></div>
					{#each Array(3) as _, i (i)}
						<div class="h-16 animate-pulse rounded-2xl bg-base-200"></div>
					{/each}
				</div>
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
			<div class="grid grid-cols-1 gap-10 lg:grid-cols-[minmax(0,7fr)_minmax(0,3fr)] lg:gap-8">
				<!-- ═══ main feed: the release wall ═══ -->
				<section aria-label="New releases">
					<div class="mb-4 flex items-baseline justify-between gap-3">
						<h2
							class="flex flex-wrap items-center gap-x-2.5 gap-y-1 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
						>
							<Disc3 class="h-4 w-4 text-accent" aria-hidden="true" />
							New releases
							<span class="text-base-content/35">· last {RELEASE_WINDOW_DAYS} days</span>
							{#if releaseTotal > 0}<span class="text-base-content/35">({releaseTotal})</span>{/if}
						</h2>
						<a
							href="/following/new-releases"
							class="group flex shrink-0 items-center gap-1 text-sm text-base-content/55 transition-colors hover:text-primary"
						>
							See all
							<ArrowRight
								class="h-3.5 w-3.5 transition-transform group-hover:translate-x-0.5"
								aria-hidden="true"
							/>
						</a>
					</div>
					{#if releases.length > 0}
						{@const heroGrid = releases.length >= 3}
						<div class="grid grid-cols-3 gap-3 sm:grid-cols-5">
							{#each releases as release, index (release.release_group_mbid)}
								{@const isHero = heroGrid && index === 0}
								<a
									href="/album/{release.release_group_mbid}"
									class="group flex flex-col gap-1.5 {isHero ? 'col-span-2 row-span-2' : ''}"
									aria-label="Open {release.title} by {release.artist_name}"
								>
									<div
										class="relative aspect-square w-full overflow-hidden rounded-2xl border border-base-content/8 transition-all duration-300 group-hover:-translate-y-0.5 group-hover:border-primary/30 group-hover:shadow-lg"
									>
										<AlbumImage
											mbid={release.release_group_mbid}
											alt={release.title}
											size="full"
											className="h-full w-full transition-transform duration-500 group-hover:scale-[1.03]"
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
										{#if isHero && release.first_release_date}
											<p
												class="font-mono text-[0.68rem] font-bold uppercase tracking-[0.18em] text-base-content/40 tabular-nums"
											>
												{release.first_release_date}
											</p>
										{/if}
										<p
											class="truncate group-hover:text-primary {isHero
												? 'font-display text-lg font-semibold tracking-tight sm:text-xl'
												: 'text-sm font-medium'}"
										>
											{release.title}
										</p>
										<p class="truncate text-sm text-base-content/55">{release.artist_name}</p>
									</div>
								</a>
							{/each}
						</div>
					{:else}
						<p
							class="flex items-center gap-3 rounded-2xl border border-dashed border-base-content/12 p-5 text-sm text-base-content/55"
						>
							<Disc3 class="h-5 w-5 shrink-0 opacity-60" aria-hidden="true" />
							Nothing released in the last {RELEASE_WINDOW_DAYS} days - quiet month.
						</p>
					{/if}
				</section>

				<!-- ═══ right rail: gigs + the follow list ═══ -->
				<aside class="space-y-10 lg:pt-1" aria-label="Coming up and your artists">
					<section aria-label="Coming up">
						<div class="mb-3 flex items-baseline justify-between gap-3">
							<h2
								class="flex items-center gap-2.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
							>
								<CalendarClock class="h-4 w-4 text-accent" aria-hidden="true" />
								Coming up
								{#if concertTotal > 0}<span class="text-base-content/35">({concertTotal})</span
									>{/if}
							</h2>
							<a
								href="/following/events"
								class="group flex shrink-0 items-center gap-1 text-sm text-base-content/55 transition-colors hover:text-primary"
							>
								See all
								<ArrowRight
									class="h-3.5 w-3.5 transition-transform group-hover:translate-x-0.5"
									aria-hidden="true"
								/>
							</a>
						</div>
						{#if concerts.length > 0}
							<ul class="flex flex-col gap-2">
								{#each concerts as concert (concert.source + concert.source_event_id + concert.artist_mbid)}
									<li
										class="flex items-center gap-3 rounded-2xl border border-base-content/8 bg-base-200/50 p-3 transition-colors hover:border-primary/30"
									>
										<div
											class="flex w-12 shrink-0 flex-col items-center rounded-xl bg-base-content/6 py-1.5"
											aria-hidden="true"
										>
											<span class="font-display text-lg leading-tight font-bold text-primary">
												{dayOfMonth(concert.local_date)}
											</span>
											<span
												class="font-mono text-xs font-semibold uppercase tracking-wider text-base-content/50"
											>
												{monthShort(concert.local_date)}
											</span>
										</div>
										<div class="min-w-0 flex-1">
											<a
												href="/artist/{concert.artist_mbid}"
												class="flex items-center gap-2 hover:text-primary"
											>
												<ArtistImage
													mbid={concert.artist_mbid}
													alt={concert.artist_name}
													size="xs"
													rounded="full"
													className="h-6 w-6 shrink-0"
												/>
												<span class="truncate text-sm font-semibold">{concert.artist_name}</span>
											</a>
											<p class="truncate text-sm text-base-content/60">{gigLine(concert)}</p>
										</div>
										{#if concert.ticket_url}
											<a
												href={concert.ticket_url}
												target="_blank"
												rel="noopener noreferrer"
												class="btn btn-ghost btn-xs shrink-0 gap-1 rounded-full bg-base-content/6"
											>
												Tickets <ExternalLink class="h-3 w-3" aria-hidden="true" />
											</a>
										{/if}
									</li>
								{/each}
							</ul>
						{:else if eventsConfigured}
							<a
								href="/following/events"
								class="flex items-center gap-3 rounded-2xl border border-dashed border-base-content/12 p-4 text-sm text-base-content/55 transition-colors hover:border-primary/40 hover:text-primary"
							>
								<CalendarClock class="h-5 w-5 shrink-0 opacity-60" aria-hidden="true" />
								Pick your cities to see gigs near you
							</a>
						{:else if authStore.isAdmin}
							<a
								href="/settings?tab=events"
								class="flex items-center gap-3 rounded-2xl border border-dashed border-base-content/12 p-4 text-sm text-base-content/55 transition-colors hover:border-primary/40 hover:text-primary"
							>
								<CalendarClock class="h-5 w-5 shrink-0 opacity-60" aria-hidden="true" />
								Set up an events source to see gigs from artists you follow
							</a>
						{/if}
					</section>

					<section aria-label="Your artists">
						<div class="mb-3 flex items-baseline justify-between gap-3">
							<h2
								class="flex items-center gap-2.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
							>
								<Heart class="h-4 w-4 text-accent" aria-hidden="true" />
								Your artists
								<span class="text-base-content/35">({artists.length})</span>
							</h2>
							<a
								href="/following/artists"
								class="group flex shrink-0 items-center gap-1 text-sm text-base-content/55 transition-colors hover:text-primary"
							>
								See all
								<ArrowRight
									class="h-3.5 w-3.5 transition-transform group-hover:translate-x-0.5"
									aria-hidden="true"
								/>
							</a>
						</div>
						<div class="flex flex-wrap items-center gap-2">
							{#each artists.slice(0, AVATAR_ROW) as artist (artist.mbid)}
								<a
									href="/artist/{artist.mbid}"
									title={artist.name}
									aria-label="Open {artist.name}"
									class="rounded-full ring-1 ring-base-content/10 transition-all hover:-translate-y-0.5 hover:ring-primary/50"
								>
									<ArtistImage mbid={artist.mbid} alt={artist.name} size="xs" rounded="full" />
								</a>
							{/each}
							{#if artists.length > AVATAR_ROW}
								<a
									href="/following/artists"
									class="flex h-8 items-center rounded-full border border-base-content/10 bg-base-200/60 px-3 text-sm font-semibold text-base-content/70 transition-colors hover:border-primary/40 hover:text-primary"
								>
									+{artists.length - AVATAR_ROW}
								</a>
							{/if}
						</div>
					</section>
				</aside>
			</div>
		{/if}
	</div>
</div>
