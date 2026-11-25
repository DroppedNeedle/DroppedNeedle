<!--
	The counter: one bounded mat holding the pick on the left, the week's
	bin on the right and the front rail (the listening post and the receipt)
	along its foot. It keeps its height in every state. The slip after an
	answer comes right after the pick in reading order, so its buttons
	follow the pick's, and lies over the receipt. On a phone the parts
	stack: the pick, the receipt, the post, then the bin.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import Panel from '$lib/ui/Panel.svelte';

	interface Props {
		label: string;
		pick: Snippet;
		slip: Snippet;
		bin: Snippet;
		post: Snippet;
		receipt: Snippet;
	}

	let { label, pick, slip, bin, post, receipt }: Props = $props();
</script>

<Panel look="mat" padding="md" as="section" class="dn-counter" aria-label={label}>
	<div class="dn-counter__pick">{@render pick()}</div>
	<div class="dn-counter__slip">{@render slip()}</div>
	<div class="dn-counter__bin">{@render bin()}</div>
	<div class="dn-counter__post">{@render post()}</div>
	<div class="dn-counter__receipt">{@render receipt()}</div>
</Panel>

<style>
	:global(.dn-counter) {
		display: grid;
		grid-template-columns: 31.625rem minmax(0, 1fr) 22.25rem;
		grid-template-areas:
			'pick pick bin'
			'post receipt receipt';
		gap: 1rem 1.375rem;
		border-radius: 22px;
	}
	:global(.dn-counter) > div {
		position: relative;
		min-width: 0;
	}
	.dn-counter__pick {
		grid-area: pick;
	}
	.dn-counter__bin {
		grid-area: bin;
	}
	.dn-counter__post {
		grid-area: post;
		display: grid;
		height: 4.25rem;
	}
	.dn-counter__receipt {
		grid-area: receipt;
		display: grid;
		align-items: center;
		height: 4.25rem;
	}
	/* over the receipt's place in the grid */
	.dn-counter__slip {
		grid-area: receipt;
		z-index: 5;
		pointer-events: none;
	}
	.dn-counter__slip > :global(*) {
		pointer-events: auto;
	}

	@media (max-width: 1279px) {
		:global(.dn-counter) {
			grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
			grid-template-areas:
				'pick pick'
				'post receipt'
				'bin bin';
		}
	}
	@media (max-width: 639px) {
		:global(.dn-counter) {
			grid-template-columns: minmax(0, 1fr);
			grid-template-areas:
				'pick'
				'receipt'
				'post'
				'bin';
			gap: 0.75rem;
			padding: 0.5rem !important;
			border-radius: 14px;
		}
		.dn-counter__post,
		.dn-counter__receipt {
			height: auto;
		}
	}
</style>
