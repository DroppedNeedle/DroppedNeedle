<!--
	The frame of a page: one container width, one heading style, an optional
	subtitle and a slot for header actions.
	Replaces the page-container class strings and `<h1>` styles each route
	writes for itself. Pages with a hero banner keep `PageHeader`.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Props {
		title: string;
		subtitle?: string;
		width?: 'narrow' | 'default' | 'wide';
		actions?: Snippet;
		children: Snippet;
	}

	let { title, subtitle, width = 'default', actions, children }: Props = $props();

	const widths = { narrow: 'max-w-4xl', default: 'max-w-7xl', wide: 'max-w-[96rem]' };
</script>

<div class="mx-auto w-full {widths[width]} px-4 py-6 sm:px-6 lg:px-8">
	<header class="mb-6 flex flex-wrap items-start justify-between gap-3">
		<div class="min-w-0">
			<h1 class="text-2xl font-bold sm:text-3xl">{title}</h1>
			{#if subtitle}
				<p class="mt-1 text-sm text-base-content/70 sm:text-base">{subtitle}</p>
			{/if}
		</div>
		{#if actions}
			<div class="flex items-center gap-2">
				{@render actions()}
			</div>
		{/if}
	</header>
	{@render children()}
</div>
