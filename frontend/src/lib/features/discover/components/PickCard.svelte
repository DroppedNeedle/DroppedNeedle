<!--
	The pick on the counter: the torn paper card with its tape, the sleeve
	with the play button on the cover, the pager, the kicker, the title and
	artist, the reason on its index card, and Keep, Later and No along the
	foot. An answered pick carries its state as a sticker on the sleeve and
	its answers are off. On a phone a swipe moves between picks.
-->
<script lang="ts">
	import { ChevronLeft, ChevronRight, Pause, Play } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Sticker from '$lib/ui/Sticker.svelte';
	import Tape from '$lib/ui/Tape.svelte';
	import VinylSleeve from '$lib/ui/VinylSleeve.svelte';
	import { themeStore } from '$lib/stores/theme.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import type { SetPick } from '$lib/features/discover/queries/week.svelte';
	import type { StateLook } from '$lib/ui/StatusChip.svelte';
	import PickActionBar from './PickActionBar.svelte';
	import ReasonCard from './ReasonCard.svelte';

	interface Props {
		album: SetPick['album'];
		reason: SetPick['reason'];
		/** The tapes over the card's top edge: the acid one, then an ink one. */
		tapes: [string, string?];
		/** "No. 04 - Around Circa Survive", or "Extra 2". */
		kicker: string;
		/** "4 / 8", or "Extra 2", and how it is spoken. */
		pager: { text: string; spoken: string };
		/** What became of it, while it is answered. */
		look?: StateLook;
		/** The sticker is landing now. */
		fresh?: boolean;
		/** A quiet line under the buttons. */
		note?: string;
		sampling: boolean;
		hasPrevious: boolean;
		hasNext: boolean;
		/** An answer is on its way or landing. */
		keeping: boolean;
		/** Keep, Later and No are off: it is answered, or an answer is landing. */
		answersOff: boolean;
		onprevious: () => void;
		onnext: () => void;
		onsample: () => void;
		onopened: () => void;
		onwhy: () => void;
		onkeep: () => void;
		onlater: () => void;
		onno: () => void;
	}

	let {
		album,
		reason,
		tapes,
		kicker,
		pager,
		look,
		fresh = false,
		note,
		sampling,
		hasPrevious,
		hasNext,
		keeping,
		answersOff,
		onprevious,
		onnext,
		onsample,
		onopened,
		onwhy,
		onkeep,
		onlater,
		onno
	}: Props = $props();

	const titleId = $props.id();
	const mbid = $derived(album.musicbrainz_id ?? '');
	const albumHref = $derived(mbid ? withBasePath(`/album/${mbid}`) : undefined);
	const artistHref = $derived(
		album.artist_id ? withBasePath(`/artist/${album.artist_id}`) : undefined
	);
	const long = $derived(album.title.length > 16);
	const TYPES = { album: 'Album', ep: 'EP', single: 'Single', other: '' } as const;
	const facts = $derived(
		[album.year ? String(album.year) : '', album.release_type ? TYPES[album.release_type] : '']
			.filter(Boolean)
			.join(' - ')
	);

	// a swipe across the card moves between picks; a scroll passes through
	const SWIPE_PX = 60;
	let swipe: { id: number; x: number; y: number } | null = null;

	function onpointerdown(event: PointerEvent) {
		if (event.pointerType === 'mouse') return;
		swipe = { id: event.pointerId, x: event.clientX, y: event.clientY };
	}

	function onpointerup(event: PointerEvent) {
		if (!swipe || swipe.id !== event.pointerId) return;
		const dx = event.clientX - swipe.x;
		const dy = event.clientY - swipe.y;
		swipe = null;
		if (Math.abs(dx) < SWIPE_PX || Math.abs(dy) > Math.abs(dx)) return;
		if (dx < 0 && hasNext) onnext();
		else if (dx > 0 && hasPrevious) onprevious();
	}
</script>

