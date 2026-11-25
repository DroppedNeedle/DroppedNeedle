<!--
	"Why this?": the pick's index card pulled out, on a paper sheet. The
	person's artists that led to the pick, each with how much they play it
	and where the pick stands in its list of neighbours, then what makes it
	new to them, how well known it is and how it was found.
-->
<script lang="ts">
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import RankRule from '$lib/ui/RankRule.svelte';
	import { createLessFromSeedMutation } from '$lib/features/discover/queries/DiscoverV3Mutations.svelte';
	import {
		getPickEvidenceQuery,
		type PickEvidenceV3
	} from '$lib/features/discover/queries/DiscoverV3Queries.svelte';
	import { dayMonthYear, figure, ordinal } from '$lib/features/discover/state/weekWords';

	interface Props {
		open: boolean;
		/** The pick's release group, while one is open. */
		mbid: string | null;
		/** "No. 04 - week 47", or "Extra 2". */
		place: string;
	}

	let { open = $bindable(false), mbid, place }: Props = $props();

	const evidence = getPickEvidenceQuery(() => (open ? mbid : null));
	const less = createLessFromSeedMutation();

	const SOURCES = { listenbrainz: 'ListenBrainz', lastfm: 'Last.fm' } as const;

	function newToYou(found: PickEvidenceV3): string {
		const said = [];
		if (found.new_to_you.no_plays) said.push('No plays in your history.');
		if (found.new_to_you.not_in_library) said.push('Not in your library.');
		return said.join(' ');
	}

	function foundBy(found: PickEvidenceV3): string {
		return found.found_by === 'co_listening'
			? 'People who play your artists also play this.'
			: `A pick from ${SOURCES[found.source]}.`;
	}
</script>

<Modal bind:open title="Why this?" description={place} paper bottomSheet closeButton size="lg">
	<QueryBoundary query={evidence} what="why this is here">
		{#snippet children(found)}
			<Card variant="index" size="lg" class="dn-why">
				<p class="dn-why__of">{found.artist_name} - {found.title}</p>
				<p class="dn-why__line">{found.reason.line}</p>

				{#if found.seeds.length}
					<h3 class="dn-why__head">Your artists that led here</h3>
					<ul class="dn-why__seeds">
						{#each found.seeds as seed (seed.artist_mbid)}
							<li class="dn-why__seed">
								<ArtistImage
									mbid={seed.artist_mbid}
									alt={seed.name}
									size="sm"
									rounded="full"
									className="dn-why__pic"
								/>
								<span class="dn-why__who">
									{seed.name}
									<span
										>you played {figure(seed.your_plays)}
										{seed.your_plays === 1 ? 'time' : 'times'}</span
									>
								</span>
								<RankRule
									ticks={seed.of}
									at={seed.rank}
									label="{ordinal(seed.rank)} closest of {figure(seed.of)}"
									class="dn-why__rank"
								/>
								<AsyncButton
									variant="paper"
									size="sm"
									disabled={seed.turned_down}
									aria-label="Less from {seed.name}"
									onclick={() => less.mutateAsync(seed.artist_mbid).catch(() => undefined)}
								>
									{seed.turned_down ? 'Less from this: on' : 'Less from this'}
								</AsyncButton>
							</li>
						{/each}
					</ul>
				{/if}

				<dl class="dn-why__facts">
					<dt>New to you</dt>
					<dd>{newToYou(found)}</dd>
					{#if found.listeners !== null && found.listeners !== undefined}
						<dt>How well known</dt>
						<dd>
							{figure(found.listeners)}
							{found.listeners === 1 ? 'listener' : 'listeners'}
							on {SOURCES[found.listeners_source ?? found.source]}.
						</dd>
					{/if}
					<dt>How it was found</dt>
					<dd>
						{foundBy(found)}
						{#if found.source_read_at}
							<small>{SOURCES[found.source]}, read {dayMonthYear(found.source_read_at)}</small>
						{/if}
					</dd>
				</dl>
			</Card>
		{/snippet}
	</QueryBoundary>
</Modal>

<style>
	.dn-why__of {
		margin: 0;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 700;
		letter-spacing: 0.14em;
		text-transform: uppercase;
	}
	.dn-why__line {
		margin: 0.375rem 0 0;
		font-size: 1.3125rem;
		font-weight: 500;
		line-height: 1.3;
		text-wrap: pretty;
	}
	.dn-why__head {
		margin: 1.25rem 0 0.25rem;
		padding-bottom: 0.375rem;
		border-bottom: 1.5px solid currentColor;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 700;
		letter-spacing: 0.16em;
		text-transform: uppercase;
	}
	.dn-why__seeds {
		display: flex;
		flex-direction: column;
	}
	.dn-why__seed {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) minmax(8rem, 12rem) auto;
		align-items: center;
		gap: 0.75rem;
		padding: 0.625rem 0;
		border-bottom: 1px solid var(--dn-paper-border);
	}
	.dn-why__seed :global(.dn-why__pic) {
		width: 2.5rem;
		height: 2.5rem;
	}
	.dn-why__who {
		display: flex;
		flex-direction: column;
		min-width: 0;
		font-weight: 700;
	}
	.dn-why__who span {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 500;
		color: var(--dn-muted);
	}
	.dn-why__facts {
		display: grid;
		grid-template-columns: minmax(7rem, auto) minmax(0, 1fr);
		margin: 1rem 0 0;
	}
	.dn-why__facts dt,
	.dn-why__facts dd {
		margin: 0;
		padding: 0.625rem 0;
		border-bottom: 1px solid var(--dn-paper-border);
	}
	.dn-why__facts dt {
		padding-right: 1rem;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 700;
		letter-spacing: 0.12em;
		text-transform: uppercase;
	}
	.dn-why__facts small {
		display: block;
		margin-top: 0.25rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	@media (max-width: 639px) {
		.dn-why__seed {
			grid-template-columns: auto minmax(0, 1fr) auto;
		}
		.dn-why__seed :global(.dn-why__rank) {
			grid-column: 1 / -1;
		}
		.dn-why__facts {
			grid-template-columns: minmax(0, 1fr);
		}
		.dn-why__facts dt {
			padding-bottom: 0;
			border-bottom: 0;
		}
	}
</style>
