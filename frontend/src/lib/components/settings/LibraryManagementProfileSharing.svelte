<script lang="ts">
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
		exportLibraryManagementProfileMutation,
		importLibraryManagementProfileMutation,
		previewLibraryManagementProfileImportMutation
	} from '$lib/queries/library-management/LibraryManagementMutations.svelte';
	import type {
		LibraryManagementProfile,
		LibraryManagementProfileExportResponse,
		LibraryManagementProfileImportPreviewResponse,
		LibraryManagementProfileImportResponse
	} from '$lib/queries/library-management/types';
	import { toastStore } from '$lib/stores/toast';

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
	const exportProfile = exportLibraryManagementProfileMutation();
	const previewImport = previewLibraryManagementProfileImportMutation();
	const importProfile = importLibraryManagementProfileMutation();
	let shareDialog: HTMLDialogElement;
	let shareHeading: HTMLHeadingElement;
	let shareResult = $state<LibraryManagementProfileExportResponse | null>(null);
	let shareError = $state('');
	let shareCopied = $state(false);
	let openedShareId = $state<string | null>(null);
	let shareOpener = $state<HTMLElement | null>(null);
	let importDialog: HTMLDialogElement;
	let importHeading: HTMLHeadingElement;
	let fileInput = $state<HTMLInputElement>();
	let openedImport = $state(false);
	let importOpener = $state<HTMLElement | null>(null);
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
		void openImportDialog();
	});

	async function openShareDialog(profile: LibraryManagementProfile): Promise<void> {
		shareOpener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
		shareResult = null;
		shareError = '';
		shareCopied = false;
		await tick();
		shareDialog.showModal();
		shareHeading.focus();
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
		shareDialog.close();
	}

	function finishShareDialog(): void {
		openedShareId = null;
		shareResult = null;
		shareError = '';
		shareCopied = false;
		onshareclose();
		shareOpener?.focus();
		shareOpener = null;
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

	async function openImportDialog(): Promise<void> {
		importOpener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
		resetImport();
		await tick();
		importDialog.showModal();
		importHeading.focus();
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
		importOpener?.focus();
		importOpener = null;
	}

	function closeImportDialog(): void {
		importDialog.close();
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
			importDialog.close();
			await tick();
			onimported(result);
			toastStore.show({ message: 'Profile imported', type: 'success' });
		} catch (error) {
			importError = error instanceof Error ? error.message : 'Could not import this profile.';
		}
	}
</script>

<dialog
	bind:this={shareDialog}
	class="modal"
	aria-labelledby="share-management-profile-title"
	onclose={finishShareDialog}
	oncancel={(event) => {
		if (exportProfile.isPending) event.preventDefault();
	}}
