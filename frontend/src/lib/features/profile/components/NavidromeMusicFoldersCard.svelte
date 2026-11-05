<script lang="ts">
	import { getRemoteFoldersQuery } from '$lib/features/remote-library/queries/RemoteQueries.svelte';
	import { createSaveRemoteFoldersMutation } from '$lib/features/remote-library/queries/RemoteMutations.svelte';
	import { CircleAlert, Folder, Save } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import RadioCards from '$lib/ui/RadioCards.svelte';

	type FolderMode = 'all' | 'selected';

	interface Props {
		userId: string;
	}

	let { userId }: Props = $props();

	const preferenceQuery = getRemoteFoldersQuery(() => Boolean(userId));
	const updateMutation = createSaveRemoteFoldersMutation();
	const preference = $derived(preferenceQuery.data);
	const unavailable = $derived(preference ? !preference.source_available : false);
	const staleIds = $derived(preference?.stale_folder_ids ?? []);

	let mode = $state<FolderMode>('all');
	let selectedIds = $state<string[]>([]);
	let loadedPreference = $state('');

	$effect(() => {
		if (!preference) return;
		const fingerprint = JSON.stringify([
			preference.mode,
			preference.folder_ids,
			preference.source_available
		]);
		if (fingerprint === loadedPreference) return;
		loadedPreference = fingerprint;
		mode = preference.mode as FolderMode;
		selectedIds = [...preference.folder_ids];
	});

	const modes: { value: FolderMode; label: string; description: string }[] = [
		{
			value: 'all',
			label: 'All folders',
			description: 'Include current and future folders from the shared Navidrome server.'
		},
		{
			value: 'selected',
			label: 'Selected folders',
			description: 'Use only the folders checked below for browsing and matching.'
		}
	];

	function toggleFolder(id: string, checked: boolean) {
		selectedIds = checked
			? [...new Set([...selectedIds, id])]
			: selectedIds.filter((selectedId) => selectedId !== id);
	}

	async function save() {
		await updateMutation.mutateAsync({
			mode,
			selected_folder_ids: mode === 'selected' ? selectedIds : []
		});
	}
</script>

<p class="mb-3 text-sm text-muted">
	Choose which Navidrome folders appear in your DroppedNeedle catalog.
</p>

{#if preferenceQuery.isPending}
	<div class="space-y-3" aria-label="Loading music folder preferences">
		<div class="skeleton h-5 w-32"></div>
		<div class="skeleton h-11 w-full"></div>
		<div class="skeleton h-11 w-full"></div>
	</div>
{:else if preferenceQuery.isError || !preference}
	<div class="alert alert-error" role="alert">
		<CircleAlert class="h-5 w-5" />
		<span>Music folder preferences could not be loaded.</span>
	</div>
{:else}
	<div class="space-y-4">
		{#if unavailable}
			<div class="alert alert-warning" role="alert">
				<CircleAlert class="h-5 w-5" />
				<span>
					Navidrome is unavailable. Your saved choice is shown, but changes cannot be verified right
					now.
				</span>
			</div>
		{/if}

		{#if staleIds.length > 0}
			<div class="alert alert-warning" role="alert">
				<CircleAlert class="h-5 w-5" />
				<div>
					<p class="font-medium">Some saved folders are no longer available.</p>
					<p class="mt-1 text-xs">
						Choose replacements or switch to All folders. The catalog will not widen automatically.
					</p>
				</div>
			</div>
		{/if}

		{#if updateMutation.isError}
			<div class="alert alert-error" role="alert">
				<CircleAlert class="h-5 w-5" />
				<span>Could not save your music folder choice. Check the selection and try again.</span>
			</div>
		{:else if updateMutation.isSuccess}
			<div class="alert alert-success" role="status">
				<span>Your music folder choice has been saved.</span>
			</div>
		{/if}

		<fieldset class="space-y-3" disabled={unavailable || updateMutation.isPending}>
			<RadioCards
				label="Navidrome music folder mode"
				options={modes}
				bind:value={mode}
				class="sm:grid-cols-2"
			/>

			{#if mode === 'selected'}
				<div class="pf-box space-y-1 p-2">
					{#if preference.available_folders.length === 0 && staleIds.length === 0}
						<div class="flex items-center gap-2 px-2 py-3 text-sm text-muted">
							<Folder class="h-4 w-4" />
							<span>No music folders were returned by Navidrome.</span>
						</div>
					{:else}
						{#each preference.available_folders as folder (folder.id)}
							<label
								class="flex cursor-pointer items-center gap-3 rounded-md px-2 py-2 hover:bg-base-300/40"
							>
								<input
									class="checkbox checkbox-sm"
									type="checkbox"
									checked={selectedIds.includes(folder.id)}
									onchange={(event) => toggleFolder(folder.id, event.currentTarget.checked)}
								/>
								<span class="min-w-0 flex-1 truncate text-sm">{folder.name}</span>
								<span class="pf-meta">{folder.id}</span>
							</label>
						{/each}
						{#each staleIds as folderId (folderId)}
							<label class="flex cursor-pointer items-center gap-3 rounded-md px-2 py-2 text-muted">
								<input
									class="checkbox checkbox-sm"
									type="checkbox"
									checked={selectedIds.includes(folderId)}
									onchange={(event) => toggleFolder(folderId, event.currentTarget.checked)}
								/>
								<span class="min-w-0 flex-1 text-sm">Unavailable folder</span>
								<span class="pf-meta">{folderId}</span>
							</label>
						{/each}
					{/if}
				</div>
			{/if}
		</fieldset>

		{#if preference.available_folders.length === 1}
			<p class="text-xs text-muted">
				Navidrome currently exposes one folder, so both choices show the same catalog.
			</p>
		{/if}

		<Button
			variant="primary"
			size="display-sm"
			icon={Save}
			loading={updateMutation.isPending}
			disabled={unavailable || (mode === 'selected' && selectedIds.length === 0)}
			onclick={() => void save()}
		>
			Save folders
		</Button>
	</div>
{/if}
