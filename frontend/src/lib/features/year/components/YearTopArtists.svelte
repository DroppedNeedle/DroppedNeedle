<!--
	The No. 1 artist's band (their picture printed as a halftone, the name
	and plays), then 2 to 10 as round tiles in a row that scrolls.
-->
<script lang="ts">
	import type { YearReview } from '$lib/features/year/queries/year.svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import MediaTile from '$lib/media/MediaTile.svelte';
	import HalftonePhoto from '$lib/ui/HalftonePhoto.svelte';
	import HorizontalCarousel from '$lib/ui/HorizontalCarousel.svelte';
	import { artistHref } from '$lib/utils/entityRoutes';
	import { plays } from './yearFormat';

	type Artist = YearReview['top_artists'][number];

	interface Props {
		/** The whole top list, the No. 1 first. */
		artists: Artist[];
	}

	let { artists }: Props = $props();

	const top = $derived(artists[0]);
	const rest = $derived(artists.slice(1, 10));

	const photoKey = $derived(`${top.mbid}|${top.id}|${top.name}`);
	let loadedKey = $state<string | null>(null);
	const initials = $derived(
		top.name
			.split(/\s+/)
			.filter((word) => /\p{L}|\p{N}/u.test(word))
			.slice(0, 2)
			.map((word) => Array.from(word)[0])
			.join('') || '?'
	);
</script>

<section aria-label="Top artists" class="yr-artists">
	<h2 class="sr-only">Top artists</h2>
	<div class="yr-artists__band">
		<HalftonePhoto class="yr-artists__photo" loaded={loadedKey === photoKey} fallback={initials}>
			{#key photoKey}
				<ArtistImage
					mbid={top.mbid ?? ''}
					localId={top.id}
					artMissing={top.art_missing || (!top.id && !top.mbid)}
					alt=""
					size="full"
					requestSize={1200}
					rounded="none"
					showPlaceholder={false}
					className="h-full w-full bg-transparent!"
					onload={() => (loadedKey = photoKey)}
				/>
			{/key}
		</HalftonePhoto>
		<div class="yr-artists__words">
			<p class="yr-artists__eyebrow">No. 1 artist</p>
			{#if top.id}
				<a href={artistHref(top.id)} class="yr-artists__name">{top.name}</a>
			{:else}
				<p class="yr-artists__name">{top.name}</p>
			{/if}
			<p class="yr-artists__plays">{plays(top.plays)}</p>
		</div>
	</div>

	{#if rest.length > 0}
		<HorizontalCarousel class="yr-artists__row">
			{#each rest as artist, i (artist.id ?? artist.name)}
				<MediaTile
					kind="artist"
					title={artist.name}
					href={artist.id ? artistHref(artist.id) : undefined}
					class="w-32 shrink-0 sm:w-40"
				>
					{#snippet art()}
						<ArtistImage
							mbid={artist.mbid ?? ''}
							localId={artist.id}
							artMissing={artist.art_missing || (!artist.id && !artist.mbid)}
							alt={artist.name}
							size="full"
							requestSize={250}
							className="h-full w-full"
						/>
					{/snippet}
					{#snippet meta()}No. {i + 2} · {plays(artist.plays)}{/snippet}
				</MediaTile>
			{/each}
		</HorizontalCarousel>
	{/if}
</section>

<style>
	.yr-artists {
		display: flex;
		flex-direction: column;
		gap: 2rem;
	}

	.yr-artists__band {
		position: relative;
		display: flex;
		align-items: flex-end;
		min-height: 22rem;
		isolation: isolate;
	}
	.yr-artists__band :global(.yr-artists__photo) {
		position: absolute;
		inset: 0 0 0 30%;
		z-index: -1;
	}

	.yr-artists__words {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		min-width: 0;
		padding-bottom: 1.5rem;
	}
	.yr-artists__eyebrow,
	.yr-artists__plays {
		font-family: var(--font-mono);
		font-size: 0.875rem;
		font-weight: 600;
		letter-spacing: 0.2em;
		text-transform: uppercase;
	}
	.yr-artists__eyebrow {
		color: var(--dn-muted);
	}
	.yr-artists__name {
		font-family: var(--font-display);
		font-size: clamp(3.5rem, 12vw, 8rem);
		line-height: 0.9;
		text-transform: uppercase;
		color: var(--dn-title);
		overflow-wrap: anywhere;
		font-synthesis: none;
	}
	a.yr-artists__name:hover {
		text-decoration: underline;
		text-decoration-thickness: 2px;
		text-underline-offset: 0.12em;
	}

	@media (max-width: 639px) {
		.yr-artists__band {
			min-height: 18rem;
		}
		.yr-artists__band :global(.yr-artists__photo) {
			inset: 0;
		}
		.yr-artists__eyebrow,
		.yr-artists__plays {
			font-size: 0.6875rem;
			letter-spacing: 0.16em;
		}
	}
</style>
