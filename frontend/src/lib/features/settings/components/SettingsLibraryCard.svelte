<script lang="ts">
	import { untrack } from 'svelte';
	import { CircleCheckBig, TriangleAlert } from 'lucide-svelte';
	import { ApiError } from '$lib/api/errors';
	import LibraryRootPolicyEditor from '$lib/features/library-management/components/LibraryRootPolicyEditor.svelte';
	import ScriptWorkbench from '$lib/features/library-management/components/ScriptWorkbench.svelte';
	import type { TypedLibrarySettings } from '$lib/features/library-management/queries/LibraryOperationsTypes';
	import Card from '$lib/ui/Card.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { createUuid } from '$lib/utils/uuid';
	import { createLibraryCard, type LibraryCard } from './libraryCard.svelte';
	import { useSuggestion } from './setup/suggest.svelte';

	interface Props {
		/** The form the page around the card also reads; the card makes its own without one. */
		card?: LibraryCard;
		/** A folder the setup guide found, added as the library folder while none is set. */
		suggested?: string;
	}

	let { card, suggested }: Props = $props();

	const library = untrack(() => card) ?? createLibraryCard();
	const { section, impact, save } = library;

	useSuggestion(section, () => suggested, addFolder);

	const warnings = $derived(library.currentSettings?.warnings ?? []);

	let impactOpen = $state(false);
	// The save button is busy while the preview loads, so focus is gone by the
	// time the dialog opens.
	let impactOpener: HTMLElement | null = null;

	const impactErrorMessage = $derived.by((): string | null => {
		if (!impact.isError) return null;
		const error = impact.error;
		if (error instanceof ApiError && error.status > 0 && error.status < 500) {
			return error.message;
		}
		return 'Could not reach the server to preview these settings. Check your connection and try again. Nothing has been saved.';
	});

	function addFolder(draft: TypedLibrarySettings, path: string): void {
		if (draft.library_roots.length > 0) return;
		const name = path.split('/').filter(Boolean).at(-1) ?? path;
		draft.library_roots = [
			{
				id: createUuid(),
				path,
				label: name.charAt(0).toUpperCase() + name.slice(1),
				policy: 'automatic',
				rules: []
			}
		];
	}

	async function previewSave(): Promise<void> {
		const draft = section.draft;
		if (!draft) return;
		impactOpener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
		let result;
		try {
			result = await impact.mutateAsync({
				settings: $state.snapshot(draft) as TypedLibrarySettings,
				expected_policy_revision: library.currentSettings?.policy_revision ?? null
			});
		} catch {
			return;
		}
		if (result.stale) {
			toastStore.show({
				message: 'Library settings changed. Reload this page before saving.',
				type: 'error'
			});
			return;
		}
		impactOpen = true;
	}

	async function confirmSave(): Promise<void> {
		if (!impact.data?.current_policy_revision) return;
		await section.save();
		if (save.error) throw save.error;
	}
</script>

<Card as="section" variant="bordered">
	{#if authStore.isAdmin}
		<QueryBoundary query={section} what="the library settings">
			{#snippet loading()}
				<div class="space-y-3">
					<div class="skeleton h-48 rounded-box"></div>
					<div class="skeleton h-28 rounded-box"></div>
				</div>
			{/snippet}
			{#snippet children(draft)}
				<div class="flex flex-col gap-6">
					<ToggleRow
						label="Local library"
						description="Scanning, identification, and file organization run while this is on. Turn it off to pause the library without removing your roots. Existing catalog data and playback keep working."
						bind:checked={draft.enabled}
					/>

					<div class="divider my-0"></div>

					<LibraryRootPolicyEditor
						roots={draft.library_roots}
						onchange={(value) => (draft.library_roots = value)}
					/>

					{#if warnings.length > 0}
						<div class="alert alert-warning items-start text-sm">
							<TriangleAlert class="mt-0.5 h-4 w-4 shrink-0" />
							<div class="min-w-0 space-y-1">
								{#each warnings as warning, index (`${index}:${warning}`)}
									<p>{warning}</p>
								{/each}
							</div>
						</div>
					{/if}

					<div class="divider my-0"></div>

					<section class="space-y-2">
						<ScriptWorkbench
							kind="import_naming"
							label="Import naming script"
							help="Where downloads and dropped files are filed."
							bind:value={draft.naming_script}
							error={section.fieldError('naming_script', 'Import naming script')}
						/>
					</section>

					<div class="divider my-0"></div>

					<SecretField
						label="AcoustID API key"
						mask={SECRET_MASK}
						placeholder="Enter AcoustID API key"
						help="Identifies untagged music by its audio and checks downloads."
						bind:value={draft.acoustid_api_key}
						error={section.fieldError('acoustid_api_key', 'AcoustID API key')}
					/>

					<div class="space-y-2 border-t border-base-content/10 pt-5">
						{#if impactErrorMessage}
							<p class="text-sm text-error">{impactErrorMessage}</p>
						{/if}
						<SaveBar
							{...section.bar}
							onsave={previewSave}
							saving={impact.isPending || section.saving}
							saveLabel="Preview and save settings"
						/>
					</div>
				</div>
			{/snippet}
		</QueryBoundary>
	{:else}
		<p class="text-sm text-warning">Sign in as an administrator to change library settings.</p>
	{/if}
</Card>

<ConfirmDialog
	bind:open={impactOpen}
	title="Save library policy changes?"
	confirmLabel="Save policies"
	focusTitle
	pending={save.isPending}
	onconfirm={confirmSave}
	onclose={() => impactOpener?.focus()}
>
	{#if impact.data}<div class="space-y-3 text-sm">
			<p>
				<strong
					>{impact.data.indexed_file_count?.toLocaleString() ?? 'An unknown number of'} indexed files</strong
				>
				and {impact.data.on_disk_file_count?.toLocaleString() ?? 'an unknown number of'} files on disk
				are in the affected scopes.
			</p>
			{#if impact.data.content_will_become_unavailable}<div class="alert alert-warning">
					<TriangleAlert class="h-4 w-4" /> Some music will become unavailable after you explicitly apply
					reconciliation.
				</div>{/if}{#if impact.data.queued_work_will_be_cancelled}<p class="text-warning">
					Queued work in these scopes will be cancelled when the policy is saved.
				</p>{/if}{#each impact.data.warnings as warning, index (`${index}:${warning}`)}<p
					class="text-muted"
				>
					{warning}
				</p>{/each}
			<p class="flex items-center gap-2 text-success">
				<CircleCheckBig class="h-4 w-4" /> Saving does not start a scan.
			</p>
		</div>{/if}
</ConfirmDialog>
