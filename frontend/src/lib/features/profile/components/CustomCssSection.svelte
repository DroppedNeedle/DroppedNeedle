<!--
	The person's own CSS: the "Use my CSS" switch, live preview, import and
	download, the editor, a rail of variables, snippets and saved versions,
	and the save bar. While live preview is on, a draft the check lets
	through is applied in place of the saved sheet after a short pause;
	Cancel, a save or leaving puts the saved one back. Mount it once per
	signed-in user (the card keys it on the user).
-->
<script lang="ts">
	import { onDestroy, untrack } from 'svelte';
	import { Braces, Download, History, Puzzle, Upload } from 'lucide-svelte';
	import {
		createSaveAppearanceMutation,
		getMyAppearanceQuery
	} from '$lib/features/profile/queries/appearance.svelte';
	import {
		customCssProblems,
		customCssSaveError,
		customCssStore,
		MAX_CUSTOM_CSS
	} from '$lib/stores/customCss.svelte';
	import { toastStore } from '$lib/stores/toast';
	import Button from '$lib/ui/Button.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';
	import Tabs from '$lib/ui/Tabs.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import UnsavedChangesGuard from '$lib/ui/UnsavedChangesGuard.svelte';
	import CssHistoryList from './CssHistoryList.svelte';
	import CssSnippetsMenu from './CssSnippetsMenu.svelte';
	import CssVariablesPanel from './CssVariablesPanel.svelte';
	import CustomCssEditor from './CustomCssEditor.svelte';
	import './customCss.css';

	interface Sheet {
		text: string;
	}

	type RailTab = 'variables' | 'snippets' | 'history';

	const PREVIEW_PAUSE_MS = 300;
	const UNDO_FOR_MS = 8000;

	const appearance = getMyAppearanceQuery();
	const saveText = createSaveAppearanceMutation();
	const saveSwitch = createSaveAppearanceMutation();

	const saved = $derived<Sheet | undefined>(
		appearance.data ? { text: appearance.data.custom_css.text } : undefined
	);
	const allowed = $derived(appearance.data?.site.allow_user_css ?? true);
	const siteHasCss = $derived(Boolean(appearance.data?.site.css.trim()));

	const section = createSettingsSection<Sheet>(
		{
			get data() {
				return saved;
			},
			get isPending() {
				return appearance.isPending;
			},
			get isError() {
				return appearance.isError;
			},
			get error() {
				return appearance.error;
			},
			refetch: () => appearance.refetch()
		},
		{
			mutateAsync: storeText,
			get isPending() {
				return saveText.isPending;
			}
		},
		{ explain: (error) => customCssSaveError(error) }
	);

	let editor = $state<CustomCssEditor | null>(null);
	let fileInput = $state<HTMLInputElement | null>(null);
	let live = $state(true);
	let rail = $state<RailTab>('variables');
	let revision = $state(0);
	let localError = $state<string | null>(null);
	let enabledShown = $derived(appearance.data?.custom_css.enabled ?? true);

	const draftText = $derived(section.draft?.text ?? '');
	const refused = $derived(customCssProblems(draftText).length > 0);
	const tabs = [
		{ id: 'variables' as const, label: 'Variables', icon: Braces },
		{ id: 'snippets' as const, label: 'Snippets', icon: Puzzle },
		{ id: 'history' as const, label: 'History', icon: History }
	];

	async function storeText(sheet: Sheet): Promise<Sheet> {
		const before = saved?.text ?? '';
		const answer = await saveText.mutateAsync({ custom_css: { text: sheet.text } });
		if (answer.custom_css.text !== before) {
			toastStore.show({
				message: 'CSS saved.',
				type: 'success',
				duration: UNDO_FOR_MS,
				action: { label: 'Undo', run: () => undo(before) }
			});
		}
		return { text: answer.custom_css.text };
	}

	async function undo(text: string): Promise<void> {
		try {
			await saveText.mutateAsync({ custom_css: { text } });
		} catch (error) {
			toastStore.show({ message: customCssSaveError(error), type: 'error' });
		}
	}

	async function save(): Promise<void> {
		localError = customCssStore.saveProblem(draftText);
		if (localError) return;
		await section.save();
	}

	function setEnabled(on: boolean): void {
		enabledShown = on;
		saveSwitch.mutate(
			{ custom_css: { enabled: on } },
			{
				onError: (error) => {
					enabledShown = !on;
					toastStore.show({ message: customCssSaveError(error), type: 'error' });
				}
			}
		);
	}

	function setDraft(text: string): void {
		if (section.draft) section.draft.text = text;
	}

	function insert(text: string): void {
		editor?.insert(text);
	}

	async function clear(): Promise<void> {
		setDraft('');
		await save();
	}

	async function importFile(event: Event & { currentTarget: HTMLInputElement }): Promise<void> {
		const input = event.currentTarget;
		const file = input.files?.[0];
		input.value = '';
		if (!file) return;
		const text = await file.text();
		if (text.length > MAX_CUSTOM_CSS) {
			toastStore.show({
				message: `${file.name} is longer than ${MAX_CUSTOM_CSS.toLocaleString()} characters.`,
				type: 'error'
			});
			return;
		}
		setDraft(text);
	}

	function download(): void {
		const url = URL.createObjectURL(new Blob([draftText], { type: 'text/css' }));
		try {
			const link = document.createElement('a');
			link.href = url;
			link.download = 'droppedneedle.css';
			document.body.appendChild(link);
			link.click();
			link.remove();
		} finally {
			URL.revokeObjectURL(url);
		}
	}

	function showDraft(text: string | null): void {
		customCssStore.preview(text);
		revision++;
	}

	// A draft that fails the check keeps the last one that passed on the page.
	$effect(() => {
		const text = draftText;
		if (!live || !section.dirty) {
			untrack(() => showDraft(null));
			return;
		}
		if (refused) return;
		const timer = setTimeout(() => showDraft(text), PREVIEW_PAUSE_MS);
		return () => clearTimeout(timer);
	});

	$effect(() => {
		void draftText;
		localError = null;
	});

	onDestroy(() => customCssStore.preview(null));

	const footerStatus = $derived(
		section.status ??
			(customCssStore.previewing
				? { tone: 'info' as const, message: 'Previewing unsaved changes.' }
				: null)
	);
