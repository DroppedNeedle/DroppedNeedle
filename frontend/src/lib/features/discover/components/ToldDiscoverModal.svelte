<!--
	Everything the person told Discover that is in effect, on the large
	index card in a paper sheet, grouped by kind under ruled heads with a
	count: not this artist, less from this, not now, know it already, no.
	Each row says how long it lasts and since when, with Undo. A long group
	shows its newest ten until asked for all. Later albums have their own
	page, linked at the foot.
-->
<script lang="ts">
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import {
		createUndoDiscoverFeedbackMutation,
		createUndoTuningMutation
	} from '$lib/features/discover/queries/DiscoverV3Mutations.svelte';
	import {
		getDiscoverFeedbackV3Query,
		getTuningsQuery
	} from '$lib/features/discover/queries/DiscoverV3Queries.svelte';
	import type {
		DiscoverFeedbackEntry,
		FeedbackKind
	} from '$lib/features/discover/queries/feedback';
	import { dayMonth, dayMonthYear } from '$lib/features/discover/state/weekWords';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import { withBasePath } from '$lib/utils/basePath';

	interface Props {
		open: boolean;
		/** Albums on the Later pile, for the foot's link. */
		laterCount?: number;
	}

	let { open = $bindable(false), laterCount = 0 }: Props = $props();

	const toldQuery = getDiscoverFeedbackV3Query(() => open);
	const tuningsQuery = getTuningsQuery(() => open);
	const undo = createUndoDiscoverFeedbackMutation();
	const undoTuning = createUndoTuningMutation();

	/** A group longer than this shows its newest until asked for all. */
	const SHOWN = 10;
	let showAll = $state<Record<string, boolean>>({});

	// Both reads answered, and neither holds anything.
	const nothing = $derived(
		toldQuery.data?.items.length === 0 && tuningsQuery.data?.items.length === 0
	);

	const GROUPS: { kind: FeedbackKind; head: string }[] = [
		{ kind: 'not_artist', head: 'Not this artist' },
		{ kind: 'not_now', head: 'Not now' },
		{ kind: 'know_it', head: 'Know it already' },
		{ kind: 'no', head: 'No' }
	];

	const sec = (seconds: number) => seconds * 1000;

	/** How long an answer lasts: a lapsing "No" is a hide carried from before. */
	function lasts(told: DiscoverFeedbackEntry): string {
		if (!told.expires_at) return 'Until you undo it';
		return told.kind === 'not_now'
			? `Back after ${dayMonth(sec(told.expires_at))}`
			: `Ends ${dayMonthYear(sec(told.expires_at))}`;
	}

	const shown = <T,>(key: string, rows: T[]) => (showAll[key] ? rows : rows.slice(0, SHOWN));
</script>

