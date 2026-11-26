<!--
	A pick of a past week, opened as it was: its cover, title and artist, the
	index card with the line it carried that week, what became of it and a
	way to its album page.
-->
<script lang="ts">
	import { ArrowRight } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import { albumHref } from '$lib/utils/entityRoutes';
	import type { PastPick } from '$lib/features/discover/queries/weeks.svelte';
	import { lookOf, type PickState } from '$lib/features/discover/state/weekDeck.svelte';
	import { dayMonthYear } from '$lib/features/discover/state/weekWords';

	interface Props {
		open: boolean;
		/** The pick on show; nothing renders without one. */
		pick: PastPick | null;
		/** The week it was in. */
		week: number;
	}

	let { open = $bindable(false), pick, week }: Props = $props();

	const HAPPENED: Record<PickState, string> = {
		arrived: 'Kept, and in your library.',
		asked: 'Kept. It is on its way.',
		awaiting_approval: 'Kept. The request waits for an admin.',
		refused: 'Kept. The request was turned down.',
		failed: "Kept. It didn't arrive.",
		later: 'On your Later pile.',
		known: 'You knew it already.',
		no: 'You said no.',
		not_now: 'You said not now.',
		not_artist: 'You turned its artist down.',
		open: 'Never answered.'
	};

	const look = $derived(pick ? lookOf(pick.state) : undefined);
	const mbid = $derived(pick?.album.musicbrainz_id ?? '');
	const byline = $derived(
		pick
			? [pick.album.artist_name, pick.album.year ? String(pick.album.year) : null]
					.filter(Boolean)
					.join(' - ')
			: ''
	);
	const since = $derived(
		pick?.state === 'arrived' && pick.arrived_at
			? `In your library since ${dayMonthYear(pick.arrived_at)}.`
			: null
	);
	const played = $derived(
		pick?.plays ? `Played ${pick.plays} ${pick.plays === 1 ? 'time' : 'times'}.` : null
	);
</script>

<Modal
	bind:open
	title={pick?.album.title ?? ''}
	description="Week {week}, No. {String(pick?.pick_no ?? 0).padStart(2, '0')}"
	paper
	bottomSheet
	closeButton
	size="lg"
>
	{#if pick}
		<div class="dn-past-pick">
			<div class="dn-past-pick__cover">
				<AlbumImage
					{mbid}
					alt="Cover of {pick.album.title}"
					size="md"
					rounded="none"
					className="size-full object-cover"
				/>
			</div>
			<div class="dn-past-pick__body">
				{#if byline}<p class="dn-past-pick__by">{byline}</p>{/if}
				<Card variant="index" size="lg">
					{#snippet header()}
						<span class="dn-past-pick__caps">Why it was here</span>
						{#if pick.reason.artists.length}
							<span class="dn-past-pick__seeds">
								{#each pick.reason.artists as artist (artist.artist_mbid)}
									<ArtistImage
										mbid={artist.artist_mbid}
										alt={artist.name}
										size="xs"
										rounded="full"
										className="dn-past-pick__seed"
									/>
								{/each}
							</span>
						{/if}
					{/snippet}
					<p class="dn-past-pick__line">{pick.reason.line}</p>
				</Card>
				<div class="dn-past-pick__did">
					{#if look}<StatusChip {look} />{/if}
					<p>
						{HAPPENED[pick.state]}
						{#if since}{since}{/if}
						{#if played}{played}{/if}
					</p>
				</div>
			</div>
		</div>
	{/if}
	{#snippet actions()}
		{#if mbid}
			<Button
				variant="paper"
				icon={ArrowRight}
				iconPosition="end"
				href={albumHref(mbid)}
				onclick={() => (open = false)}
			>
				Open the album
			</Button>
		{/if}
	{/snippet}
</Modal>

<style>
	.dn-past-pick {
		display: grid;
		grid-template-columns: 10rem minmax(0, 1fr);
		gap: 1.25rem;
		align-items: start;
	}
	@media (max-width: 639px) {
		.dn-past-pick {
			grid-template-columns: 6rem minmax(0, 1fr);
			gap: 0.875rem;
		}
	}
	.dn-past-pick__cover {
		aspect-ratio: 1;
		overflow: hidden;
		box-shadow: 6px 14px 26px -12px rgb(0 0 0 / 0.6);
	}
	.dn-past-pick__by {
		margin: 0 0 0.75rem;
		font-weight: 700;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		font-size: 0.8125rem;
	}
	.dn-past-pick__caps {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 700;
		letter-spacing: 0.16em;
		text-transform: uppercase;
	}
	.dn-past-pick__seeds {
		display: flex;
		margin-left: auto;
	}
	.dn-past-pick__seeds :global(.dn-past-pick__seed) {
		width: 1.625rem;
		height: 1.625rem;
		margin-left: -0.45rem;
		box-shadow: 0 0 0 2px var(--dn-paper-white);
	}
	.dn-past-pick__line {
		margin: 0;
		font-size: 1.0625rem;
		font-weight: 500;
		line-height: 1.35;
		text-wrap: pretty;
	}
	.dn-past-pick__did {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.5rem 0.75rem;
		margin-top: 1rem;
	}
	.dn-past-pick__did p {
		margin: 0;
		font-size: 0.875rem;
	}
</style>
