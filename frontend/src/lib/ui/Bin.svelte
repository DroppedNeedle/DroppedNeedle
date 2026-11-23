<!--
	A record bin: a box with an inset floor and a front lip, rows filed in it
	behind divider tabs, and a paper label in a holder on the lip. Each group
	is a list named by its tab's words, share included ("Around Poppy, 35% of
	your listening"); a group with no name is named by the bin's title. A row is a button (number, cover, title, artist, state) that
	calls `onselect`; the row whose key is `current` is a paper slip pulled
	forward out of the bin and carries `aria-current`. A `struck` row's title
	is crossed out. `cover` draws a row's cover in a square box and `state`
	its state at the row's end, told whether the row is the current one.
	`arriving` drops the tabs and rows in one after another and stamps the
	label last, for a set's first showing.
-->
<script lang="ts" module>
	export interface BinRow {
		key: string;
		number: string;
		title: string;
		artist: string;
		struck?: boolean;
	}

	export interface BinGroup<T> {
		/** The tab's words, given by the page: "Around Poppy". */
		name?: string;
		share?: string;
		shareSpoken?: string;
		rows: T[];
	}
</script>

<script lang="ts" generics="T extends BinRow">
	import type { Snippet } from 'svelte';
	import DividerTab from './DividerTab.svelte';

	interface Props {
		title: string;
		/** Beside the title: "3 of 8 answered". */
		tally?: string;
		/** The paper label on the lip. */
		label?: string;
		groups: BinGroup<T>[];
		/** The key of the row pulled forward. */
		current?: string;
		onselect?: (row: T) => void;
		cover: Snippet<[T]>;
		state?: Snippet<[T, boolean]>;
		/** Under the rows, inside the box. */
		foot?: Snippet;
		arriving?: boolean;
		/** Layout only: width, margin. */
		class?: string;
	}

	let {
		title,
		tally,
		label,
		groups,
		current,
		onselect,
		cover,
		state,
		foot,
		arriving = false,
		class: className = ''
	}: Props = $props();

	const uid = $props.id();
	const titleId = `${uid}-title`;
	// every other tab sits a little along the line
	const STAGGER_REM = 0.75;

	// the list hears the tab's words; the tab itself is hidden so they are not read twice
	const groupLabel = (group: BinGroup<T>) =>
		group.share ? `${group.name}, ${group.shareSpoken ?? group.share}` : (group.name ?? '');

	// each tab and row's place in the arrival
	const order = $derived.by(() => {
		let n = 0;
		return groups.map((group) => ({
			tab: group.name ? n++ : -1,
			rows: group.rows.map(() => n++)
		}));
	});
</script>

