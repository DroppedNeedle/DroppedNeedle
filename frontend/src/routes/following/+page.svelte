<script lang="ts">
	import { CalendarDays, Check, Disc3, ExternalLink, Heart, MapPin, Users } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import MediaTile from '$lib/media/MediaTile.svelte';
	import { outsideLink } from '$lib/ui/outsideLink';
	import ListPageHeader from '$lib/ui/ListPageHeader.svelte';
	import Button from '$lib/ui/Button.svelte';
	import FollowPanel from '$lib/features/following/components/FollowPanel.svelte';
	import FollowEmpty from '$lib/features/following/components/FollowEmpty.svelte';
	import {
		getConcertsQuery,
		getFollowedArtistsQuery,
		getRecentReleasesQuery
	} from '$lib/features/following/queries/FollowQueries.svelte';
	import type { Concert } from '$lib/features/following/queries/types';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { KM_PER_MILE } from '$lib/constants';
	import { withBasePath } from '$lib/utils/basePath';
	import { albumHref, artistHref } from '$lib/utils/entityRoutes';

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
	const heroGrid = $derived(releases.length >= 3);

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
	<ListPageHeader
		title="Following"
		tagline="New releases, gigs and the artists you follow."
		ticket="DN / CC 3.2"
	/>

	<div class="px-4 pb-10 sm:px-6 lg:px-8">
		{#if loading}
			<div class="fl-hub" aria-hidden="true">
				<div class="fl-skel">
					<div class="skeleton skeleton-shimmer h-8 w-2/3"></div>
					<div class="fl-covers fl-covers--hero">
						{#each Array(RELEASE_STRIP) as _, i (i)}
							<div class="skeleton skeleton-shimmer aspect-square w-full rounded-lg"></div>
						{/each}
					</div>
				</div>
				<div class="fl-skel">
					<div class="skeleton skeleton-shimmer h-8 w-1/2"></div>
					{#each Array(3) as _, i (i)}
						<div class="skeleton skeleton-shimmer h-14 w-full rounded-lg"></div>
					{/each}
				</div>
				<div class="fl-skel fl-hub__wide">
					<div class="skeleton skeleton-shimmer h-8 w-1/3"></div>
					<div class="skeleton skeleton-shimmer h-16 w-full rounded-lg"></div>
				</div>
			</div>
		{:else}
			<div class="fl-hub">
				<FollowPanel
					title="New releases · last {RELEASE_WINDOW_DAYS} days"
					count={releaseTotal}
					href={withBasePath('/following/new-releases')}
					icon={Disc3}
				>
					{#if releases.length > 0}
						<div class="fl-covers" class:fl-covers--hero={heroGrid}>
							{#each releases as release (release.release_group_mbid)}
								<MediaTile
									title={release.title}
									href={albumHref(release.release_group_mbid)}
									label="Open {release.title} by {release.artist_name}{release.in_library
										? ', in your library'
										: ''}"
									class="dn-follow-over"
								>
									{#snippet art()}
										<AlbumImage
											mbid={release.release_group_mbid}
											alt={release.title}
											size="full"
											requestSize={heroGrid ? 500 : 250}
											className="h-full w-full"
										/>
									{/snippet}
									{#snippet meta()}{release.artist_name}{/snippet}
									{#snippet overlay()}
										{#if release.in_library}
											<span class="fl-tick" title="In your library">
												<Check class="size-3.5" strokeWidth={3} aria-hidden="true" />
											</span>
										{/if}
									{/snippet}
								</MediaTile>
							{/each}
						</div>
					{:else}
						<FollowEmpty
							icon={Disc3}
							title="Nothing released yet"
							description="When an artist you follow puts out something new, it shows up here."
						/>
					{/if}
				</FollowPanel>

				<FollowPanel
					title="Coming up"
					count={concertTotal}
					href={withBasePath('/following/events')}
					icon={CalendarDays}
				>
					{#if concerts.length > 0}
						<ul class="fl-gigs">
							{#each concerts as concert (concert.source + concert.source_event_id + concert.artist_mbid)}
								<li class="fl-gig">
									<div class="fl-gig__date" aria-hidden="true">
										<span class="fl-gig__day">{dayOfMonth(concert.local_date)}</span>
										<span class="fl-gig__month">{monthShort(concert.local_date)}</span>
									</div>
									<a
										href={artistHref(concert.artist_mbid)}
										class="dn-follow-over fl-gig__face shrink-0 rounded-full"
										tabindex="-1"
										aria-hidden="true"
									>
										<ArtistImage
											mbid={concert.artist_mbid}
											alt=""
											size="sm"
											className="size-11"
											rounded="full"
										/>
									</a>
									<div class="min-w-0 flex-1">
										<a href={artistHref(concert.artist_mbid)} class="dn-follow-over fl-gig__name">
											{concert.artist_name}
										</a>
										<p class="fl-gig__line">{gigLine(concert)}</p>
									</div>
									{#if outsideLink(concert.ticket_url)}
										<Button
											variant="outline"
											size="sm"
											icon={ExternalLink}
											iconPosition="end"
											href={outsideLink(concert.ticket_url)}
											target="_blank"
											rel="noopener noreferrer"
											class="dn-follow-over shrink-0"
										>
											Tickets
										</Button>
									{/if}
								</li>
							{/each}
						</ul>
					{:else if eventsConfigured}
						<FollowEmpty
							look="dark"
							icon={MapPin}
							title="Pick your cities to see gigs near you"
							description="Add one or more cities to see upcoming gigs from the artists you follow."
							ctaLabel="Set your cities"
							ctaHref={withBasePath('/following/events')}
						/>
					{:else if authStore.isAdmin}
						<FollowEmpty
							look="dark"
							icon={CalendarDays}
							title="Set up an events source"
							description="Then gigs from the artists you follow show up here."
							ctaLabel="Set up events"
							ctaHref={withBasePath('/settings?tab=events')}
						/>
					{/if}
				</FollowPanel>

				<FollowPanel
					title="Your artists"
					count={artists.length}
					href={withBasePath('/following/artists')}
					icon={Users}
					class="fl-hub__wide"
				>
					{#if artists.length > 0}
						<div class="fl-faces">
							{#each artists.slice(0, AVATAR_ROW) as artist (artist.mbid)}
								<a
									href={artistHref(artist.mbid)}
									title={artist.name}
									aria-label="Open {artist.name}"
									class="dn-follow-over fl-face"
								>
									<ArtistImage
										mbid={artist.mbid}
										alt={artist.name}
										size="full"
										requestSize={250}
										className="h-full w-full"
										rounded="full"
									/>
								</a>
							{/each}
							{#if artists.length > AVATAR_ROW}
								<a
									href={withBasePath('/following/artists')}
									class="dn-follow-over fl-face fl-face--more"
									aria-label="{artists.length - AVATAR_ROW} more artists"
								>
									+{artists.length - AVATAR_ROW}
								</a>
							{/if}
						</div>
					{:else}
						<FollowEmpty
							look="band"
							icon={Heart}
							title="You're not following anyone yet"
							description="Follow artists you love and this page fills with their new releases and gigs near you."
							ctaLabel="Discover artists"
							ctaHref={withBasePath('/discover')}
						/>
					{/if}
				</FollowPanel>
			</div>
		{/if}
	</div>
</div>

<style>
	.fl-hub {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: 1.25rem;
	}
	@media (min-width: 1100px) {
		.fl-hub {
			grid-template-columns: minmax(0, 1.2fr) minmax(0, 1fr);
		}
		.fl-hub :global(.fl-hub__wide) {
			grid-column: 1 / -1;
		}
	}

	.fl-skel {
		display: flex;
		flex-direction: column;
		gap: 1rem;
		padding: 1.25rem;
		border: 1px solid var(--dn-line);
		border-radius: 0.875rem;
	}

	/* up to seven covers, the first one large when there are three or more */
	.fl-covers {
		display: grid;
		grid-template-columns: repeat(5, minmax(0, 1fr));
		gap: 1rem 0.875rem;
	}
	.fl-covers--hero > :global(:first-child) {
		grid-column: span 2;
		grid-row: span 2;
	}
	@media (max-width: 639px) {
		.fl-covers {
			grid-template-columns: repeat(3, minmax(0, 1fr));
			gap: 0.75rem 0.625rem;
		}
		/* a phone shows two even rows of three */
		.fl-covers--hero > :global(:first-child) {
			grid-column: auto;
			grid-row: auto;
		}
		.fl-covers > :global(:nth-child(7)) {
			display: none;
		}
	}

	.fl-tick {
		position: absolute;
		top: 0.375rem;
		right: 0.375rem;
		display: grid;
		place-items: center;
		width: 1.5rem;
		height: 1.5rem;
		border: 1px solid var(--dn-action-edge);
		border-radius: 9999px;
		background-color: var(--dn-action);
		color: var(--dn-action-content);
		box-shadow: 0 2px 6px rgb(0 0 0 / 0.35);
	}

	.fl-gigs {
		display: flex;
		flex-direction: column;
	}
	.fl-gig {
		display: flex;
		align-items: center;
		gap: 0.875rem;
		padding: 0.75rem 0.25rem;
	}
	.fl-gig + .fl-gig {
		border-top: 1px solid var(--dn-line);
	}
	.fl-gig__date {
		display: flex;
		flex: none;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		width: 3.25rem;
		height: 3.25rem;
		border: 1px solid var(--dn-edge);
		border-radius: 0.5rem;
		background-color: var(--color-base-100);
	}
	.fl-gig__day {
		font-family: var(--font-display);
		font-size: 1.5rem;
		line-height: 1;
		color: var(--dn-title);
		font-synthesis: none;
	}
	.fl-gig__month {
		margin-top: 0.125rem;
		font-family: var(--font-mono);
		font-size: 0.625rem;
		font-weight: 700;
		letter-spacing: 0.14em;
		text-transform: uppercase;
	}
	.fl-gig__name {
		display: block;
		overflow: hidden;
		font-weight: 700;
		text-overflow: ellipsis;
		white-space: nowrap;
		transition: color 150ms;
	}
	.fl-gig__name:hover {
		color: var(--dn-title);
	}
	.fl-gig__line {
		overflow: hidden;
		margin-top: 0.125rem;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		color: var(--color-muted);
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.fl-faces {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.75rem;
		padding: 0.25rem;
	}
	.fl-face {
		display: grid;
		place-items: center;
		width: 4.5rem;
		height: 4.5rem;
		overflow: hidden;
		border-radius: 9999px;
		box-shadow: 0 0 0 2px var(--color-base-100);
		transition: box-shadow 150ms;
	}
	.fl-face:hover,
	.fl-face:focus-visible {
		box-shadow:
			0 0 0 2px var(--color-base-100),
			0 0 0 4px var(--dn-title);
	}
	.fl-face--more {
		border: 2px dashed var(--dn-edge);
		font-family: var(--font-mono);
		font-size: 1rem;
		font-weight: 700;
		color: var(--color-base-content);
	}
	.fl-face--more:hover {
		color: var(--dn-title);
	}
	@media (max-width: 639px) {
		.fl-face {
			width: 3.5rem;
			height: 3.5rem;
		}
		.fl-gig {
			gap: 0.625rem;
		}
		.fl-gig__line {
			display: -webkit-box;
			-webkit-box-orient: vertical;
			-webkit-line-clamp: 2;
			line-clamp: 2;
			white-space: normal;
		}
	}
	/* a narrow phone drops the gig's picture (its name links the artist) */
	@media (max-width: 379px) {
		.fl-gig__face {
			display: none;
		}
	}
</style>
