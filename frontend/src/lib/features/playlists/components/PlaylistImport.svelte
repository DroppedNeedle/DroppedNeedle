<!--
	Import a playlist file into a new playlist: a small dialog with the
	library-only choice and the file picker, the upload, and the match
	report. The file's stem names the new playlist. The page opens it by
	setting `open`.
-->
<script lang="ts">
	import { Upload } from 'lucide-svelte';
	import {
		createImportPlaylistV3Mutation,
		type ImportPlaylistResponseV3,
		type ImportUnmatchedReasonV3
	} from '$lib/features/playlists/queries/PlaylistV3Mutations.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { withBasePath } from '$lib/utils/basePath';
	import Button from '$lib/ui/Button.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

	const MAX_BYTES = 2 * 1024 * 1024;
	const SHOWN_UNMATCHED = 200;
	const REASONS: Record<ImportUnmatchedReasonV3, string> = {
		names_nothing: 'names no file',
		no_match: 'no local file matches',
		duplicate: 'already in the file'
	};

	interface Props {
		/** The dialog that picks the file. */
		open?: boolean;
	}

	let { open = $bindable(false) }: Props = $props();

	const mutation = createImportPlaylistV3Mutation();

	let fileInput = $state<HTMLInputElement | null>(null);
	let importing = $state(false);
	let reportOpen = $state(false);
	let libraryOnly = $state(false);
	let report = $state<ImportPlaylistResponseV3 | null>(null);

	function chooseFile() {
		fileInput?.click();
	}

	async function handleFile(event: Event) {
		const input = event.currentTarget as HTMLInputElement;
		const file = input.files?.[0];
		input.value = '';
		if (!file) return;
		const format = formatOf(file.name);
		if (!format) {
			toastStore.show({ message: 'Choose an .m3u, .m3u8 or .csv file', type: 'error' });
			return;
		}
		if (file.size > MAX_BYTES) {
			toastStore.show({ message: 'That playlist file is larger than 2 MB', type: 'error' });
			return;
		}
		importing = true;
		open = false;
		try {
			const content = await file.text();
			report = await mutation.mutateAsync({
				name: playlistName(file.name),
				format,
				content,
				library_only: libraryOnly
			});
			reportOpen = true;
		} catch (error) {
			toastStore.show({
				message:
					error instanceof Error && error.message ? error.message : "Couldn't import that playlist",
				type: 'error'
			});
		} finally {
			importing = false;
		}
	}

	function formatOf(name: string): 'm3u8' | 'csv' | null {
		const lower = name.toLowerCase();
		if (lower.endsWith('.csv')) return 'csv';
		if (lower.endsWith('.m3u') || lower.endsWith('.m3u8')) return 'm3u8';
		return null;
	}

	function playlistName(name: string): string {
		const stem = name.replace(/\.(m3u8?|csv)$/i, '').trim();
		return stem || 'Imported playlist';
	}
</script>

<Modal
	bind:open
	title="Import a playlist file"
	description=".m3u, .m3u8 or .csv, up to 2 MB"
	icon={Upload}
	size="sm"
	closeButton
>
	<ToggleRow label="Only songs already in the library" bind:checked={libraryOnly} />
	{#snippet actions()}
		<Button variant="ghost" onclick={() => (open = false)}>Cancel</Button>
		<Button variant="primary" icon={Upload} loading={importing} onclick={chooseFile}>
			Choose a file
		</Button>
	{/snippet}
</Modal>
<input
	type="file"
	accept=".m3u,.m3u8,.csv"
	class="hidden"
	aria-label="Playlist file"
	bind:this={fileInput}
	onchange={handleFile}
/>

<Modal
	bind:open={reportOpen}
	title="Playlist imported"
	description="What the file's entries became"
	icon={Upload}
	size="lg"
	closeButton
>
	{#if report}
		<div class="flex flex-col gap-4">
			<p class="text-sm">
				<strong>{report.matched}</strong>
				{report.matched === 1 ? 'track' : 'tracks'} added to
				<strong>{report.name}</strong>.
				{#if report.kept > 0}
					<strong>{report.kept}</strong>
					kept to request.
				{/if}
				{#if report.unmatched.length > 0}
					{report.unmatched.length}
					{report.unmatched.length === 1 ? 'entry was' : 'entries were'} left out.
				{/if}
				{#if report.over_limit > 0}
					{report.over_limit}
					{report.over_limit === 1 ? 'entry' : 'entries'} past the 10,000 limit
					{report.over_limit === 1 ? 'was' : 'were'} not read.
				{/if}
			</p>
			{#if report.unmatched.length > 0}
				<div class="scrollbar-hide max-h-[min(50vh,22rem)] overflow-y-auto">
					<ul class="flex flex-col divide-y divide-line">
						{#each report.unmatched.slice(0, SHOWN_UNMATCHED) as row, i (i)}
							<li class="flex flex-col gap-0.5 py-2 text-sm">
								<span class="font-semibold">{row.title || 'Untitled'}</span>
								<span class="text-xs text-muted">
									{row.artist ? `${row.artist} - ` : ''}{REASONS[row.reason]}
								</span>
							</li>
						{/each}
					</ul>
					{#if report.unmatched.length > SHOWN_UNMATCHED}
						<p class="pt-2 text-xs text-muted">
							Showing the first {SHOWN_UNMATCHED} of {report.unmatched.length}.
						</p>
					{/if}
				</div>
			{/if}
		</div>
	{/if}
	{#snippet actions()}
		<Button variant="ghost" onclick={() => (reportOpen = false)}>Close</Button>
		<Button
			variant="primary"
			href={withBasePath(`/playlists/${report?.playlist_id ?? ''}`)}
			disabled={!report}
			onclick={() => (reportOpen = false)}
		>
			Open playlist
		</Button>
	{/snippet}
</Modal>
