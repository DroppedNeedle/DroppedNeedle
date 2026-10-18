<!--
	A card's row of buttons, for a row that Card's `actions` cannot lay out:
	another alignment, a divider above it, or a message beside the buttons.
	Its layout comes from `class`.
	Replaces the hand-written `card-actions` rows.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { HTMLAttributes } from 'svelte/elements';

	interface Props extends Omit<HTMLAttributes<HTMLDivElement>, 'children'> {
		children?: Snippet;
	}

	let { class: className = '', children, ...rest }: Props = $props();
</script>

<div {...rest} class={['card-actions', className]}>
	{@render children?.()}
</div>
