<!-- A destination's icon with its corner mark, and room for a badge on top. -->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { NavItem } from './navItems';

	let { item, children }: { item: NavItem; children?: Snippet } = $props();
</script>

<span class="relative flex h-5 w-5 shrink-0">
	<item.icon class="h-5 w-5" />
	{#if item.iconMark}
		<span
			class="absolute -right-1.5 -bottom-1.5 grid h-4 w-4 place-items-center rounded-full bg-base-100 text-library-manage"
		>
			<item.iconMark class="h-3 w-3" />
		</span>
	{/if}
	{@render children?.()}
</span>