<div class="dn-pick-lift">
	<Card
		as="article"
		variant="torn"
		padding="none"
		class="dn-pick"
		aria-labelledby={titleId}
		{onpointerdown}
		{onpointerup}
		onpointercancel={() => (swipe = null)}
	>
		<div class="dn-pick__ink" data-theme={themeStore.paper}>
			<p class="dn-pick__tapes">
				<Tape>{tapes[0]}</Tape>
				{#if tapes[1]}<Tape look="ink">{tapes[1]}</Tape>{/if}
			</p>
			<div class="dn-pick__pager">
				<IconButton
					variant="ghost"
					size="sm"
					icon={ChevronLeft}
					label="Previous pick"
					disabled={!hasPrevious}
					onclick={onprevious}
				/>
				<span class="dn-pick__count" aria-label={pager.spoken}>{pager.text}</span>
				<IconButton
					variant="ghost"
					size="sm"
					icon={ChevronRight}
					label="Next pick"
					disabled={!hasNext}
					onclick={onnext}
				/>
			</div>

			<div class="dn-pick__body">
				<div class="dn-pick__media">
					<VinylSleeve class="dn-pick__sleeve">
						<a href={albumHref} aria-label="Open {album.title}" onclick={onopened}>
							<AlbumImage
								{mbid}
								alt="Cover of {album.title}"
								size="full"
								requestSize={500}
								responsiveSizes="(max-width: 640px) 250px, 352px"
								lazy={false}
								rounded="none"
								className="aspect-square w-full object-cover"
							/>
						</a>
						<IconButton
							variant="disc"
							size="lg"
							class="dn-pick__play"
							label={sampling ? 'Stop the sample' : `Play a sample of ${album.title}`}
							aria-pressed={sampling}
							aria-keyshortcuts="S"
							onclick={onsample}
						>
							{#snippet glyph(size)}
								{#if sampling}
									<Pause class={size} aria-hidden="true" />
								{:else}
									<Play class="{size} fill-current" aria-hidden="true" />
								{/if}
							{/snippet}
						</IconButton>
					</VinylSleeve>
					{#if look}
						<Sticker state={look} {fresh} class="dn-pick__sticker" />
					{/if}
				</div>

				<div class="dn-pick__text">
					<p class="dn-pick__kicker">{kicker}</p>
					<h2 id={titleId} class="dn-pick__title" class:is-long={long}>
						<a href={albumHref} onclick={onopened}>{album.title}</a>
					</h2>
					<div class="dn-pick__byline">
						{#if artistHref}
							<a class="dn-pick__artist" href={artistHref}>{album.artist_name}</a>
						{:else}
							<span class="dn-pick__artist">{album.artist_name}</span>
						{/if}
						{#if facts}<span class="dn-pick__facts">{facts}</span>{/if}
					</div>
					<ReasonCard {reason} {onwhy} />
				</div>
			</div>

			<PickActionBar look="pick" {keeping} off={answersOff} {onkeep} {onlater} {onno} />
			{#if note}<p class="dn-pick__note">{note}</p>{/if}
		</div>
	</Card>
</div>

<style>
	/* the torn edges come from a mask, so the lift is a shadow on a wrapper */
	.dn-pick-lift {
		height: 100%;
		filter: drop-shadow(0 16px 18px rgb(0 0 0 / 0.45));
	}
	:global([data-theme='light']) .dn-pick-lift {
		filter: drop-shadow(
			0 10px 18px color-mix(in oklab, var(--color-base-content) 16%, transparent)
		);
	}
	:global(.dn-pick) {
		height: 100%;
		min-height: 34.5rem;
		touch-action: pan-y;
	}

	/* the paper's own colours: the sheet switches to its own theme */
	.dn-pick__ink {
		position: relative;
		display: flex;
		flex-direction: column;
		height: 100%;
		padding: 3.25rem 2.375rem 1.625rem;
		color: var(--dn-paper-content);
		container-type: inline-size;
	}

	.dn-pick__tapes {
		position: absolute;
		top: 0.375rem;
		left: 1.375rem;
		z-index: 4;
		display: flex;
		margin: 0;
	}
	.dn-pick__tapes > :global(* + *) {
		margin-left: -0.25rem;
	}

	.dn-pick__pager {
		position: absolute;
		top: 1.25rem;
		right: 1.875rem;
		z-index: 4;
		display: flex;
		align-items: center;
		gap: 2px;
	}
	.dn-pick__count {
		padding: 0.2rem 0.75rem;
		border: 1.5px solid currentColor;
		border-radius: 9999px;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		white-space: nowrap;
	}

	.dn-pick__body {
		display: grid;
		grid-template-columns: 33.5rem minmax(0, 1fr);
		gap: 1.125rem;
	}
	.dn-pick__media {
		position: relative;
		align-self: start;
	}
	.dn-pick__media :global(.dn-pick__sleeve) {
		width: 30.5rem;
	}
	.dn-pick__media a {
		display: block;
	}

	.dn-pick__media :global(.dn-pick__play) {
		position: absolute;
		left: 1.125rem;
		bottom: 1.125rem;
		z-index: 3;
	}

	.dn-pick__media :global(.dn-pick__sticker) {
		position: absolute;
		top: -0.875rem;
		left: 19.5rem;
		z-index: 3;
	}

	.dn-pick__text {
		display: flex;
		flex-direction: column;
		min-width: 0;
		padding-top: 1.25rem;
	}
	.dn-pick__kicker {
		margin: 0 0 0.5625rem;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 600;
		letter-spacing: 0.16em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.dn-pick__title {
		margin: 0;
		font-family: var(--font-display);
		font-size: 3.125rem;
		font-weight: 400;
		line-height: 0.94;
		text-transform: uppercase;
		text-wrap: balance;
		overflow-wrap: anywhere;
		font-synthesis: none;
	}
	.dn-pick__title.is-long {
		font-size: 2.25rem;
	}
	.dn-pick__title a:hover,
	a.dn-pick__artist:hover {
		color: var(--color-primary);
	}
	.dn-pick__byline {
		display: flex;
		flex-direction: column;
	}
	.dn-pick__artist {
		width: fit-content;
		margin-top: 0.625rem;
		font-size: 1.0625rem;
		font-weight: 700;
		letter-spacing: 0.1em;
		text-transform: uppercase;
	}
	.dn-pick__facts {
		margin-top: 0.375rem;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 500;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.dn-pick__note {
		margin: 0.75rem 0 0;
		font-size: 0.8125rem;
		color: var(--dn-muted);
	}

	/* a narrower counter: a smaller sleeve beside the words */
	@container (max-width: 54rem) {
		.dn-pick__body {
			grid-template-columns: 22rem minmax(0, 1fr);
		}
		.dn-pick__media :global(.dn-pick__sleeve) {
			width: 22rem;
		}
		.dn-pick__media :global(.dn-pick__sticker) {
			left: 13.5rem;
		}
		.dn-pick__title {
			font-size: 2.5rem;
		}
	}

	/* a phone: the sleeve over the words, year and type beside the artist,
	   sized so the reason and its note clear the answer bar on a first screen */
	@container (max-width: 36rem) {
		.dn-pick__body {
			display: block;
		}
		.dn-pick__media :global(.dn-pick__sleeve) {
			width: min(100%, 15rem);
		}
		.dn-pick__media :global(.dn-pick__sticker) {
			left: auto;
			right: 0.5rem;
		}
		.dn-pick__media :global(.dn-pick__play) {
			left: 0.875rem;
			bottom: 0.875rem;
		}
		.dn-pick__text {
			padding-top: 0;
		}
		.dn-pick__kicker {
			margin: 0.375rem 0 0.25rem;
		}
		.dn-pick__title {
			font-size: 2.375rem;
		}
		.dn-pick__title.is-long {
			font-size: 1.6875rem;
		}
		.dn-pick__byline {
			flex-direction: row;
			flex-wrap: wrap;
			align-items: baseline;
			justify-content: space-between;
			gap: 0 0.75rem;
		}
		.dn-pick__artist {
			margin-top: 0.25rem;
			font-size: 0.875rem;
		}
		.dn-pick__facts {
			margin-top: 0;
			font-size: 0.71875rem;
			white-space: nowrap;
		}
	}

	@media (max-width: 639px) {
		:global(.dn-pick) {
			min-height: 0;
		}
		.dn-pick__ink {
			padding: 1.75rem 1rem 1rem;
		}
		.dn-pick__note {
			margin-top: 0.375rem;
		}
		.dn-pick__tapes {
			top: 0.125rem;
			left: 0.5rem;
		}
		.dn-pick__pager {
			display: none;
		}
	}
</style>