{#snippet rowBody(row: T, isCurrent: boolean)}
	<span class="dn-bin__n">{row.number}</span>
	<span class="dn-bin__cover">{@render cover(row)}</span>
	<span class="dn-bin__text">
		<span class="dn-bin__t">{row.title}</span>
		<span class="dn-bin__a">{row.artist}</span>
	</span>
	<span class="dn-bin__state">{@render state?.(row, isCurrent)}</span>
{/snippet}

<section class="dn-bin {className}" class:is-arriving={arriving} aria-labelledby={titleId}>
	<header class="dn-bin__head">
		<h2 id={titleId} class="dn-bin__title">{title}</h2>
		{#if tally}<span class="dn-bin__tally">{tally}</span>{/if}
	</header>

	<div class="dn-bin__floor">
		{#each groups as group, g (g)}
			{#if group.name}
				<div class="dn-bin__drop" style:--i={order[g]?.tab} aria-hidden="true">
					<DividerTab
						name={group.name}
						share={group.share}
						shareSpoken={group.shareSpoken}
						offset={g % 2 === 1 ? STAGGER_REM : 0}
					/>
				</div>
			{/if}
			<ul
				class="dn-bin__list"
				aria-label={group.name ? groupLabel(group) : undefined}
				aria-labelledby={group.name ? undefined : titleId}
			>
				{#each group.rows as row, r (row.key)}
					{@const isCurrent = row.key === current}
					<li class="dn-bin__drop" style:--i={order[g]?.rows[r]}>
						{#if onselect}
							<button
								type="button"
								class="dn-bin__row"
								class:is-struck={row.struck}
								aria-current={isCurrent ? 'true' : undefined}
								onclick={() => onselect(row)}
							>
								{@render rowBody(row, isCurrent)}
							</button>
						{:else}
							<div
								class="dn-bin__row"
								class:is-struck={row.struck}
								aria-current={isCurrent ? 'true' : undefined}
							>
								{@render rowBody(row, isCurrent)}
							</div>
						{/if}
					</li>
				{/each}
			</ul>
		{/each}
		{@render foot?.()}
	</div>

	{#if label}
		<div class="dn-bin__lip">
			<p class="dn-bin__holder"><span>{label}</span></p>
		</div>
	{/if}
</section>

<style>
	.dn-bin {
		display: flex;
		flex-direction: column;
		padding: 0.875rem 0.75rem 0;
		border: 1px solid color-mix(in oklab, var(--dn-line) 80%, black);
		border-radius: 0.875rem;
		background: linear-gradient(var(--color-base-300), var(--color-base-200) 30%);
		box-shadow:
			inset 0 2px 10px rgb(0 0 0 / 0.55),
			0 1px 0 rgb(255 255 255 / 0.07);
	}
	:global([data-theme='light']) .dn-bin {
		border-color: var(--dn-line);
		box-shadow:
			inset 0 2px 8px rgb(0 0 0 / 0.12),
			0 1px 0 rgb(255 255 255 / 0.9);
	}

	.dn-bin__head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 0 0.25rem 0.125rem;
	}
	.dn-bin__title {
		margin: 0;
		font-family: var(--font-display);
		font-size: 1.5625rem;
		font-weight: 400;
		line-height: 1.1;
		letter-spacing: 0.015em;
		text-transform: uppercase;
		font-synthesis: none;
		color: var(--dn-title);
	}
	.dn-bin__tally {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		white-space: nowrap;
		color: var(--dn-muted);
	}

	.dn-bin__floor {
		flex: 1;
	}
	.dn-bin__floor > .dn-bin__drop:first-child {
		--divider-space: 0.375rem;
	}
	.dn-bin__list {
		margin: 0;
		padding: 0;
		list-style: none;
	}

	.dn-bin__row {
		display: grid;
		grid-template-columns: 1.25rem 2.25rem minmax(0, 1fr) auto;
		gap: 0.625rem;
		align-items: center;
		width: 100%;
		min-height: 2.4375rem;
		padding: 0 0.5rem 0 0.375rem;
		border: 0;
		border-radius: 6px;
		background: transparent;
		color: var(--color-base-content);
		font: inherit;
		text-align: left;
		transition:
			background-color 150ms,
			translate 200ms var(--ease-spring);
	}
	@media (pointer: coarse) {
		.dn-bin__row {
			min-height: 2.75rem;
		}
	}
	button.dn-bin__row {
		cursor: pointer;
	}
	button.dn-bin__row:hover {
		background-color: var(--color-neutral);
	}
	.dn-bin__row:focus-visible {
		outline: 2px solid var(--dn-focus);
		outline-offset: -2px;
	}

	/* the row on the counter: a paper slip pulled forward, a few pixels proud,
	   marked by its stock and its place, never by a coloured edge */
	.dn-bin__row[aria-current='true'],
	button.dn-bin__row[aria-current='true']:hover {
		--dn-muted: color-mix(in oklab, var(--dn-paper-content) 72%, var(--dn-paper));
		--dn-title: var(--dn-paper-content);
		--dn-warn-ink: var(--dn-paper-warning);
		background-color: var(--dn-paper);
		color: var(--dn-paper-content);
		box-shadow: 0 2px 0 rgb(0 0 0 / 0.35);
		translate: -0.375rem 0;
	}

	.dn-bin__n {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 500;
		color: var(--dn-muted);
	}
	.dn-bin__cover {
		display: block;
		width: 2.25rem;
		aspect-ratio: 1;
		overflow: hidden;
		border-radius: 2px;
	}
	.dn-bin__text {
		display: block;
		min-width: 0;
	}
	.dn-bin__t,
	.dn-bin__a {
		display: block;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.dn-bin__t {
		font-size: 0.875rem;
		font-weight: 600;
		line-height: 1.2;
	}
	.dn-bin__a {
		font-family: var(--font-mono);
		font-size: 0.65625rem;
		font-weight: 500;
		line-height: 1.3;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.dn-bin__row.is-struck .dn-bin__t {
		text-decoration: line-through;
		text-decoration-color: var(--dn-muted);
		color: var(--dn-muted);
	}
	.dn-bin__state {
		display: inline-flex;
		align-items: center;
		gap: 0.375rem;
	}

	/* the front lip, with a paper label in a metal holder */
	.dn-bin__lip {
		margin: 0.625rem -0.75rem 0;
		padding: 0.5625rem 0.875rem;
		border-top: 1px solid rgb(0 0 0 / 0.5);
		border-radius: 0 0 0.8125rem 0.8125rem;
		background: linear-gradient(
			color-mix(in oklab, var(--color-base-300) 60%, var(--color-base-200)),
			var(--color-base-100)
		);
		box-shadow: inset 0 1px 0 rgb(255 255 255 / 0.07);
	}
	:global([data-theme='light']) .dn-bin__lip {
		border-top-color: var(--dn-line);
		box-shadow: inset 0 1px 0 rgb(255 255 255 / 0.9);
	}
	.dn-bin__holder {
		margin: 0;
		padding: 3px;
		border-radius: 4px;
		background: linear-gradient(
			color-mix(in oklab, var(--dn-vinyl), white 30%),
			color-mix(in oklab, var(--dn-vinyl), white 14%)
		);
		box-shadow:
			inset 0 1px 0 rgb(255 255 255 / 0.25),
			0 1px 2px rgb(0 0 0 / 0.6);
	}
	.dn-bin__holder span {
		display: block;
		padding: 0.3125rem 0.5625rem;
		overflow: hidden;
		border-radius: 2px;
		background-color: var(--dn-paper);
		color: var(--dn-paper-content);
		font-family: var(--font-mono);
		font-size: 0.625rem;
		font-weight: 700;
		line-height: 1.2;
		letter-spacing: 0.13em;
		text-overflow: ellipsis;
		text-transform: uppercase;
		white-space: nowrap;
	}

	/* a set's first showing: everything drops in, 70ms apart, the label last */
	@keyframes dn-bin-drop {
		from {
			translate: 0 -1.125rem;
			opacity: 0;
		}
	}
	@keyframes dn-bin-stamp {
		0% {
			scale: 1.7;
			opacity: 0;
			rotate: -6deg;
		}
		70% {
			scale: 0.96;
			opacity: 1;
		}
		100% {
			scale: 1;
		}
	}
	.dn-bin.is-arriving .dn-bin__drop {
		animation: dn-bin-drop 360ms var(--ease-spring) both;
		animation-delay: calc(var(--i, 0) * 70ms + 250ms);
	}
	.dn-bin.is-arriving .dn-bin__holder {
		animation: dn-bin-stamp 320ms var(--ease-spring) both 900ms;
	}
	/* the app's reduced-motion rule shortens animations but keeps their delays */
	@media (prefers-reduced-motion: reduce) {
		.dn-bin.is-arriving .dn-bin__drop,
		.dn-bin.is-arriving .dn-bin__holder {
			animation: none;
		}
	}

	@media (forced-colors: active) {
		.dn-bin__row[aria-current='true'] {
			outline: 2px solid Highlight;
			outline-offset: -2px;
		}
	}
</style>
