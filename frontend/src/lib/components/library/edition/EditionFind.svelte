<!--
	"Not listed?": paste a MusicBrainz release link or id to compare that
	release, or search MusicBrainz by title and artist.
-->
<script lang="ts">
	import { Search } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import TextField from '$lib/ui/TextField.svelte';

	interface Props {
		title: string;
		artist: string;
		searching: boolean;
		onpaste: (release: string) => void;
		onsearch: () => void;
	}

	let {
		title = $bindable(''),
		artist = $bindable(''),
		searching,
		onpaste,
		onsearch
	}: Props = $props();

	const MBID = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i;
	const pasted = $derived(title.match(MBID)?.[0]?.toLowerCase() ?? null);

	let lastPasted: string | null = null;
	$effect(() => {
		if (pasted && pasted !== lastPasted) onpaste(pasted);
		lastPasted = pasted;
	});

	function submit(event: SubmitEvent): void {
		event.preventDefault();
		if (pasted) onpaste(pasted);
		else if (title.trim()) onsearch();
	}
</script>

<form class="flex flex-col gap-1" aria-label="Find an edition" onsubmit={submit}>
	<h3 class="text-xs font-semibold tracking-wide text-base-content/55 uppercase">Not listed?</h3>
	<TextField
		label="Paste a MusicBrainz link, or search by title"
		placeholder="https://musicbrainz.org/release/... or a title"
		bind:value={title}
	/>
	{#if !pasted}
		<TextField label="Artist" bind:value={artist} />
		<Button
			type="submit"
			variant="outline"
			size="sm"
			icon={Search}
			loading={searching}
			disabled={!title.trim()}
			class="mt-1 self-start">Search MusicBrainz</Button
		>
	{/if}
</form>
