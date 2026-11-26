<!--
	Your taste: the groups the person's picks are shared out by, under paper
	divider tabs that carry each group's name, share and trend, with the
	artists they named on a tab of their own. A group's tab opens its
	artists and the buttons that tune it. Beside it, what the buttons do and
	what the person changed. While a change is still being followed the
	header says so; this week's set never changes.
-->
<script lang="ts">
	import ToldDiscoverModal from '$lib/features/discover/components/ToldDiscoverModal.svelte';
	import StatedArtists from '$lib/features/discover/components/StatedArtists.svelte';
	import TasteChanges from '$lib/features/discover/components/TasteChanges.svelte';
	import TasteGroupPanel from '$lib/features/discover/components/TasteGroupPanel.svelte';
	import TasteShare from '$lib/features/discover/components/TasteShare.svelte';
	import { createUndoTuningMutation } from '$lib/features/discover/queries/DiscoverV3Mutations.svelte';
	import { getStatedArtistsQuery } from '$lib/features/discover/queries/statedArtists.svelte';
	import {
		createTasteStatementMutation,
		getTasteQuery,
		type TasteResponse,
		type TasteStatement
	} from '$lib/features/discover/queries/taste.svelte';
	import type { ArtistTuning } from '$lib/features/discover/state/tuningWords';
	import { dayMonth } from '$lib/features/discover/state/weekWords';
	import Card from '$lib/ui/Card.svelte';
	import ListPageHeader from '$lib/ui/ListPageHeader.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import Tabs, { type TabItem } from '$lib/ui/Tabs.svelte';
	import { withBasePath } from '$lib/utils/basePath';

	const NAMED = 'named';

	const tasteQuery = getTasteQuery();
	const statedQuery = getStatedArtistsQuery();
	const say = createTasteStatementMutation();
	const undo = createUndoTuningMutation();
	let toldOpen = $state(false);

	const taste = $derived(tasteQuery.data ?? null);
	const busy = $derived(say.isPending || undo.isPending);

	const tabs = $derived<TabItem[]>([
		...(taste?.groups ?? []).map((group) => ({
			id: String(group.group_no),
			label: group.anchor_name
		})),
		{ id: NAMED, label: 'Named by you' }
	]);
	let chosen = $state<string | null>(null);
	// The first group is open until another tab is chosen, and again once
	// the chosen group has left.
	const selected = $derived(
		chosen && tabs.some((tab) => tab.id === chosen) ? chosen : (tabs[0]?.id ?? NAMED)
	);

	const open = $derived(taste?.groups.find((group) => String(group.group_no) === selected) ?? null);
	const named = $derived((statedQuery.data?.items ?? []).map((artist) => artist.artist_name));

	function madeFrom(data: TasteResponse) {
		const made = data.made_from;
		if (!made) return null;
		const plays = made.plays_counted.toLocaleString();
		const since = made.history_from_at ? ` since ${dayMonth(made.history_from_at * 1000)}` : '';
		const imported =
			made.imported_listens_counted > 0
				? `+ ${made.imported_listens_counted.toLocaleString()} imported`
				: 'In the app';
		return {
			figure: plays,
			lines: [`Plays${since}`, imported] as const,
			spoken: `${plays} plays${since}, ${imported}`,
			phone: `From ${plays} plays${since}`
		};
	}
	const facts = $derived(taste ? madeFrom(taste) : null);

	function onsay(statement: TasteStatement) {
		say.mutate(statement);
	}
	function onundo(change: { kind: ArtistTuning; target: string; group: boolean }) {
		undo.mutate(change);
	}
</script>

<svelte:head>
	<title>Your taste - DroppedNeedle</title>
</svelte:head>

<ListPageHeader
	title="Your taste"
	ticket="DN / CC 3.4"
	facts={facts ?? undefined}
	lede="What your picks are made from. Each taste gets its share of a set."
	backHref={withBasePath('/discover')}
	backLabel="Back to Discover"
	backStyle="link"
