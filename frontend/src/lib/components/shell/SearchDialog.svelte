<!-- The search dialog the sidebar and the bottom bar open. -->
<script lang="ts">
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { X } from 'lucide-svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import SearchSuggestions from '$lib/components/SearchSuggestions.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import type { SuggestResult } from '$lib/types';

	let { open = $bindable(false) }: { open: boolean } = $props();

	const titleId = $props.id();
	let dialog = $state<HTMLDialogElement>();
	let query = $state('');

	$effect(() => {
		if (!dialog) return;
		if (open && !dialog.open) dialog.showModal();
		else if (!open && dialog.open) dialog.close();
	});

	function handleSearch() {
		if (!query.trim()) return;
		goto(withBasePath(`/search?q=${encodeURIComponent(query)}`));
		open = false;
		query = '';
	}

	function handleSelect(result: SuggestResult) {
		open = false;
		const routeId = result.type === 'artist' ? '/artist/[id]' : '/album/[id]';
		goto(resolve(routeId, { id: result.musicbrainz_id }));
	}
</script>

<dialog bind:this={dialog} class="modal" aria-labelledby={titleId} onclose={() => (open = false)}>
	<div class="modal-box overflow-visible">
		<form method="dialog">
			<IconButton type="submit" size="sm" icon={X} label="Close" class="absolute right-2 top-2" />
		</form>
		<h3 id={titleId} class="font-bold text-lg mb-4">Search</h3>
		<SearchSuggestions
			bind:query
			onSearch={handleSearch}
			onSelect={handleSelect}
			placeholder="Search albums or artists..."
			autofocus={true}
			id="modal-suggest"
		/>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close modal">close</button>
	</form>
</dialog>
