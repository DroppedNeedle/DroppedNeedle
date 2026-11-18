<!--
	Export one playlist as M3U or CSV, with an optional path prefix for a
	player that stores music under a folder of its own. The prefix is kept on
	this browser: it depends on the player, not the account.
-->
<script lang="ts">
	import { Download } from 'lucide-svelte';
	import { downloadPlaylistExport, type PlaylistExportFormat } from '$lib/api/playlists';
	import { toastStore } from '$lib/stores/toast';
	import Button from '$lib/ui/Button.svelte';
	import Input from '$lib/ui/Input.svelte';
	import Modal from '$lib/ui/Modal.svelte';

	const PREFIX_KEY = 'droppedneedle_playlist_export_prefix';
	const MAX_PREFIX = 200;

	interface Props {
		playlistId: string;
	}

	let { playlistId }: Props = $props();

	let open = $state(false);
	let format = $state<PlaylistExportFormat>('m3u8');
	let prefix = $state('');
	let saving = $state(false);

	export function show(chosen: PlaylistExportFormat) {
		format = chosen;
		prefix = keptPrefix();
		open = true;
	}

	function keptPrefix(): string {
		try {
			return localStorage.getItem(PREFIX_KEY) ?? '';
		} catch {
			return '';
		}
	}

	function keepPrefix(value: string) {
		try {
			if (value) localStorage.setItem(PREFIX_KEY, value);
			else localStorage.removeItem(PREFIX_KEY);
		} catch {
			// Storage refused (private window, quota): the prefix just isn't kept.
		}
	}

	async function exportNow() {
		if (saving) return;
		saving = true;
		const trimmed = prefix.trim();
		try {
			const skipped = await downloadPlaylistExport(playlistId, format, trimmed);
			keepPrefix(trimmed);
			open = false;
			if (skipped > 0) {
				const tracks = skipped === 1 ? 'track' : 'tracks';
				toastStore.show({
					message: `Playlist exported. ${skipped} ${tracks} with no local file ${skipped === 1 ? 'was' : 'were'} left out.`,
					type: 'info'
				});
			} else {
				toastStore.show({ message: 'Playlist exported', type: 'success' });
			}
		} catch (error) {
			toastStore.show({
				message:
					error instanceof Error && error.message ? error.message : "Couldn't export this playlist",
				type: 'error'
			});
		} finally {
			saving = false;
		}
	}
</script>

<Modal
	bind:open
	title={format === 'csv' ? 'Export as CSV' : 'Export as M3U'}
	description="One file with this playlist's tracks"
	icon={Download}
	size="sm"
	dismissible={!saving}
>
	<div class="flex flex-col gap-3">
		<Input
			label="Path prefix"
			placeholder="Music/"
			mono
			maxlength={MAX_PREFIX}
			bind:value={prefix}
			disabled={saving}
			onkeydown={(event) => {
				if (event.key === 'Enter') void exportNow();
			}}
		/>
		<p class="text-xs text-muted">
			Paths are relative to the library root. Add a prefix if your player stores music under a
			folder.
		</p>
	</div>
	{#snippet actions()}
		<Button variant="ghost" disabled={saving} onclick={() => (open = false)}>Cancel</Button>
		<Button variant="accent" loading={saving} onclick={() => void exportNow()}>Export</Button>
	{/snippet}
</Modal>
