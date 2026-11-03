<script lang="ts">
	import { QueryClientProvider } from '@tanstack/svelte-query';
	import { queryClient } from '$lib/queries/QueryClient';
	import type { SuggestResult } from '$lib/types';
	import SearchSuggestions from './SearchSuggestions.svelte';

	interface Props {
		query: string;
		onSearch: () => void;
		onSelect: (result: SuggestResult) => void;
		placeholder?: string;
		inputClass?: string;
		autofocus?: boolean;
		id?: string;
	}

	let {
		query = $bindable(),
		onSearch,
		onSelect,
		placeholder = 'Search...',
		inputClass = '',
		autofocus = false,
		id = 'suggest'
	}: Props = $props();
</script>

<QueryClientProvider client={queryClient}>
	<SearchSuggestions bind:query {onSearch} {onSelect} {placeholder} {inputClass} {autofocus} {id} />
</QueryClientProvider>
