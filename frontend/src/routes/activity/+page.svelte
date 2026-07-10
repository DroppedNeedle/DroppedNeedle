<script lang="ts">
	/*
	 * Activity — the Sonic Circle. Real-time fidelity feeds from this server:
	 * who's listening right now (live now-playing sessions), fresh needle drops
	 * from followed artists, and upcoming shows. Everything on this page is
	 * live data — presence via SSE, releases and gigs from the following engine.
	 */
	import PageHero from '$lib/ui/PageHero.svelte';
	import NowPlayingWidget from '$lib/components/NowPlayingWidget.svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import ArtistImage from '$lib/components/ArtistImage.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import { nowPlayingStore } from '$lib/stores/nowPlayingSessions.svelte';
	import {
		getRecentReleasesQuery,
		getConcertsQuery
	} from '$lib/queries/following/FollowQueries.svelte';
	import type { Concert } from '$lib/queries/following/types';
	import { KM_PER_MILE } from '$lib/constants';
	import {
		Activity,
		Radio,
		Disc3,
		CalendarClock,
		ArrowRight,
		ExternalLink,
		Check
	} from 'lucide-svelte';

	const RELEASES = 8;
	const GIGS = 5;

	const releasesQuery = getRecentReleasesQuery(
		() => 30,
		() => RELEASES
	);
	const concertsQuery = getConcertsQuery();

	const sessions = $derived(nowPlayingStore.sessions);
	const releases = $derived((releasesQuery.data?.items ?? []).slice(0, RELEASES));
	const concerts = $derived((concertsQuery.data?.items ?? []).slice(0, GIGS));
	const loading = $derived(releasesQuery.isPending && concertsQuery.isPending);

	function parseLocalDate(value: string): Date {
		const [year, month, day] = value.split('-').map(Number);
		return new Date(year, (month || 1) - 1, day || 1);
	}

	function gigLine(concert: Concert): string {
		const distance =
			concert.distance_km != null ? `${Math.round(concert.distance_km / KM_PER_MILE)} mi` : null;
		return [concert.venue_name, concert.city, distance].filter(Boolean).join(' · ');
	}
</script>

<svelte:head>
	<title>Activity - DroppedNeedle</title>
</svelte:head>

