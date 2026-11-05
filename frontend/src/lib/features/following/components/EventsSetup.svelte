<!--
	Upcoming Events' first run: a torn paper card, taped to the page, that
	asks for the first city. A halftone print of a followed artist's photo
	sits tilted behind its right edge (decoration; the word LIVE on a dot
	field when there is no photo). The paper is a layer under the card's
	content, so the search's list can hang below the tear.
-->
<script lang="ts">
	import { CalendarClock } from 'lucide-svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import Card from '$lib/ui/Card.svelte';
	import HalftonePhoto from '$lib/ui/HalftonePhoto.svelte';
	import CitySearchInput from '$lib/features/following/components/CitySearchInput.svelte';
	import { getFollowedArtistsQuery } from '$lib/features/following/queries/FollowQueries.svelte';
	import type { CitySearchResult } from '$lib/features/following/queries/types';

	interface Props {
		onpick: (city: CitySearchResult) => void;
	}

	let { onpick }: Props = $props();

	const followed = getFollowedArtistsQuery();
	const printed = $derived(
		(followed.data ?? []).find((artist) => artist.image_url) ?? followed.data?.[0] ?? null
	);
	let photoLoaded = $state(false);
</script>

<section class="dn-gig-setup" aria-labelledby="gig-setup-title">
	<div class="dn-gig-setup__print" aria-hidden="true">
		<HalftonePhoto loaded={photoLoaded} fallback="Live" fade="bottom" class="size-full">
			{#if printed}
				<ArtistImage
					mbid={printed.mbid}
					alt=""
					size="full"
					requestSize={1200}
					rounded="none"
					showPlaceholder={false}
					remoteUrl={printed.image_url}
					className="h-full w-full bg-transparent!"
					onload={() => (photoLoaded = true)}
				/>
			{/if}
		</HalftonePhoto>
	</div>

	<div class="dn-gig-setup__sheet">
		<Card variant="torn" padding="none" class="dn-gig-setup__paper" aria-hidden="true" />
		<span class="dn-decor dn-gig-setup__tape" aria-hidden="true"></span>

		<div class="dn-gig-setup__body">
			<div class="dn-gig-setup__mark" aria-hidden="true">
				<svg class="dn-gig-setup__burst" viewBox="0 0 40 48">
					<path d="M36 12 L22 4 M38 25 L20 24 M36 38 L22 45" />
				</svg>
				<span class="dn-gig-setup__icon">
					<CalendarClock class="size-9" strokeWidth={1.75} />
				</span>
				<svg class="dn-gig-setup__burst dn-gig-setup__burst--end" viewBox="0 0 40 48">
					<path d="M36 12 L22 4 M38 25 L20 24 M36 38 L22 45" />
				</svg>
			</div>
			<h2 id="gig-setup-title" class="dn-gig-setup__title">Where do you go to gigs?</h2>
			<CitySearchInput {onpick} autofocus className="dn-gig-setup__search" />
			<p class="dn-gig-setup__hint">
				Pick as many cities as you like - we'll show upcoming concerts from artists you follow near
				each one.
			</p>
		</div>
	</div>
</section>

<style>
	.dn-gig-setup {
		position: relative;
		display: grid;
		place-items: center;
		padding: 2.5rem 1rem 4rem;
	}

	/* the photo print, tilted, its left half behind the card */
	.dn-gig-setup__print {
		position: absolute;
		top: 0.5rem;
		left: calc(50% + 9rem);
		width: min(30rem, 42vw);
		aspect-ratio: 4 / 5;
		rotate: 5deg;
		pointer-events: none;
		mask-image: radial-gradient(ellipse 62% 58% at 50% 45%, #000 55%, transparent 100%);
		-webkit-mask-image: radial-gradient(ellipse 62% 58% at 50% 45%, #000 55%, transparent 100%);
	}

	.dn-gig-setup__sheet {
		position: relative;
		z-index: 1;
		width: min(36rem, 100%);
	}
	.dn-gig-setup__sheet :global(.dn-gig-setup__paper) {
		position: absolute;
		inset: 0;
		rotate: -1.75deg;
		filter: drop-shadow(0 22px 30px rgb(0 0 0 / 0.5));
	}
	:global([data-theme='light']) .dn-gig-setup__sheet :global(.dn-gig-setup__paper) {
		filter: drop-shadow(0 16px 22px rgb(14 46 29 / 0.22));
	}

	/* a strip of acid tape across the top edge */
	.dn-gig-setup__tape {
		position: absolute;
		z-index: 1;
		top: -1.1rem;
		left: 50%;
		width: 9rem;
		height: 2.25rem;
		translate: -50% 0;
		rotate: 3deg;
		background-color: color-mix(in oklab, var(--dn-action) 88%, transparent);
		clip-path: polygon(
			0 8%,
			4% 0,
			8% 10%,
			13% 2%,
			100% 0,
			96% 30%,
			100% 55%,
			97% 80%,
			100% 100%,
			0 100%,
			3% 70%,
			0 45%,
			4% 25%
		);
		box-shadow: 0 2px 4px rgb(0 0 0 / 0.2);
	}

	.dn-gig-setup__body {
		position: relative;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 1.25rem;
		padding: 3.25rem 3rem 3rem;
		text-align: center;
		color: var(--dn-paper-content);
	}

	.dn-gig-setup__mark {
		display: flex;
		align-items: center;
		gap: 0.375rem;
	}
	.dn-gig-setup__icon {
		display: grid;
		width: 5rem;
		height: 5rem;
		place-items: center;
		border-radius: 9999px;
		background-color: var(--dn-paper-content);
		color: var(--dn-action);
	}
	/* three strokes either side, acid over a dark edge so they hold on paper */
	.dn-gig-setup__burst {
		width: 2rem;
		height: 2.4rem;
		fill: none;
		stroke: var(--dn-action);
		stroke-linecap: round;
		stroke-width: 4;
		paint-order: stroke;
		filter: drop-shadow(0 0 0.6px var(--dn-paper-content))
			drop-shadow(0 0 0.6px var(--dn-paper-content));
	}
	.dn-gig-setup__burst--end {
		scale: -1 1;
	}

	.dn-gig-setup__title {
		font-family: var(--font-display);
		font-size: 2.75rem;
		font-weight: 400;
		line-height: 1;
		letter-spacing: 0.01em;
		text-transform: uppercase;
		color: var(--dn-paper-content);
		font-synthesis: none;
	}
	.dn-gig-setup__body :global(.dn-gig-setup__search) {
		width: 100%;
	}
	.dn-gig-setup__hint {
		max-width: 26rem;
		font-size: 0.9375rem;
		line-height: 1.5;
		color: color-mix(in oklab, var(--dn-paper-content) 76%, var(--dn-paper));
	}

	@media (max-width: 1023px) {
		.dn-gig-setup__print {
			left: calc(50% + 6rem);
			width: min(22rem, 46vw);
		}
	}

	@media (max-width: 639px) {
		.dn-gig-setup {
			padding: 2rem 0.25rem 3rem;
		}
		.dn-gig-setup__print {
			display: none;
		}
		.dn-gig-setup__body {
			gap: 1rem;
			padding: 2.75rem 1.25rem 2.25rem;
		}
		.dn-gig-setup__icon {
			width: 4rem;
			height: 4rem;
		}
		.dn-gig-setup__title {
			font-size: 2rem;
		}
		.dn-gig-setup__hint {
			font-size: 0.875rem;
		}
	}

	@media (prefers-contrast: more), (forced-colors: active) {
		.dn-gig-setup__print,
		.dn-gig-setup__tape {
			display: none;
		}
	}
</style>
