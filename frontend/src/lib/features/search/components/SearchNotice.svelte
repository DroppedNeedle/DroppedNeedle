<!-- A remote search's state (cached, timed out, partial, down) with Retry. -->
<script lang="ts">
	import { Info, RefreshCw, TriangleAlert } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import type { SearchStatusNotice } from './searchStatus';

	interface Props {
		notice: SearchStatusNotice;
		onretry: () => void;
	}

	let { notice, onretry }: Props = $props();

	const warning = $derived(notice.className === 'alert-warning');
</script>

<div
	class="flex flex-wrap items-center gap-x-4 gap-y-2 rounded-box border px-4 py-3 {warning
		? 'border-warning/60 bg-warning/8'
		: 'border-info/60 bg-info/8'}"
	role="status"
>
	{#if warning}
		<TriangleAlert class="size-5 shrink-0 text-warning" aria-hidden="true" />
	{:else}
		<Info class="size-5 shrink-0 text-info" aria-hidden="true" />
	{/if}
	<p class="min-w-0 flex-1 basis-56 text-sm">{notice.message}</p>
	<Button variant="outline" size="display-sm" icon={RefreshCw} onclick={onretry}>Retry</Button>
</div>
