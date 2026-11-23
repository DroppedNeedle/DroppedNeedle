<!--
	One week's picks standing on a shelf: each cover square and untouched on
	a rule, its title under it. A kept record stands proud with its KEPT
	sticker; a passed one is drawn smaller, sits low and has its title
	struck; the rest stand level, with their state's sticker when they have
	one. The whole row reads at a glance as what worked. It is a list named
	by `label` ("What you did with week 47"), and each record says its title
	and its state in words. The dark look is for past weeks on the page,
	`paper` for a shelf laid on a paper card. Four across on a phone.
-->
<script lang="ts" module>
	import type { StateLook } from './StatusChip.svelte';

	export interface WeekShelfItem {
		key: string;
		title: string;
		/** What became of it; none while it is open. */
		state?: StateLook;
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
		/** Layout only: margin, width. */
		class?: string;
	}

	let { items, label, cover, look = 'dark', class: className = '' }: Props = $props();
</script>

<ol class="dn-shelf {className}" data-look={look} aria-label={label}>
	{#each items as item (item.key)}
		<li class="dn-shelf__item" data-state={item.state}>
			{#if item.state && item.state !== 'passed'}
				<span class="dn-shelf__sticker" aria-hidden="true"
					><Sticker state={item.state} size="sm" /></span
				>
			{/if}
			<span class="dn-shelf__cover">{@render cover(item)}</span>
			<span class="dn-shelf__title">{item.title}</span>
			{#if item.state}<span class="sr-only">, {STATE_LOOKS[item.state].word}</span>{/if}
		</li>
	{/each}
</ol>

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

	.dn-shelf__item {
		position: relative;
		min-width: 0;
		padding-bottom: 0.5rem;
	}
	.dn-shelf__item[data-state='kept'] {
		translate: 0 -0.875rem;
	}

	.dn-shelf__cover {
		display: block;
		width: 100%;
		aspect-ratio: 1;
		overflow: hidden;
		border-radius: 2px;
		box-shadow: 0 8px 14px -8px rgb(0 0 0 / 0.8);
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

	.dn-shelf__title {
		display: block;
		margin-top: 0.375rem;
		overflow: hidden;
		font-size: 0.75rem;
		font-weight: 600;
		line-height: 1.2;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.dn-shelf__item[data-state='passed'] .dn-shelf__title {
		text-decoration: line-through;
		color: var(--dn-muted);
	}
</style>
