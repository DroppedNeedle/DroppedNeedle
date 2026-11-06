<script lang="ts">
	import { ArrowDown, ArrowUp, Copy, FileCode, Plus } from 'lucide-svelte';

	import type { ManagementScriptSettings } from '$lib/features/library-management/queries/types';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import { createUuid } from '$lib/utils/uuid';

	import ScriptWorkbench from './ScriptWorkbench.svelte';

	interface Props {
		kind: 'naming' | 'tagging';
		scripts: ManagementScriptSettings[];
		selectedIds: string[];
		onchange: (scripts: ManagementScriptSettings[], selectedIds: string[]) => void;
	}

	let { kind, scripts, selectedIds, onchange }: Props = $props();
	const initialEditorId = (): string => selectedIds[0] ?? scripts[0]?.id ?? '';
	let editorScriptId = $state(initialEditorId());

	const selectedScript = $derived(scripts.find((script) => script.id === editorScriptId) ?? null);
	const attached = $derived(selectedIds.includes(editorScriptId));
	const namingAssignment = $derived.by(() => {
		if (kind !== 'naming') return '';
		const single = selectedIds[0] === editorScriptId;
		const multi = selectedIds[1] === editorScriptId;
		if (single && multi) return 'Assigned to single-disc and multi-disc';
		if (single) return 'Assigned to single-disc';
		if (multi) return 'Assigned to multi-disc';
		return 'Not assigned';
	});
	function updateScript(update: Partial<Pick<ManagementScriptSettings, 'name' | 'source'>>): void {
		if (!selectedScript) return;
		onchange(
			scripts.map((script) =>
				script.id === selectedScript.id ? { ...script, ...update } : script
			),
			selectedIds
		);
	}

	function addScript(copySource = false): void {
		const source =
			copySource && selectedScript
				? selectedScript.source
				: kind === 'naming'
					? '{albumartist}/{album}{conditional(is_empty(year), "", concat(" (", year, ")"))}/{track:02d} - {title}.{ext}'
					: 'set title = title(title)';
		const script: ManagementScriptSettings = {
			id: createUuid(),
			name: copySource && selectedScript ? `${selectedScript.name} copy` : `New ${kind} script`,
			source,
			revision: '',
			preset_origin: null,
			preset_version: null
		};
		editorScriptId = script.id;
		onchange([...scripts, script], selectedIds);
	}

	function toggleAttached(): void {
		if (!selectedScript) return;
		if (kind === 'naming') return;
		onchange(
			scripts,
			attached
				? selectedIds.filter((id) => id !== selectedScript.id)
				: [...selectedIds, selectedScript.id]
		);
	}

	function moveAttached(direction: -1 | 1): void {
		const index = selectedIds.indexOf(editorScriptId);
		const target = index + direction;
		if (index < 0 || target < 0 || target >= selectedIds.length) return;
		const next = [...selectedIds];
		[next[index], next[target]] = [next[target], next[index]];
		onchange(scripts, next);
	}
</script>

<div class="management-script-editor">
	<div class="management-script-toolbar">
		<div class="min-w-0 flex-1">
			<SelectField
				label="Script currently being edited"
				options={scripts.map((script) => ({ value: script.id, label: script.name }))}
				bind:value={editorScriptId}
			/>
		</div>
		<Button variant="outline" size="sm" icon={Plus} onclick={() => addScript(false)}>New</Button>
		<Button
			variant="outline"
			size="sm"
			icon={Copy}
			disabled={!selectedScript}
			onclick={() => addScript(true)}>Duplicate</Button
		>
	</div>

	{#if selectedScript}
		<div class="management-script-workbench">
			<div class="flex flex-wrap items-center justify-between gap-2">
				<div class="flex items-center gap-2">
					<FileCode class="h-4 w-4 text-library-manage" />
					<span class="text-xs text-muted">
						{kind === 'naming' ? 'Changes paths, never tags.' : 'Changes metadata, never paths.'}
					</span>
				</div>
				<div class="flex items-center gap-1">
					{#if kind === 'naming'}<span class="badge badge-ghost badge-sm">{namingAssignment}</span
						>{/if}
					{#if kind === 'tagging'}
						<Button variant="ghost" size="xs" onclick={toggleAttached} aria-pressed={attached}>
							{attached ? 'Attached' : 'Attach'}
						</Button>
					{/if}
					{#if kind === 'tagging' && attached}
						<IconButton
							size="xs"
							shape="square"
							icon={ArrowUp}
							label="Run script earlier"
							disabled={selectedIds.indexOf(editorScriptId) === 0}
							onclick={() => moveAttached(-1)}
						/>
						<IconButton
							size="xs"
							shape="square"
							icon={ArrowDown}
							label="Run script later"
							disabled={selectedIds.indexOf(editorScriptId) === selectedIds.length - 1}
							onclick={() => moveAttached(1)}
						/>
					{/if}
				</div>
			</div>

			<div class="mt-3">
				<TextField
					label="Script name"
					error={selectedScript.name.trim() ? null : 'The script needs a name.'}
					bind:value={() => selectedScript.name, (name) => updateScript({ name })}
				/>
			</div>
			<div class="mt-3">
				{#key selectedScript.id}
					<ScriptWorkbench
						{kind}
						label={`${kind === 'naming' ? 'Naming' : 'Tagging'} script source`}
						rows={kind === 'naming' ? 2 : 6}
						value={selectedScript.source}
						oninput={(source) => updateScript({ source })}
					/>
				{/key}
			</div>
			{#if selectedScript.preset_origin === 'legacy_naming_template'}
				<p class="mt-3 rounded-lg border border-info/20 bg-info/5 p-2 text-xs text-muted">
					Carried over from v2. Imports use the script in Settings &gt; Library.
				</p>
			{/if}
		</div>
	{:else}
		<div class="rounded-xl border border-dashed border-base-content/15 p-4 text-sm text-muted">
			No {kind} scripts exist yet. Create one to begin.
		</div>
	{/if}
</div>

<style>
	.management-script-editor {
		display: grid;
		gap: 0.75rem;
	}

	.management-script-toolbar {
		display: flex;
		align-items: flex-end;
		gap: 0.5rem;
	}

	.management-script-workbench {
		padding: 0.85rem;
		border: 1px solid color-mix(in oklab, var(--color-library-manage) 18%, transparent);
		border-radius: 0.8rem;
		background: color-mix(in oklab, var(--color-base-200) 58%, var(--color-base-100));
	}

	@media (max-width: 639px) {
		.management-script-toolbar {
			align-items: stretch;
			flex-direction: column;
		}
	}
</style>
