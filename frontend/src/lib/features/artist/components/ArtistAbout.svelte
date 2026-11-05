<!--
	The artist page's About card: the biography (Last.fm's when it has one,
	else MusicBrainz's), a column of real facts and the artist's links, then
	the MusicBrainz link line for a library artist and the stores that
	support the artist.
-->
<script lang="ts" module>
	export interface AboutFact {
		label: string;
		value: string;
	}
</script>

<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { components } from '$lib/api/v3/openapi';
	import Card from '$lib/ui/Card.svelte';
	import ArtistDescription from './ArtistDescription.svelte';
	import ArtistLinks from './ArtistLinks.svelte';
	import LastFmEnrichment from '$lib/features/lastfm/components/LastFmEnrichment.svelte';

	type ExternalLink = components['schemas']['ExternalLink'];
	type LastFmArtistEnrichment = components['schemas']['LastFmArtistEnrichment'];

	interface Props {
		name: string;
		description?: string | null;
		descriptionLoading?: boolean;
		lastfm?: LastFmArtistEnrichment | null;
		lastfmLoading?: boolean;
		facts: AboutFact[];
		links: ExternalLink[];
		musicbrainzUrl?: string | null;
		/** The MusicBrainz link line and the stores, under the rest. */
		footer?: Snippet;
		/** Layout only. */
		class?: string;
	}

	let {
		name,
		description = null,
		descriptionLoading = false,
		lastfm = null,
		lastfmLoading = false,
		facts,
		links,
		musicbrainzUrl = null,
		footer,
		class: className = ''
	}: Props = $props();

	const hasSide = $derived(facts.length > 0 || links.length > 0 || !!musicbrainzUrl);
</script>

<Card
	as="section"
	variant="bordered"
	padding="none"
	id="section-about"
	class="dn-about scroll-mt-32 {className}"
	aria-labelledby="artist-about-title"
>
	<div class="dn-about__grid" class:dn-about__grid--side={hasSide}>
		<div class="dn-about__main">
			<h2 id="artist-about-title" class="dn-about__title">About {name}</h2>
			<div class="dn-about__text mt-4 text-[0.9375rem] text-base-content">
				{#if !lastfm?.bio}
					<ArtistDescription {description} loading={descriptionLoading} />
				{/if}
				{#if lastfmLoading || lastfm}
					<LastFmEnrichment enrichment={lastfm} loading={lastfmLoading} />
				{/if}
			</div>
		</div>

		{#if hasSide}
			<div class="dn-about__side">
				{#if facts.length > 0}
					<dl class="dn-about__facts">
						{#each facts as fact (fact.label)}
							<dt>{fact.label}</dt>
							<dd>{fact.value}</dd>
						{/each}
					</dl>
				{/if}
				<ArtistLinks {links} {musicbrainzUrl} />
			</div>
		{/if}
	</div>

	{#if footer}
		<div class="dn-about__footer">{@render footer()}</div>
	{/if}
</Card>

<style>
	/* the biography fills whatever height the card is given */
	:global(.dn-about) {
		display: flex;
		flex-direction: column;
	}
	.dn-about__grid {
		display: grid;
		flex: 1 1 auto;
		gap: 1.5rem;
		padding: 1.25rem;
	}
	.dn-about__main {
		display: flex;
		min-width: 0;
		min-height: 0;
		flex-direction: column;
	}
	.dn-about__text {
		display: flex;
		flex: 1 1 auto;
		min-height: 0;
		flex-direction: column;
		gap: 1rem;
	}
	@media (min-width: 640px) {
		.dn-about__grid {
			padding: 1.5rem;
		}
		.dn-about__grid--side {
			grid-template-columns: minmax(0, 1fr) minmax(12rem, 17.5rem);
			gap: 2rem;
		}
		.dn-about__side {
			padding-left: 1.5rem;
			border-left: 1px solid var(--dn-line);
		}
	}

	.dn-about__title {
		font-family: var(--font-display);
		font-size: 2rem;
		line-height: 1;
		color: var(--color-base-content);
		overflow-wrap: anywhere;
	}

	.dn-about__side {
		display: flex;
		min-width: 0;
		flex-direction: column;
		gap: 1rem;
	}

	.dn-about__facts {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr);
		gap: 0.375rem 1.25rem;
		padding-bottom: 1rem;
		border-bottom: 1px solid var(--dn-line);
		font-size: 0.875rem;
	}
	.dn-about__facts dt {
		padding-top: 0.125rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.12em;
		text-transform: uppercase;
	}
	.dn-about__facts dd {
		overflow-wrap: anywhere;
	}

	.dn-about__footer {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		padding: 1rem 1.25rem 1.25rem;
		border-top: 1px solid var(--dn-line);
	}
	.dn-about__footer:empty {
		display: none;
	}
	@media (min-width: 640px) {
		.dn-about__footer {
			padding-inline: 1.5rem;
		}
	}
</style>
