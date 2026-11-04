<!-- The search dialog the sidebar and the bottom bar open. -->
<script lang="ts">
	import { goto } from '$app/navigation';
	import { albumHref, artistHref } from '$lib/utils/entityRoutes';
	import Modal from '$lib/ui/Modal.svelte';
	import SearchSuggestions from '$lib/features/search/components/SearchSuggestions.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import type { SuggestResult } from '$lib/features/search/queries/types';

	let { open = $bindable(false) }: { open: boolean } = $props();

	let query = $state('');

	function handleSearch() {
		if (!query.trim()) return;
		goto(withBasePath(`/search?q=${encodeURIComponent(query)}`));
		open = false;
		query = '';
	}

	function handleSelect(result: SuggestResult) {
		open = false;
		goto(
			result.type === 'artist'
				? artistHref(result.musicbrainz_id)
				: albumHref(result.musicbrainz_id)
		);
	}
</script>

<Modal bind:open title="Search" closeButton overflowVisible>
	<SearchSuggestions
		bind:query
		onSearch={handleSearch}
		onSelect={handleSelect}
		placeholder="Search albums or artists..."
		autofocus={true}
		id="modal-suggest"
	/>
</Modal>
