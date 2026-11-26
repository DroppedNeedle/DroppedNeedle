<!--
	Home's way into Discover: the pick card cut down to a torn strip. While
	the week has an open pick it shows that pick (the "NEW TO YOU" tape, its
	cover in the sleeve, "DISCOVER - 3 OF 8 THIS WEEK", the title, artist and
	year, and its reason); with every pick answered it says so, with the day
	the next set is out and the week's covers small. The whole strip is one
	link to Discover. On a phone the cover and title sit side by side, the
	reason under them, then the count and "OPEN".
-->
<script lang="ts">
	import { ArrowRight } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Tape from '$lib/ui/Tape.svelte';
	import VinylSleeve from '$lib/ui/VinylSleeve.svelte';
	import { themeStore } from '$lib/stores/theme.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { countWord, longDayDate } from '$lib/features/discover/state/weekWords';
	import type { DiscoverPreview } from '$lib/features/discover/queries/types';

	interface Props {
		preview: DiscoverPreview;
	}

	let { preview }: Props = $props();

	const next = $derived(preview.next ?? null);
	const count = $derived(preview.albums.length);
	const byline = $derived(
		next
			? [next.album.artist_name ?? '', next.album.year ? String(next.album.year) : '']
					.filter(Boolean)
					.join(' - ')
			: ''
	);
</script>

<Card variant="torn" padding="none" href={withBasePath('/discover')} class="dn-teaser">
	<div class="dn-teaser__ink" class:is-done={!next} data-theme={themeStore.paper}>
		<p class="dn-teaser__tapes">
			<Tape>{next ? 'New to you' : `Week ${preview.week_number}`}</Tape>
		</p>
		{#if next}
			<VinylSleeve class="dn-teaser__sleeve">
				<AlbumImage
					mbid={next.album.musicbrainz_id ?? undefined}
					alt=""
					size="full"
					requestSize={250}
					rounded="none"
					className="aspect-square w-full object-cover"
				/>
			</VinylSleeve>
			<p class="dn-teaser__kicker">Discover - {next.pick_no} of {count} this week</p>
			<h2 class="dn-teaser__title">{next.album.title}</h2>
			{#if byline}<p class="dn-teaser__by">{byline}</p>{/if}
			<p class="dn-teaser__why">{next.reason}</p>
		{:else}
			<p class="dn-teaser__kicker">Discover - {count} of {count} answered</p>
			<h2 class="dn-teaser__title">All {countWord(count)} answered</h2>
			<p class="dn-teaser__why">The next set is out {longDayDate(preview.next_set_at)}.</p>
			<ul class="dn-teaser__covers" aria-label="This week's albums">
				{#each preview.albums as album, i (album.musicbrainz_id ?? i)}
					<li>
						<AlbumImage
							mbid={album.musicbrainz_id ?? undefined}
							alt={album.title}
							size="sm"
							rounded="none"
							className="size-full object-cover"
						/>
					</li>
				{/each}
			</ul>
		{/if}
		<span class="dn-teaser__go">
			Open<span class="dn-teaser__go-long">&nbsp;Discover</span>
			<ArrowRight class="size-4" aria-hidden="true" />
		</span>
	</div>
</Card>

<style>
	/* the torn edges come from a mask, so the lift is a shadow on the strip */
	:global(.dn-teaser) {
		filter: drop-shadow(0 12px 14px rgb(0 0 0 / 0.45));
	}
	:global([data-theme='light'] .dn-teaser) {
		filter: drop-shadow(
			0 10px 18px color-mix(in oklab, var(--color-base-content) 16%, transparent)
		);
	}

	/* the paper's own colours: the sheet switches to its own theme */
	.dn-teaser__ink {
		position: relative;
		display: grid;
		grid-template-columns: 10rem minmax(0, 1fr) auto;
		grid-template-areas:
			'sleeve kicker go'
			'sleeve title go'
			'sleeve by go'
			'sleeve why go';
		grid-template-rows: auto auto auto 1fr;
		column-gap: 1.5rem;
		align-items: start;
		padding: 2.25rem 1.75rem 1.5rem 1.875rem;
		color: var(--dn-paper-content);
	}
	.dn-teaser__ink.is-done {
		grid-template-columns: minmax(0, 1fr) auto;
		grid-template-areas:
			'kicker go'
			'title go'
			'why go'
			'covers go';
		grid-template-rows: auto;
	}

	.dn-teaser__tapes {
		position: absolute;
		top: 0.125rem;
		left: 1.25rem;
		z-index: 3;
		margin: 0;
	}

	.dn-teaser__ink :global(.dn-teaser__sleeve) {
		grid-area: sleeve;
		align-self: center;
		width: 100%;
	}

	.dn-teaser__kicker {
		grid-area: kicker;
		margin: 0 0 0.375rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.dn-teaser__title {
		grid-area: title;
		margin: 0;
		font-family: var(--font-display);
		font-size: 2.5rem;
		font-weight: 400;
		line-height: 0.95;
		letter-spacing: 0.015em;
		text-transform: uppercase;
		text-wrap: balance;
		overflow-wrap: anywhere;
		font-synthesis: none;
	}
	.dn-teaser__by {
		grid-area: by;
		margin: 0.375rem 0 0;
		font-size: 0.875rem;
		font-weight: 700;
		letter-spacing: 0.1em;
		text-transform: uppercase;
	}
	.dn-teaser__why {
		grid-area: why;
		margin: 0.625rem 0 0;
		font-size: 1.0625rem;
		font-weight: 500;
		line-height: 1.35;
	}

	.dn-teaser__covers {
		grid-area: covers;
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
		margin: 0.75rem 0 0;
		padding: 0;
		list-style: none;
	}
	.dn-teaser__covers li {
		width: 2.5rem;
		height: 2.5rem;
		overflow: hidden;
	}

	.dn-teaser__go {
		grid-area: go;
		align-self: center;
		display: inline-flex;
		align-items: center;
		gap: 0.375rem;
		min-height: 2.75rem;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 700;
		letter-spacing: 0.16em;
		text-transform: uppercase;
		text-decoration: underline;
		text-underline-offset: 4px;
		text-decoration-thickness: 1.5px;
		white-space: nowrap;
	}

	/* a phone: cover and title side by side, the reason under them, then the
	   count and the way in */
	@media (max-width: 639px) {
		.dn-teaser__ink {
			grid-template-columns: 6rem minmax(0, 1fr);
			grid-template-areas:
				'sleeve title'
				'sleeve by'
				'why why'
				'kicker go';
			grid-template-rows: auto 1fr auto auto;
			gap: 0.375rem 1rem;
			padding: 1.875rem 1rem 0.875rem;
		}
		.dn-teaser__ink.is-done {
			grid-template-columns: minmax(0, 1fr);
			grid-template-areas:
				'title'
				'why'
				'covers'
				'kicker'
				'go';
		}
		.dn-teaser__title {
			font-size: 1.875rem;
		}
		.dn-teaser__why {
			margin: 0.25rem 0 0;
			font-size: 0.96875rem;
		}
		.dn-teaser__kicker {
			align-self: center;
			margin: 0;
		}
		.dn-teaser__go {
			justify-self: end;
		}
		.dn-teaser__go-long {
			display: none;
		}
	}
</style>