<div class="min-h-[calc(100vh-200px)]">
	<PageHero
		title="Sonic Circle"
		subtitle="Real-time fidelity feeds from this server — who's spinning what, and what just dropped."
		eyebrow="Community"
		tint="var(--color-info)"
	>
		{#snippet icon()}
			<Activity class="h-7 w-7" />
		{/snippet}
	</PageHero>

	<div
		class="grid grid-cols-1 gap-8 px-4 pb-12 sm:px-6 lg:grid-cols-[minmax(0,7fr)_minmax(0,3fr)] lg:px-8"
	>
		<!-- main feed -->
		<div class="space-y-10">
			<section aria-label="Listening now">
				<h2
					class="mb-4 flex items-center gap-2.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
				>
					<Radio class="h-4 w-4 text-accent" />
					Listening now
				</h2>
				{#if sessions.length > 0}
					<NowPlayingWidget {sessions} />
				{:else}
					<p
						class="flex items-center gap-3 rounded-2xl border border-dashed border-base-content/12 p-5 text-sm text-base-content/55"
					>
						<Disc3 class="h-5 w-5 shrink-0 opacity-60" />
						Nobody's spinning right now. Start something — your session shows up here for everyone on
						this server.
					</p>
				{/if}
			</section>

			<section aria-label="Recent needle drops">
				<div class="mb-4 flex items-baseline justify-between gap-3">
					<h2
						class="flex items-center gap-2.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
					>
						<Disc3 class="h-4 w-4 text-accent" />
						Recent needle drops
					</h2>
					<a
						href="/following/new-releases"
						class="flex items-center gap-1 text-sm text-base-content/55 hover:text-primary"
					>
						See all
						<ArrowRight class="h-3.5 w-3.5" />
					</a>
				</div>
				{#if loading}
					<div class="grid grid-cols-2 gap-3 sm:grid-cols-4">
						{#each Array(4) as _, i (`drop-skeleton-${i}`)}
							<div class="aspect-square animate-pulse rounded-2xl bg-base-200"></div>
						{/each}
					</div>
				{:else if releases.length > 0}
					<div class="grid grid-cols-2 gap-3 sm:grid-cols-4">
						{#each releases as release (release.release_group_mbid)}
							<a
								href="/album/{release.release_group_mbid}"
								class="group flex flex-col gap-1.5"
								aria-label="Open {release.title} by {release.artist_name}"
							>
								<div class="relative aspect-square w-full overflow-hidden rounded-2xl">
									<AlbumImage
										mbid={release.release_group_mbid}
										alt={release.title}
										size="full"
										className="h-full w-full"
										rounded="xl"
									/>
									{#if release.in_library}
										<span
											class="badge badge-success badge-sm absolute top-2 right-2 gap-0.5 border-0"
											title="In your library"
										>
											<Check class="h-3 w-3" />
											<span class="sr-only">In your library</span>
										</span>
									{/if}
								</div>
								<div class="min-w-0">
									<p class="truncate text-sm font-medium group-hover:text-primary">
										{release.title}
									</p>
									<p class="truncate text-xs text-base-content/55">{release.artist_name}</p>
								</div>
							</a>
						{/each}
					</div>
				{:else}
					<EmptyState
						icon={Disc3}
						title="No drops this month"
						description="Follow artists and their new releases land here the moment they drop."
						ctaLabel="Find artists"
						ctaHref="/discover"
					/>
				{/if}
			</section>
		</div>

		<!-- right rail -->
		<aside class="space-y-8 lg:pt-1" aria-label="Coming up">
			<section>
				<h2
					class="mb-3 flex items-center gap-2.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
				>
					<CalendarClock class="h-4 w-4 text-accent" />
					Coming up
				</h2>
				{#if concerts.length > 0}
					<ul class="flex flex-col gap-2">
						{#each concerts as concert (concert.source + concert.source_event_id + concert.artist_mbid)}
							<li
								class="flex items-center gap-3 rounded-2xl border border-base-content/8 bg-base-200/50 p-3"
							>
								<div
									class="flex h-11 w-11 shrink-0 flex-col items-center justify-center rounded-xl bg-base-content/6"
								>
									<span class="font-display text-sm font-bold leading-none"
										>{parseLocalDate(concert.local_date).getDate()}</span
									>
									<span class="text-[0.6rem] uppercase opacity-55"
										>{parseLocalDate(concert.local_date).toLocaleDateString(undefined, {
											month: 'short'
										})}</span
									>
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
										<span class="truncate text-sm font-medium">{concert.artist_name}</span>
									</a>
									<p class="truncate text-xs text-base-content/50">{gigLine(concert)}</p>
								</div>
								{#if concert.ticket_url}
									<a
										href={concert.ticket_url}
										target="_blank"
										rel="noopener noreferrer"
										class="btn btn-ghost btn-xs btn-circle"
										aria-label="Tickets for {concert.artist_name}"
									>
										<ExternalLink class="h-3.5 w-3.5" />
									</a>
								{/if}
							</li>
						{/each}
					</ul>
					<a
						href="/following/events"
						class="mt-3 flex items-center gap-1 text-sm text-base-content/55 hover:text-primary"
					>
						All events
						<ArrowRight class="h-3.5 w-3.5" />
					</a>
				{:else}
					<p
						class="rounded-2xl border border-dashed border-base-content/12 p-4 text-sm text-base-content/55"
					>
						No gigs on the radar. Add your cities in
						<a href="/following/events" class="link">Following → Events</a>.
					</p>
				{/if}
			</section>
		</aside>
	</div>
</div>
