<script lang="ts">
	import { FingerprintPattern } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';

	import type { LibraryArtistSummary } from '$lib/types';

	interface Props {
		artist: LibraryArtistSummary;
	}

	let { artist }: Props = $props();
	const identityDeskUrl = $derived(
		`/library/management/artists?artist=${encodeURIComponent(artist.id)}&q=${encodeURIComponent(artist.name)}`
	);
</script>

<Button variant="ghost" size="sm" icon={FingerprintPattern} href={identityDeskUrl}
	>Open artist identity desk</Button
>
