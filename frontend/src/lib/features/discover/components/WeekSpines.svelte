<!--
	The phone's week at a glance: one small spine per pick, drawn by what
	became of it, the one showing tall and ringed. The whole row is one
	button that jumps to the bin, and its name says every state in words.
-->
<script lang="ts" module>
	import type { StateLook } from '$lib/ui/StatusChip.svelte';

	export interface Spine {
		key: string;
		title: string;
		look?: StateLook;
	}
</script>

<script lang="ts">
	import Button from '$lib/ui/Button.svelte';
	import { STATE_LOOKS } from '$lib/ui/StatusChip.svelte';

	interface Props {
		spines: Spine[];
		current: string | null;
		onjump: () => void;
	}

	let { spines, current, onjump }: Props = $props();

	const name = $derived(
		`This week's ${spines.length}: ` +
			spines
				.map((spine) => {
					const word =
						spine.key === current ? 'showing' : spine.look ? STATE_LOOKS[spine.look].word : 'open';
					return `${spine.title}, ${word.toLowerCase()}`;
				})
				.join('; ')
	);
</script>

<Button variant="ghost" class="dn-spines" aria-label={name} onclick={onjump}>
	{#each spines as spine (spine.key)}
		<i data-look={spine.key === current ? 'current' : (spine.look ?? 'open')}></i>
	{/each}
</Button>

<style>
	:global(.dn-spines) {
		align-items: flex-end;
		gap: 3px;
		margin-left: auto;
		padding-block: 0.3125rem;
		padding-inline: 2px;
	}
	:global(.dn-spines) i {
		width: 11px;
		height: 22px;
		border-radius: 1.5px;
		background: color-mix(in oklab, var(--color-base-content) 22%, transparent);
	}
	:global(.dn-spines) i[data-look='kept'] {
		background: var(--dn-action);
	}
	:global(.dn-spines) i[data-look='later'] {
		background: var(--color-base-content);
	}
	:global(.dn-spines) i[data-look='known'] {
		background: transparent;
		box-shadow: inset 0 0 0 1.5px var(--dn-muted);
	}
	:global(.dn-spines) i[data-look='passed'] {
		height: 12px;
	}
	:global(.dn-spines) i[data-look='waiting'] {
		background: repeating-linear-gradient(-45deg, var(--dn-action) 0 2px, transparent 2px 4px);
		box-shadow: inset 0 0 0 1px var(--dn-action);
	}
	:global(.dn-spines) i[data-look='refused'] {
		height: 12px;
		background: transparent;
		box-shadow: inset 0 0 0 1.5px var(--dn-muted);
	}
	:global(.dn-spines) i[data-look='missed'] {
		height: 12px;
		background: var(--color-warning);
	}
	:global(.dn-spines) i[data-look='current'] {
		height: 34px;
		background: var(--dn-paper);
		box-shadow: 0 0 0 1.5px var(--dn-action);
	}
</style>
