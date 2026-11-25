<!--
	Keep, Later and No for the pick on the counter. On the pick card they sit
	along its foot with their letter keys; on a phone the same three are a
	bar under the card that stays above the tab bar while the card is in
	view, Keep widest and under the right thumb. It sits in the page's flow,
	so it never covers what comes after the card.
-->
<script lang="ts">
	import { Download } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import { playerStore } from '$lib/features/player/state/player.svelte';

	interface Props {
		look: 'pick' | 'bar';
		keeping: boolean;
		/** All three off: the pick is answered, or an answer is landing. */
		off?: boolean;
		onkeep: () => void;
		onlater: () => void;
		onno: () => void;
	}

	let { look, keeping, off = false, onkeep, onlater, onno }: Props = $props();

	const other = $derived(look === 'pick' ? 'paper' : 'outline');
</script>

{#snippet cap(letter: string)}
	{#if look === 'pick'}<kbd class="dn-actions__cap" aria-hidden="true">{letter}</kbd>{/if}
{/snippet}

<div
	class="dn-actions"
	data-look={look}
	class:above-player={look === 'bar' && playerStore.isPlayerVisible}
	role="group"
	aria-label="Answer this pick"
>
	{#if look === 'bar'}
		<Button variant={other} size="display" class="dn-actions__btn" disabled={off} onclick={onno}
			>No</Button
		>
		<Button variant={other} size="display" class="dn-actions__btn" disabled={off} onclick={onlater}
			>Later</Button
		>
	{/if}
	<Button
		variant="primary"
		size="display"
		icon={Download}
		loading={keeping}
		disabled={off}
		class="dn-actions__btn dn-actions__keep"
		aria-keyshortcuts={look === 'pick' ? 'K' : undefined}
		onclick={onkeep}
	>
		Keep{@render cap('K')}
	</Button>
	{#if look === 'pick'}
		<Button
			variant={other}
			size="display"
			class="dn-actions__btn"
			disabled={off}
			aria-keyshortcuts="L"
			onclick={onlater}
		>
			Later{@render cap('L')}
		</Button>
		<Button
			variant={other}
			size="display"
			class="dn-actions__btn"
			disabled={off}
			aria-keyshortcuts="X"
			onclick={onno}
		>
			No{@render cap('X')}
		</Button>
	{/if}
</div>

<style>
	.dn-actions[data-look='pick'] {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		margin-top: auto;
		padding-top: 1.375rem;
	}
	.dn-actions[data-look='pick'] :global(.dn-actions__btn) {
		min-height: 3.375rem;
		padding-inline: 1.625rem;
		font-size: 1.375rem;
		border-width: 2px;
	}
	.dn-actions[data-look='pick'] :global(.dn-actions__keep) {
		padding-inline: 2.375rem;
	}
	.dn-actions__cap {
		display: inline-grid;
		place-items: center;
		min-width: 1.25rem;
		height: 1.25rem;
		margin-left: 0.5rem;
		padding: 0 0.25rem;
		border: 1.5px solid currentColor;
		border-radius: 0.25rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 700;
		line-height: 1;
		opacity: 0.7;
	}

	.dn-actions[data-look='bar'] {
		display: none;
	}
	@media (max-width: 639px) {
		.dn-actions[data-look='pick'] {
			display: none;
		}
		.dn-actions[data-look='bar'] {
			position: sticky;
			bottom: var(--ms-bottom-nav-offset, 0px);
			z-index: 30;
			display: grid;
			grid-template-columns: 1fr 1fr 1.5fr;
			gap: 0.5rem;
			margin-top: 0.5rem;
			padding: 0.375rem 0 0.625rem;
			background: linear-gradient(transparent, var(--dn-mat, var(--color-base-100)) 22%);
		}
		.dn-actions[data-look='bar'].above-player {
			bottom: calc(var(--ms-bottom-nav-offset, 0px) + var(--ms-player-height, 0px));
		}
		.dn-actions[data-look='bar'] :global(.dn-actions__btn) {
			min-height: 3.25rem;
			font-size: 1.3125rem;
			border-width: 2px;
		}
	}
</style>
