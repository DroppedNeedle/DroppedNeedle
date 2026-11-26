<!--
	One week's picks standing on a shelf: each cover square and untouched on
	a rule, its title under it. A kept record stands proud with its KEPT
	sticker; a passed one is drawn smaller, sits low and has its title
	struck; the rest stand level, with their state's sticker when they have
	one. The whole row reads at a glance as what worked. It is a list named
	by `label` ("What you did with week 47"), and each record says its title
	and its state in words. The dark look is for past weeks on the page,
	`paper` for a shelf laid on a paper card. Four across on a phone.

	`tab` stands a cream tab at the left of the shelf: "WEEK", the week's
	number, its date and the week's tally. A record may carry its artist and,
	once it has plays, "PLAYED {n} TIMES" under its name. With `onopen` each
	record is a button that opens it.
-->
<script lang="ts" module>
	import type { StateLook } from './StatusChip.svelte';

	export interface WeekShelfItem {
		key: string;
		title: string;
		/** A second line under the title. */
		artist?: string;
		/** What became of it; none while it is open. */
		state?: StateLook;
		/** How often the person played it; nothing shows while there are none. */
		plays?: number | null;
	}

	export interface WeekShelfTab {
		/** The week's number, set large. */
		week: number;
		/** Its date, in mono caps under the number. */
		date: string;
		/** "1 kept - 1 later - 5 open". */
		tally: string;
	}
</script>

<script lang="ts" generics="T extends WeekShelfItem">
	import type { Snippet } from 'svelte';
	import Sticker from './Sticker.svelte';
	import { STATE_LOOKS } from './StatusChip.svelte';

	interface Props {
		items: T[];
		label: string;
		cover: Snippet<[T]>;
		look?: 'dark' | 'paper';
		/** A tab at the shelf's left naming the week. */
		tab?: WeekShelfTab;
		/** Opens a record; each record is a button with it. */
		onopen?: (item: T) => void;
		/** Layout only: margin, width. */
		class?: string;
	}

	let { items, label, cover, look = 'dark', tab, onopen, class: className = '' }: Props = $props();

	const playsLine = (plays: number) => `Played ${plays} ${plays === 1 ? 'time' : 'times'}`;
	const spoken = (item: T) =>
		[
			item.artist ? `${item.title} by ${item.artist}` : item.title,
			item.state ? STATE_LOOKS[item.state].word : null,
			item.plays ? playsLine(item.plays) : null
		]
			.filter(Boolean)
			.join(', ');
</script>

