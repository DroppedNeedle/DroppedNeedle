<script lang="ts">
	import { FingerprintPattern } from 'lucide-svelte';

	import type { LibraryArtistSummary } from '$lib/types';

	interface Props {
		artist: LibraryArtistSummary;
	}

	let { artist }: Props = $props();
	const identityDeskUrl = $derived(
		`/library/management/artists?artist=${encodeURIComponent(artist.id)}&q=${encodeURIComponent(artist.name)}`
	);
</script>

<a class="btn btn-ghost btn-sm gap-2" href={identityDeskUrl}>
	<FingerprintPattern class="h-4 w-4" />
	Open artist identity desk
</a>
