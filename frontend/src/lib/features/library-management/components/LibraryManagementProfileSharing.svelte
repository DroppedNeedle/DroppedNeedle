<script lang="ts">
	import '$lib/features/library-management/components/library-management.css';
	import { tick } from 'svelte';
	import {
		TriangleAlert,
		Check,
		Clipboard,
		Download,
		FileBraces,
		FileUp,
		Hash,
		PackageCheck
	} from 'lucide-svelte';

	import {
		createExportLibraryManagementProfileMutation,
		createImportLibraryManagementProfileMutation,
		createPreviewLibraryManagementProfileImportMutation
	} from '$lib/features/library-management/queries/LibraryManagementMutations.svelte';
	import type {
		LibraryManagementProfile,
		LibraryManagementProfileExportResponse,
		LibraryManagementProfileImportPreviewResponse,
		LibraryManagementProfileImportResponse
	} from '$lib/features/library-management/queries/types';
	import { toastStore } from '$lib/stores/toast';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Field from '$lib/ui/Field.svelte';
	import Input from '$lib/ui/Input.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import TextAreaField from '$lib/ui/TextAreaField.svelte';

	interface Props {
		shareProfile: LibraryManagementProfile | null;
		importOpen: boolean;
		settingsRevision: string;
		onshareclose: () => void;
		onimportclose: () => void;
		onimported: (result: LibraryManagementProfileImportResponse) => void;
	}

	let {
		shareProfile,
		importOpen,
		settingsRevision,
		onshareclose,
		onimportclose,
		onimported
	}: Props = $props();
	const exportProfile = createExportLibraryManagementProfileMutation();
	const previewImport = createPreviewLibraryManagementProfileImportMutation();
	const importProfile = createImportLibraryManagementProfileMutation();
	let shareOpen = $state(false);
	let shareResult = $state<LibraryManagementProfileExportResponse | null>(null);
	let shareError = $state('');
	let shareCopied = $state(false);
	let openedShareId = $state<string | null>(null);
	let importShown = $state(false);
	let fileInput = $state<HTMLInputElement>();
	let openedImport = $state(false);
	let importContent = $state('');
	let importFilename = $state('');
	let importPreview = $state<LibraryManagementProfileImportPreviewResponse | null>(null);
	let importName = $state('');
	let importError = $state('');
	let dragging = $state(false);

	$effect(() => {
		if (!shareProfile || shareProfile.id === openedShareId) return;
		openedShareId = shareProfile.id;
		void openShareDialog(shareProfile);
	});

	$effect(() => {
		if (!importOpen || openedImport) return;
		openedImport = true;
		openImportDialog();
	});

	async function openShareDialog(profile: LibraryManagementProfile): Promise<void> {
		shareResult = null;
		shareError = '';
		shareCopied = false;
		shareOpen = true;
		try {
			shareResult = await exportProfile.mutateAsync({
				profileId: profile.id,
				request: { expected_settings_revision: settingsRevision }
			});
		} catch (error) {
			shareError = error instanceof Error ? error.message : 'Could not prepare this profile.';
		}
	}

	function closeShareDialog(): void {
		shareOpen = false;
		finishShareDialog();
	}

	function finishShareDialog(): void {
		openedShareId = null;
		shareResult = null;
		shareError = '';
		shareCopied = false;
		onshareclose();
	}

	function downloadProfile(): void {
		if (!shareResult) return;
		const blob = new Blob([shareResult.document], { type: shareResult.mime_type });
		const url = URL.createObjectURL(blob);
		const link = document.createElement('a');
		link.href = url;
		link.download = shareResult.filename;
		link.click();
		URL.revokeObjectURL(url);
		toastStore.show({ message: 'Profile download started', type: 'success' });
	}

	async function copyShareCode(): Promise<void> {
		if (!shareResult) return;
		try {
			await navigator.clipboard.writeText(shareResult.share_code);
			shareCopied = true;
			toastStore.show({ message: 'Profile code copied', type: 'success' });
		} catch {
			toastStore.show({
				message: 'Could not copy automatically - select the code below',
				type: 'error'
			});
		}
	}

	function openImportDialog(): void {
		resetImport();
		importShown = true;
	}

	function resetImport(): void {
		importContent = '';
		importFilename = '';
		importPreview = null;
		importName = '';
		importError = '';
		dragging = false;
		if (fileInput) fileInput.value = '';
	}

	function finishImportDialog(): void {
		openedImport = false;
		resetImport();
		onimportclose();
	}

	function closeImportDialog(): void {
		importShown = false;
		finishImportDialog();
	}

	function updateContent(value: string, filename = ''): void {
		importContent = value;
		importFilename = filename;
		importPreview = null;
		importName = '';
		importError = '';
	}

	async function readFile(file: File): Promise<void> {
		updateContent('');
		if (file.size > 1_048_576) {
			importError = 'Profile files must be 1 MiB or smaller.';
			return;
		}
		try {
			updateContent(await file.text(), file.name);
		} catch {
			updateContent('');
			importError = 'Could not read that profile file.';
		}
	}

	async function chooseFile(event: Event): Promise<void> {
		const file = (event.currentTarget as HTMLInputElement).files?.[0];
		if (file) await readFile(file);
	}

	async function dropFile(event: DragEvent): Promise<void> {
		event.preventDefault();
		dragging = false;
		const file = event.dataTransfer?.files[0];
		if (file) await readFile(file);
	}

	async function reviewImport(): Promise<void> {
		if (!importContent.trim()) return;
		importError = '';
		try {
			importPreview = await previewImport.mutateAsync({
				content: importContent,
				expected_settings_revision: settingsRevision
			});
			importName = importPreview.profile.name;
		} catch (error) {
			importError = error instanceof Error ? error.message : 'Could not read this profile.';
		}
	}

	async function confirmImport(): Promise<void> {
		if (!importPreview || !importName.trim()) return;
		importError = '';
		try {
			const result = await importProfile.mutateAsync({
				content: importContent,
				reviewed_bundle_hash: importPreview.bundle_hash,
				name: importName.trim(),
				expected_settings_revision: importPreview.settings_revision
			});
			closeImportDialog();
			await tick();
			onimported(result);
			toastStore.show({ message: 'Profile imported', type: 'success' });
		} catch (error) {
			importError = error instanceof Error ? error.message : 'Could not import this profile.';
		}
	}
