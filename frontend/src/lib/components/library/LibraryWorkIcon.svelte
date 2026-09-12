<script lang="ts">
	import {
		TriangleAlert,
		BookOpenCheck,
		FolderCog,
		FolderSync,
		ListChecks,
		RefreshCcw,
		ScanSearch,
		Wrench
	} from 'lucide-svelte';
	import type { LibraryWorkItem } from '$lib/queries/library/LibraryOperationsTypes';

	interface Props {
		item: LibraryWorkItem;
		className?: string;
	}

	let { item, className = 'h-5 w-5' }: Props = $props();
</script>

{#if item.effect === 'attention'}
	<TriangleAlert class={className} aria-hidden="true" />
{:else if item.kind === 'scan'}
	<FolderSync class={className} aria-hidden="true" />
{:else if item.kind === 'identification'}
	<ScanSearch class={className} aria-hidden="true" />
{:else if item.kind === 'identity_preparation' || item.kind === 'reidentification'}
	<BookOpenCheck class={className} aria-hidden="true" />
{:else if item.kind === 'identity_review'}
	<ListChecks class={className} aria-hidden="true" />
{:else if item.kind === 'recovery'}
	<RefreshCcw class={className} aria-hidden="true" />
{:else if item.kind === 'maintenance'}
	<Wrench class={className} aria-hidden="true" />
{:else}
	<FolderCog class={className} aria-hidden="true" />
{/if}
