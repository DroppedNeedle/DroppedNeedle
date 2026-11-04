<!--
	"Not listed?": paste a MusicBrainz release link or id to compare that
	release, a release group link to list that album's editions, or search
	MusicBrainz by title and artist.
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
		/** A release group link was pasted: list its editions. */
		onpastegroup: (group: string) => void;
		onsearch: () => void;
	}

	let {
		title = $bindable(''),
		artist = $bindable(''),
		searching,
		onpaste,
		onpastegroup,
		onsearch
	}: Props = $props();

	const MBID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

	/**
	 * What was pasted, read from the link's path: `/release/<id>` or a bare
	 * id is a release, `/release-group/<id>` an album whose editions to
	 * list. Anything else is a title to search for.
	 */
	function readPaste(text: string): { kind: 'release' | 'group'; mbid: string } | null {
		const value = text.trim();
		if (MBID.test(value)) return { kind: 'release', mbid: value.toLowerCase() };
		let url: URL;
		try {
			url = new URL(value);
		} catch {
			return null;
		}
		const [kind, id] = url.pathname.split('/').filter(Boolean);
		if (!id || !MBID.test(id)) return null;
		if (kind === 'release') return { kind: 'release', mbid: id.toLowerCase() };
		if (kind === 'release-group') return { kind: 'group', mbid: id.toLowerCase() };
		return null;
	}

	const pasted = $derived(readPaste(title));

	function use(paste: NonNullable<typeof pasted>): void {
		if (paste.kind === 'group') onpastegroup(paste.mbid);
		else onpaste(paste.mbid);
	}

	let lastPasted: string | null = null;
	$effect(() => {
		const key = pasted ? `${pasted.kind}:${pasted.mbid}` : null;
		if (pasted && key !== lastPasted) use(pasted);
		lastPasted = key;
	});

	function submit(event: SubmitEvent): void {
		event.preventDefault();
		if (pasted) use(pasted);
		else if (title.trim()) onsearch();
	}
</script>

<form class="flex flex-col gap-1" aria-label="Find an edition" onsubmit={submit}>
	<h3 class="text-xs font-semibold tracking-wide text-muted uppercase">Not listed?</h3>
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
