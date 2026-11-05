<!--
	The top of every artist page, MusicBrainz or local: the artist's photo
	printed large as a halftone at the right, fading into the page; the name
	as big as it can wrap in Anton; a mono caps line of real facts; then the
	page's buttons, and under them what hangs off them (the auto-download
	switch, the play-from choice). The counts sit at the lower right and the
	"DN / CC 3.2 - ART" ticket over the photo's top edge. On a phone the
	photo runs across the top behind the name. The name is the page's only h1.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import BarcodeTicket from '$lib/ui/BarcodeTicket.svelte';
	import HalftonePhoto from '$lib/ui/HalftonePhoto.svelte';
	import StatRow, { type Stat } from '$lib/ui/StatRow.svelte';
	import { outsideLink } from '$lib/ui/outsideLink';
	import { isValidMbid } from '$lib/utils/formatting';
	import { getArtistImageCreditQuery } from '$lib/features/artist/queries/ArtistImageCreditQuery.svelte';

	interface Props {
		name: string;
		/** The MusicBrainz id, for the photo and its credit. */
		mbid?: string | null;
		/** The library id, for a local artist's picture. */
		localId?: string | null;
		/** The server says there is no picture. */
		artMissing?: boolean;
		/** A picture from the provider, used when remote images load directly. */
		remoteUrl?: string | null;
		/** The mono line: type, disambiguation, country, years. */
		facts: string[];
		stats: Stat[];
		/** The way back, at the top left. */
		back?: Snippet;
		/** Chips beside the mono line (In library). */
		badges?: Snippet;
		actions: Snippet;
		/** What hangs under the buttons. */
		below?: Snippet;
	}

	let {
		name,
		mbid = null,
		localId = null,
		artMissing = false,
		remoteUrl = null,
		facts,
		stats,
		back,
		badges,
		actions,
		below
	}: Props = $props();

	// The photo counts as loaded only for the artist it was loaded for.
	const photoKey = $derived(`${mbid}|${localId}`);
	let loadedKey = $state<string | null>(null);
	const loaded = $derived(loadedKey === photoKey);
	const providerId = $derived(mbid && isValidMbid(mbid) ? mbid : null);

	const creditQuery = getArtistImageCreditQuery(() => ({
		mbid: providerId ?? '',
		enabled: loaded && providerId !== null
	}));
	const credit = $derived(providerId ? (creditQuery.data ?? null) : null);
	const creditFileLink = $derived(outsideLink(credit?.file_url));
	const creditLicenceLink = $derived(outsideLink(credit?.licence_url));

	// A long name steps down so it wraps to a few lines, never clips.
	const size = $derived(name.length > 26 ? 'long' : name.length > 8 ? 'mid' : 'short');
	const initials = $derived(
		name
			.split(/\s+/)
			.filter((word) => /\p{L}|\p{N}/u.test(word))
			.slice(0, 2)
			.map((word) => Array.from(word)[0])
			.join('') || '?'
	);
</script>

