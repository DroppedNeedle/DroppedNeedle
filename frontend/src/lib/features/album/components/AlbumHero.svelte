<!--
	The top of every album page, local or MusicBrainz: the cover in its
	sleeve, a small eyebrow, the title as large as it can wrap, one meta
	line, the details (edition plate, badges), then one row of actions. The
	page's dot frame shows through at the band's top corners. From 1024 a
	column on the right lists the release's real facts, under a decorative
	barcode from 1280; narrower, the same facts run as one line under the
	details. The title is the page's only h1.
-->
<script lang="ts" module>
	export interface AlbumFact {
		label: string;
		value: string;
	}
</script>

<script lang="ts">
	import type { Snippet } from 'svelte';
	import BarcodeOrnament from '$lib/ui/BarcodeOrnament.svelte';
	import VinylSleeve from '$lib/ui/VinylSleeve.svelte';

	interface Props {
		eyebrow: string;
		title: string;
		/** A short note under the title, such as MusicBrainz's disambiguation. */
		note?: string | null;
		/** The cover, filling a square. */
		cover: Snippet;
		/** The meta line: artist, year, counts, formats, stars. */
		meta: Snippet;
		/** The edition plate and the album's badges, above the actions. */
		details?: Snippet;
		actions: Snippet;
		/** A running download, under the actions. */
		status?: Snippet;
		/** The way back, above everything. */
		back?: Snippet;
		/** Real facts only (label, country, date); none leaves the column out. */
		facts?: AlbumFact[];
	}

	let {
		eyebrow,
		title,
		note = null,
		cover,
		meta,
		details,
		actions,
		status,
		back,
		facts = []
	}: Props = $props();

	// A long title steps down so it wraps to a few lines, never clips.
	const titleSize = $derived(title.length > 48 ? 'long' : title.length > 22 ? 'mid' : 'short');
</script>

