<!--
	A paper pill naming one thing a person chose (an artist they love), with
	a round picture, the name and a remove button whose target is 44 px.
	`picture` draws the picture, at most 2rem. `removing`
	holds the button while the removal is on its way.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import { X } from 'lucide-svelte';
	import IconButton from './IconButton.svelte';

	interface Props {
		name: string;
		picture?: Snippet;
		removing?: boolean;
		onremove: () => void;
	}

	let { name, picture, removing = false, onremove }: Props = $props();
</script>

<span class="dn-name-tag">
	{#if picture}
		<span class="dn-name-tag__picture">{@render picture()}</span>
	{/if}
	<span class="dn-name-tag__name">{name}</span>
	<IconButton
		label={`Remove ${name}`}
		icon={X}
		size="sm"
		variant="ghost"
		class="dn-name-tag__remove"
		disabled={removing}
		onclick={onremove}
	/>
</span>

<style>
	.dn-name-tag {
		display: inline-flex;
		align-items: center;
		gap: 0.5rem;
		max-width: 100%;
		min-height: 2.75rem;
		padding: 0 0 0 0.25rem;
		border: 1px solid var(--dn-paper-border);
		border-radius: 999px;
		background: var(--dn-paper-white);
		color: var(--dn-paper-content);
	}
	.dn-name-tag__picture {
		display: inline-flex;
		flex-shrink: 0;
		overflow: hidden;
		border-radius: 999px;
	}
	.dn-name-tag__name {
		min-width: 0;
		overflow: hidden;
		font-size: 0.875rem;
		font-weight: 600;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.dn-name-tag :global(.dn-name-tag__remove) {
		flex-shrink: 0;
		min-width: 2.75rem;
		min-height: 2.75rem;
		color: inherit;
	}
</style>
