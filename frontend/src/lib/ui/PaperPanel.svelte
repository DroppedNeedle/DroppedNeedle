<!--
	The light island: the one large paper shape on a page (an album's track
	list). In dark it is cream stock with dark ink, in light paper-white with
	a visible edge. Inside, the light theme applies, so every control in it
	is already measured for paper. No texture, ever. `head` is the panel's
	top band (a source's name and its Play All), ruled off from the rest.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Props {
		head?: Snippet;
		/** Layout only: margin, width. */
		class?: string;
		children: Snippet;
	}

	let { head, class: className = '', children }: Props = $props();
</script>

<div class="dn-paper {className}">
	<div class="dn-paper__sheet" data-theme="light">
		{#if head}
			<div class="dn-paper__head">{@render head()}</div>
		{/if}
		{@render children()}
	</div>
</div>

<style>
	/* Read the page theme's paper here, before the sheet switches to light. */
	.dn-paper {
		--paper-bg: var(--dn-paper);
		--paper-edge: var(--dn-paper-border);
	}

	.dn-paper__sheet {
		overflow: hidden;
		border: 1px solid var(--paper-edge);
		border-radius: var(--radius-box);
		background-color: var(--paper-bg);
		color: var(--dn-paper-content);
	}

	.dn-paper__head {
		border-bottom: 1px solid var(--dn-paper-border);
	}
	.dn-paper__head > :global(* + *) {
		border-top: 1px solid var(--dn-paper-border);
	}
</style>
