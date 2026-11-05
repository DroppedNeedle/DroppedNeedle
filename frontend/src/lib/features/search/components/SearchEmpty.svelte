<!-- A search bucket with nothing in it, in the Library lists' empty style. -->
<script lang="ts">
	import type { KitIcon } from '$lib/ui/Button.svelte';

	interface Props {
		icon: KitIcon;
		title: string;
		text?: string;
		/** A short row inside a page of sections, rather than a whole page. */
		compact?: boolean;
	}

	let { icon: Icon, title, text, compact = false }: Props = $props();
</script>

<div
	class="flex flex-col items-center justify-center text-center {compact
		? 'rounded-box border border-line py-8'
		: 'min-h-80'}"
>
	<Icon class="mb-3 size-10 text-muted" strokeWidth={1.5} aria-hidden="true" />
	<p class="text-xl font-semibold">{title}</p>
	{#if text}<p class="mt-1 text-muted">{text}</p>{/if}
</div>