<section id="section-overview" class="dn-artist-hero" aria-labelledby="artist-hero-title">
	<HalftonePhoto class="dn-artist-hero__photo" {loaded} fallback={initials}>
		{#key photoKey}
			<ArtistImage
				mbid={mbid ?? ''}
				{localId}
				{artMissing}
				{remoteUrl}
				alt=""
				size="full"
				requestSize={1200}
				rounded="none"
				priority
				showPlaceholder={false}
				className="h-full w-full bg-transparent!"
				onload={() => (loadedKey = photoKey)}
			/>
		{/key}
	</HalftonePhoto>

	<div class="dn-decor dn-artist-hero__halo" aria-hidden="true"></div>

	<BarcodeTicket label="DN / CC 3.2 - ART" class="dn-artist-hero__ticket" />

	<div class="dn-artist-hero__inner">
		{#if back}
			<div class="dn-artist-hero__back">{@render back()}</div>
		{/if}

		<div class="dn-artist-hero__body">
			<div class="dn-artist-hero__main">
				<h1 id="artist-hero-title" class="dn-artist-hero__title hero-title" data-size={size}>
					{name}
				</h1>
				{#if facts.length > 0 || badges}
					<div class="dn-artist-hero__facts">
						{#if facts.length > 0}
							<p>
								{#each facts as fact, index (index)}
									{#if index > 0}<span class="dn-artist-hero__slash" aria-hidden="true">/</span
										>{/if}<span>{fact}</span>
								{/each}
							</p>
						{/if}
						{@render badges?.()}
					</div>
				{/if}
				<div class="dn-artist-hero__actions">
					{@render actions()}
				</div>
				{#if below}
					<div class="dn-artist-hero__below">{@render below()}</div>
				{/if}
			</div>

			<StatRow {stats} label="Releases" class="dn-artist-hero__stats" />
		</div>
	</div>

	{#if credit && creditFileLink}
		<p class="dn-artist-hero__credit">
			<a href={creditFileLink} target="_blank" rel="noopener noreferrer"
				>Photo: {credit.author ?? 'Wikimedia Commons'}</a
			>{#if credit.licence},
				{#if creditLicenceLink}
					<a href={creditLicenceLink} target="_blank" rel="noopener noreferrer license"
						>{credit.licence}</a
					>
				{:else}
					{credit.licence}
				{/if}
			{/if}
		</p>
	{/if}
</section>

<style>
	.dn-artist-hero {
		/* no stacking context of its own, so the photo blends with the page's
		   dot frame behind it */
		position: relative;
		overflow: clip;
		scroll-margin-top: 8rem;
	}

	/* phone: the photo runs across the top, the name sits under its fade */
	.dn-artist-hero :global(.dn-artist-hero__photo) {
		position: absolute;
		z-index: -1;
		inset: 0 0 auto 0;
		height: 19rem;
	}
	.dn-artist-hero :global(.dn-artist-hero__photo .dn-halftone__print) {
		mask-image: linear-gradient(to top, transparent 0, #000 60%);
		-webkit-mask-image: linear-gradient(to top, transparent 0, #000 60%);
	}

	/* an acid dither round the photo's edges, behind it: the photo's light
	   covers it on the faces, so it shows only where the print goes dark */
	.dn-artist-hero__halo {
		position: absolute;
		z-index: -2;
		inset: 0 0 auto 0;
		height: 20rem;
		background-image: radial-gradient(circle at center, var(--dn-frit) 0 1.5px, transparent 1.9px);
		background-size: 7px 7px;
		opacity: var(--dn-frit-opacity);
		pointer-events: none;
		mask-image: radial-gradient(
			ellipse 62% 70% at 55% 38%,
			transparent 30%,
			#000 52%,
			transparent 78%
		);
		-webkit-mask-image: radial-gradient(
			ellipse 62% 70% at 55% 38%,
			transparent 30%,
			#000 52%,
			transparent 78%
		);
	}

	.dn-artist-hero :global(.dn-artist-hero__ticket) {
		position: absolute;
		z-index: 2;
		top: 1rem;
		right: 0.75rem;
		width: 8rem;
		rotate: -5deg;
	}

	.dn-artist-hero__inner {
		position: relative;
		width: 100%;
		max-width: 80rem;
		margin: 0 auto;
		padding: 1rem 1rem 1.5rem;
	}

	.dn-artist-hero__back {
		margin-bottom: 0.5rem;
	}

	.dn-artist-hero__body {
		display: flex;
		flex-direction: column;
		gap: 1.5rem;
		padding-top: 9.5rem;
	}

	.dn-artist-hero__main {
		display: flex;
		min-width: 0;
		flex-direction: column;
		align-items: flex-start;
	}

	.dn-artist-hero__title {
		max-width: 100%;
		font-size: clamp(3.75rem, 1rem + 11vw, 10.5rem);
		line-height: 0.9;
		letter-spacing: 0.005em;
		overflow-wrap: break-word;
		hyphens: auto;
		text-wrap: balance;
	}
	.dn-artist-hero__title[data-size='mid'] {
		font-size: clamp(2.75rem, 0.9rem + 6.4vw, 6.75rem);
		line-height: 0.94;
	}
	.dn-artist-hero__title[data-size='long'] {
		font-size: clamp(2.125rem, 0.9rem + 3.9vw, 4.5rem);
		line-height: 0.95;
	}

	.dn-artist-hero__facts {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.5rem 0.875rem;
		margin-top: 1rem;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 500;
		letter-spacing: 0.14em;
		text-transform: uppercase;
	}
	.dn-artist-hero__slash {
		margin-inline: 0.625rem;
		color: var(--dn-muted);
	}

	.dn-artist-hero__actions {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.5rem 0.625rem;
		margin-top: 1.25rem;
	}

	/* a phone keeps the buttons on one line */
	@media (max-width: 639px) {
		.dn-artist-hero__actions {
			gap: 0.5rem 0.375rem;
		}
		.dn-artist-hero__actions :global(.btn:not(.btn-circle, .btn-square)) {
			--btn-p: 0.75rem;
		}
	}

	.dn-artist-hero__below {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.5rem;
		margin-top: 0.75rem;
	}
	.dn-artist-hero__below:empty {
		display: none;
	}

	.dn-artist-hero :global(.dn-artist-hero__stats) {
		width: 100%;
	}

	/* under the counts on a phone, at the photo's lower corner on a wider
	   screen */
	.dn-artist-hero__credit {
		margin: -0.75rem 1rem 0.75rem;
		overflow: hidden;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		text-align: right;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--dn-muted);
	}
	.dn-artist-hero__credit a:hover {
		color: var(--color-base-content);
		text-decoration: underline;
	}

	@media (min-width: 768px) {
		.dn-artist-hero__inner {
			padding: 1.25rem 1.5rem 1.75rem;
		}
		.dn-artist-hero :global(.dn-artist-hero__photo) {
			inset: 0 0 0 auto;
			width: min(62%, 60rem);
			height: auto;
		}
		.dn-artist-hero :global(.dn-artist-hero__photo .dn-halftone__print) {
			mask-image:
				linear-gradient(to right, transparent 0, #000 26%),
				linear-gradient(to top, transparent 0, transparent 8%, #000 48%);
			-webkit-mask-image:
				linear-gradient(to right, transparent 0, #000 26%),
				linear-gradient(to top, transparent 0, transparent 8%, #000 48%);
		}
		.dn-artist-hero__halo {
			inset: 0 0 0 auto;
			width: min(70%, 66rem);
			height: auto;
			mask-image: radial-gradient(
				ellipse 58% 72% at 58% 42%,
				transparent 40%,
				#000 60%,
				transparent 92%
			);
			-webkit-mask-image: radial-gradient(
				ellipse 58% 72% at 58% 42%,
				transparent 40%,
				#000 60%,
				transparent 92%
			);
		}
		.dn-artist-hero__body {
			flex-direction: row;
			align-items: flex-end;
			justify-content: space-between;
			gap: 2rem;
			padding-top: 0.5rem;
			min-height: 17rem;
		}
		.dn-artist-hero__main {
			flex: 1 1 auto;
			max-width: 60%;
		}
		.dn-artist-hero__credit {
			position: absolute;
			right: 0.75rem;
			bottom: 0.375rem;
			max-width: 60%;
			margin: 0;
		}
		/* the counts sit on the photo's fade; a soft shadow of the page keeps
		   them clear of any light left in it */
		.dn-artist-hero :global(.dn-artist-hero__stats) {
			width: auto;
			flex: none;
			margin-bottom: 0.25rem;
			text-shadow:
				0 0 10px var(--color-base-100),
				0 0 4px var(--color-base-100);
		}
		.dn-artist-hero :global(.dn-artist-hero__ticket) {
			top: 1.75rem;
			right: 2rem;
			width: 10rem;
		}
	}

	@media (min-width: 1024px) {
		.dn-artist-hero__inner {
			padding: 1.5rem 2rem 2rem;
		}
		.dn-artist-hero__body {
			min-height: 18rem;
		}
	}

	/* the plain photo, kept clear of the words */
	@media (prefers-contrast: more), (forced-colors: active) {
		.dn-artist-hero :global(.dn-artist-hero__photo .dn-halftone__print) {
			mask-image: none;
			-webkit-mask-image: none;
		}
		.dn-artist-hero :global(.dn-artist-hero__photo) {
			height: 9rem;
		}
	}
	@media (min-width: 768px) and ((prefers-contrast: more) or (forced-colors: active)) {
		.dn-artist-hero :global(.dn-artist-hero__photo) {
			width: 36%;
			height: 60%;
		}
	}
</style>