>
	{#snippet actions()}
		<div class="dn-taste-page__follow" role="status">
			{#if taste?.updating}
				<span class="dn-taste-page__updating">
					<span class="dn-taste-page__dot" aria-hidden="true"></span>Updating
				</span>
			{/if}
			<span>Your next set will follow this.</span>
			<span class="dn-taste-page__muted">This week's set stays as it is.</span>
		</div>
	{/snippet}
</ListPageHeader>

<div class="dn-taste-page">
	{#if facts}<p class="dn-taste-page__phone-facts">{facts.phone}</p>{/if}

	<QueryBoundary query={tasteQuery} what="your taste">
		{#snippet children(data)}
			{#if data.groups.length === 0}
				<div class="dn-taste-page__empty">
					<Card variant="paper" padding="lg">
						<p>Play something or name a few artists and this page fills in.</p>
						<StatedArtists />
					</Card>
					<TasteChanges ontold={() => (toldOpen = true)} />
				</div>
			{:else}
				<div class="dn-taste-page__body">
					<div class="dn-taste-page__file">
						<Tabs
							{tabs}
							{selected}
							onselect={(id) => (chosen = id)}
							label="Your tastes"
							idPrefix="taste"
							look="folder"
							scroll
						>
							{#snippet tab(item, current)}
								{@const group = data.groups.find((one) => String(one.group_no) === item.id)}
								{#if group}
									<span class="dn-taste-tab__name">
										<span>Around {group.anchor_name}</span>
										<span>{Math.round(group.share * 100)}%</span>
									</span>
									<TasteShare share={group.share} trend={group.trend} />
								{:else}
									<span class="dn-taste-tab__name"><span>Named by you</span></span>
									{#if named.length && !current}
										<span class="dn-taste-tab__named">{named.slice(0, 3).join(', ')}</span>
									{/if}
								{/if}
							{/snippet}
							{#if open}
								<TasteGroupPanel group={open} {busy} {onsay} {onundo} />
							{:else}
								<div class="dn-taste-page__named">
									<StatedArtists />
								</div>
							{/if}
						</Tabs>
					</div>

					<aside class="dn-taste-page__aside">
						<Card variant="torn" padding="md">
							<h2 class="dn-taste-page__note-head">What the buttons do</h2>
							<p><b>More</b> and <b>Less</b>: more or fewer picks start from this artist.</p>
							<p>
								<b>That was a phase</b>: its plays stop counting. Hearts, follows and requests still
								do.
							</p>
						</Card>
						<TasteChanges ontold={() => (toldOpen = true)} />
					</aside>
				</div>
			{/if}
		{/snippet}
	</QueryBoundary>
</div>

<ToldDiscoverModal bind:open={toldOpen} />

<style>
	.dn-taste-page {
		max-width: 88rem;
		padding: 0.5rem 2rem 4rem;
	}
	.dn-taste-page__follow {
		display: flex;
		flex-direction: column;
		align-items: flex-end;
		gap: 0.25rem;
		font-size: 0.9375rem;
		text-align: right;
	}
	.dn-taste-page__muted {
		font-size: 0.8125rem;
		color: var(--dn-muted);
	}
	.dn-taste-page__updating {
		display: inline-flex;
		align-items: center;
		gap: 0.5rem;
		margin-bottom: 0.25rem;
		padding: 0.3rem 0.625rem;
		border: 1px solid var(--dn-edge);
		border-radius: 4px;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 700;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		color: var(--dn-title);
	}
	.dn-taste-page__dot {
		width: 0.5rem;
		height: 0.5rem;
		border-radius: 9999px;
		background-color: var(--dn-title);
	}
	.dn-taste-page__phone-facts {
		display: none;
	}
	.dn-taste-page__body {
		display: flex;
		align-items: flex-start;
		gap: 1.75rem;
	}
	.dn-taste-page__file {
		flex: 1;
		min-width: 0;
	}
	.dn-taste-page__aside {
		display: flex;
		flex: none;
		flex-direction: column;
		gap: 1.25rem;
		width: 19rem;
	}
	.dn-taste-page__aside p {
		margin: 0.5rem 0 0;
		font-size: 0.875rem;
		line-height: 1.5;
	}
	.dn-taste-page__note-head {
		margin: 0;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 700;
		letter-spacing: 0.16em;
		text-transform: uppercase;
	}
	.dn-taste-page__named {
		padding: 1.25rem 1.5rem;
	}
	.dn-taste-page__empty {
		display: flex;
		flex-direction: column;
		gap: 1.25rem;
		max-width: 40rem;
	}
	.dn-taste-page__empty p {
		margin: 0 0 1rem;
	}
	.dn-taste-tab__name {
		display: flex;
		justify-content: space-between;
		gap: 1rem;
		min-width: 11rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 700;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		white-space: nowrap;
	}
	.dn-taste-tab__named {
		max-width: 12rem;
		overflow: hidden;
		font-family: var(--font-mono);
		font-size: 0.625rem;
		text-overflow: ellipsis;
		text-transform: uppercase;
		white-space: nowrap;
		opacity: 0.8;
	}

	@media (max-width: 1023px) {
		.dn-taste-page__body {
			flex-direction: column;
			align-items: stretch;
		}
		.dn-taste-page__aside {
			width: auto;
		}
	}
	@media (max-width: 767px) {
		.dn-taste-page {
			padding: 0.25rem 0.5rem 3rem;
		}
		.dn-taste-page__phone-facts {
			display: block;
			margin: 0 0.5rem 0.75rem;
			font-family: var(--font-mono);
			font-size: 0.6875rem;
			letter-spacing: 0.12em;
			text-transform: uppercase;
			color: var(--dn-muted);
		}
		.dn-taste-page__follow {
			align-items: flex-start;
			text-align: left;
		}
		.dn-taste-page__muted {
			display: none;
		}
		.dn-taste-tab__name {
			min-width: 0;
		}
		.dn-taste-page__named {
			padding: 0.875rem;
		}
	}
</style>
