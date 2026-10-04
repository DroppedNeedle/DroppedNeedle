<script lang="ts">
	import NavidromeIcon from '$lib/components/NavidromeIcon.svelte';
	import { getRemoteFoldersQuery } from '$lib/queries/remotes/RemoteQueries.svelte';
	import { createSaveRemoteFoldersMutation } from '$lib/queries/remotes/RemoteMutations.svelte';
	import { CircleAlert, Folder, Save } from 'lucide-svelte';

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

	function setMode(nextMode: FolderMode) {
		mode = nextMode;
	}

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

<section
	class="crate-card overflow-hidden rounded-xl border border-base-300/40 bg-base-200/50 backdrop-blur-sm"
	aria-labelledby="navidrome-music-folders-heading"
>
	<div class="flex items-center gap-3 border-b border-base-300/30 px-5 py-4">
		<div class="flex h-10 w-10 items-center justify-center rounded-lg bg-base-300/60 text-primary">
			<NavidromeIcon class="h-5 w-5" />
		</div>
		<div>
			<h2 id="navidrome-music-folders-heading" class="text-sm font-semibold">Music folders</h2>
			<p class="mt-0.5 text-xs text-base-content/50">
				Choose which Navidrome folders appear in your DroppedNeedle catalog.
			</p>
		</div>
	</div>

	{#if preferenceQuery.isPending}
		<div class="space-y-3 p-5" aria-label="Loading music folder preferences">
			<div class="skeleton h-5 w-32"></div>
			<div class="skeleton h-11 w-full"></div>
			<div class="skeleton h-11 w-full"></div>
		</div>
	{:else if preferenceQuery.isError || !preference}
		<div class="p-5">
			<div class="alert alert-error" role="alert">
				<CircleAlert class="h-5 w-5" />
				<span>Music folder preferences could not be loaded.</span>
			</div>
		</div>
	{:else}
		<div class="space-y-5 p-5">
			{#if unavailable}
				<div class="alert alert-warning" role="alert">
					<CircleAlert class="h-5 w-5" />
					<span>
						Navidrome is unavailable. Your saved choice is shown, but changes cannot be verified
						right now.
					</span>
				</div>
			{/if}

			{#if staleIds.length > 0}
				<div class="alert alert-warning" role="alert">
					<CircleAlert class="h-5 w-5" />
					<div>
						<p class="font-medium">Some saved folders are no longer available.</p>
						<p class="mt-1 text-xs">
							Choose replacements or switch to All folders. The catalog will not widen
							automatically.
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
				<legend class="sr-only">Navidrome music folder mode</legend>
				<label
					class="flex cursor-pointer items-start gap-3 rounded-lg border border-base-300/50 bg-base-100/30 p-4"
				>
					<input
						class="radio radio-primary radio-sm mt-0.5"
						type="radio"
						name="navidrome-folder-mode"
						checked={mode === 'all'}
						onchange={() => setMode('all')}
					/>
					<span>
						<span class="block text-sm font-medium">All folders</span>
						<span class="mt-1 block text-xs text-base-content/50">
							Include current and future folders from the shared Navidrome server.
						</span>
					</span>
				</label>

				<label
					class="flex cursor-pointer items-start gap-3 rounded-lg border border-base-300/50 bg-base-100/30 p-4"
				>
					<input
						class="radio radio-primary radio-sm mt-0.5"
						type="radio"
						name="navidrome-folder-mode"
						checked={mode === 'selected'}
						onchange={() => setMode('selected')}
					/>
					<span>
						<span class="block text-sm font-medium">Selected folders</span>
						<span class="mt-1 block text-xs text-base-content/50">
							Use only the folders checked below for browsing and matching.
						</span>
					</span>
				</label>

				{#if mode === 'selected'}
					<div class="ml-0 space-y-2 rounded-lg bg-base-300/20 p-3 sm:ml-8">
						{#if preference.available_folders.length === 0 && staleIds.length === 0}
							<div class="flex items-center gap-2 px-2 py-3 text-sm text-base-content/60">
								<Folder class="h-4 w-4" />
								<span>No music folders were returned by Navidrome.</span>
							</div>
						{:else}
							{#each preference.available_folders as folder (folder.id)}
								<label
									class="flex cursor-pointer items-center gap-3 rounded-md px-2 py-2 hover:bg-base-300/30"
								>
									<input
										class="checkbox checkbox-primary checkbox-sm"
										type="checkbox"
										checked={selectedIds.includes(folder.id)}
										onchange={(event) => toggleFolder(folder.id, event.currentTarget.checked)}
									/>
									<span class="min-w-0 flex-1 truncate text-sm">{folder.name}</span>
									<span class="text-xs text-base-content/35">{folder.id}</span>
								</label>
							{/each}
							{#each staleIds as folderId (folderId)}
								<label
									class="flex cursor-pointer items-center gap-3 rounded-md px-2 py-2 opacity-70"
								>
									<input
										class="checkbox checkbox-sm"
										type="checkbox"
										checked={selectedIds.includes(folderId)}
										onchange={(event) => toggleFolder(folderId, event.currentTarget.checked)}
									/>
									<span class="min-w-0 flex-1 text-sm">Unavailable folder</span>
									<span class="text-xs text-base-content/45">{folderId}</span>
								</label>
							{/each}
						{/if}
					</div>
				{/if}
			</fieldset>

			{#if preference.available_folders.length === 1}
				<p class="text-xs text-base-content/50">
					Navidrome currently exposes one folder, so both choices show the same catalog.
				</p>
			{/if}

			<div class="flex justify-end">
				<button
					class="btn btn-primary btn-sm gap-2"
					disabled={unavailable ||
						updateMutation.isPending ||
						(mode === 'selected' && selectedIds.length === 0)}
					onclick={() => void save()}
				>
					{#if updateMutation.isPending}
						<span class="loading loading-spinner loading-xs"></span>
					{:else}
						<Save class="h-4 w-4" />
					{/if}
					Save folders
				</button>
			</div>
		</div>
	{/if}
</section>
