<!--
	A sticker on a sleeve's corner saying what became of a pick, in the same
	word and mark as the `StatusChip` look of that state: KEPT, LATER, KNOW
	IT, PASSED, WAITING, REFUSED, DIDN'T ARRIVE. `fresh` lands it (scaled
	down from 1.9 in 0.26s, a slap), for the moment a person answers; under
	reduced motion it is simply there. `sm` suits a shelf of small covers.
	The caller places it with `class`.
-->
<script lang="ts">
	import { STATE_LOOKS, type StateLook } from './StatusChip.svelte';

	interface Props {
		state: StateLook;
		fresh?: boolean;
		size?: 'sm' | 'md';
		/** Layout only: position, margin. */
		class?: string;
	}

	let { state, fresh = false, size = 'md', class: className = '' }: Props = $props();

	const Icon = $derived(STATE_LOOKS[state].icon);
</script>

<span class="dn-sticker {className}" data-state={state} data-size={size} class:is-fresh={fresh}>
	{#if Icon}<Icon class="dn-sticker__icon" aria-hidden="true" />{/if}
	{STATE_LOOKS[state].word}
</span>

<style>
	.dn-sticker {
		--sticker-ink: var(--dn-paper-content);
		display: inline-flex;
		align-items: center;
		gap: 0.3125rem;
		padding: 0.375rem 0.75rem 0.375rem 0.5625rem;
		border: 1.5px solid var(--sticker-ink);
		border-radius: 9999px;
		background-color: var(--dn-paper-white);
		color: var(--sticker-ink);
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 700;
		line-height: 1;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		white-space: nowrap;
		rotate: 7deg;
		box-shadow: 0 3px 8px -2px rgb(0 0 0 / 0.5);
	}
	.dn-sticker[data-size='sm'] {
		gap: 0.25rem;
		padding: 0.25rem 0.5rem 0.25rem 0.375rem;
		font-size: 0.625rem;
		letter-spacing: 0.1em;
		rotate: -4deg;
	}
	.dn-sticker :global(.dn-sticker__icon) {
		width: 0.875rem;
		height: 0.875rem;
		flex: none;
		stroke-width: 3;
	}
	.dn-sticker[data-size='sm'] :global(.dn-sticker__icon) {
		width: 0.6875rem;
		height: 0.6875rem;
	}

	.dn-sticker[data-state='kept'] {
		--sticker-ink: var(--dn-action-content);
		background-color: var(--dn-action);
	}
	.dn-sticker[data-state='known'] {
		border: 4px double var(--sticker-ink);
		border-radius: 4px;
		background-color: color-mix(in oklab, var(--dn-paper-white) 82%, transparent);
		box-shadow: none;
		rotate: -8deg;
	}
	.dn-sticker[data-size='sm'][data-state='known'] {
		border-width: 3px;
	}
	.dn-sticker[data-state='passed'],
	.dn-sticker[data-state='refused'] {
		--sticker-ink: color-mix(in oklab, var(--dn-paper-content) 74%, var(--dn-paper-white));
		rotate: -5deg;
	}
	.dn-sticker[data-state='waiting'] {
		border-style: dashed;
	}
	.dn-sticker[data-state='missed'] {
		--sticker-ink: var(--color-warning-content);
		background-color: var(--color-warning);
		rotate: 4deg;
	}

	@keyframes dn-sticker-slap {
		0% {
			scale: 1.9;
			opacity: 0;
		}
		60% {
			scale: 0.94;
			opacity: 1;
		}
		100% {
			scale: 1;
		}
	}
	.dn-sticker.is-fresh {
		animation: dn-sticker-slap 260ms var(--ease-spring) both;
	}

	@media (forced-colors: active) {
		.dn-sticker {
			border-color: CanvasText;
		}
	}
</style>