</script>

<Modal
	bind:open={shareOpen}
	title="Share {shareProfile?.name ?? 'profile'}"
	description="The file contains the profile and its scripts. Roots, assignments, and activation settings stay on this server."
	icon={FileBraces}
	size="lg"
	focusTitle
	dismissible={!exportProfile.isPending}
	onclose={finishShareDialog}
>
	<div class="space-y-4">
		{#if exportProfile.isPending}
			<div class="flex min-h-36 items-center justify-center gap-3 text-sm text-muted" role="status">
				<span class="loading loading-spinner loading-sm text-library-manage"></span>
				Packaging the saved profile…
			</div>
		{:else if shareError}
			<div class="alert alert-error text-sm" role="alert">{shareError}</div>
		{:else if shareResult}
			<Card as="section" variant="bordered" padding="md">
				<div class="flex flex-wrap items-center justify-between gap-3">
					<div>
						<strong class="flex items-center gap-2 text-sm"
							><PackageCheck class="h-4 w-4 text-library-manage" />Profile file</strong
						>
						<p class="mt-1 text-xs text-muted">
							Attach {shareResult.filename} wherever you share the profile.
						</p>
					</div>
					<Button variant="warning" size="sm" icon={Download} onclick={downloadProfile}>
						Download .dnprofile
					</Button>
				</div>
			</Card>
			<Card as="section" variant="bordered" padding="md">
				<div class="flex flex-wrap items-start justify-between gap-3">
					<div>
						<strong class="text-sm">Copyable text code</strong>
						<p class="mt-1 text-xs text-muted">
							Use this when you cannot attach a file. Some services may reject long profile codes.
						</p>
					</div>
					<Button variant="outline" size="sm" onclick={() => void copyShareCode()}>
						{#if shareCopied}<Check class="h-4 w-4" data-testid="profile-code-copied-icon" /> Code copied{:else}<Clipboard
								class="h-4 w-4"
							/> Copy code{/if}
					</Button>
				</div>
				<details class="mt-3">
					<summary class="cursor-pointer text-xs font-semibold text-muted">Show text code</summary>
					<TextAreaField
						label="Profile share code"
						mono
						readonly
						rows={4}
						value={shareResult.share_code}
					/>
				</details>
			</Card>
			<p class="flex items-center gap-2 font-mono text-[10px] text-muted">
				<Hash class="h-3.5 w-3.5" /> Bundle checksum: SHA-256 {shareResult.bundle_hash}
			</p>
		{/if}
	</div>
	{#snippet actions()}
		<Button variant="ghost" disabled={exportProfile.isPending} onclick={closeShareDialog}
			>Close</Button
		>
	{/snippet}
</Modal>

<Modal
	bind:open={importShown}
	title="Import profile"
	description="Check the summary, warnings, and bundled scripts before importing. Shared files aren't signed."
	icon={FileUp}
	size="xl"
	focusTitle
	dismissible={!previewImport.isPending && !importProfile.isPending}
	onclose={finishImportDialog}
>
	{#if !importPreview}
		<div class="grid gap-4 md:grid-cols-[1.05fr_0.95fr]">
			<section>
				<button
					type="button"
					class="flex min-h-48 w-full flex-col items-center justify-center rounded-2xl border-2 border-dashed p-6 text-center transition-colors {dragging
						? 'border-library-manage bg-library-manage/10'
						: 'border-base-content/15 bg-base-200/35 hover:border-library-manage/45'}"
					onclick={() => fileInput?.click()}
					ondragover={(event) => {
						event.preventDefault();
						dragging = true;
					}}
					ondragleave={() => (dragging = false)}
					ondrop={(event) => void dropFile(event)}
				>
					<span class="rounded-2xl bg-library-manage/10 p-3 text-library-manage"
						><FileBraces class="h-7 w-7" /></span
					>
					<strong class="mt-3 text-sm">Choose or drop a .dnprofile file</strong>
					<small class="mt-1 text-muted">.dnprofile files up to 1 MiB</small>
					{#if importFilename}<span class="badge badge-outline mt-3 max-w-full truncate"
							>{importFilename}</span
						>{/if}
				</button>
				<input
					bind:this={fileInput}
					type="file"
					aria-label="Profile file"
					accept=".dnprofile,application/json,application/vnd.droppedneedle.profile+json"
					class="hidden"
					onchange={(event) => void chooseFile(event)}
				/>
			</section>
			<section class="flex flex-col">
				<TextAreaField
					label="Or paste a text code"
					help="Codes begin with DNLP2:. Pasting a code replaces the selected file."
					mono
					rows={6}
					placeholder="DNLP2:…"
					value={importFilename ? '' : importContent}
					oninput={updateContent}
				/>
			</section>
		</div>
		{#if importError}<div class="alert alert-error mt-4 text-sm" role="alert">
				{importError}
			</div>{/if}
	{:else}
		<div class="space-y-5">
			<Card as="section" variant="bordered" padding="md">
				<div class="flex flex-wrap items-start justify-between gap-3">
					<div>
						<p class="management-step">Profile preview</p>
						<h3 class="font-display text-xl font-semibold">{importPreview.profile.name}</h3>
						<p class="mt-1 max-w-2xl text-sm text-muted">
							{importPreview.profile.description || 'No description supplied.'}
						</p>
					</div>
					<span class="badge badge-outline gap-1"
						><Check class="h-3.5 w-3.5" /> Custom and inactive</span
					>
				</div>
				<div class="mt-4 flex flex-wrap gap-2">
					{#each importPreview.aspects as aspect (aspect)}<span class="management-aspect"
							>{aspect}</span
						>{/each}
				</div>
			</Card>

			{#if importPreview.warnings.length}
				<section aria-labelledby="profile-import-warnings-title">
					<h3 id="profile-import-warnings-title" class="text-sm font-semibold">
						Review these write behaviors
					</h3>
					<div class="mt-2 grid gap-2 sm:grid-cols-2">
						{#each importPreview.warnings as warning (warning.code)}
							<div
								class="rounded-xl border p-3 text-sm {warning.severity === 'danger'
									? 'border-error/25 bg-error/5'
									: 'border-warning/25 bg-warning/5'}"
							>
								<strong class="flex items-center gap-2"
									><TriangleAlert
										class="h-4 w-4 {warning.severity === 'danger' ? 'text-error' : 'text-warning'}"
									/>{warning.title}</strong
								>
								<p class="mt-1 text-xs text-muted">{warning.message}</p>
							</div>
						{/each}
					</div>
				</section>
			{/if}

			<section class="grid gap-3 sm:grid-cols-2">
				<div>
					<h3 class="text-sm font-semibold">Naming scripts</h3>
					<div class="mt-2 space-y-2">
						{#each importPreview.naming_scripts as script (script.id)}
							<details class="rounded-xl border border-base-content/10 p-3">
								<summary class="cursor-pointer text-xs font-semibold">{script.name}</summary>
								<pre
									class="mt-2 overflow-x-auto whitespace-pre-wrap rounded-lg bg-base-200/70 p-3 text-[11px]"><code
										>{script.source}</code
									></pre>
							</details>
						{/each}
					</div>
				</div>
				<div>
					<h3 class="text-sm font-semibold">Tagging scripts</h3>
					{#if importPreview.tagging_scripts.length}
						<div class="mt-2 space-y-2">
							{#each importPreview.tagging_scripts as script (script.id)}
								<details class="rounded-xl border border-base-content/10 p-3">
									<summary class="cursor-pointer text-xs font-semibold">{script.name}</summary>
									<pre
										class="mt-2 overflow-x-auto whitespace-pre-wrap rounded-lg bg-base-200/70 p-3 text-[11px]"><code
											>{script.source}</code
										></pre>
								</details>
							{/each}
						</div>
					{:else}<p class="mt-2 text-xs text-muted">No tagging scripts included.</p>{/if}
				</div>
			</section>

			<Field
				label="Imported profile name"
				help="The profile and its scripts will be saved without assigning them to a root or enabling automation."
			>
				{#snippet children(control)}
					<Input
						{...control}
						label="Imported profile name"
						class="w-full"
						maxlength={120}
						bind:value={importName}
					/>
				{/snippet}
			</Field>
			{#if importError}<div class="alert alert-error text-sm" role="alert">
					{importError}
				</div>{/if}
		</div>
	{/if}
	{#snippet actions()}
		{#if importPreview}
			<Button
				variant="ghost"
				class="mr-auto"
				disabled={importProfile.isPending}
				onclick={() => {
					importPreview = null;
					importName = '';
					importError = '';
				}}>Back</Button
			>
		{/if}
		<Button
			variant="ghost"
			disabled={previewImport.isPending || importProfile.isPending}
			onclick={closeImportDialog}>Cancel</Button
		>
		{#if importPreview}
			<Button
				variant="warning"
				loading={importProfile.isPending}
				disabled={!importName.trim()}
				onclick={() => void confirmImport()}
			>
				Import custom profile
			</Button>
		{:else}
			<Button
				variant="warning"
				loading={previewImport.isPending}
				disabled={!importContent.trim()}
				onclick={() => void reviewImport()}
			>
				Review profile
			</Button>
		{/if}
	{/snippet}
</Modal>
