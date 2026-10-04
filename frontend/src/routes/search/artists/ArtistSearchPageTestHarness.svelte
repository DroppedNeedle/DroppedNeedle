<script lang="ts">
	import { QueryClientProvider } from '@tanstack/svelte-query';
	import { queryClient } from '$lib/queries/QueryClient';
	import ArtistSearchPage from './+page.svelte';

	interface Props {
		data: { query: string };
	}

	let { data }: Props = $props();
</script>

<QueryClientProvider client={queryClient}>
	<ArtistSearchPage {data} />
</QueryClientProvider>