<section class="dn-album-hero" aria-labelledby="album-hero-title">
	<div class="dn-decor dn-album-hero__wash" aria-hidden="true"></div>

	<div class="dn-album-hero__inner">
		{#if back}
			<div class="dn-album-hero__back">{@render back()}</div>
		{/if}

		<div class="dn-album-hero__grid">
			<VinylSleeve class="dn-album-hero__sleeve">
				{@render cover()}
			</VinylSleeve>

			<div class="dn-album-hero__main">
				<p class="dn-album-hero__eyebrow">{eyebrow}</p>
				<h1 id="album-hero-title" class="dn-album-hero__title" data-size={titleSize}>
					{title}
				</h1>
				{#if note}
					<p class="text-sm text-muted italic">({note})</p>
				{/if}
				<div class="dn-album-hero__meta">
					{@render meta()}
				</div>
				{#if details}
					<div class="dn-album-hero__details">{@render details()}</div>
				{/if}
				{#if facts.length > 0}
					<p class="dn-album-hero__facts-line">
						{#each facts as fact, index (fact.label)}
							{#if index > 0}<span aria-hidden="true" class="mx-1.5">·</span>{/if}<span
								><span class="sr-only">{fact.label}: </span>{fact.value}</span
							>
						{/each}
					</p>
				{/if}
				<div class="dn-album-hero__actions">
					{@render actions()}
				</div>
				{#if status}
					<div class="dn-album-hero__status">{@render status()}</div>
				{/if}
			</div>

			{#if facts.length > 0}
				<div class="dn-album-hero__side">
					<BarcodeOrnament class="dn-album-hero__barcode" />
					<dl class="dn-album-hero__facts">
						{#each facts as fact (fact.label)}
							<div>
								<dt>{fact.label}</dt>
								<dd>{fact.value}</dd>
							</div>
						{/each}
					</dl>
				</div>
			{/if}
		</div>
	</div>
</section>

<style>
	.dn-album-hero {
		position: relative;
		isolation: isolate;
		overflow: clip;
	}

	/* Light only: a faint green cloud behind the words, like ink on paper. */
	.dn-album-hero__wash {
		display: none;
		position: absolute;
		inset: 0;
		z-index: -1;
		pointer-events: none;
	}
	:global([data-theme='light']) .dn-album-hero__wash {
		display: block;
		background:
			radial-gradient(
				ellipse 50% 75% at 42% 45%,
				color-mix(in oklab, var(--dn-action) 30%, transparent),
				transparent 72%
			),
			radial-gradient(
				ellipse 35% 80% at 100% 0%,
				color-mix(in oklab, var(--dn-action) 28%, transparent),
				transparent 75%
			),
			radial-gradient(
				ellipse 30% 80% at 0% 0%,
				color-mix(in oklab, var(--dn-action) 22%, transparent),
				transparent 75%
			);
	}

	.dn-album-hero__inner {
		position: relative;
		width: 100%;
		max-width: 80rem;
		margin: 0 auto;
		padding: 1rem 1rem 1.5rem;
	}

	.dn-album-hero__back {
		margin-bottom: 0.5rem;
	}

	.dn-album-hero__grid {
		display: grid;
		gap: 1.5rem;
		align-items: center;
	}

	.dn-album-hero__grid > :global(.dn-album-hero__sleeve) {
		/* a phone shows the cover alone, centred */
		justify-self: center;
		width: min(100%, 19rem);
	}

	.dn-album-hero__main {
		display: flex;
		min-width: 0;
		flex-direction: column;
		align-items: flex-start;
	}

	.dn-album-hero__eyebrow {
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 700;
		letter-spacing: 0.22em;
		text-transform: uppercase;
	}

	.dn-album-hero__title {
		max-width: 100%;
		margin-top: 0.375rem;
		font-size: clamp(2.75rem, 1.1rem + 4.3vw, 4.75rem);
		line-height: 0.92;
		overflow-wrap: break-word;
		hyphens: auto;
		text-wrap: balance;
	}
	.dn-album-hero__title[data-size='mid'] {
		font-size: clamp(2.25rem, 0.9rem + 3.3vw, 3.75rem);
	}
	.dn-album-hero__title[data-size='long'] {
		font-size: clamp(1.875rem, 0.85rem + 2.3vw, 2.875rem);
		line-height: 0.98;
	}

	.dn-album-hero__meta {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.25rem 0.625rem;
		margin-top: 0.75rem;
		font-size: 0.9375rem;
	}

	.dn-album-hero__details {
		display: flex;
		width: 100%;
		max-width: 40rem;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.5rem;
		margin-top: 0.875rem;
	}

	.dn-album-hero__facts-line {
		margin-top: 0.625rem;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}

	.dn-album-hero__actions {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.625rem 0.75rem;
		margin-top: 1.125rem;
	}

	.dn-album-hero__status {
		width: 100%;
		max-width: 40rem;
		margin-top: 0.875rem;
	}

	.dn-album-hero__side {
		display: none;
	}

	.dn-album-hero__details:empty,
	.dn-album-hero__status:empty {
		display: none;
	}

	.dn-album-hero__facts {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		padding-block: 0.75rem;
		border-block: 2px solid currentColor;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		line-height: 1.2;
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}
	.dn-album-hero__facts dt {
		font-size: 0.6875rem;
		color: var(--dn-muted);
	}
	.dn-album-hero__facts dd {
		font-weight: 700;
		overflow-wrap: anywhere;
	}

	@media (min-width: 480px) {
		.dn-album-hero__grid > :global(.dn-album-hero__sleeve) {
			justify-self: start;
			width: min(100%, 24rem);
		}
	}

	@media (min-width: 768px) {
		.dn-album-hero__inner {
			padding: 1.25rem 1.5rem 2rem;
		}
		.dn-album-hero__grid {
			grid-template-columns: 16rem minmax(0, 1fr);
			gap: 2rem;
		}
		.dn-album-hero__grid > :global(.dn-album-hero__sleeve) {
			width: 100%;
		}
	}

	@media (min-width: 1024px) {
		.dn-album-hero__inner {
			padding: 1.5rem 2rem 2.25rem;
		}
		.dn-album-hero__grid {
			grid-template-columns: 19rem minmax(0, 1fr) auto;
			gap: 2.25rem;
		}
		.dn-album-hero__facts-line {
			display: none;
		}
		.dn-album-hero__side {
			display: flex;
			flex-direction: column;
			gap: 0.75rem;
			width: 9.5rem;
			align-self: stretch;
			justify-content: center;
			padding-left: 1.75rem;
			border-left: 1px solid var(--dn-line);
		}
	}

	@media (min-width: 1280px) {
		.dn-album-hero__grid {
			grid-template-columns: 23rem minmax(0, 1fr) auto;
			gap: 2.5rem;
		}
		.dn-album-hero__side {
			width: 11rem;
		}
	}

	.dn-album-hero__side :global(.dn-album-hero__barcode) {
		display: none;
		width: 100%;
		height: auto;
	}
	@media (min-width: 1280px) {
		.dn-album-hero__side :global(.dn-album-hero__barcode) {
			display: block;
		}
	}
</style>
