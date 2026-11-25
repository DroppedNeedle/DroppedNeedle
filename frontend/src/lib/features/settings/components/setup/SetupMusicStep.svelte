<script lang="ts">
	import type {
		FolderLookState,
		FoundFolder
	} from '$lib/features/settings/queries/setupFound.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import SettingsImportFolder from '../SettingsImportFolder.svelte';
	import SettingsLibraryCard from '../SettingsLibraryCard.svelte';
	import SetupSuggestion from './SetupSuggestion.svelte';

	interface Props {
		folders: FoundFolder[];
		look: FolderLookState;
		acoustidSet: boolean;
		/** A library folder is set up, whether or not it was found here. */
		librarySet: boolean;
	}

	let { folders, look, acoustidSet, librarySet }: Props = $props();

	let open = $state<string | null>(null);
	let dismissed = $state<string[]>([]);

	const shown = $derived(folders.filter((folder) => !dismissed.includes(id(folder))));
	const libraries = $derived(shown.filter((folder) => folder.role === 'library'));
	const imports = $derived(shown.filter((folder) => folder.role === 'import_folder'));
	const downloads = $derived(folders.filter((folder) => folder.role === 'downloads'));

	function id(folder: FoundFolder): string {
		return `${folder.role}:${folder.path}`;
	}

	function countText(folder: FoundFolder): string {
		if (!folder.counted_all && folder.audio_files === 0) {
			return look === 'scan_running' ? 'Counted after the scan' : 'Too big to count quickly';
		}
		const count = folder.audio_files.toLocaleString();
		const files = folder.audio_files === 1 ? 'audio file' : 'audio files';
		return folder.counted_all ? `${count} ${files}` : `${count}+ ${files}`;
	}
</script>

<div class="space-y-4">
	<p class="text-sm text-muted">
		Where your music is. Saving a library folder starts its first scan.
	</p>

	{#if look === 'not_read'}
		<p class="text-sm text-warning">Could not read the folders. Reload this page to look again.</p>
	{:else if look === 'scan_running'}
		<p class="text-sm text-muted">A scan is running: other folders are counted after it.</p>
	{/if}

	{#each libraries as folder (id(folder))}
		<SetupSuggestion
			title="Library folder"
			detail={folder.path}
			meta={countText(folder)}
			note={folder.writable ? undefined : 'Can be scanned and played, not tagged or renamed.'}
			done={folder.already_set}
			open={open === id(folder)}
			setupLabel={folder.already_set ? 'Edit' : 'Use as library folder'}
			onopen={() => (open = id(folder))}
			ondismiss={folder.already_set ? undefined : () => (dismissed = [...dismissed, id(folder)])}
			onskip={() => (open = null)}
		>
			<SettingsLibraryCard suggested={folder.already_set ? undefined : folder.path} />
		</SetupSuggestion>
	{:else}
		<Card variant="bordered" padding="md">
			<div class="flex flex-wrap items-center justify-between gap-3">
				{#if librarySet}
					<p class="text-sm">Your library folder is set up. No other music folder was found.</p>
				{:else}
					<p class="text-sm">No music folder found. Add the folder your music is in.</p>
				{/if}
				{#if open !== 'library:'}
					<Button variant="primary" size="sm" onclick={() => (open = 'library:')}>
						{librarySet ? 'Edit' : 'Add a library folder'}
					</Button>
				{/if}
			</div>
			{#if open === 'library:'}
				<div class="mt-4"><SettingsLibraryCard /></div>
			{/if}
		</Card>
	{/each}

	{#each imports as folder (id(folder))}
		<SetupSuggestion
			title="Import folder"
			detail={folder.path}
			meta={`${countText(folder)}. Music copied here is filed into your library.`}
			done={folder.already_set}
			open={open === id(folder)}
			setupLabel={folder.already_set ? 'Edit' : 'Use as import folder'}
			onopen={() => (open = id(folder))}
			ondismiss={folder.already_set ? undefined : () => (dismissed = [...dismissed, id(folder)])}
			onskip={() => (open = null)}
		>
			<SettingsImportFolder suggested={folder.already_set ? undefined : folder.path} />
		</SetupSuggestion>
	{/each}

	{#if downloads.length > 0}
		<p class="text-sm text-muted">
			Download folders found: <span class="font-mono break-all">
				{downloads.map((folder) => folder.path).join(', ')}
			</span>. They come up in the next step.
		</p>
	{/if}

	{#if !acoustidSet}
		<p class="text-sm text-muted">
			Add an AcoustID key in the library folder's form to identify music with no tags.
		</p>
	{/if}
</div>
