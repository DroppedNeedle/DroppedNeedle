<!--
	A card's heading, for one that Card's `title` cannot hold: an `h3`, a size
	of its own (through `class`), or a heading inside a `header` snippet beside
	an icon or a line of text.
	Replaces the hand-written `card-title` headings.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { HTMLAttributes } from 'svelte/elements';

	interface Props extends Omit<HTMLAttributes<HTMLHeadingElement>, 'children'> {
		as?: 'h2' | 'h3';
		children?: Snippet;
	}

	let { as = 'h2', class: className = '', children, ...rest }: Props = $props();
</script>

<svelte:element this={as} {...rest} class={['card-title', className]}>
	{@render children?.()}
</svelte:element>