>
	<div class="modal-box max-w-2xl overflow-hidden p-0">
		<header class="border-b border-base-content/10 bg-base-200/60 px-6 py-5">
			<div class="flex items-start gap-3">
				<span
					class="rounded-xl border border-library-manage/20 bg-library-manage/10 p-2.5 text-library-manage"
				>
					<FileBraces class="h-5 w-5" aria-hidden="true" />
				</span>
				<div>
					<p class="management-step">Portable profile</p>
					<h2
						bind:this={shareHeading}
						id="share-management-profile-title"
						tabindex="-1"
						class="font-display text-xl font-semibold"
					>
						Share {shareProfile?.name ?? 'profile'}
					</h2>
					<p class="mt-1 text-sm text-base-content/60">
						The file contains the profile and its scripts. Roots, assignments, and activation
						settings stay on this server.
					</p>
				</div>
			</div>
		</header>
		<div class="space-y-4 p-6">
			{#if exportProfile.isPending}
				<div
					class="flex min-h-36 items-center justify-center gap-3 text-sm text-base-content/60"
					role="status"
				>
					<span class="loading loading-spinner loading-sm text-library-manage"></span>
					Packaging the saved profile…
				</div>
			{:else if shareError}
				<div class="alert alert-error text-sm" role="alert">{shareError}</div>
			{:else if shareResult}
				<section class="rounded-xl border border-library-manage/20 bg-library-manage/5 p-4">
					<div class="flex flex-wrap items-center justify-between gap-3">
						<div>
							<strong class="flex items-center gap-2 text-sm"
								><PackageCheck class="h-4 w-4 text-library-manage" />Profile file</strong
							>
							<p class="mt-1 text-xs text-base-content/55">
								Attach {shareResult.filename} wherever you share the profile.
							</p>
						</div>
						<button class="btn management-btn btn-sm" onclick={downloadProfile}>
							<Download class="h-4 w-4" /> Download .dnprofile
						</button>
					</div>
				</section>
				<section class="rounded-xl border border-base-content/10 p-4">
					<div class="flex flex-wrap items-start justify-between gap-3">
						<div>
							<strong class="text-sm">Copyable text code</strong>
							<p class="mt-1 text-xs text-base-content/55">
								Use this when you cannot attach a file. Some services may reject long profile codes.
							</p>
						</div>
						<button class="btn btn-outline btn-sm" onclick={() => void copyShareCode()}>
							{#if shareCopied}<Check class="h-4 w-4" data-testid="profile-code-copied-icon" /> Code copied{:else}<Clipboard
									class="h-4 w-4"
								/> Copy code{/if}
						</button>
					</div>
					<details class="mt-3">
						<summary class="cursor-pointer text-xs font-semibold text-base-content/65"
							>Show text code</summary
						>
						<textarea
							class="textarea textarea-bordered mt-2 h-28 w-full resize-y bg-base-200/50 font-mono text-[11px]"
							readonly
							aria-label="Profile share code"
							value={shareResult.share_code}
						></textarea>
					</details>
				</section>
				<p class="flex items-center gap-2 font-mono text-[10px] text-base-content/40">
					<Hash class="h-3.5 w-3.5" /> Bundle checksum: SHA-256 {shareResult.bundle_hash}
				</p>
			{/if}
		</div>
		<div class="modal-action border-t border-base-content/10 px-6 py-4">
			<button class="btn btn-ghost" disabled={exportProfile.isPending} onclick={closeShareDialog}
				>Close</button
			>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close profile sharing" disabled={exportProfile.isPending}>close</button>
	</form>
</dialog>

<dialog
	bind:this={importDialog}
	class="modal"
	aria-labelledby="import-management-profile-title"
	onclose={finishImportDialog}
	oncancel={(event) => {
		if (previewImport.isPending || importProfile.isPending) event.preventDefault();
	}}
>
	<div class="modal-box max-w-3xl overflow-hidden p-0">
		<header class="border-b border-base-content/10 bg-base-200/60 px-6 py-5">
			<div class="flex items-start gap-3">
				<span
					class="rounded-xl border border-library-manage/20 bg-library-manage/10 p-2.5 text-library-manage"
				>
					<FileUp class="h-5 w-5" aria-hidden="true" />
				</span>
				<div>
					<p class="management-step">Shared profile</p>
					<h2
						bind:this={importHeading}
						id="import-management-profile-title"
						tabindex="-1"
						class="font-display text-xl font-semibold"
					>
						Import profile
					</h2>
					<p class="mt-1 text-sm text-base-content/60">
						Check the summary, warnings, and bundled scripts before importing. Shared files aren't
						signed.
					</p>
				</div>
			</div>
		</header>

		<div class="max-h-[70vh] overflow-y-auto p-6">
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
							<small class="mt-1 text-base-content/50">.dnprofile files up to 1 MiB</small>
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
						<label for="management-profile-code" class="text-sm font-semibold"
							>Or paste a text code</label
						>
						<p class="mt-1 text-xs text-base-content/50">
							Codes begin with DNLP1:. Pasting a code replaces the selected file.
						</p>
						<textarea
							id="management-profile-code"
							class="textarea textarea-bordered mt-3 min-h-36 flex-1 resize-y bg-base-100 font-mono text-xs"
							placeholder="DNLP1:…"
							value={importFilename ? '' : importContent}
							oninput={(event) => updateContent(event.currentTarget.value)}
						></textarea>
					</section>
				</div>
				{#if importError}<div class="alert alert-error mt-4 text-sm" role="alert">
						{importError}
					</div>{/if}
			{:else}
				<div class="space-y-5">
					<section class="rounded-2xl border border-library-manage/20 bg-library-manage/5 p-5">
						<div class="flex flex-wrap items-start justify-between gap-3">
							<div>
								<p class="management-step">Profile preview</p>
								<h3 class="font-display text-xl font-semibold">{importPreview.profile.name}</h3>
								<p class="mt-1 max-w-2xl text-sm text-base-content/60">
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
					</section>

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
												class="h-4 w-4 {warning.severity === 'danger'
													? 'text-error'
													: 'text-warning'}"
											/>{warning.title}</strong
										>
										<p class="mt-1 text-xs text-base-content/60">{warning.message}</p>
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
							{:else}<p class="mt-2 text-xs text-base-content/50">
									No tagging scripts included.
								</p>{/if}
						</div>
					</section>

					<label class="grid gap-1.5 text-sm">
						<span class="font-semibold">Imported profile name</span>
						<input
							class="input input-bordered bg-base-100"
							maxlength="120"
							bind:value={importName}
						/>
						<small class="text-base-content/50"
							>The profile and its scripts will be saved without assigning them to a root or
							enabling automation.</small
						>
					</label>
					{#if importError}<div class="alert alert-error text-sm" role="alert">
							{importError}
						</div>{/if}
				</div>
			{/if}
		</div>

		<div class="modal-action border-t border-base-content/10 px-6 py-4">
			{#if importPreview}
				<button
					class="btn btn-ghost mr-auto"
					disabled={importProfile.isPending}
					onclick={() => {
						importPreview = null;
						importName = '';
						importError = '';
					}}>Back</button
				>
			{/if}
			<button
				class="btn btn-ghost"
				disabled={previewImport.isPending || importProfile.isPending}
				onclick={closeImportDialog}>Cancel</button
			>
			{#if importPreview}
				<button
					class="btn management-btn"
					disabled={!importName.trim() || importProfile.isPending}
					onclick={() => void confirmImport()}
				>
					{#if importProfile.isPending}<span class="loading loading-spinner loading-sm"></span>{/if}
					Import custom profile
				</button>
			{:else}
				<button
					class="btn management-btn"
					disabled={!importContent.trim() || previewImport.isPending}
					onclick={() => void reviewImport()}
				>
					{#if previewImport.isPending}<span class="loading loading-spinner loading-sm"></span>{/if}
					Review profile
				</button>
			{/if}
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button
			aria-label="Cancel profile import"
			disabled={previewImport.isPending || importProfile.isPending}>close</button
		>
	</form>
</dialog>