{#snippet groupHead(head: string, count: number)}
	<h3 class="dn-told__head"><span>{head}</span><span>{count}</span></h3>
{/snippet}

{#snippet more(key: string, count: number)}
	{#if count > SHOWN && !showAll[key]}
		<Button variant="caps" size="xs" onclick={() => (showAll = { ...showAll, [key]: true })}>
			Show all
		</Button>
	{/if}
{/snippet}

{#snippet row(
	picture: { mbid: string; name: string; round: boolean },
	name: string,
	sub: string,
	until: string,
	since: number,
	label: string,
	onundo: () => Promise<unknown>
)}
	<li class="dn-told__row">
		{#if picture.round}
			<ArtistImage
				mbid={picture.mbid}
				alt={picture.name}
				size="sm"
				rounded="full"
				className="shrink-0"
			/>
		{:else}
			<AlbumImage mbid={picture.mbid} alt={picture.name} size="sm" className="shrink-0" />
		{/if}
		<div class="dn-told__what">
			<p class="dn-told__name">{name}</p>
			{#if sub}<p class="dn-told__sub">{sub}</p>{/if}
		</div>
		<p class="dn-told__when">
			<span>{until}</span>
			<span>Since {dayMonth(sec(since))}</span>
		</p>
		<AsyncButton variant="paper" size="sm" aria-label={label} onclick={onundo}>Undo</AsyncButton>
	</li>
{/snippet}

<Modal
	bind:open
	title="What you told Discover"
	description="Undo any of it"
	paper
	bottomSheet
	closeButton
	size="lg"
>
	<Card variant="index" size="lg" class="dn-told">
		{#if nothing}
			<p class="dn-told__nothing">Nothing yet.</p>
		{/if}
		<QueryBoundary query={toldQuery} what="what you told Discover">
			{#snippet children(data)}
				{#each GROUPS.slice(0, 1) as group (group.kind)}
					{@render feedbackGroup(group, data.items)}
				{/each}
			{/snippet}
		</QueryBoundary>
		<QueryBoundary query={tuningsQuery} what="how you tuned Discover">
			{#snippet children(data)}
				{@const tunings = data.items.filter((tuning) => tuning.kind === 'less_from_seed')}
				{#if tunings.length}
					<section>
						{@render groupHead('Less from this', tunings.length)}
						<ul>
							{#each shown('less', tunings) as tuning (`${tuning.kind}:${tuning.target}`)}
								{@const name = tuning.name || tuning.target}
								{@render row(
									{ mbid: tuning.target, name, round: true },
									name,
									'',
									'Until you undo it',
									tuning.created_at,
									`Undo less from ${name}`,
									() =>
										undoTuning
											.mutateAsync({ kind: tuning.kind, target: tuning.target })
											.catch(() => undefined)
								)}
							{/each}
						</ul>
						{@render more('less', tunings.length)}
					</section>
				{/if}
			{/snippet}
		</QueryBoundary>
		{#if toldQuery.data}
			{#each GROUPS.slice(1) as group (group.kind)}
				{@render feedbackGroup(group, toldQuery.data.items)}
			{/each}
		{/if}

		<p class="dn-told__foot">
			Albums you put on Later are on their own page.
			<Button variant="caps" size="xs" href={withBasePath('/discover/later')}>
				Later ({laterCount})
			</Button>
		</p>
	</Card>
</Modal>

{#snippet feedbackGroup(
	group: { kind: FeedbackKind; head: string },
	items: DiscoverFeedbackEntry[]
)}
	{@const rows = items.filter((told) => told.kind === group.kind)}
	{#if rows.length}
		<section>
			{@render groupHead(group.head, rows.length)}
			<ul>
				{#each shown(group.kind, rows) as told (`${told.target}:${told.target_mbid}`)}
					{@const artist = told.target === 'artist'}
					{@const name = (artist ? told.artist_name : told.release_name) || told.target_mbid}
					{@render row(
						{ mbid: told.target_mbid, name, round: artist },
						name,
						artist ? '' : told.artist_name,
						lasts(told),
						told.created_at,
						`Undo for ${name}`,
						() =>
							undo
								.mutateAsync({ target: told.target, mbid: told.target_mbid })
								.catch(() => undefined)
					)}
				{/each}
			</ul>
			{@render more(group.kind, rows.length)}
		</section>
	{/if}
{/snippet}

<style>
	.dn-told__nothing {
		padding: 1.5rem 0;
		text-align: center;
		font-size: 0.875rem;
		color: var(--dn-muted);
	}
	.dn-told__head {
		display: flex;
		justify-content: space-between;
		margin: 1.25rem 0 0;
		padding-bottom: 0.375rem;
		border-bottom: 1.5px solid currentColor;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 700;
		letter-spacing: 0.16em;
		text-transform: uppercase;
	}
	section:first-of-type .dn-told__head {
		margin-top: 0;
	}
	.dn-told__row {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto auto;
		align-items: center;
		gap: 0.75rem;
		padding: 0.5rem 0;
		border-bottom: 1px solid var(--dn-paper-border);
	}
	.dn-told__what {
		min-width: 0;
	}
	.dn-told__name {
		margin: 0;
		overflow: hidden;
		font-size: 0.875rem;
		font-weight: 700;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.dn-told__sub {
		margin: 0;
		overflow: hidden;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--dn-muted);
	}
	.dn-told__when {
		display: flex;
		flex-direction: column;
		align-items: flex-end;
		margin: 0;
		font-family: var(--font-mono);
		font-size: 0.65625rem;
		font-weight: 600;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		white-space: nowrap;
	}
	.dn-told__when span + span {
		color: var(--dn-muted);
	}
	.dn-told__foot {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.25rem 0.75rem;
		margin: 1.25rem 0 0;
		font-size: 0.8125rem;
		color: var(--dn-muted);
	}
	@media (max-width: 639px) {
		.dn-told__row {
			grid-template-columns: auto minmax(0, 1fr) auto;
		}
		.dn-told__when {
			grid-column: 2;
			grid-row: 2;
			align-items: flex-start;
		}
	}
</style>