{#snippet record(item: T)}
	{#if item.state && item.state !== 'passed'}
		<span class="dn-shelf__sticker" aria-hidden="true">
			<Sticker state={item.state} size="sm" />
		</span>
	{/if}
	<span class="dn-shelf__cover">{@render cover(item)}</span>
	<span class="dn-shelf__title">{item.title}</span>
	{#if item.artist}<span class="dn-shelf__artist">{item.artist}</span>{/if}
	{#if item.plays}<span class="dn-shelf__plays">{playsLine(item.plays)}</span>{/if}
	{#if item.state}<span class="sr-only">, {STATE_LOOKS[item.state].word}</span>{/if}
{/snippet}

{#snippet shelf()}
	<ol class="dn-shelf {tab ? '' : className}" data-look={look} aria-label={label}>
		{#each items as item (item.key)}
			<li class="dn-shelf__item" data-state={item.state}>
				{#if onopen}
					<button
						type="button"
						class="dn-shelf__open"
						aria-label={spoken(item)}
						onclick={() => onopen(item)}
					>
						{@render record(item)}
					</button>
				{:else}
					{@render record(item)}
				{/if}
			</li>
		{/each}
	</ol>
{/snippet}

{#if tab}
	<div class="dn-shelf-row {className}" data-look={look}>
		<div class="dn-shelf-tab">
			<span class="dn-shelf-tab__caps">Week</span>
			<span class="dn-shelf-tab__week">{tab.week}</span>
			<span class="dn-shelf-tab__caps">{tab.date}</span>
			{#if tab.tally}<span class="dn-shelf-tab__tally">{tab.tally}</span>{/if}
		</div>
		{@render shelf()}
	</div>
{:else}
	{@render shelf()}
{/if}

<style>
	.dn-shelf {
		display: grid;
		grid-template-columns: repeat(8, minmax(0, 1fr));
		gap: 0.75rem;
		align-items: end;
		margin: 0;
		padding: 0;
		list-style: none;
		border-bottom: 4px solid var(--color-neutral);
	}
	.dn-shelf[data-look='paper'] {
		--dn-muted: color-mix(in oklab, var(--dn-paper-content) 74%, var(--dn-paper));
		border-bottom-color: color-mix(in oklab, var(--dn-paper-content) 20%, transparent);
		color: var(--dn-paper-content);
	}
	@media (max-width: 639px) {
		.dn-shelf {
			grid-template-columns: repeat(4, minmax(0, 1fr));
			row-gap: 1.5rem;
		}
	}

	.dn-shelf-row {
		display: grid;
		grid-template-columns: 9.5rem minmax(0, 1fr);
		gap: 1.25rem;
		align-items: end;
	}
	.dn-shelf-row .dn-shelf {
		padding-top: 1rem;
	}
	@media (max-width: 639px) {
		.dn-shelf-row {
			grid-template-columns: minmax(0, 1fr);
			gap: 0.75rem;
		}
	}

	.dn-shelf-tab {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		align-self: stretch;
		justify-content: flex-end;
		padding: 1rem 0.875rem 0.875rem;
		border-radius: 3px 3px 0 0;
		background: var(--dn-paper);
		color: var(--dn-paper-content);
	}
	.dn-shelf-tab__caps,
	.dn-shelf-tab__tally {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.14em;
		text-transform: uppercase;
	}
	.dn-shelf-tab__week {
		font-family: var(--font-display);
		font-size: 3rem;
		line-height: 0.95;
		font-synthesis: none;
	}
	.dn-shelf-tab__tally {
		margin-top: 0.5rem;
		line-height: 1.6;
		letter-spacing: 0.08em;
	}
	@media (max-width: 639px) {
		.dn-shelf-tab {
			flex-direction: row;
			flex-wrap: wrap;
			align-items: baseline;
			justify-content: flex-start;
			column-gap: 0.625rem;
			padding: 0.625rem 0.875rem;
			border-radius: 3px;
		}
		.dn-shelf-tab__week {
			font-size: 2rem;
		}
		.dn-shelf-tab__tally {
			flex-basis: 100%;
			margin-top: 0;
		}
	}

	.dn-shelf__item {
		position: relative;
		min-width: 0;
		padding-bottom: 0.5rem;
	}
	.dn-shelf__item[data-state='kept'] {
		translate: 0 -0.875rem;
	}

	.dn-shelf__open {
		display: block;
		width: 100%;
		padding: 0;
		border: 0;
		background: transparent;
		color: inherit;
		text-align: left;
		cursor: pointer;
	}
	.dn-shelf__open:focus-visible {
		outline: 2px solid var(--dn-focus);
		outline-offset: 3px;
		border-radius: 2px;
	}

	.dn-shelf__cover {
		display: block;
		width: 100%;
		aspect-ratio: 1;
		overflow: hidden;
		border-radius: 2px;
		box-shadow: 0 8px 14px -8px rgb(0 0 0 / 0.8);
	}
	/* The cover fills its square, whatever size the image was asked at. */
	.dn-shelf__cover > :global(*) {
		width: 100%;
		height: 100%;
	}
	.dn-shelf[data-look='paper'] .dn-shelf__cover {
		box-shadow: 0 8px 14px -8px rgb(0 0 0 / 0.6);
	}
	.dn-shelf__item[data-state='passed'] .dn-shelf__cover {
		width: 80%;
	}

	.dn-shelf__sticker {
		position: absolute;
		top: -0.5625rem;
		left: -0.3125rem;
		z-index: 1;
	}

	.dn-shelf__title,
	.dn-shelf__artist,
	.dn-shelf__plays {
		display: block;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.dn-shelf__title {
		margin-top: 0.375rem;
		font-size: 0.75rem;
		font-weight: 600;
		line-height: 1.2;
	}
	.dn-shelf__item[data-state='passed'] .dn-shelf__title {
		text-decoration: line-through;
		color: var(--dn-muted);
	}
	.dn-shelf__artist,
	.dn-shelf__plays {
		margin-top: 0.125rem;
		font-family: var(--font-mono);
		font-size: 0.625rem;
		letter-spacing: 0.08em;
		line-height: 1.3;
		text-transform: uppercase;
	}
	.dn-shelf__artist {
		color: var(--dn-muted);
	}
	/* A play count wraps rather than lose its number. */
	.dn-shelf__plays {
		font-weight: 700;
		white-space: normal;
		color: var(--color-action);
	}
	.dn-shelf[data-look='paper'] .dn-shelf__plays {
		color: inherit;
	}
</style>