</script>

<UnsavedChangesGuard />
<p class="pf-group mt-6" aria-hidden="true">Custom CSS</p>
<QueryBoundary query={section} what="your custom CSS">
	{#snippet children(draft)}
		<div class="css-root">
			<div class="css-layout">
				<div class="css-switches">
					<div class="css-switch">
						<ToggleRow
							label="Use my CSS"
							disabled={!allowed || saveSwitch.isPending}
							bind:checked={() => enabledShown, setEnabled}
						/>
					</div>
					<div class="css-switch">
						<ToggleRow label="Live preview" disabled={!allowed} bind:checked={live} />
					</div>
				</div>
				<div class="css-files">
					<input
						type="file"
						accept=".css,text/css"
						class="hidden"
						aria-label="Choose a CSS file"
						bind:this={fileInput}
						onchange={importFile}
					/>
					<Button
						variant="outline"
						size="sm"
						icon={Upload}
						disabled={!allowed}
						onclick={() => fileInput?.click()}
					>
						Import
					</Button>
					<Button
						variant="outline"
						size="sm"
						icon={Download}
						disabled={!draft.text}
						onclick={download}
					>
						Download
					</Button>
					{#if !allowed && saved?.text}
						<Button variant="danger-outline" size="sm" onclick={clear} disabled={section.saving}>
							Clear
						</Button>
					{/if}
				</div>

				<div class="css-editor">
					{#if !allowed}
						<p class="mb-2 text-sm text-warning">Custom CSS is turned off on this server.</p>
					{:else if customCssStore.disabled}
						<p class="mb-2 text-sm text-warning">Off until the page is reloaded.</p>
					{/if}
					<CustomCssEditor
						bind:this={editor}
						bind:value={draft.text}
						readonly={!allowed}
						error={localError}
						help={siteHasCss
							? "Goes on top of the site's CSS. Add ?nocustomcss=1 to the address to turn both off."
							: 'Add ?nocustomcss=1 to the address to turn it off.'}
					/>
				</div>

				<div class="css-rail">
					<Tabs
						{tabs}
						selected={rail}
						onselect={(id) => (rail = id)}
						label="Editor helpers"
						idPrefix="css-rail"
						look="border"
						panelClass="css-rail-panel"
					>
						{#if rail === 'variables'}
							<CssVariablesPanel oninsert={insert} disabled={!allowed} {revision} />
						{:else if rail === 'snippets'}
							<CssSnippetsMenu oninsert={insert} disabled={!allowed} />
						{:else}
							<CssHistoryList onrestore={setDraft} disabled={!allowed} />
						{/if}
					</Tabs>
				</div>

				<div class="css-footer">
					<SaveBar
						{...section.bar}
						onsave={save}
						discardLabel="Cancel"
						disabled={refused}
						status={footerStatus}
					/>
				</div>
			</div>
		</div>
	{/snippet}
</QueryBoundary>
